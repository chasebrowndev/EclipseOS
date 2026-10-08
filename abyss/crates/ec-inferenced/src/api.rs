// SPDX-License-Identifier: AGPL-3.0-only
//! The API backend: one non-streaming `POST /v1/messages` per completion.
//!
//! Building the body, parsing the reply and mapping HTTP statuses are pure
//! functions; the transport is the [`Http`] trait so the server's tests need
//! no network. [`UreqHttp`] is the real one.

use crate::secret::ApiKey;
use ec_inference_wire::{Completion, Failure, Request, Value};
use serde_json::json;
use std::sync::{Arc, Once};
use std::time::Duration;

pub const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2023-06-01";
/// Server-side fallback beta: `fallbacks: "default"` lets the API answer from
/// a fallback model when the requested one is unavailable; the reply's
/// `model` says which one did.
pub const BETA: &str = "server-side-fallback-2026-07-01";
/// A long agentic turn with adaptive thinking can take minutes.
pub const TIMEOUT: Duration = Duration::from_secs(600);
/// Longest API error text carried into a Failure message.
const MAX_ERR_TEXT: usize = 500;

pub fn failure(kind: &str, message: impl Into<String>) -> Failure {
    Failure {
        kind: kind.into(),
        message: message.into(),
    }
}

/// The JSON body for `req`. `system` and `tools` are left out when empty.
pub fn build_body(req: &Request) -> Value {
    let mut b = serde_json::Map::new();
    b.insert("model".into(), json!(req.model));
    b.insert("max_tokens".into(), json!(req.max_tokens));
    if !req.system.is_empty() {
        b.insert("system".into(), json!(req.system));
    }
    b.insert("messages".into(), req.messages.clone());
    if req.tools.as_array().is_some_and(|t| !t.is_empty()) {
        b.insert("tools".into(), req.tools.clone());
    }
    b.insert("thinking".into(), json!({"type": "adaptive"}));
    b.insert("output_config".into(), json!({"effort": "medium"}));
    b.insert("fallbacks".into(), json!("default"));
    Value::Object(b)
}

/// What the transport hands back: any HTTP status with its body.
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A transport failure (no HTTP status was received).
#[derive(Debug, PartialEq, Eq)]
pub enum HttpError {
    Timeout,
    /// DNS, connect, TLS, truncated body. Never holds the key.
    Network(String),
}

/// The one network call. The key is handed over only for the call.
pub trait Http: Send + Sync {
    fn post_messages(&self, key: &ApiKey, body: &[u8]) -> Result<HttpResponse, HttpError>;
}

/// Run one completion against `http`. `Failure`s are already free of the key.
pub fn complete(http: &dyn Http, key: &ApiKey, req: &Request) -> Result<Completion, Failure> {
    let body = serde_json::to_vec(&build_body(req))
        .map_err(|_| failure("bad_request", "request is not serialisable"))?;
    match http.post_messages(key, &body) {
        Ok(r) => parse_response(r.status, &r.body, &req.model),
        Err(HttpError::Timeout) => Err(failure("timeout", "the API call timed out")),
        Err(HttpError::Network(m)) => Err(failure("provider_error", format!("cannot reach the API: {m}"))),
    }
}

/// Status and body to a `Completion` or a mapped `Failure`.
pub fn parse_response(status: u16, body: &[u8], requested_model: &str) -> Result<Completion, Failure> {
    if !(200..300).contains(&status) {
        return Err(status_failure(status, body));
    }
    let v: Value =
        serde_json::from_slice(body).map_err(|_| failure("provider_error", "the API reply was not JSON"))?;
    let content = v.get("content").cloned().filter(Value::is_array);
    let stop = v.get("stop_reason").and_then(Value::as_str);
    let (Some(content), Some(stop)) = (content, stop) else {
        return Err(failure(
            "provider_error",
            "the API reply had no content or stop_reason",
        ));
    };
    let usage = v.get("usage");
    let n = |k: &str| usage.and_then(|u| u.get(k)).and_then(Value::as_u64).unwrap_or(0);
    Ok(Completion {
        content,
        stop_reason: stop.to_owned(),
        model: v
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(requested_model)
            .to_owned(),
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cost_usd: None,
    })
}

/// HTTP error to a Failure. The API's `error.message` is safe to pass on (it
/// never contains the key); it is length-capped.
pub fn status_failure(status: u16, body: &[u8]) -> Failure {
    let detail = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message")?.as_str().map(str::to_owned))
        .map(|m| m.chars().take(MAX_ERR_TEXT).collect::<String>());
    match status {
        401 | 403 => failure(
            "no_credential",
            format!("the stored Anthropic API key was rejected (HTTP {status}): rotate it with `ec-secret rotate anthropic-api-key`"),
        ),
        429 => failure("rate_limited", "the API rate-limited this request (HTTP 429)"),
        400 | 413 | 422 => failure(
            "bad_request",
            format!("the API refused the request (HTTP {status}): {}", detail.unwrap_or_default()),
        ),
        _ => failure(
            "provider_error",
            match detail {
                Some(d) => format!("the API failed (HTTP {status}): {d}"),
                None => format!("the API failed (HTTP {status})"),
            },
        ),
    }
}

/// The real transport: ureq over rustls with the system's root certificates.
pub struct UreqHttp {
    agent: ureq::Agent,
}

impl UreqHttp {
    pub fn new() -> Result<UreqHttp, String> {
        static PROVIDER: Once = Once::new();
        // ureq's `rustls-no-provider` uses the process default; install ring.
        // An already-installed default is fine.
        PROVIDER.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
        let loaded = rustls_native_certs::load_native_certs();
        let certs: Vec<ureq::tls::Certificate<'static>> = loaded
            .certs
            .iter()
            .map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned())
            .collect();
        if certs.is_empty() {
            return Err("no system root certificates found".into());
        }
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::new_with_certs(&certs))
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // A 4xx/5xx is a reply to read, not a transport error.
            .http_status_as_error(false)
            .tls_config(tls)
            .build()
            .into();
        Ok(UreqHttp { agent })
    }
}

impl Http for UreqHttp {
    fn post_messages(&self, key: &ApiKey, body: &[u8]) -> Result<HttpResponse, HttpError> {
        let mut resp = self
            .agent
            .post(ENDPOINT)
            .header("content-type", "application/json")
            .header("x-api-key", key.expose())
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", BETA)
            .send(body)
            .map_err(map_err)?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_vec()
            .map_err(map_err)?;
        Ok(HttpResponse { status, body })
    }
}

fn map_err(e: ureq::Error) -> HttpError {
    match e {
        ureq::Error::Timeout(_) => HttpError::Timeout,
        other => HttpError::Network(other.to_string()),
    }
}

/// Keeps `Arc<dyn Http>` call sites short.
pub type SharedHttp = Arc<dyn Http>;

#[cfg(test)]
mod tests {
    use super::*;
    use ec_inference_wire::Backend;

    fn req() -> Request {
        Request {
            id: 1,
            task: "T".into(),
            package: "ec-claude-agent".into(),
            backend: Backend::Api,
            model: "claude-opus-5-5".into(),
            system: "be brief".into(),
            messages: json!([{"role": "user", "content": "hi"}]),
            tools: json!([{"name": "t", "description": "d", "input_schema": {"type": "object"}}]),
            max_tokens: 1000,
            account: String::new(),
        }
    }

    #[test]
    fn body_is_exact() {
        assert_eq!(
            build_body(&req()),
            json!({
                "model": "claude-opus-5-5",
                "max_tokens": 1000,
                "system": "be brief",
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{"name": "t", "description": "d", "input_schema": {"type": "object"}}],
                "thinking": {"type": "adaptive"},
                "output_config": {"effort": "medium"},
                "fallbacks": "default",
            })
        );
    }

    #[test]
    fn empty_system_and_tools_are_omitted() {
        let mut r = req();
        r.system.clear();
        r.tools = json!([]);
        let b = build_body(&r);
        assert!(b.get("system").is_none());
        assert!(b.get("tools").is_none());
        assert_eq!(b["fallbacks"], "default");
    }

    #[test]
    fn parses_text_and_tool_use() {
        let body = json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "model": "claude-fallback",
            "content": [
                {"type": "text", "text": "ok"},
                {"type": "tool_use", "id": "toolu_1", "name": "t", "input": {"a": 1}},
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 12, "output_tokens": 34},
        });
        let c = parse_response(200, body.to_string().as_bytes(), "claude-opus-5-5").unwrap();
        assert_eq!(c.stop_reason, "tool_use");
        assert_eq!(c.model, "claude-fallback");
        assert_eq!((c.input_tokens, c.output_tokens), (12, 34));
        assert_eq!(c.content[1]["type"], "tool_use");
        assert_eq!(c.content[1]["input"], json!({"a": 1}));
    }

    #[test]
    fn refusal_is_a_completion_with_that_stop_reason() {
        let body = json!({"content": [], "stop_reason": "refusal", "usage": {"input_tokens": 1, "output_tokens": 0}});
        let c = parse_response(200, body.to_string().as_bytes(), "m").unwrap();
        assert_eq!(c.stop_reason, "refusal");
        // No `model` in the reply: the requested one stands in.
        assert_eq!(c.model, "m");
    }

    #[test]
    fn a_malformed_success_is_a_provider_error() {
        assert_eq!(
            parse_response(200, b"nope", "m").unwrap_err().kind,
            "provider_error"
        );
        assert_eq!(
            parse_response(200, b"{\"content\":[]}", "m").unwrap_err().kind,
            "provider_error"
        );
    }

    #[test]
    fn statuses_map() {
        let err = json!({"type": "error", "error": {"type": "invalid_request_error", "message": "max_tokens too big"}});
        let e = err.to_string();
        for s in [401, 403] {
            let f = status_failure(s, e.as_bytes());
            assert_eq!(f.kind, "no_credential");
            assert!(f.message.contains("rejected"));
        }
        assert_eq!(status_failure(429, b"").kind, "rate_limited");
        let f = status_failure(400, e.as_bytes());
        assert_eq!(f.kind, "bad_request");
        assert!(f.message.contains("max_tokens too big"));
        for s in [500, 502, 503, 529] {
            assert_eq!(status_failure(s, b"<html>").kind, "provider_error");
        }
    }

    #[test]
    fn api_error_text_is_capped() {
        let long = "x".repeat(5000);
        let e = json!({"error": {"message": long}}).to_string();
        assert!(status_failure(400, e.as_bytes()).message.len() < 700);
    }

    struct Fixed(Result<(u16, Vec<u8>), HttpError>);
    impl Http for Fixed {
        fn post_messages(&self, _: &ApiKey, _: &[u8]) -> Result<HttpResponse, HttpError> {
            match &self.0 {
                Ok((s, b)) => Ok(HttpResponse {
                    status: *s,
                    body: b.clone(),
                }),
                Err(HttpError::Timeout) => Err(HttpError::Timeout),
                Err(HttpError::Network(m)) => Err(HttpError::Network(m.clone())),
            }
        }
    }

    #[test]
    fn transport_errors_map() {
        let k = ApiKey::from_bytes(b"sk-x").unwrap();
        let f = complete(&Fixed(Err(HttpError::Network("refused".into()))), &k, &req()).unwrap_err();
        assert_eq!(f.kind, "provider_error");
        let f = complete(&Fixed(Err(HttpError::Timeout)), &k, &req()).unwrap_err();
        assert_eq!(f.kind, "timeout");
    }
}
