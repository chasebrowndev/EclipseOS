// SPDX-License-Identifier: AGPL-3.0-only
//! Agent package manifests (A-07 §2, §6, §7). TCB: the install policy, and
//! so every grant a task can get, is computed from what this reads.
//!
//! A manifest is a request and a disclosure, never an authorization (A-07
//! §1). Every human-readable string in it is untrusted: this module carries
//! them to the review modal unchanged, and the compositor renders them in the
//! untrusted block.
//!
//! ```kdl
//! agent {
//!   id "ec-ref-agent"; name "Reference agent"; version "0.1.0"
//!   publisher "eclipse"; entrypoint "bin/ec-ref-agent"
//!   task { default_deadline "2h"; max_depth 0 }
//!   capabilities {
//!     scene.list scope { app_id "org.mozilla.firefox" }
//!     because "Find the invoice portal window"
//!   }
//!   resumable #false
//! }
//! ```
//!
//! Choices the spec leaves open, for the owner's review:
//! - Durations are strings (`"2h"`). A-07 §2's bare `2h` is not valid KDL.
//! - `because` is the node after the capability it explains, as A-07 §2
//!   lays it out; a capability with no `because` is refused (§7).
//! - A capability with no `scope` block asks for everything (§2). It is
//!   refused at install: a grant line with no scopes reaches nothing
//!   (`ec_policy_eval::grant::Capability`), so it could never be issued as
//!   reviewed, and §7 already refuses wildcard-only scopes.
//! - One value per scope kind. `app_id "a" "b"` would be an OR, which one
//!   grant line cannot say; it is refused rather than split.
//! - Packages under `/usr/share/eclipse/agents` are the OS's own, installed
//!   and integrity-checked by the system package manager; they use publisher
//!   `eclipse`. `local` is valid only under the owner's agent directory (§4).
//!   Any other publisher needs a signature, which is not implemented, so it is
//!   refused.

use kdl::{KdlDocument, KdlNode, KdlValue};
use std::path::{Component, Path, PathBuf};

/// Owner ceiling on `max_depth` (A-04 §10 default).
pub const MAX_DEPTH_CEILING: u32 = 3;

/// The A-04 §13.4 default deadline, when a manifest gives none.
pub const DEFAULT_DEADLINE_MS: u64 = 2 * 3_600_000;

/// Manifest string fields longer than this are refused, not truncated: a
/// review that cannot show the whole string is not a review.
const MAX_STRING: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requested {
    /// The capability, e.g. `scene.list`.
    pub name: String,
    /// `kind:value` clauses (S-01 §3), sorted, so two requests compare by
    /// what they ask for rather than how they were written.
    pub scopes: Vec<String>,
    /// Untrusted justification.
    pub because: String,
}

impl Requested {
    /// The grant line this request becomes: `<name> <scope> <scope>...`, the
    /// form `tasks::capabilities` reads.
    pub fn line(&self) -> String {
        let mut s = self.name.clone();
        for sc in &self.scopes {
            s.push(' ');
            s.push_str(sc);
        }
        s
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `~/.local/share/eclipse/agents`: the owner's own.
    Owner,
    /// `/usr/share/eclipse/agents`: shipped by the OS package.
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub entrypoint: Vec<String>,
    pub default_deadline_ms: u64,
    pub max_depth: u32,
    pub capabilities: Vec<Requested>,
    /// As written; [`Manifest::is_resumable`] applies the default.
    pub resumable: Option<bool>,
    /// Sandbox declarations, `(kind, value)`, shown in review.
    pub sandbox: Vec<(String, String)>,
    /// `inference { backend "api"|"claude-code"; model "…" }` (ADR 0076):
    /// which router backend and model serve the package's
    /// `inference.complete`. Absent: the tool is not offered.
    pub inference: Option<Inference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inference {
    pub backend: String,
    pub model: String,
}

/// Why a manifest is refused at install, with no review offered (A-07 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    Unreadable,
    Syntax,
    /// A field missing, repeated, of the wrong type, or too long; names it.
    Field(&'static str),
    UnknownNode(String),
    NoBecause(String),
    Unscoped(String),
    WildcardScope(String),
    BadScope(String),
    DepthOverCeiling,
    EntrypointOutsideTree,
    /// `secret.expose` from anyone but the owner (§6, §7).
    SecretExpose,
    Publisher,
    /// A channel schema not in the registry (§7, A-03 §8). The registry is
    /// empty at v1, so any `channels { create }` is refused.
    UnregisteredSchema,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unreadable => f.write_str("manifest_unreadable"),
            Refusal::Syntax => f.write_str("manifest_syntax"),
            Refusal::Field(n) => write!(f, "manifest_field:{n}"),
            Refusal::UnknownNode(_) => f.write_str("manifest_unknown_node"),
            Refusal::NoBecause(_) => f.write_str("capability_without_because"),
            Refusal::Unscoped(_) => f.write_str("capability_unscoped"),
            Refusal::WildcardScope(_) => f.write_str("wildcard_scope"),
            Refusal::BadScope(_) => f.write_str("bad_scope"),
            Refusal::DepthOverCeiling => f.write_str("max_depth_over_ceiling"),
            Refusal::EntrypointOutsideTree => f.write_str("entrypoint_outside_package"),
            Refusal::SecretExpose => f.write_str("secret_expose_not_local"),
            Refusal::Publisher => f.write_str("publisher_not_allowed"),
            Refusal::UnregisteredSchema => f.write_str("unregistered_channel_schema"),
        }
    }
}

/// The package roots, owner's first.
pub fn roots() -> Vec<(PathBuf, Origin)> {
    let mut out = Vec::new();
    if let Some(d) = std::env::var_os("ECLIPSE_AGENTS_DIR") {
        out.push((PathBuf::from(d), Origin::Owner));
    } else if let Some(h) = std::env::var_os("HOME") {
        out.push((
            PathBuf::from(h).join(".local/share/eclipse/agents"),
            Origin::Owner,
        ));
    }
    out.push((PathBuf::from("/usr/share/eclipse/agents"), Origin::System));
    out
}

/// Which root `dir` (canonical) is a `<id>/<version>` package under.
pub fn origin_of(dir: &Path, roots: &[(PathBuf, Origin)]) -> Option<Origin> {
    roots.iter().find_map(|(r, o)| {
        let r = r.canonicalize().ok()?;
        let rest = dir.strip_prefix(&r).ok()?;
        (rest.components().count() == 2).then_some(*o)
    })
}

impl Manifest {
    /// Appendix F-24: `resumable` defaults to true for the owner's own
    /// (`local`) packages and false for everyone else's.
    pub fn is_resumable(&self) -> bool {
        self.resumable.unwrap_or(self.publisher == "local")
    }
}

/// Reads and validates `<dir>/manifest.kdl`.
pub fn load(dir: &Path, origin: Origin) -> Result<Manifest, Refusal> {
    let text = std::fs::read_to_string(dir.join("manifest.kdl")).map_err(|_| Refusal::Unreadable)?;
    let m = parse(&text)?;
    validate(&m, origin)?;
    // The directory is `<id>/<version>`: a manifest cannot claim to be
    // another package than the one the owner put there.
    let v = dir.file_name().and_then(|s| s.to_str());
    let i = dir.parent().and_then(Path::file_name).and_then(|s| s.to_str());
    if v != Some(m.version.as_str()) {
        return Err(Refusal::Field("version"));
    }
    if i != Some(m.id.as_str()) {
        return Err(Refusal::Field("id"));
    }
    Ok(m)
}

fn string(v: &KdlValue, what: &'static str) -> Result<String, Refusal> {
    let s = v.as_string().ok_or(Refusal::Field(what))?;
    if s.len() > MAX_STRING {
        return Err(Refusal::Field(what));
    }
    Ok(s.to_owned())
}

/// The node's single positional argument.
/// `inference { backend "api"|"claude-code"; model "…" }`. Both required;
/// anything else in the block is refused, as everywhere in a manifest.
fn inference(n: &KdlNode) -> Result<Inference, Refusal> {
    let (mut backend, mut model) = (None, None);
    for c in n.children().map(KdlDocument::nodes).unwrap_or_default() {
        let slot = match c.name().value() {
            "backend" => &mut backend,
            "model" => &mut model,
            other => return Err(Refusal::UnknownNode(format!("inference.{other}"))),
        };
        if slot.is_some() {
            return Err(Refusal::Field("inference"));
        }
        *slot = Some(string(one(c, "inference")?, "inference")?);
    }
    let backend = backend.ok_or(Refusal::Field("inference.backend"))?;
    let model = model.ok_or(Refusal::Field("inference.model"))?;
    let model_ok = !model.is_empty()
        && model.len() <= 64
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.');
    if !matches!(backend.as_str(), "api" | "claude-code") {
        return Err(Refusal::Field("inference.backend"));
    }
    if !model_ok {
        return Err(Refusal::Field("inference.model"));
    }
    Ok(Inference { backend, model })
}

fn one<'a>(n: &'a KdlNode, what: &'static str) -> Result<&'a KdlValue, Refusal> {
    let mut args = n.entries().iter().filter(|e| e.name().is_none());
    match (
        args.next(),
        args.next(),
        n.entries().iter().any(|e| e.name().is_some()),
    ) {
        (Some(e), None, false) => Ok(e.value()),
        _ => Err(Refusal::Field(what)),
    }
}

fn duration_ms(s: &str) -> Option<u64> {
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = num.parse().ok()?;
    let ms = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => return None,
    };
    n.checked_mul(ms)
}

fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && !s.starts_with('.')
}

pub fn parse(text: &str) -> Result<Manifest, Refusal> {
    let doc: KdlDocument = text.parse().map_err(|_| Refusal::Syntax)?;
    let [agent] = doc.nodes() else {
        return Err(Refusal::Syntax);
    };
    if agent.name().value() != "agent" || !agent.entries().is_empty() {
        return Err(Refusal::Syntax);
    }
    let body = agent.children().ok_or(Refusal::Syntax)?;
    let mut m = Manifest {
        id: String::new(),
        name: String::new(),
        version: String::new(),
        publisher: String::new(),
        entrypoint: Vec::new(),
        default_deadline_ms: DEFAULT_DEADLINE_MS,
        max_depth: 0,
        capabilities: Vec::new(),
        resumable: None,
        inference: None,
        sandbox: Vec::new(),
    };
    let mut seen: Vec<&str> = Vec::new();
    for n in body.nodes() {
        let name = n.name().value();
        if seen.contains(&name) {
            return Err(Refusal::UnknownNode(name.to_owned()));
        }
        seen.push(name);
        match name {
            "id" => m.id = string(one(n, "id")?, "id")?,
            "name" => m.name = string(one(n, "name")?, "name")?,
            "version" => m.version = string(one(n, "version")?, "version")?,
            "publisher" => m.publisher = string(one(n, "publisher")?, "publisher")?,
            "entrypoint" => {
                if n.entries().iter().any(|e| e.name().is_some()) || n.children().is_some() {
                    return Err(Refusal::Field("entrypoint"));
                }
                m.entrypoint = n
                    .entries()
                    .iter()
                    .map(|e| string(e.value(), "entrypoint"))
                    .collect::<Result<_, _>>()?;
            }
            "runtime" | "sdk" => {
                string(one(n, "runtime")?, "runtime")?;
            }
            "resumable" => {
                m.resumable = Some(
                    one(n, "resumable")?
                        .as_bool()
                        .ok_or(Refusal::Field("resumable"))?,
                )
            }
            "task" => {
                for t in n.children().map(KdlDocument::nodes).unwrap_or_default() {
                    match t.name().value() {
                        "default_deadline" => {
                            let s = string(one(t, "default_deadline")?, "default_deadline")?;
                            m.default_deadline_ms =
                                duration_ms(&s).ok_or(Refusal::Field("default_deadline"))?;
                        }
                        "max_depth" => {
                            let d = one(t, "max_depth")?
                                .as_integer()
                                .and_then(|d| u32::try_from(d).ok())
                                .ok_or(Refusal::Field("max_depth"))?;
                            m.max_depth = d;
                        }
                        other => return Err(Refusal::UnknownNode(other.to_owned())),
                    }
                }
            }
            "capabilities" => m.capabilities = capabilities(n)?,
            "sandbox" => {
                for s in n.children().map(KdlDocument::nodes).unwrap_or_default() {
                    let v = string(one(s, "sandbox")?, "sandbox")?;
                    m.sandbox.push((s.name().value().to_owned(), v));
                }
            }
            "channels" => {
                if n.children().is_some_and(|c| !c.nodes().is_empty()) {
                    return Err(Refusal::UnregisteredSchema);
                }
            }
            "inference" => m.inference = Some(inference(n)?),
            // Shown nowhere yet and authorises nothing; parsed so a manifest
            // that has it is not refused for it.
            "integrity" => {}
            other => return Err(Refusal::UnknownNode(other.to_owned())),
        }
    }
    for (v, f) in [
        (&m.id, "id"),
        (&m.name, "name"),
        (&m.version, "version"),
        (&m.publisher, "publisher"),
    ] {
        if v.is_empty() {
            return Err(Refusal::Field(f));
        }
    }
    if !id_ok(&m.id) {
        return Err(Refusal::Field("id"));
    }
    if !id_ok(&m.version) {
        return Err(Refusal::Field("version"));
    }
    if m.entrypoint.is_empty() {
        return Err(Refusal::Field("entrypoint"));
    }
    Ok(m)
}

fn capabilities(n: &KdlNode) -> Result<Vec<Requested>, Refusal> {
    let mut out: Vec<Requested> = Vec::new();
    let mut open: Option<Requested> = None;
    for c in n.children().map(KdlDocument::nodes).unwrap_or_default() {
        let name = c.name().value();
        if name == "because" {
            let mut r = open.take().ok_or(Refusal::NoBecause(String::new()))?;
            r.because = string(one(c, "because")?, "because")?;
            out.push(r);
            continue;
        }
        if let Some(r) = open.take() {
            return Err(Refusal::NoBecause(r.name));
        }
        if !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_')
            || name.is_empty()
        {
            return Err(Refusal::UnknownNode(name.to_owned()));
        }
        // `scope { ... }` is the only shape: the bare word `scope` and a
        // children block, nothing else on the line.
        let args: Vec<_> = c.entries().iter().collect();
        let scoped = match args.as_slice() {
            [] => false,
            [e] if e.name().is_none() && e.value().as_string() == Some("scope") => true,
            _ => return Err(Refusal::BadScope(name.to_owned())),
        };
        let clauses = c.children().map(KdlDocument::nodes).unwrap_or_default();
        if !scoped || clauses.is_empty() {
            return Err(Refusal::Unscoped(name.to_owned()));
        }
        let mut scopes = Vec::new();
        for s in clauses {
            let v = string(one(s, "scope")?, "scope")?;
            if v.chars().all(|ch| ch == '*' || ch == '?') {
                return Err(Refusal::WildcardScope(name.to_owned()));
            }
            let clause = format!("{}:{v}", s.name().value());
            if ec_policy_eval::scope::Scope::parse(&clause).is_err() || clause.contains(char::is_whitespace) {
                return Err(Refusal::BadScope(name.to_owned()));
            }
            scopes.push(clause);
        }
        scopes.sort();
        scopes.dedup();
        open = Some(Requested {
            name: name.to_owned(),
            scopes,
            because: String::new(),
        });
    }
    if let Some(r) = open {
        return Err(Refusal::NoBecause(r.name));
    }
    Ok(out)
}

/// The A-07 §6–§7 rules that depend on where the package came from.
pub fn validate(m: &Manifest, origin: Origin) -> Result<(), Refusal> {
    let publisher_ok = match origin {
        Origin::Owner => m.publisher == "local",
        Origin::System => m.publisher == "eclipse",
    };
    if !publisher_ok {
        return Err(Refusal::Publisher);
    }
    if m.max_depth > MAX_DEPTH_CEILING {
        return Err(Refusal::DepthOverCeiling);
    }
    if origin != Origin::Owner && m.capabilities.iter().any(|c| c.name == "secret.expose") {
        return Err(Refusal::SecretExpose);
    }
    for (kind, v) in &m.sandbox {
        if kind == "net.egress" && v.chars().all(|c| c == '*' || c == '?') {
            return Err(Refusal::WildcardScope(kind.clone()));
        }
    }
    // A path entrypoint must stay inside the package; a bare word is a
    // command the runtime provides.
    let first = Path::new(&m.entrypoint[0]);
    if m.entrypoint[0].contains('/')
        && !first
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(Refusal::EntrypointOutsideTree);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: &str = r#"
agent {
  id "ec-ref-agent"
  name "Reference agent"
  version "0.1.0"
  publisher "eclipse"
  entrypoint "bin/ec-ref-agent"
  task { default_deadline "2h"; max_depth 0 }
  capabilities {}
  resumable #false
}
"#;

    const INVOICE: &str = r#"
agent {
  id "invoice-triage"
  name "Invoice Triage"
  version "0.3.1"
  publisher "local"
  entrypoint "python" "-m" "invoice_triage"
  runtime "python3.13"
  task { default_deadline "90m"; max_depth 0 }
  capabilities {
    scene.list scope { app_id "org.mozilla.firefox" }
    because "Find the invoice portal window"
    scene.tree scope { url "https://*.acme-invoices.com/*"; app_id "org.mozilla.firefox" }
    because "Read the invoice table"
  }
  sandbox { fs.read "~/Documents/invoices" }
}
"#;

    #[test]
    fn the_reference_and_the_spec_example_parse() {
        let r = parse(REF).unwrap();
        assert_eq!(r.entrypoint, ["bin/ec-ref-agent"]);
        assert_eq!(r.default_deadline_ms, DEFAULT_DEADLINE_MS);
        assert!(r.capabilities.is_empty());
        validate(&r, Origin::System).unwrap();
        assert!(!r.is_resumable(), "explicit #false");
        assert!(
            parse(INVOICE).unwrap().is_resumable(),
            "local defaults to resumable (F-24)"
        );
        assert!(
            !parse(&REF.replace("  resumable #false\n", ""))
                .unwrap()
                .is_resumable(),
            "everyone else defaults to not"
        );

        let m = parse(INVOICE).unwrap();
        assert_eq!(m.default_deadline_ms, 90 * 60_000);
        assert_eq!(
            m.capabilities[1].line(),
            "scene.tree app_id:org.mozilla.firefox url:https://*.acme-invoices.com/*",
            "scopes sorted, so order of writing does not matter"
        );
        assert_eq!(m.capabilities[1].because, "Read the invoice table");
        validate(&m, Origin::Owner).unwrap();
    }

    #[test]
    fn an_inference_block_names_a_backend_and_a_model() {
        let with = |block: &str| parse(&INVOICE.replace("  sandbox {", &format!("  {block}\n  sandbox {{")));
        let m = with(r#"inference { backend "api"; model "claude-opus-5-5" }"#).unwrap();
        assert_eq!(
            m.inference,
            Some(Inference {
                backend: "api".into(),
                model: "claude-opus-5-5".into()
            })
        );
        assert_eq!(
            parse(INVOICE).unwrap().inference,
            None,
            "absent means no inference"
        );
        assert!(with(r#"inference { backend "claude-code"; model "sonnet" }"#).is_ok());
        for bad in [
            r#"inference { backend "openai"; model "x" }"#,
            r#"inference { backend "api" }"#,
            r#"inference { model "x" }"#,
            r#"inference { backend "api"; model "has space" }"#,
            r#"inference { backend "api"; model "a"; model "b" }"#,
            r#"inference { backend "api"; model "a"; key "sk-..." }"#,
        ] {
            assert!(with(bad).is_err(), "{bad}");
        }
    }

    fn with_caps(caps: &str) -> String {
        INVOICE.replace(
            &INVOICE[INVOICE.find("  capabilities {").unwrap()..INVOICE.find("  sandbox").unwrap()],
            &format!("  capabilities {{\n{caps}\n  }}\n"),
        )
    }

    #[test]
    fn section_seven_refusals() {
        let bad = |caps: &str| parse(&with_caps(caps)).unwrap_err();
        assert!(matches!(
            bad(r#"scene.list scope { app_id "a" }"#),
            Refusal::NoBecause(_)
        ));
        assert!(matches!(bad(r#"scene.tree"#), Refusal::Unscoped(_)));
        assert!(matches!(bad("scene.tree\nbecause \"x\""), Refusal::Unscoped(_)));
        assert!(matches!(
            bad("scene.list scope { app_id \"*\" }\nbecause \"x\""),
            Refusal::WildcardScope(_)
        ));
        assert!(matches!(
            bad("scene.list scope { bogus \"x\" }\nbecause \"x\""),
            Refusal::BadScope(_)
        ));
        assert!(matches!(
            bad("scene.list scope { app_id \"a\" \"b\" }\nbecause \"x\""),
            Refusal::Field("scope")
        ));

        let m = parse(&INVOICE.replace("max_depth 0", "max_depth 4")).unwrap();
        assert_eq!(validate(&m, Origin::Owner), Err(Refusal::DepthOverCeiling));
        let m = parse(&INVOICE.replace("\"python\" \"-m\"", "\"../../bin/sh\" \"-m\"")).unwrap();
        assert_eq!(validate(&m, Origin::Owner), Err(Refusal::EntrypointOutsideTree));
        let m = parse(&INVOICE.replace("\"python\" \"-m\"", "\"/usr/bin/sh\" \"-m\"")).unwrap();
        assert_eq!(validate(&m, Origin::Owner), Err(Refusal::EntrypointOutsideTree));
        let m = parse(INVOICE).unwrap();
        assert_eq!(
            validate(&m, Origin::System),
            Err(Refusal::Publisher),
            "local outside the owner dir"
        );
        let m = parse(&with_caps(
            "secret.expose scope { host \"a.example\" }\nbecause \"x\"",
        ))
        .unwrap();
        assert_eq!(
            validate(
                &Manifest {
                    publisher: "eclipse".into(),
                    ..m
                },
                Origin::System
            ),
            Err(Refusal::SecretExpose)
        );
        assert_eq!(
            parse(&INVOICE.replace("  sandbox", "  channels { create \"x\" schema \"y\" }\n  sandbox")),
            Err(Refusal::UnregisteredSchema)
        );
        let m = parse(&INVOICE.replace("fs.read \"~/Documents/invoices\"", "net.egress \"*\"")).unwrap();
        assert!(matches!(
            validate(&m, Origin::Owner),
            Err(Refusal::WildcardScope(_))
        ));
    }

    #[test]
    fn hostile_strings_are_carried_or_refused_never_reshaped() {
        // Written as KDL escapes: KDL refuses the raw code points.
        let evil = "Fine\u{202e}gnp.exe\u{1b}[2J";
        let m = parse(&INVOICE.replace("Invoice Triage", r"Fine\u{202e}gnp.exe\u{1b}[2J")).unwrap();
        assert_eq!(m.name, evil, "carried verbatim; the review modal sanitises");
        let long = "x".repeat(100_000);
        assert_eq!(
            parse(&INVOICE.replace("Invoice Triage", &long)),
            Err(Refusal::Field("name"))
        );
        assert_eq!(
            parse(&INVOICE.replace("\"invoice-triage\"", "\"Invoice\"")),
            Err(Refusal::Field("id"))
        );
        assert_eq!(parse("not kdl {"), Err(Refusal::Syntax));
        assert_eq!(parse("agent {}\nagent {}"), Err(Refusal::Syntax));
        assert!(matches!(
            parse(&INVOICE.replace("  sandbox", "  surprise 1\n  sandbox")),
            Err(Refusal::UnknownNode(_))
        ));
    }

    #[test]
    fn a_package_is_where_it_says_it_is() {
        let root = std::env::temp_dir().join(format!("ec-manifest-{}", std::process::id()));
        let dir = root.join("ec-ref-agent/0.1.0");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.kdl"), REF).unwrap();
        let roots = [(root.clone(), Origin::System)];
        let canon = dir.canonicalize().unwrap();
        assert_eq!(origin_of(&canon, &roots), Some(Origin::System));
        assert_eq!(origin_of(&root.canonicalize().unwrap(), &roots), None);
        assert!(load(&canon, Origin::System).is_ok());
        let wrong = root.join("other/0.1.0");
        std::fs::create_dir_all(&wrong).unwrap();
        std::fs::write(wrong.join("manifest.kdl"), REF).unwrap();
        assert_eq!(load(&wrong, Origin::System), Err(Refusal::Field("id")));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
