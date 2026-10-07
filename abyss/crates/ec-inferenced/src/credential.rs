// SPDX-License-Identifier: AGPL-3.0-only
//! Getting the API key (and the Claude Code token) from brokerd.
//!
//! brokerd releases a value only through `Substitute` (S-08 §3.1), which it
//! allows to a `Proxy` peer. The router is the proxy for the model API: it
//! asks for `anthropic-api-key` at `api.anthropic.com:443` as
//! `agent:<package>`, holds the value for one HTTP call and drops it.
//! Nothing is cached: a rotate or revoke takes effect on the next request.
//!
//! The Claude Code backend asks the same way for `claude-code-token`, once per
//! session start (the sandboxed `claude` keeps it for the session's life; see
//! `claude_code`).

use crate::api::failure;
use crate::secret::ApiKey;
use ec_brokerd::broker::Status;
use ec_brokerd::wire::{Request, Response, SubstituteFields, SubstituteWire, MAX_MESSAGE};
use ec_inference_wire::Failure;
use rustix::net::{self, AddressFamily, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags, SocketType};
use std::path::PathBuf;

pub const SECRET_NAME: &str = "anthropic-api-key";
/// The Claude Code login token (`claude setup-token`), released the same way
/// and bound to the same host.
pub const TOKEN_NAME: &str = "claude-code-token";
pub const HOST: &str = "api.anthropic.com";
pub const PORT: u16 = 443;

pub trait Credentials: Send + Sync {
    /// The key for a request made on behalf of `package`.
    fn api_key(&self, package: &str) -> Result<ApiKey, Failure>;

    /// The Claude Code OAuth token for a session started on behalf of
    /// `package`. Released once per session start and handed to the sandboxed
    /// `claude`, never cached here. The default answers "unavailable" so a
    /// test double that only serves the API backend need not implement it.
    fn claude_code_token(&self, _package: &str) -> Result<ApiKey, Failure> {
        Err(failure("backend_unavailable", "no Claude Code token source"))
    }
}

/// The Substitute request for `package`.
pub fn substitute_request(package: &str) -> Request {
    substitute_request_for(package, SECRET_NAME)
}

/// The Substitute request for the secret `name` (same host) on behalf of
/// `package`.
pub fn substitute_request_for(package: &str, name: &str) -> Request {
    Request::Substitute(SubstituteWire(SubstituteFields {
        principal: format!("agent:{package}"),
        grant: None,
        name: name.into(),
        host: HOST.into(),
        port: PORT,
        url: None,
        holds_use: true,
        erot: None,
    }))
}

/// A brokerd response to a key, or the Failure the caller should return.
pub fn interpret(resp: Response) -> Result<ApiKey, Failure> {
    interpret_with(
        resp,
        "the stored value is not usable as an API key: rotate it with `ec-secret rotate anthropic-api-key`",
        "no key stored: run `ec-secret add anthropic-api-key --bind host:api.anthropic.com`",
    )
}

/// A brokerd response to the Claude Code token, or the Failure to return.
pub fn interpret_token(resp: Response) -> Result<ApiKey, Failure> {
    interpret_with(
        resp,
        "the stored value is not usable as a Claude Code token: rotate it with `ec-secret rotate claude-code-token`",
        "no Claude Code token stored: run `claude setup-token`, then `ec-secret add claude-code-token --bind host:api.anthropic.com`",
    )
}

fn interpret_with(resp: Response, unusable: &str, none: &str) -> Result<ApiKey, Failure> {
    match resp {
        Response::Value { value, .. } => {
            ApiKey::from_bytes(&value).ok_or_else(|| failure("no_credential", unusable))
        }
        Response::Err(code) => Err(match Status::from_code(code) {
            Some(Status::BrokerLocked) => {
                failure("broker_locked", "brokerd is locked: run `ec-secret unlock`")
            }
            // brokerd answers an unknown name with no_capability and a name
            // not bound to this host with out_of_scope; both are "no usable key".
            Some(Status::NoCapability | Status::OutOfScope) => failure("no_credential", none),
            Some(Status::RateLimited) => failure("rate_limited", "brokerd is rate-limiting key requests"),
            Some(Status::InvalidArgument) => failure("bad_request", "brokerd refused the key request"),
            _ => failure(
                "backend_unavailable",
                format!("brokerd failed the key request (status {code})"),
            ),
        }),
        _ => Err(failure("backend_unavailable", "brokerd sent an unexpected reply")),
    }
}

/// The real client: SEQPACKET to `brokerd.sock`, one packet each way.
pub struct BrokerdClient {
    pub socket: PathBuf,
}

impl BrokerdClient {
    /// `$XDG_RUNTIME_DIR/eclipse/brokerd.sock`.
    pub fn from_env() -> Option<BrokerdClient> {
        let run = std::env::var_os("XDG_RUNTIME_DIR")?;
        Some(BrokerdClient {
            socket: PathBuf::from(run).join("eclipse/brokerd.sock"),
        })
    }

    fn exchange(&self, packet: &[u8]) -> Option<Vec<u8>> {
        let fd = net::socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .ok()?;
        net::connect(&fd, &SocketAddrUnix::new(&self.socket).ok()?).ok()?;
        net::send(&fd, packet, SendFlags::empty()).ok()?;
        let mut buf = zeroize::Zeroizing::new(vec![0u8; MAX_MESSAGE]);
        let (n, _) = net::recv(&fd, &mut buf[..], RecvFlags::empty()).ok()?;
        if n == 0 {
            return None;
        }
        // Moved out as a plain Vec for decode; the Zeroizing buffer wipes
        // the receive copy on drop.
        Some(buf[..n].to_vec())
    }

    fn substitute(
        &self,
        req: Request,
        interpret: fn(Response) -> Result<ApiKey, Failure>,
    ) -> Result<ApiKey, Failure> {
        let unavailable = || failure("backend_unavailable", "brokerd is not reachable");
        let packet = zeroize::Zeroizing::new(req.encode());
        let reply = self.exchange(&packet).ok_or_else(unavailable)?;
        let reply = zeroize::Zeroizing::new(reply);
        let resp = Response::decode(&reply).map_err(|_| unavailable())?;
        interpret(resp)
    }
}

impl Credentials for BrokerdClient {
    fn api_key(&self, package: &str) -> Result<ApiKey, Failure> {
        self.substitute(substitute_request(package), interpret)
    }

    fn claude_code_token(&self, package: &str) -> Result<ApiKey, Failure> {
        self.substitute(substitute_request_for(package, TOKEN_NAME), interpret_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_brokerd::wire::Secret;

    #[test]
    fn request_matches_the_brief() {
        let Request::Substitute(SubstituteWire(f)) = substitute_request("ec-claude-agent") else {
            panic!("not a substitute");
        };
        assert_eq!(f.principal, "agent:ec-claude-agent");
        assert_eq!(
            (f.name.as_str(), f.host.as_str(), f.port),
            ("anthropic-api-key", "api.anthropic.com", 443)
        );
        assert!(f.holds_use && f.grant.is_none() && f.url.is_none() && f.erot.is_none());
        // And it survives the wire.
        let again = Request::decode(&substitute_request("ec-claude-agent").encode()).unwrap();
        assert_eq!(again, substitute_request("ec-claude-agent"));
    }

    #[test]
    fn token_request_and_messages() {
        let Request::Substitute(SubstituteWire(f)) =
            substitute_request_for("ec-claude-code-agent", TOKEN_NAME)
        else {
            panic!("not a substitute");
        };
        assert_eq!(f.principal, "agent:ec-claude-code-agent");
        assert_eq!(
            (f.name.as_str(), f.host.as_str(), f.port),
            ("claude-code-token", "api.anthropic.com", 443)
        );
        let f = interpret_token(Response::Err(Status::NoCapability.code())).unwrap_err();
        assert_eq!(f.kind, "no_credential");
        assert_eq!(
            f.message,
            "no Claude Code token stored: run `claude setup-token`, then `ec-secret add claude-code-token --bind host:api.anthropic.com`"
        );
        let k = interpret_token(Response::Value {
            rotation_counter: 1,
            value: Secret::new(b"sk-ant-oat01-x\n".to_vec()),
        })
        .unwrap();
        assert_eq!(k.expose(), "sk-ant-oat01-x");
        assert_eq!(
            interpret_token(Response::Err(Status::BrokerLocked.code()))
                .unwrap_err()
                .kind,
            "broker_locked"
        );
    }

    #[test]
    fn statuses_map() {
        let k = interpret(Response::Value {
            rotation_counter: 1,
            value: Secret::new(b"sk-1".to_vec()),
        })
        .unwrap();
        assert_eq!(k.expose(), "sk-1");
        let f = interpret(Response::Err(Status::BrokerLocked.code())).unwrap_err();
        assert_eq!(f.kind, "broker_locked");
        assert_eq!(f.message, "brokerd is locked: run `ec-secret unlock`");
        let f = interpret(Response::Err(Status::NoCapability.code())).unwrap_err();
        assert_eq!(f.kind, "no_credential");
        assert!(f
            .message
            .starts_with("no key stored: run `ec-secret add anthropic-api-key"));
        assert_eq!(
            interpret(Response::Err(Status::OutOfScope.code()))
                .unwrap_err()
                .kind,
            "no_credential"
        );
        assert_eq!(
            interpret(Response::Err(Status::Internal.code()))
                .unwrap_err()
                .kind,
            "backend_unavailable"
        );
        assert_eq!(interpret(Response::Ok).unwrap_err().kind, "backend_unavailable");
    }
}
