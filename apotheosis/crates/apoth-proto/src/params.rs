// SPDX-License-Identifier: Apache-2.0
//! Typed request parameters, one struct per method, all `deny_unknown_fields`.
//!
//! Targets (T-12 §3) are T-11 §7 match-language strings in `target`; omitted means
//! `self`. This crate bounds their length; the match grammar is checked by the server.
//! Only `action.run`, `config.*`, and `notify` reuse `apoth-action` (they *are* registry
//! Actions); `launch` and `input.send-keys` are socket-level requests.

use std::collections::BTreeMap;

use apoth_action::{
    Action, Call, Class, ClassContext, ClassSet, Classification, ErrorKind, EventKind, KeyChord,
    OverrideClass, ParseError, Refusal,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::caps::Cap;
use crate::error::{ErrorCode, WireError};
use crate::limits::*;
use crate::method::Method;

fn is_false(b: &bool) -> bool {
    !*b
}

/// `{}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// Params that carry only a target.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetOnly {
    /// Match expression; `self` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// `hello {proto, client, want}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hello {
    /// Protocol version the client speaks.
    pub proto: u32,
    /// Client name (informational; identity comes from the peer, never from this).
    pub client: String,
    /// Capability classes the client asks for.
    #[serde(default)]
    pub want: Vec<Cap>,
}

/// `action.run {action, target}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRun {
    /// One Action in the textual action syntax (T-11 §1).
    pub action: String,
    /// Match expression; `self` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// `input.send-keys {keys, target}`: chords encoded to PTY bytes by `apoth-input`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendKeys {
    /// Key chords in `KeyChord` text form (`ctrl+c`).
    pub keys: Vec<String>,
    /// Match expression; `self` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// What part of the pane `pane.text` returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Extent {
    /// The visible screen.
    #[default]
    Screen,
    /// Scrollback only.
    Scrollback,
    /// Scrollback and screen.
    All,
    /// The selection.
    Selection,
}

/// `pane.text {extent, sgr, wrap}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneText {
    /// Match expression; `self` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Region.
    #[serde(default)]
    pub extent: Extent,
    /// Include SGR escapes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sgr: bool,
    /// Join soft-wrapped rows into logical lines.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wrap: bool,
}

/// `pane.blocks {limit}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneBlocks {
    /// Match expression; `self` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Newest N records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `config.get {key}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigGet {
    /// Dotted kebab-case key.
    pub key: String,
}

/// `config.effective {key, target}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigEffective {
    /// One key, or all when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Scope the value is resolved for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// `subscribe {events, match}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subscribe {
    /// Event names from the closed `apoth-action` vocabulary.
    pub events: Vec<String>,
    /// T-11 §7 filter on event fields.
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub match_expr: Option<String>,
}

/// `unsubscribe {subscription}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unsubscribe {
    /// Id returned by `subscribe`.
    pub subscription: u64,
}

/// `pane.watch {since}` (R2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneWatch {
    /// Match expression; `self` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Row change sequence number to resume after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
}

/// `config.set {key, value}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSet {
    /// Dotted kebab-case key. `socket.*` (override class `grant`) is refused at decode.
    pub key: String,
    /// New value in config text form.
    pub value: String,
}

/// `config.reset {key}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigReset {
    /// Dotted kebab-case key.
    pub key: String,
}

/// `config.save-override {scope}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSaveOverride {
    /// Scope (`Scope` text of `apoth-action`); the server's default when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// `notify {title, body, urgency}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notify {
    /// Title.
    pub title: String,
    /// Body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Urgency name (`Urgency` text of `apoth-action`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<String>,
}

/// `launch`: the second-launch hand-off (T-12 §5), one request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    /// Command to run (`-e argv…`); the profile's shell when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv: Vec<String>,
    /// Working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Environment of the launching process.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Activation token (xdg-activation) for focus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_token: Option<String>,
    /// `-o key=value` overrides.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub overrides: BTreeMap<String, String>,
    /// `--title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `--app-id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// `--profile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// `--layout`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// `--hold`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hold: bool,
    /// `--wait`: the reply is deferred until the child exits and carries its exit code.
    #[serde(default, skip_serializing_if = "is_false")]
    pub wait: bool,
}

/// All typed params.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Params {
    Hello(Hello),
    ActionRun(ActionRun),
    InputSendKeys(SendKeys),
    Tree(TargetOnly),
    PaneInfo(TargetOnly),
    PaneProcesses(TargetOnly),
    PaneText(PaneText),
    PaneBlocks(PaneBlocks),
    ConfigGet(ConfigGet),
    ConfigEffective(ConfigEffective),
    KeyboardState(Empty),
    GraphicsList(TargetOnly),
    MediaList(TargetOnly),
    WaylandInfo(Empty),
    RenderStatus(Empty),
    Version(Empty),
    Subscribe(Subscribe),
    Unsubscribe(Unsubscribe),
    PaneWatch(PaneWatch),
    ConfigSet(ConfigSet),
    ConfigReset(ConfigReset),
    ConfigReload(Empty),
    ConfigPersist(ConfigSet),
    ConfigSaveOverride(ConfigSaveOverride),
    Notify(Notify),
    Launch(Box<Launch>),
}

fn de<T: DeserializeOwned>(v: Value) -> Result<T, WireError> {
    serde_json::from_value(v).map_err(|e| WireError::bad_request(format!("params: {e}")))
}

fn bad(msg: impl Into<String>) -> WireError {
    WireError::bad_request(msg)
}

fn check_len(what: &str, s: &str, max: usize) -> Result<(), WireError> {
    if s.len() > max {
        return Err(bad(format!("{what} is {} bytes, limit is {max}", s.len())));
    }
    Ok(())
}

fn check_target(t: &Option<String>) -> Result<(), WireError> {
    if let Some(t) = t {
        if t.is_empty() {
            return Err(bad("target is empty"));
        }
        check_len("target", t, MAX_TARGET_BYTES)?;
    }
    Ok(())
}

fn check_opt(what: &str, s: &Option<String>, max: usize) -> Result<(), WireError> {
    match s {
        Some(s) => check_len(what, s, max),
        None => Ok(()),
    }
}

/// Dotted kebab-case key, same rule as the config-key check in `apoth-action`.
fn check_key(k: &str) -> Result<(), WireError> {
    let ok = !k.is_empty()
        && k.len() <= MAX_SHORT_BYTES
        && k.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if ok {
        Ok(())
    } else {
        Err(bad("config key must be dotted kebab-case"))
    }
}

/// `grant` keys (everything under `socket`) are never writable over the socket (T-10
/// §1.1, T-12 §4). The full override-class check is the server's; this hard stop means a
/// malformed or unknown class table can never let one through the decoder.
fn check_writable_key(k: &str) -> Result<(), WireError> {
    check_key(k)?;
    if k == "socket" || k.starts_with("socket.") {
        return Err(WireError::new(
            ErrorCode::Denied,
            "grant keys are never writable over the socket",
        ));
    }
    Ok(())
}

fn parse_error(e: ParseError) -> WireError {
    let code = match e.kind {
        ErrorKind::UnknownAction => ErrorCode::UnknownAction,
        _ => ErrorCode::BadRequest,
    };
    WireError::new(code, e.message)
}

impl Params {
    /// Decode and validate the params object of `method`.
    pub fn decode(method: Method, v: Value) -> Result<Params, WireError> {
        let p = match method {
            Method::Hello => Params::Hello(de(v)?),
            Method::ActionRun => Params::ActionRun(de(v)?),
            Method::InputSendKeys => Params::InputSendKeys(de(v)?),
            Method::Tree => Params::Tree(de(v)?),
            Method::PaneInfo => Params::PaneInfo(de(v)?),
            Method::PaneProcesses => Params::PaneProcesses(de(v)?),
            Method::PaneText => Params::PaneText(de(v)?),
            Method::PaneBlocks => Params::PaneBlocks(de(v)?),
            Method::ConfigGet => Params::ConfigGet(de(v)?),
            Method::ConfigEffective => Params::ConfigEffective(de(v)?),
            Method::KeyboardState => Params::KeyboardState(de(v)?),
            Method::GraphicsList => Params::GraphicsList(de(v)?),
            Method::MediaList => Params::MediaList(de(v)?),
            Method::WaylandInfo => Params::WaylandInfo(de(v)?),
            Method::RenderStatus => Params::RenderStatus(de(v)?),
            Method::Version => Params::Version(de(v)?),
            Method::Subscribe => Params::Subscribe(de(v)?),
            Method::Unsubscribe => Params::Unsubscribe(de(v)?),
            Method::PaneWatch => Params::PaneWatch(de(v)?),
            Method::ConfigSet => Params::ConfigSet(de(v)?),
            Method::ConfigReset => Params::ConfigReset(de(v)?),
            Method::ConfigReload => Params::ConfigReload(de(v)?),
            Method::ConfigPersist => Params::ConfigPersist(de(v)?),
            Method::ConfigSaveOverride => Params::ConfigSaveOverride(de(v)?),
            Method::Notify => Params::Notify(de(v)?),
            Method::Launch => Params::Launch(Box::new(de(v)?)),
        };
        p.validate()?;
        Ok(p)
    }

    /// The method these params belong to.
    pub fn method(&self) -> Method {
        match self {
            Params::Hello(_) => Method::Hello,
            Params::ActionRun(_) => Method::ActionRun,
            Params::InputSendKeys(_) => Method::InputSendKeys,
            Params::Tree(_) => Method::Tree,
            Params::PaneInfo(_) => Method::PaneInfo,
            Params::PaneProcesses(_) => Method::PaneProcesses,
            Params::PaneText(_) => Method::PaneText,
            Params::PaneBlocks(_) => Method::PaneBlocks,
            Params::ConfigGet(_) => Method::ConfigGet,
            Params::ConfigEffective(_) => Method::ConfigEffective,
            Params::KeyboardState(_) => Method::KeyboardState,
            Params::GraphicsList(_) => Method::GraphicsList,
            Params::MediaList(_) => Method::MediaList,
            Params::WaylandInfo(_) => Method::WaylandInfo,
            Params::RenderStatus(_) => Method::RenderStatus,
            Params::Version(_) => Method::Version,
            Params::Subscribe(_) => Method::Subscribe,
            Params::Unsubscribe(_) => Method::Unsubscribe,
            Params::PaneWatch(_) => Method::PaneWatch,
            Params::ConfigSet(_) => Method::ConfigSet,
            Params::ConfigReset(_) => Method::ConfigReset,
            Params::ConfigReload(_) => Method::ConfigReload,
            Params::ConfigPersist(_) => Method::ConfigPersist,
            Params::ConfigSaveOverride(_) => Method::ConfigSaveOverride,
            Params::Notify(_) => Method::Notify,
            Params::Launch(_) => Method::Launch,
        }
    }

    /// The `target` expression, when the method has one.
    pub fn target(&self) -> Option<&str> {
        let t = match self {
            Params::ActionRun(p) => &p.target,
            Params::InputSendKeys(p) => &p.target,
            Params::Tree(p)
            | Params::PaneInfo(p)
            | Params::PaneProcesses(p)
            | Params::GraphicsList(p)
            | Params::MediaList(p) => &p.target,
            Params::PaneText(p) => &p.target,
            Params::PaneBlocks(p) => &p.target,
            Params::ConfigEffective(p) => &p.target,
            Params::PaneWatch(p) => &p.target,
            _ => return None,
        };
        t.as_deref()
    }

    /// Serialise to the `params` object.
    pub fn to_value(&self) -> Value {
        let r = match self {
            Params::Hello(p) => serde_json::to_value(p),
            Params::ActionRun(p) => serde_json::to_value(p),
            Params::InputSendKeys(p) => serde_json::to_value(p),
            Params::Tree(p)
            | Params::PaneInfo(p)
            | Params::PaneProcesses(p)
            | Params::GraphicsList(p)
            | Params::MediaList(p) => serde_json::to_value(p),
            Params::PaneText(p) => serde_json::to_value(p),
            Params::PaneBlocks(p) => serde_json::to_value(p),
            Params::ConfigGet(p) => serde_json::to_value(p),
            Params::ConfigEffective(p) => serde_json::to_value(p),
            Params::KeyboardState(p)
            | Params::WaylandInfo(p)
            | Params::RenderStatus(p)
            | Params::Version(p)
            | Params::ConfigReload(p) => serde_json::to_value(p),
            Params::Subscribe(p) => serde_json::to_value(p),
            Params::Unsubscribe(p) => serde_json::to_value(p),
            Params::PaneWatch(p) => serde_json::to_value(p),
            Params::ConfigSet(p) | Params::ConfigPersist(p) => serde_json::to_value(p),
            Params::ConfigReset(p) => serde_json::to_value(p),
            Params::ConfigSaveOverride(p) => serde_json::to_value(p),
            Params::Notify(p) => serde_json::to_value(p),
            Params::Launch(p) => serde_json::to_value(p),
        };
        // Plain derived structs of strings, numbers and maps cannot fail to serialise.
        r.unwrap_or(Value::Null)
    }

    /// Bounds and cross-field checks.
    pub fn validate(&self) -> Result<(), WireError> {
        check_target(&self.target().map(str::to_owned))?;
        match self {
            Params::Hello(p) => {
                check_len("client", &p.client, MAX_SHORT_BYTES)?;
                if p.want.len() > MAX_CAPS {
                    return Err(bad("too many capabilities in want"));
                }
            }
            Params::ActionRun(p) => {
                check_len("action", &p.action, apoth_action::text::MAX_SCRIPT_BYTES)?;
                Action::parse(&p.action).map_err(parse_error)?;
            }
            Params::InputSendKeys(p) => {
                if p.keys.is_empty() || p.keys.len() > MAX_KEYS {
                    return Err(bad("keys must hold between 1 and 1024 chords"));
                }
                for k in &p.keys {
                    check_len("key chord", k, MAX_SHORT_BYTES)?;
                    KeyChord::parse(k).map_err(parse_error)?;
                }
            }
            Params::ConfigGet(p) => check_key(&p.key)?,
            Params::ConfigEffective(p) => {
                if let Some(k) = &p.key {
                    check_key(k)?;
                }
            }
            Params::Subscribe(p) => {
                if p.events.is_empty() || p.events.len() > MAX_SUBSCRIBE_EVENTS {
                    return Err(bad("events must hold between 1 and 64 names"));
                }
                for e in &p.events {
                    if EventKind::parse(e).is_none() {
                        return Err(bad("unknown event name"));
                    }
                }
                check_opt("match", &p.match_expr, MAX_TARGET_BYTES)?;
            }
            Params::ConfigSet(p) | Params::ConfigPersist(p) => {
                check_writable_key(&p.key)?;
                check_len("value", &p.value, MAX_ELEM_BYTES)?;
                self.to_action().transpose().map_err(parse_error)?;
            }
            Params::ConfigReset(p) => {
                check_writable_key(&p.key)?;
                self.to_action().transpose().map_err(parse_error)?;
            }
            Params::ConfigSaveOverride(p) => {
                check_opt("scope", &p.scope, MAX_SHORT_BYTES)?;
                self.to_action().transpose().map_err(parse_error)?;
            }
            Params::Notify(p) => {
                check_len("title", &p.title, MAX_SHORT_BYTES)?;
                check_opt("body", &p.body, MAX_ELEM_BYTES)?;
                self.to_action().transpose().map_err(parse_error)?;
            }
            Params::Launch(p) => p.validate()?,
            Params::Tree(_)
            | Params::PaneInfo(_)
            | Params::PaneProcesses(_)
            | Params::PaneText(_)
            | Params::PaneBlocks(_)
            | Params::KeyboardState(_)
            | Params::GraphicsList(_)
            | Params::MediaList(_)
            | Params::WaylandInfo(_)
            | Params::RenderStatus(_)
            | Params::Version(_)
            | Params::Unsubscribe(_)
            | Params::PaneWatch(_)
            | Params::ConfigReload(_) => {}
        }
        Ok(())
    }

    /// The registry Action these params stand for, when there is one (`action.run`,
    /// `config.set|reset|persist|reload|save-override`, `notify`). Parse errors are
    /// returned so the caller maps them; `None` for every other method.
    pub fn to_action(&self) -> Option<Result<Action, ParseError>> {
        let call = match self {
            Params::ActionRun(p) => return Some(Action::parse(&p.action)),
            Params::ConfigSet(p) => call("config.set", &[&p.key, &p.value], &[]),
            Params::ConfigPersist(p) => call("config.persist", &[&p.key, &p.value], &[]),
            Params::ConfigReset(p) => call("config.reset", &[&p.key], &[]),
            Params::ConfigReload(_) => call("config.reload", &[], &[]),
            Params::ConfigSaveOverride(p) => {
                call("config.save-override", &[], &[("scope", &p.scope)])
            }
            Params::Notify(p) => call(
                "notify",
                &[&p.title],
                &[("body", &p.body), ("urgency", &p.urgency)],
            ),
            _ => return None,
        };
        Some(Action::from_call(&call))
    }

    /// The capability classes this request needs. Registry-backed methods are classified
    /// by `apoth-action` (so a macro, an argv, or a `grant` key behaves as in T-11 §1);
    /// the rest use the static table. A refusal rides along in the result.
    pub fn classify(&self, ctx: &dyn ClassContext) -> Classification {
        match self.to_action() {
            Some(Ok(a)) => a.classify(ctx),
            _ => {
                let mut c = Classification {
                    classes: self.method().classes(),
                    refused: None,
                };
                if let Params::Launch(l) = self {
                    // `-o key=value` is a config write: classify each key as `config.set`
                    // does, so a security/behaviour key needs `config-write` and a grant
                    // (or unknown) key is refused (T-12 §4, §6).
                    for k in l.overrides.keys() {
                        match ctx.override_class(k).unwrap_or(OverrideClass::Grant) {
                            OverrideClass::Appearance => c.add(Class::Appearance),
                            OverrideClass::Behaviour | OverrideClass::Security => {
                                c.add(Class::ConfigWrite);
                            }
                            OverrideClass::Grant => {
                                c.add(Class::ConfigWrite);
                                c.refuse(Refusal::GrantKey);
                            }
                        }
                    }
                }
                c
            }
        }
    }

    /// The static class set, ignoring arguments.
    pub fn static_classes(&self) -> ClassSet {
        self.method().classes()
    }
}

fn call(id: &str, pos: &[&str], named: &[(&str, &Option<String>)]) -> Call {
    let mut c = Call::new(id);
    c.positional = pos.iter().map(|s| (*s).to_owned()).collect();
    for (k, v) in named {
        if let Some(v) = v {
            c.named.push(((*k).to_owned(), v.clone()));
        }
    }
    c
}

impl Launch {
    fn validate(&self) -> Result<(), WireError> {
        if self.argv.len() > MAX_ARGV {
            return Err(bad("too many argv elements"));
        }
        for a in &self.argv {
            check_len("argv element", a, MAX_ELEM_BYTES)?;
            if a.contains('\0') {
                return Err(bad("argv element contains NUL"));
            }
        }
        check_opt("cwd", &self.cwd, MAX_PATH_BYTES)?;
        if self.env.len() > MAX_ENV {
            return Err(bad("too many environment entries"));
        }
        for (k, v) in &self.env {
            if k.is_empty() || k.contains(['=', '\0']) || k.len() > MAX_SHORT_BYTES {
                return Err(bad("invalid environment name"));
            }
            check_len("environment value", v, MAX_ELEM_BYTES)?;
            if v.contains('\0') {
                return Err(bad("environment value contains NUL"));
            }
        }
        check_opt("activation token", &self.activation_token, MAX_TOKEN_BYTES)?;
        if self.overrides.len() > MAX_OVERRIDES {
            return Err(bad("too many overrides"));
        }
        for (k, v) in &self.overrides {
            check_writable_key(k)?;
            check_len("override value", v, MAX_ELEM_BYTES)?;
        }
        check_opt("title", &self.title, MAX_SHORT_BYTES)?;
        check_opt("app id", &self.app_id, MAX_SHORT_BYTES)?;
        check_opt("profile", &self.profile, MAX_SHORT_BYTES)?;
        check_opt("layout", &self.layout, MAX_SHORT_BYTES)?;
        Ok(())
    }
}
