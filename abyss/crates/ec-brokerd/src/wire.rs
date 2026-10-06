// SPDX-License-Identifier: AGPL-3.0-only
//! The `brokerd.sock` wire format: one canonical-CBOR map per SEQPACKET
//! packet (ADR 0044, via `ec-policy-eval::cbor`). Requests carry `op`;
//! responses carry `r`. No serde, no free text in an error: a failure is a
//! status number, so there is nowhere for a value to leak into one.
//!
//! Request fields (absent means the op does not take it):
//! `op`, `name`, `principal`, `grant` (16 bytes), `value`, `pass`, `nonce`,
//! `cancel`, `kind`, `bound` (array of `host:`/`url:`/`app:` text), `modes`,
//! `prompt`, `hint`, `host`, `port`, `url`, `role`, `cred`, `app`, `gen`,
//! `egen`, `use`, `seat`, `listed`, `expose`, `erot`, `target`.
//!
//! Message size is bounded by [`MAX_MESSAGE`]; the transport reads with
//! truncation detection and drops a peer that sends more.

use crate::bind::Binding;
use crate::broker::{
    Broker, FillReq, MaterializeReq, NodeRole, Status, SubstituteReq, TotpReq, UnlockAnswer, UnlockMethod,
    UnlockRequest,
};
use crate::gate::{Op, Peer};
use crate::record::{Kind, Meta, Mode, NewSecret};
use ec_policy_eval::cbor::{enc, MapBuilder, Reader, Writer};
use ec_policy_eval::Ulid;
use zeroize::Zeroizing;

pub const MAX_MESSAGE: usize = 32 * 1024;

/// Bytes that must not be printed, compared in the clear or left behind: the
/// wire layer's passphrases and values. `Debug` is redacted so a stray
/// `{:?}` in a log line cannot leak one.
#[derive(PartialEq, Eq)]
pub struct Secret(Zeroizing<Vec<u8>>);

impl Secret {
    pub fn new(v: Vec<u8>) -> Secret {
        Secret(Zeroizing::new(v))
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl std::ops::Deref for Secret {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<{} bytes>)", self.0.len())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    Status,
    BeginUnlock,
    UnlockAnswer {
        nonce: u64,
        passphrase: Option<Secret>,
        cancel: bool,
    },
    Init {
        pass: Option<Secret>,
    },
    Lock,
    Add {
        spec: NewSecret,
        value: Secret,
    },
    Rotate {
        name: String,
        value: Secret,
    },
    Revoke {
        name: String,
    },
    List,
    Substitute(SubstituteWire),
    FieldFill(FillWire),
    Materialize(MaterializeWire),
    Totp(TotpWire),
}

// The request structs in `broker` hold no `PartialEq` (they are not data a
// caller compares), so the wire layer keeps comparable mirrors for its tests.
#[derive(Debug, PartialEq, Eq)]
pub struct SubstituteWire(pub SubstituteFields);
#[derive(Debug, PartialEq, Eq)]
pub struct FillWire(pub FillFields);
#[derive(Debug, PartialEq, Eq)]
pub struct MaterializeWire(pub MaterializeFields);
#[derive(Debug, PartialEq, Eq)]
pub struct TotpWire(pub TotpFields);

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SubstituteFields {
    pub principal: String,
    pub grant: Option<[u8; 16]>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub url: Option<String>,
    pub holds_use: bool,
    pub erot: Option<u64>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct FillFields {
    pub principal: String,
    pub grant: Option<[u8; 16]>,
    pub name: String,
    /// `password`, `textfield` or `other`.
    pub role: String,
    pub cred: bool,
    pub app: String,
    pub url: Option<String>,
    pub gen: u64,
    pub egen: u64,
    pub holds_use: bool,
    pub holds_seat: bool,
    pub listed: bool,
    pub erot: Option<u64>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct MaterializeFields {
    pub principal: String,
    pub grant: Option<[u8; 16]>,
    pub name: String,
    pub target: String,
    pub holds_expose: bool,
    pub erot: Option<u64>,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct TotpFields {
    pub principal: String,
    pub grant: Option<[u8; 16]>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub holds_use: bool,
}

/// A record's metadata as listed: no value, ever.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct MetaView {
    pub name: String,
    pub id: [u8; 16],
    pub kind: String,
    pub bound_to: Vec<String>,
    pub modes: Vec<String>,
    pub requires_prompt: bool,
    pub rotation_counter: u64,
}

impl From<&Meta> for MetaView {
    fn from(m: &Meta) -> Self {
        MetaView {
            name: m.name.clone(),
            id: m.id.0,
            kind: m.kind.as_str().into(),
            bound_to: m.bound_to.iter().map(Binding::to_text).collect(),
            modes: m.modes.iter().map(|x| x.as_str().into()).collect(),
            requires_prompt: m.requires_prompt,
            rotation_counter: m.rotation_counter,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Response {
    Ok,
    State {
        unlocked: bool,
        initialised: bool,
        passphrase: bool,
    },
    Unlock {
        nonce: u64,
        passphrase: bool,
        attempts_left: u32,
        expires: u64,
    },
    Unlocked(bool),
    Value {
        rotation_counter: u64,
        value: Secret,
    },
    Code {
        code: String,
        life_s: u64,
    },
    List(Vec<MetaView>),
    Err(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireError;

impl From<ec_policy_eval::cbor::Error> for WireError {
    fn from(_: ec_policy_eval::cbor::Error) -> Self {
        WireError
    }
}

// ---- decoding ------------------------------------------------------------

#[derive(Default)]
struct F {
    op: Option<String>,
    name: Option<String>,
    principal: Option<String>,
    grant: Option<[u8; 16]>,
    value: Option<Secret>,
    pass: Option<Secret>,
    nonce: Option<u64>,
    cancel: Option<bool>,
    kind: Option<String>,
    bound: Option<Vec<String>>,
    modes: Option<Vec<String>>,
    prompt: Option<bool>,
    hint: Option<u64>,
    host: Option<String>,
    port: Option<u64>,
    url: Option<String>,
    role: Option<String>,
    cred: Option<bool>,
    app: Option<String>,
    gen: Option<u64>,
    egen: Option<u64>,
    use_: Option<bool>,
    seat: Option<bool>,
    listed: Option<bool>,
    expose: Option<bool>,
    erot: Option<u64>,
    target: Option<String>,
}

fn texts(r: &mut Reader<'_>, max: u64) -> Result<Vec<String>, WireError> {
    let n = r.array_len()?;
    if n > max {
        return Err(WireError);
    }
    (0..n)
        .map(|_| Ok::<String, WireError>(r.text()?.to_owned()))
        .collect()
}

fn parse(buf: &[u8]) -> Result<F, WireError> {
    if buf.len() > MAX_MESSAGE {
        return Err(WireError);
    }
    let mut r = Reader::new(buf);
    let n = r.map_begin()?;
    let mut f = F::default();
    for _ in 0..n {
        let s = |r: &mut Reader<'_>| -> Result<Option<String>, WireError> { Ok(Some(r.text()?.to_owned())) };
        match r.key()? {
            "app" => f.app = s(&mut r)?,
            "bound" => f.bound = Some(texts(&mut r, 32)?),
            "cancel" => f.cancel = Some(r.bool()?),
            "cred" => f.cred = Some(r.bool()?),
            "egen" => f.egen = Some(r.u64()?),
            "erot" => f.erot = Some(r.u64()?),
            "expose" => f.expose = Some(r.bool()?),
            "gen" => f.gen = Some(r.u64()?),
            "grant" => f.grant = Some(r.byte_array::<16>()?),
            "hint" => f.hint = Some(r.u64()?),
            "host" => f.host = s(&mut r)?,
            "kind" => f.kind = s(&mut r)?,
            "listed" => f.listed = Some(r.bool()?),
            "modes" => f.modes = Some(texts(&mut r, 3)?),
            "name" => f.name = s(&mut r)?,
            "nonce" => f.nonce = Some(r.u64()?),
            "op" => f.op = s(&mut r)?,
            "pass" => f.pass = Some(Secret::new(r.bytes()?.to_vec())),
            "port" => f.port = Some(r.u64()?),
            "principal" => f.principal = s(&mut r)?,
            "prompt" => f.prompt = Some(r.bool()?),
            "role" => f.role = s(&mut r)?,
            "seat" => f.seat = Some(r.bool()?),
            "target" => f.target = s(&mut r)?,
            "url" => f.url = s(&mut r)?,
            "use" => f.use_ = Some(r.bool()?),
            "value" => f.value = Some(Secret::new(r.bytes()?.to_vec())),
            _ => return Err(WireError),
        }
    }
    r.map_end()?;
    r.finish()?;
    Ok(f)
}

fn port(p: Option<u64>) -> Result<u16, WireError> {
    p.and_then(|p| u16::try_from(p).ok()).ok_or(WireError)
}

impl Request {
    pub fn decode(buf: &[u8]) -> Result<Request, WireError> {
        let mut f = parse(buf)?;
        let need = |o: Option<String>| o.ok_or(WireError);
        let op = need(f.op.take())?;
        Ok(match op.as_str() {
            "status" => Request::Status,
            "begin_unlock" => Request::BeginUnlock,
            "unlock_answer" => Request::UnlockAnswer {
                nonce: f.nonce.ok_or(WireError)?,
                passphrase: f.pass.take(),
                cancel: f.cancel.unwrap_or(false),
            },
            "init" => Request::Init { pass: f.pass.take() },
            "lock" => Request::Lock,
            "list" => Request::List,
            "revoke" => Request::Revoke {
                name: need(f.name.take())?,
            },
            "rotate" => Request::Rotate {
                name: need(f.name.take())?,
                value: f.value.take().ok_or(WireError)?,
            },
            "add" => {
                let mut bound_to = Vec::new();
                for b in f.bound.take().ok_or(WireError)? {
                    bound_to.push(Binding::parse(&b).map_err(|_| WireError)?);
                }
                let mut modes = Vec::new();
                for m in f.modes.take().unwrap_or_default() {
                    modes.push(Mode::parse(&m).ok_or(WireError)?);
                }
                Request::Add {
                    spec: NewSecret {
                        name: need(f.name.take())?,
                        kind: Kind::parse(&need(f.kind.take())?).ok_or(WireError)?,
                        bound_to,
                        modes,
                        requires_prompt: f.prompt.unwrap_or(false),
                        rotation_hint_days: match f.hint {
                            Some(h) => Some(u32::try_from(h).map_err(|_| WireError)?),
                            None => None,
                        },
                    },
                    value: f.value.take().ok_or(WireError)?,
                }
            }
            "substitute" => Request::Substitute(SubstituteWire(SubstituteFields {
                principal: need(f.principal.take())?,
                grant: f.grant,
                name: need(f.name.take())?,
                host: need(f.host.take())?,
                port: port(f.port)?,
                url: f.url.take(),
                holds_use: f.use_.unwrap_or(false),
                erot: f.erot,
            })),
            "field_fill" => Request::FieldFill(FillWire(FillFields {
                principal: need(f.principal.take())?,
                grant: f.grant,
                name: need(f.name.take())?,
                role: need(f.role.take())?,
                cred: f.cred.unwrap_or(false),
                app: need(f.app.take())?,
                url: f.url.take(),
                gen: f.gen.ok_or(WireError)?,
                egen: f.egen.ok_or(WireError)?,
                holds_use: f.use_.unwrap_or(false),
                holds_seat: f.seat.unwrap_or(false),
                listed: f.listed.unwrap_or(false),
                erot: f.erot,
            })),
            "materialize" => Request::Materialize(MaterializeWire(MaterializeFields {
                principal: need(f.principal.take())?,
                grant: f.grant,
                name: need(f.name.take())?,
                target: need(f.target.take())?,
                holds_expose: f.expose.unwrap_or(false),
                erot: f.erot,
            })),
            "totp" => Request::Totp(TotpWire(TotpFields {
                principal: need(f.principal.take())?,
                grant: f.grant,
                name: need(f.name.take())?,
                host: need(f.host.take())?,
                port: port(f.port)?,
                holds_use: f.use_.unwrap_or(false),
            })),
            _ => return Err(WireError),
        })
    }

    pub fn op(&self) -> Op {
        match self {
            Request::Status => Op::Status,
            Request::BeginUnlock => Op::BeginUnlock,
            Request::UnlockAnswer { .. } => Op::AnswerUnlock,
            Request::Init { .. } => Op::Init,
            Request::Lock => Op::Lock,
            Request::Add { .. } => Op::Add,
            Request::Rotate { .. } => Op::Rotate,
            Request::Revoke { .. } => Op::Revoke,
            Request::List => Op::List,
            Request::Substitute(_) => Op::Substitute,
            Request::FieldFill(_) => Op::FieldFill,
            Request::Materialize(_) => Op::Materialize,
            Request::Totp(_) => Op::Totp,
        }
    }

    /// Client-side encoding (`ec-secret`, the compositor, the proxy, tests).
    pub fn encode(&self) -> Vec<u8> {
        let mut m = MapBuilder::new();
        let t = |s: &str| enc(|w| w.text(s));
        let b = |v: bool| enc(|w| w.bool(v));
        let u = |v: u64| enc(|w| w.u64(v));
        let by = |v: &[u8]| enc(|w| w.bytes(v));
        let ts = |v: Vec<String>| {
            enc(|w| {
                w.array(v.len());
                for s in &v {
                    w.text(s);
                }
            })
        };
        let op = |m: &mut MapBuilder, o: &str| m.insert("op", enc(|w| w.text(o)));
        match self {
            Request::Status => op(&mut m, "status"),
            Request::BeginUnlock => op(&mut m, "begin_unlock"),
            Request::Lock => op(&mut m, "lock"),
            Request::List => op(&mut m, "list"),
            Request::UnlockAnswer {
                nonce,
                passphrase,
                cancel,
            } => {
                op(&mut m, "unlock_answer");
                m.insert("nonce", u(*nonce));
                m.insert_opt("pass", passphrase.as_ref().map(|p| by(p)));
                if *cancel {
                    m.insert("cancel", b(true));
                }
            }
            Request::Init { pass } => {
                op(&mut m, "init");
                m.insert_opt("pass", pass.as_ref().map(|p| by(p)));
            }
            Request::Revoke { name } => {
                op(&mut m, "revoke");
                m.insert("name", t(name));
            }
            Request::Rotate { name, value } => {
                op(&mut m, "rotate");
                m.insert("name", t(name));
                m.insert("value", by(value));
            }
            Request::Add { spec, value } => {
                op(&mut m, "add");
                m.insert("name", t(&spec.name));
                m.insert("kind", t(spec.kind.as_str()));
                m.insert("bound", ts(spec.bound_to.iter().map(Binding::to_text).collect()));
                m.insert(
                    "modes",
                    ts(spec.modes.iter().map(|x| x.as_str().to_owned()).collect()),
                );
                m.insert("prompt", b(spec.requires_prompt));
                m.insert_opt("hint", spec.rotation_hint_days.map(|h| u(h as u64)));
                m.insert("value", by(value));
            }
            Request::Substitute(SubstituteWire(x)) => {
                op(&mut m, "substitute");
                m.insert("principal", t(&x.principal));
                m.insert_opt("grant", x.grant.map(|g| by(&g)));
                m.insert("name", t(&x.name));
                m.insert("host", t(&x.host));
                m.insert("port", u(x.port as u64));
                m.insert_opt("url", x.url.as_deref().map(t));
                m.insert("use", b(x.holds_use));
                m.insert_opt("erot", x.erot.map(u));
            }
            Request::FieldFill(FillWire(x)) => {
                op(&mut m, "field_fill");
                m.insert("principal", t(&x.principal));
                m.insert_opt("grant", x.grant.map(|g| by(&g)));
                m.insert("name", t(&x.name));
                m.insert("role", t(&x.role));
                m.insert("cred", b(x.cred));
                m.insert("app", t(&x.app));
                m.insert_opt("url", x.url.as_deref().map(t));
                m.insert("gen", u(x.gen));
                m.insert("egen", u(x.egen));
                m.insert("use", b(x.holds_use));
                m.insert("seat", b(x.holds_seat));
                m.insert("listed", b(x.listed));
                m.insert_opt("erot", x.erot.map(u));
            }
            Request::Materialize(MaterializeWire(x)) => {
                op(&mut m, "materialize");
                m.insert("principal", t(&x.principal));
                m.insert_opt("grant", x.grant.map(|g| by(&g)));
                m.insert("name", t(&x.name));
                m.insert("target", t(&x.target));
                m.insert("expose", b(x.holds_expose));
                m.insert_opt("erot", x.erot.map(u));
            }
            Request::Totp(TotpWire(x)) => {
                op(&mut m, "totp");
                m.insert("principal", t(&x.principal));
                m.insert_opt("grant", x.grant.map(|g| by(&g)));
                m.insert("name", t(&x.name));
                m.insert("host", t(&x.host));
                m.insert("port", u(x.port as u64));
                m.insert("use", b(x.holds_use));
            }
        }
        m.finish()
    }
}

// ---- responses -----------------------------------------------------------

impl Response {
    /// The encoded response. Wrapped so the bytes of a `Value` are zeroed
    /// once sent. The `Value` arm writes straight into a buffer sized up
    /// front, so no reallocation leaves an unzeroed copy behind.
    pub fn encode(&self) -> Zeroizing<Vec<u8>> {
        if let Response::Value {
            rotation_counter,
            value,
        } = self
        {
            // Canonical key order: encoded length, then bytes: r, rc, value.
            let mut w = Writer::with_capacity(value.len() + 64);
            w.map(3);
            w.text("r");
            w.text("value");
            w.text("rc");
            w.u64(*rotation_counter);
            w.text("value");
            w.bytes(value);
            return Zeroizing::new(w.finish());
        }
        let mut m = MapBuilder::new();
        let r = |k: &str| enc(|w| w.text(k));
        match self {
            Response::Ok => m.insert("r", r("ok")),
            Response::Err(s) => {
                m.insert("r", r("err"));
                m.insert("status", enc(|w| w.u64(*s)));
            }
            Response::State {
                unlocked,
                initialised,
                passphrase,
            } => {
                m.insert("r", r("state"));
                m.insert("unlocked", enc(|w| w.bool(*unlocked)));
                m.insert("init", enc(|w| w.bool(*initialised)));
                m.insert("pass", enc(|w| w.bool(*passphrase)));
            }
            Response::Unlock {
                nonce,
                passphrase,
                attempts_left,
                expires,
            } => {
                m.insert("r", r("unlock"));
                m.insert("nonce", enc(|w| w.u64(*nonce)));
                m.insert("pass", enc(|w| w.bool(*passphrase)));
                m.insert("left", enc(|w| w.u64(*attempts_left as u64)));
                m.insert("expires", enc(|w| w.u64(*expires)));
            }
            Response::Unlocked(ok) => {
                m.insert("r", r("unlocked"));
                m.insert("ok", enc(|w| w.bool(*ok)));
            }
            Response::Code { code, life_s } => {
                m.insert("r", r("code"));
                m.insert("code", enc(|w| w.text(code)));
                m.insert("life", enc(|w| w.u64(*life_s)));
            }
            Response::List(items) => {
                m.insert("r", r("list"));
                m.insert(
                    "items",
                    enc(|w| {
                        w.array(items.len());
                        for i in items {
                            let mut e = MapBuilder::new();
                            e.insert("name", enc(|w| w.text(&i.name)));
                            e.insert("id", enc(|w| w.bytes(&i.id)));
                            e.insert("kind", enc(|w| w.text(&i.kind)));
                            e.insert(
                                "bound",
                                enc(|w| {
                                    w.array(i.bound_to.len());
                                    i.bound_to.iter().for_each(|s| w.text(s));
                                }),
                            );
                            e.insert(
                                "modes",
                                enc(|w| {
                                    w.array(i.modes.len());
                                    i.modes.iter().for_each(|s| w.text(s));
                                }),
                            );
                            e.insert("prompt", enc(|w| w.bool(i.requires_prompt)));
                            e.insert("rc", enc(|w| w.u64(i.rotation_counter)));
                            w.raw(&e.finish());
                        }
                    }),
                );
            }
            Response::Value { .. } => unreachable!("handled above"),
        }
        Zeroizing::new(m.finish())
    }

    pub fn decode(buf: &[u8]) -> Result<Response, WireError> {
        let mut r = Reader::new(buf);
        let n = r.map_begin()?;
        let (mut kind, mut status, mut rc, mut value, mut code, mut life) =
            (None, None, None, None, None, None);
        let (mut unlocked, mut init, mut pass, mut nonce, mut left, mut expires, mut ok) =
            (None, None, None, None, None, None, None);
        let mut items = Vec::new();
        for _ in 0..n {
            match r.key()? {
                "r" => kind = Some(r.text()?.to_owned()),
                "status" => status = Some(r.u64()?),
                "rc" => rc = Some(r.u64()?),
                "value" => value = Some(Secret::new(r.bytes()?.to_vec())),
                "code" => code = Some(r.text()?.to_owned()),
                "life" => life = Some(r.u64()?),
                "unlocked" => unlocked = Some(r.bool()?),
                "init" => init = Some(r.bool()?),
                "pass" => pass = Some(r.bool()?),
                "nonce" => nonce = Some(r.u64()?),
                "left" => left = Some(r.u64()?),
                "expires" => expires = Some(r.u64()?),
                "ok" => ok = Some(r.bool()?),
                "items" => {
                    for _ in 0..r.array_len()? {
                        let k = r.map_begin()?;
                        let mut v = MetaView {
                            name: String::new(),
                            id: [0; 16],
                            kind: String::new(),
                            bound_to: vec![],
                            modes: vec![],
                            requires_prompt: false,
                            rotation_counter: 0,
                        };
                        for _ in 0..k {
                            match r.key()? {
                                "name" => v.name = r.text()?.to_owned(),
                                "id" => v.id = r.byte_array::<16>()?,
                                "kind" => v.kind = r.text()?.to_owned(),
                                "bound" => v.bound_to = texts(&mut r, 32)?,
                                "modes" => v.modes = texts(&mut r, 3)?,
                                "prompt" => v.requires_prompt = r.bool()?,
                                "rc" => v.rotation_counter = r.u64()?,
                                _ => return Err(WireError),
                            }
                        }
                        r.map_end()?;
                        items.push(v);
                    }
                }
                _ => return Err(WireError),
            }
        }
        r.map_end()?;
        r.finish()?;
        Ok(match kind.ok_or(WireError)?.as_str() {
            "ok" => Response::Ok,
            "err" => Response::Err(status.ok_or(WireError)?),
            "state" => Response::State {
                unlocked: unlocked.ok_or(WireError)?,
                initialised: init.ok_or(WireError)?,
                passphrase: pass.ok_or(WireError)?,
            },
            "unlock" => Response::Unlock {
                nonce: nonce.ok_or(WireError)?,
                passphrase: pass.ok_or(WireError)?,
                attempts_left: u32::try_from(left.ok_or(WireError)?).map_err(|_| WireError)?,
                expires: expires.ok_or(WireError)?,
            },
            "unlocked" => Response::Unlocked(ok.ok_or(WireError)?),
            "value" => Response::Value {
                rotation_counter: rc.ok_or(WireError)?,
                value: value.ok_or(WireError)?,
            },
            "code" => Response::Code {
                code: code.ok_or(WireError)?,
                life_s: life.ok_or(WireError)?,
            },
            "list" => Response::List(items),
            _ => return Err(WireError),
        })
    }
}

// ---- dispatch ------------------------------------------------------------

fn ulid(g: Option<[u8; 16]>) -> Option<Ulid> {
    g.map(Ulid)
}

fn role(s: &str) -> NodeRole {
    match s {
        "password" => NodeRole::Password,
        "textfield" => NodeRole::TextField,
        _ => NodeRole::Other,
    }
}

fn err(s: Status) -> Response {
    Response::Err(s.code())
}

impl Broker {
    /// One packet in, one packet out. A packet that does not decode is
    /// `invalid_argument`; the transport has already identified `peer`.
    pub fn handle(&mut self, peer: Peer, packet: &[u8], now: u64) -> Zeroizing<Vec<u8>> {
        let resp = match Request::decode(packet) {
            Err(_) => err(Status::InvalidArgument),
            Ok(req) => self.dispatch(peer, req, now),
        };
        resp.encode()
    }

    fn dispatch(&mut self, peer: Peer, req: Request, now: u64) -> Response {
        let res: Result<Response, Status> = (|| {
            Ok(match req {
                Request::Status => Response::State {
                    unlocked: self.is_unlocked(),
                    initialised: self.is_initialised(),
                    passphrase: self.sealer_needs_passphrase(),
                },
                Request::BeginUnlock => {
                    let UnlockRequest {
                        nonce,
                        method,
                        attempts_left,
                        expires_unix,
                    } = self.begin_unlock(peer, now)?;
                    Response::Unlock {
                        nonce,
                        passphrase: method == UnlockMethod::Passphrase,
                        attempts_left,
                        expires: expires_unix,
                    }
                }
                Request::UnlockAnswer {
                    nonce,
                    passphrase,
                    cancel,
                } => {
                    let ans = if cancel {
                        UnlockAnswer::Cancel { nonce }
                    } else {
                        UnlockAnswer::Submit { nonce, passphrase }
                    };
                    Response::Unlocked(
                        self.answer_unlock(peer, ans, now)? == crate::broker::UnlockOutcome::Unlocked,
                    )
                }
                Request::Init { pass } => {
                    self.initialize(peer, pass.as_deref())?;
                    Response::Ok
                }
                Request::Lock => {
                    self.lock(peer)?;
                    Response::Ok
                }
                Request::List => Response::List(self.list(peer)?.iter().map(MetaView::from).collect()),
                Request::Add { spec, value } => {
                    self.add(peer, &spec, &value, now)?;
                    Response::Ok
                }
                Request::Rotate { name, value } => {
                    self.rotate(peer, &name, &value)?;
                    Response::Ok
                }
                Request::Revoke { name } => {
                    self.revoke(peer, &name)?;
                    Response::Ok
                }
                Request::Substitute(SubstituteWire(x)) => {
                    let r = self.substitute(
                        peer,
                        &SubstituteReq {
                            principal: x.principal,
                            grant_id: ulid(x.grant),
                            name: x.name,
                            host: x.host,
                            port: x.port,
                            url: x.url,
                            holds_secret_use: x.holds_use,
                            expected_rotation: x.erot,
                        },
                    )?;
                    released(r)
                }
                Request::FieldFill(FillWire(x)) => {
                    let r = self.field_fill(
                        peer,
                        &FillReq {
                            principal: x.principal,
                            grant_id: ulid(x.grant),
                            name: x.name,
                            role: role(&x.role),
                            credential: x.cred,
                            app_id: x.app,
                            url: x.url,
                            generation: x.gen,
                            expected_generation: x.egen,
                            holds_secret_use: x.holds_use,
                            holds_seat_text: x.holds_seat,
                            app_listed: x.listed,
                            expected_rotation: x.erot,
                        },
                    )?;
                    released(r)
                }
                Request::Materialize(MaterializeWire(x)) => {
                    let r = self.materialize(
                        peer,
                        &MaterializeReq {
                            principal: x.principal,
                            grant_id: ulid(x.grant),
                            name: x.name,
                            target: x.target,
                            holds_secret_expose: x.holds_expose,
                            expected_rotation: x.erot,
                        },
                    )?;
                    released(r)
                }
                Request::Totp(TotpWire(x)) => {
                    let (code, life_s) = self.totp(
                        peer,
                        &TotpReq {
                            principal: x.principal,
                            grant_id: ulid(x.grant),
                            name: x.name,
                            host: x.host,
                            port: x.port,
                            holds_secret_use: x.holds_use,
                        },
                        now,
                    )?;
                    Response::Code { code, life_s }
                }
            })
        })();
        res.unwrap_or_else(err)
    }
}

fn released(r: crate::broker::Released) -> Response {
    Response::Value {
        rotation_counter: r.rotation_counter,
        value: Secret::new(r.value.as_slice().to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let reqs = vec![
            Request::Status,
            Request::BeginUnlock,
            Request::Lock,
            Request::List,
            Request::Revoke { name: "a".into() },
            Request::UnlockAnswer {
                nonce: 7,
                passphrase: Some(Secret::new(b"pw".to_vec())),
                cancel: false,
            },
            Request::Init { pass: None },
            Request::Rotate {
                name: "a".into(),
                value: Secret::new(b"v".to_vec()),
            },
            Request::Substitute(SubstituteWire(SubstituteFields {
                principal: "agent:a".into(),
                grant: Some([1; 16]),
                name: "n".into(),
                host: "api.acme.com".into(),
                port: 443,
                url: None,
                holds_use: true,
                erot: Some(2),
            })),
            Request::FieldFill(FillWire(FillFields {
                principal: "agent:a".into(),
                grant: None,
                name: "n".into(),
                role: "password".into(),
                cred: false,
                app: "org.x".into(),
                url: Some("https://a.com/".into()),
                gen: 4,
                egen: 4,
                holds_use: true,
                holds_seat: true,
                listed: true,
                erot: None,
            })),
        ];
        for r in reqs {
            let b = r.encode();
            assert_eq!(Request::decode(&b).unwrap(), r);
        }
    }

    #[test]
    fn responses_round_trip_and_value_is_canonical() {
        let rs = vec![
            Response::Ok,
            Response::Err(20),
            Response::State {
                unlocked: true,
                initialised: true,
                passphrase: false,
            },
            Response::Unlock {
                nonce: 9,
                passphrase: true,
                attempts_left: 5,
                expires: 100,
            },
            Response::Unlocked(true),
            Response::Code {
                code: "123456".into(),
                life_s: 12,
            },
            Response::Value {
                rotation_counter: 2,
                value: Secret::new(b"secret".to_vec()),
            },
            Response::List(vec![MetaView {
                name: "n".into(),
                id: [1; 16],
                kind: "bearer".into(),
                bound_to: vec!["host:a.com:443".into()],
                modes: vec!["proxy_header".into()],
                requires_prompt: false,
                rotation_counter: 0,
            }]),
        ];
        for r in rs {
            let b = r.encode();
            assert_eq!(Response::decode(&b).unwrap(), r);
        }
    }

    #[test]
    fn junk_unknown_keys_and_oversize_are_refused() {
        assert!(Request::decode(b"\xff").is_err());
        let unknown = enc(|w| {
            w.map(1);
            w.text("zz");
            w.u64(1);
        });
        assert!(Request::decode(&unknown).is_err());
        assert!(Request::decode(&vec![0u8; MAX_MESSAGE + 1]).is_err());
        let bad_op = MapBuilder::new();
        let mut bad_op = bad_op;
        bad_op.insert("op", enc(|w| w.text("dance")));
        assert!(Request::decode(&bad_op.finish()).is_err());
    }
}
