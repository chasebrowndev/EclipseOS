// SPDX-License-Identifier: AGPL-3.0-only
//! Dispatch: installing a package, previewing a draft, and creating the task
//! the human committed (A-07 §3, A-08 §5.2, COMP-19 §6). TCB.
//!
//! ```text
//!   install_begin{path}  → manifest validated → install_review{display}
//!   install_answer{yes}  → install policy stored (the reviewed lines)
//!   preview_task{draft}  → lines = install policy ∩ narrowing
//!                        → preview{id, display}   (id bound to table + install)
//!   create_task{id}      → same versions? → task + grant of exactly `lines`
//!                        → task_created → console;  provision → agentd
//! ```
//!
//! **What was shown is what is submitted** (A-08 §5.2): a preview stores the
//! capability lines it displayed, and `create_task` mints those lines and no
//! others. A preview is single use and names the table version and the
//! install record it was computed against; either changing refuses
//! `preview_stale`, and the slot re-previews.
//!
//! Choices, for the owner's review:
//! - Narrowing may only remove whole capability lines. A line that is not
//!   byte-identical to an install-policy line is "not provably narrower" and
//!   refused (A-07 §5's rule, applied to narrowing).
//! - Owner policy does not yet bound issuance: the compiled table has no
//!   grant ceilings (S-02). The intersection is manifest ∩ install policy ∩
//!   narrowing; the table still decides every act at check time.
//! - The principal is `agent:<package>-<6 hex>`, one per task: A-04 §2 allows
//!   one live task per principal, and two tasks of one package are two
//!   principals.
//! - A slot unpause is accepted only for a pause policyd saw a human make
//!   (A-08 §14.5 proposes breaker and incident pauses be panel-only). A pause
//!   from before a restart has no recorded cause and is refused here.
//! - Install review always runs, on every version (A-07 §3 "once per
//!   version"); the diff marks a line added unless it is byte-identical to a
//!   line of the previous install policy.

use crate::manifest::{self, Manifest, Origin, Refusal};
use crate::tasks::{TaskError, TaskStore};
use ec_policy_eval::audit::{Emission, Kind};
use ec_policy_eval::cbor::{enc, MapBuilder, Reader};
use ec_policy_eval::link::{CancelMode, FromPolicyd};
use ec_policy_eval::task::{TaskEvent, TaskState, Ulid};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A-04 §13.4 ceiling on a task deadline from a draft.
pub const MAX_DEADLINE_MS: u64 = 24 * 3_600_000;

/// A-08 §5.1.
pub const MAX_STATEMENT: usize = 1_000;

/// An unanswered install review is dropped after this.
const REVIEW_TTL_MS: u64 = 5 * 60_000;

/// Previews kept at once; COMP-19 §8 bounds slots at 4 per client, so this is
/// generous and only stops an unbounded map.
const MAX_PREVIEWS: usize = 64;

/// Where a message goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum To {
    /// The connection that asked.
    Asker,
    /// Every agentd connection.
    Agentd,
    /// Every compositor and agentd connection.
    All,
}

pub type Out = Vec<(To, FromPolicyd)>;

/// An approved install: what the human allowed this version to be granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub id: String,
    pub version: String,
    pub dir: PathBuf,
    /// BLAKE3 of `manifest.kdl` as reviewed.
    pub manifest_hash: [u8; 32],
    /// The reviewed capability lines (`Requested::line`).
    pub lines: Vec<String>,
    /// Bumped on every install of this package: a preview names it.
    pub seq: u64,
}

struct Review {
    req: u64,
    manifest: Manifest,
    dir: PathBuf,
    hash: [u8; 32],
    expires_ms: u64,
}

struct Preview {
    slot: u64,
    package: String,
    version: String,
    statement: String,
    deadline_ms: u64,
    lines: Vec<String>,
    continuation: String,
    resumes: String,
    table_version: u64,
    install_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PausedBy {
    Human,
}

pub struct Dispatch {
    dir: PathBuf,
    roots: Vec<(PathBuf, Origin)>,
    installs: HashMap<String, Install>,
    reviews: HashMap<u64, Review>,
    previews: HashMap<u64, Preview>,
    /// Task ids (text) policyd saw a human pause, this run.
    paused: HashMap<String, PausedBy>,
    next: u64,
}

fn reason(r: &str) -> String {
    r.to_owned()
}

fn refused(req: u64, why: impl Into<String>) -> Out {
    vec![(
        To::Asker,
        FromPolicyd::Refused {
            req,
            reason: why.into(),
        },
    )]
}

/// An answer, or a journal failure that must end the daemon.
pub type Result<T> = std::result::Result<T, TaskError>;

fn store_err(e: TaskError) -> Result<Out> {
    match e {
        TaskError::Store(_) => Err(e),
        _ => Ok(Vec::new()),
    }
}

impl Dispatch {
    /// Opens the install records under `dir` (policyd's state directory).
    pub fn open(dir: &Path, roots: Vec<(PathBuf, Origin)>) -> std::io::Result<Dispatch> {
        let dir = dir.join("installs");
        std::fs::create_dir_all(&dir)?;
        let mut installs = HashMap::new();
        for e in std::fs::read_dir(&dir)? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "cbor") {
                // A record that does not read is skipped, which leaves the
                // package uninstalled: the safe direction.
                if let Some(i) = std::fs::read(&p).ok().and_then(|b| decode_install(&b)) {
                    installs.insert(i.id.clone(), i);
                }
            }
        }
        Ok(Dispatch {
            dir,
            roots,
            installs,
            reviews: HashMap::new(),
            previews: HashMap::new(),
            paused: HashMap::new(),
            next: 1,
        })
    }

    fn id(&mut self) -> u64 {
        // Not a secret, but not guessable from outside either: it is only
        // ever compared against what policyd itself handed out.
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).expect("entropy");
        self.next += 1;
        u64::from_le_bytes(b) ^ self.next
    }

    pub fn installed(&self, id: &str) -> Option<&Install> {
        self.installs.get(id)
    }

    // ---- install (A-07 §3) ------------------------------------------------

    pub fn install_begin(&mut self, req: u64, path: &str, now_ms: u64) -> Out {
        self.reviews.retain(|_, r| r.expires_ms > now_ms);
        let Ok(dir) = Path::new(path).canonicalize() else {
            return refused(req, "not_found");
        };
        let Some(origin) = manifest::origin_of(&dir, &self.roots) else {
            return refused(req, "outside_agent_dirs");
        };
        let m = match manifest::load(&dir, origin) {
            Ok(m) => m,
            Err(r) => return refused(req, r.to_string()),
        };
        let Ok(bytes) = std::fs::read(dir.join("manifest.kdl")) else {
            return refused(req, Refusal::Unreadable.to_string());
        };
        let hash = *blake3::hash(&bytes).as_bytes();
        let display = review_display(&m, self.installs.get(&m.id));
        let review = self.id();
        self.reviews.insert(
            review,
            Review {
                req,
                manifest: m,
                dir,
                hash,
                expires_ms: now_ms + REVIEW_TTL_MS,
            },
        );
        vec![(To::Asker, FromPolicyd::InstallReview { req, review, display })]
    }

    /// The human's answer, from the compositor's review modal.
    pub fn install_answer(
        &mut self,
        store: &mut TaskStore,
        review: u64,
        approve: bool,
        now_ms: u64,
    ) -> Result<Out> {
        let Some(r) = self.reviews.remove(&review).filter(|r| r.expires_ms > now_ms) else {
            return Ok(Vec::new());
        };
        if !approve {
            return Ok(refused(r.req, "refused_by_human"));
        }
        // Re-read: the package must still be the one reviewed.
        let same =
            std::fs::read(r.dir.join("manifest.kdl")).is_ok_and(|b| *blake3::hash(&b).as_bytes() == r.hash);
        if !same {
            return Ok(refused(r.req, "package_changed"));
        }
        let seq = self.installs.get(&r.manifest.id).map_or(1, |i| i.seq + 1);
        let install = Install {
            id: r.manifest.id.clone(),
            version: r.manifest.version.clone(),
            dir: r.dir.clone(),
            manifest_hash: r.hash,
            lines: r.manifest.capabilities.iter().map(|c| c.line()).collect(),
            seq,
        };
        // Journal before answer (A-04 §5): the audit record, then the file.
        let mut body = MapBuilder::new();
        body.insert("op", enc(|w| w.text("install")));
        body.insert("package", enc(|w| w.text(&install.id)));
        body.insert("version", enc(|w| w.text(&install.version)));
        body.insert("manifest_hash", enc(|w| w.bytes(&install.manifest_hash)));
        body.insert("lines", enc(|w| w.u64(install.lines.len() as u64)));
        store.record(Emission {
            kind: Kind::Policy,
            principal: "human".into(),
            grant_id: None,
            task_id: None,
            chain_id: None,
            req_id: None,
            serial: None,
            body: body.finish(),
        })?;
        if write_install(&self.dir, &install).is_err() {
            return Err(TaskError::Store(
                std::io::Error::other("install record not written").into(),
            ));
        }
        self.installs.insert(install.id.clone(), install);
        Ok(vec![(To::Asker, FromPolicyd::Done { req: r.req })])
    }

    // ---- preview and create (A-08 §5.2) -----------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn preview(
        &mut self,
        store: &TaskStore,
        req: u64,
        slot: u64,
        package: &str,
        statement: &str,
        deadline_ms: u64,
        narrowing: &[u8],
        continuation: &str,
        resumes: &str,
        table_version: u64,
        agentd_live: bool,
    ) -> Out {
        if table_version == 0 {
            return refused(req, "policy_unavailable");
        }
        if !agentd_live {
            return refused(req, "agentd_unavailable");
        }
        let n = statement.chars().count();
        if n == 0 || n > MAX_STATEMENT {
            return refused(req, "statement_length");
        }
        let Some(install) = self.installs.get(package).cloned() else {
            return refused(req, "not_installed");
        };
        // Installed means reviewed *and still present as reviewed*.
        let m = match self.current_manifest(&install) {
            Ok(m) => m,
            Err(r) => return refused(req, r),
        };
        let deadline = match deadline_ms {
            0 => m.default_deadline_ms,
            d => d,
        };
        if deadline > MAX_DEADLINE_MS {
            return refused(req, "deadline_too_long");
        }
        let lines = match narrow(&install.lines, narrowing) {
            Some(l) => l,
            None => return refused(req, "narrowing_widens"),
        };
        if !continuation.is_empty() && !resumes.is_empty() {
            return refused(req, "continuation_and_resumes");
        }
        if !resumes.is_empty() {
            if let Err(why) = resume_ok(store, &m, resumes, statement) {
                return refused(req, why);
            }
        }
        if !continuation.is_empty() {
            let closed = store
                .tasks()
                .iter()
                .any(|t| t.id.to_text() == continuation && matches!(t.state, TaskState::Closed(_)));
            if !closed {
                return refused(req, "continuation_not_closed");
            }
        }
        let display = slot_display(
            &m,
            statement,
            deadline,
            &lines,
            narrowing.len() > 1,
            continuation,
            resumes,
        );
        if self.previews.len() >= MAX_PREVIEWS {
            self.previews.clear();
        }
        // One live preview per slot: a new draft retires the old one.
        self.previews.retain(|_, p| p.slot != slot);
        let preview = self.id();
        self.previews.insert(
            preview,
            Preview {
                slot,
                package: m.id.clone(),
                version: m.version.clone(),
                statement: statement.to_owned(),
                deadline_ms: deadline,
                lines,
                continuation: continuation.to_owned(),
                resumes: resumes.to_owned(),
                table_version,
                install_seq: install.seq,
            },
        );
        vec![(
            To::Asker,
            FromPolicyd::Preview {
                req,
                preview,
                display,
            },
        )]
    }

    fn current_manifest(&self, install: &Install) -> std::result::Result<Manifest, String> {
        let bytes = std::fs::read(install.dir.join("manifest.kdl")).map_err(|_| reason("not_installed"))?;
        if *blake3::hash(&bytes).as_bytes() != install.manifest_hash {
            return Err(reason("package_changed"));
        }
        let origin = manifest::origin_of(&install.dir, &self.roots).ok_or_else(|| reason("not_installed"))?;
        manifest::load(&install.dir, origin).map_err(|r| r.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &mut self,
        store: &mut TaskStore,
        req: u64,
        slot: u64,
        preview: u64,
        table_version: u64,
        agentd_live: bool,
        now_ms: u64,
    ) -> Result<Out> {
        let Some(p) = self.previews.remove(&preview) else {
            return Ok(refused(req, "preview_stale"));
        };
        let install_seq = self.installs.get(&p.package).map(|i| i.seq);
        if p.slot != slot || p.table_version != table_version || install_seq != Some(p.install_seq) {
            return Ok(refused(req, "preview_stale"));
        }
        if !agentd_live {
            return Ok(refused(req, "agentd_unavailable"));
        }
        let mut suffix = [0u8; 3];
        getrandom::fill(&mut suffix).expect("entropy");
        let principal = format!(
            "agent:{}-{:02x}{:02x}{:02x}",
            p.package, suffix[0], suffix[1], suffix[2]
        );
        let deadline = now_ms + p.deadline_ms;
        let scope = p.lines.join("\n");
        let resumes = if p.resumes.is_empty() {
            None
        } else {
            // Checked at preview; checked again, because a session can be
            // resumed only while its task is still closed and still known.
            match store.tasks().iter().find(|t| t.id.to_text() == p.resumes) {
                Some(t) if !t.state.is_live() => Some(t.id),
                _ => return Ok(refused(req, "preview_stale")),
            }
        };
        let (task, grant) = match store.open_for_human(
            &principal,
            &p.statement,
            deadline,
            &scope,
            &format!("slot:{slot}"),
            resumes,
            now_ms,
        ) {
            Ok(v) => v,
            Err(TaskError::Store(e)) => return Err(TaskError::Store(e)),
            Err(e) => return Ok(refused(req, format!("refused:{}", short(&e)))),
        };
        let task = task.to_text();
        Ok(vec![
            (
                To::Asker,
                FromPolicyd::TaskCreated {
                    req,
                    task: task.clone(),
                },
            ),
            (
                To::Agentd,
                FromPolicyd::Provision {
                    task,
                    principal,
                    package: p.package,
                    version: p.version,
                    statement: p.statement,
                    deadline_ms: deadline,
                    grant,
                    continuation: p.continuation,
                    resumes: p.resumes,
                },
            ),
        ])
    }

    // ---- task state from the console and the slot -------------------------

    fn find(store: &TaskStore, task: &str) -> Option<(Ulid, TaskState, String)> {
        store
            .tasks()
            .iter()
            .find(|t| t.id.to_text() == task)
            .map(|t| (t.id, t.state, t.principal.clone()))
    }

    /// Applies `event` and says so to everyone: `task_state`, and `revoked`
    /// when it closed the task (its grants went with it).
    fn transition(
        store: &mut TaskStore,
        id: Ulid,
        principal: &str,
        event: TaskEvent,
        why: &str,
    ) -> Result<Out> {
        let next = match store.apply(id, event) {
            Ok(n) => n,
            Err(e) => return store_err(e),
        };
        let (state, reason) = match next {
            TaskState::Closed(r) => ("closed", r.as_str().to_owned()),
            s => (s.as_str(), why.to_owned()),
        };
        let mut out = vec![(
            To::All,
            FromPolicyd::TaskState {
                task: id.to_text(),
                state: state.into(),
                reason,
            },
        )];
        if state == "closed" {
            out.push((
                To::All,
                FromPolicyd::Revoked {
                    principal: principal.to_owned(),
                },
            ));
        }
        Ok(out)
    }

    pub fn pause(&mut self, store: &mut TaskStore, req: u64, task: &str) -> Result<Out> {
        let Some((id, TaskState::Active, principal)) = Self::find(store, task) else {
            return Ok(refused(req, "not_active"));
        };
        let mut out = Self::transition(store, id, &principal, TaskEvent::Pause, "human")?;
        if out.is_empty() {
            return Ok(refused(req, "not_active"));
        }
        self.paused.insert(task.to_owned(), PausedBy::Human);
        out.insert(0, (To::Asker, FromPolicyd::Done { req }));
        Ok(out)
    }

    pub fn unpause(&mut self, store: &mut TaskStore, req: u64, task: &str) -> Result<Out> {
        let Some((id, TaskState::Paused, principal)) = Self::find(store, task) else {
            return Ok(refused(req, "not_paused"));
        };
        if self.paused.get(task) != Some(&PausedBy::Human) {
            return Ok(refused(req, "panel_only"));
        }
        let mut out = Self::transition(store, id, &principal, TaskEvent::Resume, "human")?;
        if out.is_empty() {
            return Ok(refused(req, "not_paused"));
        }
        self.paused.remove(task);
        out.insert(0, (To::Asker, FromPolicyd::Done { req }));
        Ok(out)
    }

    pub fn cancel(&mut self, store: &mut TaskStore, req: u64, task: &str, mode: CancelMode) -> Result<Out> {
        let Some((id, state, principal)) = Self::find(store, task) else {
            return Ok(refused(req, "not_found"));
        };
        if !state.is_live() {
            return Ok(refused(req, "closed"));
        }
        let event = match mode {
            CancelMode::Drain if state != TaskState::Draining => TaskEvent::Drain,
            _ => TaskEvent::Cancel,
        };
        let mut out = Self::transition(store, id, &principal, event, "human")?;
        if out.is_empty() {
            return Ok(refused(req, "illegal_transition"));
        }
        self.paused.remove(task);
        out.insert(0, (To::Asker, FromPolicyd::Done { req }));
        Ok(out)
    }

    /// The agent process ended (agentd). `completed` closes the task as
    /// completed through a drain; anything else closes it failed.
    pub fn exited(&mut self, store: &mut TaskStore, req: u64, task: &str, how: &str) -> Result<Out> {
        let Some((id, state, principal)) = Self::find(store, task) else {
            return Ok(refused(req, "not_found"));
        };
        if !state.is_live() {
            return Ok(vec![(To::Asker, FromPolicyd::Done { req })]);
        }
        let mut out = Vec::new();
        if how == "completed" {
            if state != TaskState::Draining {
                let o = Self::transition(store, id, &principal, TaskEvent::Drain, "exited")?;
                out.extend(o);
            }
            out.extend(Self::transition(
                store,
                id,
                &principal,
                TaskEvent::Drained,
                "exited",
            )?);
        } else {
            out.extend(Self::transition(
                store,
                id,
                &principal,
                TaskEvent::Fault,
                "exited",
            )?);
        }
        self.paused.remove(task);
        out.insert(0, (To::Asker, FromPolicyd::Done { req }));
        Ok(out)
    }
}

/// A-08 §5.4 eligibility, refused at preview: the package must be
/// resumable (F-24) and still installed (the caller has its manifest), the
/// task closed, of this package, and not closed by an incident; and the
/// statement must be the original verbatim, which the human commits again.
///
/// No close reason today marks an S-11 incident (I3, I5, I6); when one does,
/// it is refused here.
fn resume_ok(
    store: &TaskStore,
    m: &Manifest,
    resumes: &str,
    statement: &str,
) -> std::result::Result<(), &'static str> {
    if !m.is_resumable() {
        return Err("not_resumable");
    }
    let t = store
        .tasks()
        .iter()
        .find(|t| t.id.to_text() == resumes)
        .ok_or("resume_unknown")?;
    if t.state.is_live() {
        return Err("resume_not_closed");
    }
    if !t
        .principal
        .strip_prefix("agent:")
        .and_then(|p| p.strip_prefix(m.id.as_str()))
        .is_some_and(|rest| rest.starts_with('-'))
    {
        return Err("resume_other_package");
    }
    if store.statement_hash(t.id) != Some(*blake3::hash(statement.as_bytes()).as_bytes()) {
        return Err("resume_statement_changed");
    }
    Ok(())
}

fn short(e: &TaskError) -> &'static str {
    match e {
        TaskError::AlreadyLive { .. } => "already_live",
        TaskError::ExpiryBeyondDeadline => "deadline",
        TaskError::BadScope => "bad_scope",
        _ => "task",
    }
}

/// `narrowing` is empty or a canonical CBOR array of capability lines, each
/// one of `install`'s. Anything else is not provably narrower.
fn narrow(install: &[String], narrowing: &[u8]) -> Option<Vec<String>> {
    if narrowing.is_empty() {
        return Some(install.to_vec());
    }
    let mut r = Reader::new(narrowing);
    let n = r.array_len().ok()?;
    if n > 256 {
        return None;
    }
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let l = r.text().ok()?;
        if !install.iter().any(|i| i == l) || out.iter().any(|o: &String| o == l) {
            return None;
        }
        out.push(l.to_owned());
    }
    r.finish().ok()?;
    Some(out)
}

/// What the slot draws (COMP-10 §3.12, Appendix F-11), canonical CBOR.
/// `could` is the capability summary phrased as possibility: each granted
/// line as `<capability> on <scopes>` (A-07 §9.2; no taxonomy registry
/// exists yet to name taxonomies instead).
#[allow(clippy::too_many_arguments)]
fn slot_display(
    m: &Manifest,
    statement: &str,
    deadline_ms: u64,
    lines: &[String],
    narrowed: bool,
    cont: &str,
    resumes: &str,
) -> Vec<u8> {
    let mut d = MapBuilder::new();
    d.insert("package_name", enc(|w| w.text(&m.name)));
    d.insert("package", enc(|w| w.text(&m.id)));
    d.insert("publisher", enc(|w| w.text(&m.publisher)));
    d.insert("statement", enc(|w| w.text(statement)));
    d.insert("deadline_ms", enc(|w| w.u64(deadline_ms)));
    d.insert(
        "could",
        enc(|w| {
            w.array(lines.len());
            for l in lines {
                w.text(l);
            }
        }),
    );
    d.insert("narrowed", enc(|w| w.bool(narrowed)));
    d.insert("continuation", enc(|w| w.text(cont)));
    d.insert("resumes", enc(|w| w.text(resumes)));
    // policyd holds no chain summaries; agentd does. Until it reports one,
    // a continuation or a resume is drawn with the warning, the cautious
    // reading (A-08 §5.4: a resumed untrusted session stays untrusted).
    d.insert(
        "untrusted_predecessor",
        enc(|w| w.bool(!cont.is_empty() || !resumes.is_empty())),
    );
    d.finish()
}

/// What the install review modal draws (A-07 §3), canonical CBOR. Strings
/// are the manifest's own and untrusted; the compositor sanitises them.
fn review_display(m: &Manifest, previous: Option<&Install>) -> Vec<u8> {
    let prev: &[String] = previous.map_or(&[], |p| &p.lines);
    let mut d = MapBuilder::new();
    d.insert("id", enc(|w| w.text(&m.id)));
    d.insert("name", enc(|w| w.text(&m.name)));
    d.insert("publisher", enc(|w| w.text(&m.publisher)));
    d.insert("version", enc(|w| w.text(&m.version)));
    d.insert_opt("previous", previous.map(|p| enc(|w| w.text(&p.version))));
    d.insert(
        "caps",
        enc(|w| {
            w.array(m.capabilities.len());
            for c in &m.capabilities {
                let mut e = MapBuilder::new();
                e.insert("line", enc(|w| w.text(&c.line())));
                e.insert("because", enc(|w| w.text(&c.because)));
                e.insert("added", enc(|w| w.bool(!prev.contains(&c.line()))));
                w.raw(&e.finish());
            }
        }),
    );
    let now: Vec<String> = m.capabilities.iter().map(|c| c.line()).collect();
    let removed: Vec<&String> = prev.iter().filter(|l| !now.contains(l)).collect();
    d.insert(
        "removed",
        enc(|w| {
            w.array(removed.len());
            for l in &removed {
                w.text(l);
            }
        }),
    );
    // A-07 §3 item 4: notable absences.
    let absent: Vec<&str> = [
        ("capture", "screen.capture"),
        ("clipboard", "clipboard."),
        ("shell", "launch.outside_sandbox"),
        ("secrets", "secret."),
    ]
    .into_iter()
    .filter(|(_, p)| !m.capabilities.iter().any(|c| c.name.starts_with(p)))
    .map(|(n, _)| n)
    .collect();
    d.insert(
        "absent",
        enc(|w| {
            w.array(absent.len());
            for a in &absent {
                w.text(a);
            }
        }),
    );
    // A-07 §3 item 3: what grant issue would refuse, shown before install.
    let egress = m.sandbox.iter().any(|(k, _)| k == "net.egress");
    let mut warn = Vec::new();
    if egress && m.capabilities.iter().any(|c| c.name == "secret.expose") {
        warn.push("secret_expose_with_egress");
    }
    d.insert(
        "warnings",
        enc(|w| {
            w.array(warn.len());
            for x in &warn {
                w.text(x);
            }
        }),
    );
    d.insert(
        "sandbox",
        enc(|w| {
            w.array(m.sandbox.len());
            for (k, v) in &m.sandbox {
                w.text(&format!("{k} {v}"));
            }
        }),
    );
    d.finish()
}

fn encode_install(i: &Install) -> Vec<u8> {
    let mut m = MapBuilder::new();
    m.insert("id", enc(|w| w.text(&i.id)));
    m.insert("version", enc(|w| w.text(&i.version)));
    m.insert("dir", enc(|w| w.text(&i.dir.to_string_lossy())));
    m.insert("manifest_hash", enc(|w| w.bytes(&i.manifest_hash)));
    m.insert("seq", enc(|w| w.u64(i.seq)));
    m.insert(
        "lines",
        enc(|w| {
            w.array(i.lines.len());
            for l in &i.lines {
                w.text(l);
            }
        }),
    );
    m.finish()
}

fn decode_install(b: &[u8]) -> Option<Install> {
    let mut r = Reader::new(b);
    let n = r.map_begin().ok()?;
    let (mut id, mut version, mut dir, mut hash, mut seq, mut lines) = (None, None, None, None, None, None);
    for _ in 0..n {
        match r.key().ok()? {
            "dir" => dir = Some(PathBuf::from(r.text().ok()?)),
            "id" => id = Some(r.text().ok()?.to_owned()),
            "lines" => {
                let k = r.array_len().ok()?;
                let mut v = Vec::new();
                for _ in 0..k.min(256) {
                    v.push(r.text().ok()?.to_owned());
                }
                lines = Some(v);
            }
            "manifest_hash" => hash = Some(r.byte_array::<32>().ok()?),
            "seq" => seq = Some(r.u64().ok()?),
            "version" => version = Some(r.text().ok()?.to_owned()),
            _ => return None,
        }
    }
    r.map_end().ok()?;
    r.finish().ok()?;
    Some(Install {
        id: id?,
        version: version?,
        dir: dir?,
        manifest_hash: hash?,
        lines: lines?,
        seq: seq?,
    })
}

/// Written whole and renamed into place, so a crash leaves the old record or
/// the new one, never half of either.
fn write_install(dir: &Path, i: &Install) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = dir.join(format!(".{}.tmp", i.id));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(&encode_install(i))?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(format!("{}.cbor", i.id)))?;
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF: &str = r#"agent {
  id "ec-ref-agent"
  name "Reference agent"
  version "0.1.0"
  publisher "eclipse"
  entrypoint "bin/ec-ref-agent"
  task { default_deadline "2h"; max_depth 0 }
  capabilities {
    scene.list scope { app_id "foot" }
    because "See the terminal"
    seat.key scope { app_id "foot" }
    because "Type in it"
  }
}
"#;

    struct Env {
        root: PathBuf,
        store: TaskStore,
        d: Dispatch,
        pkg: PathBuf,
    }

    fn env(name: &str) -> Env {
        let root = std::env::temp_dir().join(format!("ec-dispatch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let agents = root.join("agents");
        let pkg = agents.join("ec-ref-agent/0.1.0");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("manifest.kdl"), REF).unwrap();
        let state = root.join("state");
        let store = TaskStore::open(&state, ed25519_dalek::SigningKey::from_bytes(&[3; 32])).unwrap();
        let d = Dispatch::open(&state, vec![(agents, Origin::System)]).unwrap();
        Env { root, store, d, pkg }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn install(e: &mut Env) {
        let out = e.d.install_begin(1, e.pkg.to_str().unwrap(), 0);
        let [(To::Asker, FromPolicyd::InstallReview { review, .. })] = out.as_slice() else {
            panic!("{out:?}")
        };
        let review = *review;
        let out = e.d.install_answer(&mut e.store, review, true, 1).unwrap();
        assert_eq!(out, vec![(To::Asker, FromPolicyd::Done { req: 1 })]);
    }

    fn preview(e: &mut Env, table: u64, narrowing: &[u8]) -> Out {
        e.d.preview(
            &e.store,
            2,
            7,
            "ec-ref-agent",
            "Say hello",
            0,
            narrowing,
            "",
            "",
            table,
            true,
        )
    }

    fn preview_id(out: &Out) -> u64 {
        match out.as_slice() {
            [(To::Asker, FromPolicyd::Preview { preview, .. })] => *preview,
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn shown_is_submitted_and_a_preview_is_single_use() {
        let mut e = env("shown");
        install(&mut e);
        let p = preview_id(&preview(&mut e, 5, &[]));
        let out = e.d.create(&mut e.store, 3, 7, p, 5, true, 1_000).unwrap();
        let [(To::Asker, FromPolicyd::TaskCreated { task, .. }), (
            To::Agentd,
            FromPolicyd::Provision {
                grant,
                principal,
                deadline_ms,
                ..
            },
        )] = out.as_slice()
        else {
            panic!("{out:?}")
        };
        let g = ec_policy_eval::grant::Grant::verify(
            grant,
            &ed25519_dalek::SigningKey::from_bytes(&[3; 32]).verifying_key(),
            2_000,
        )
        .unwrap();
        let lines: Vec<String> = g
            .capabilities
            .iter()
            .map(|c| format!("{} {}", c.name, c.scopes.join(" ")))
            .collect();
        assert_eq!(lines, ["scene.list app_id:foot", "seat.key app_id:foot"]);
        assert_eq!(*deadline_ms, 1_000 + 2 * 3_600_000);
        assert!(principal.starts_with("agent:ec-ref-agent-"));
        let t = e.store.tasks().iter().find(|t| &t.id.to_text() == task).unwrap();
        assert_eq!(t.origin_ref, "slot:7");
        assert_eq!(
            e.d.create(&mut e.store, 4, 7, p, 5, true, 1_000).unwrap(),
            refused(4, "preview_stale"),
            "single use"
        );
    }

    #[test]
    fn a_table_or_install_change_makes_the_preview_stale() {
        let mut e = env("stale");
        install(&mut e);
        let p = preview_id(&preview(&mut e, 5, &[]));
        assert_eq!(
            e.d.create(&mut e.store, 3, 7, p, 6, true, 1_000).unwrap(),
            refused(3, "preview_stale")
        );
        let p = preview_id(&preview(&mut e, 6, &[]));
        install(&mut e);
        assert_eq!(
            e.d.create(&mut e.store, 3, 7, p, 6, true, 1_000).unwrap(),
            refused(3, "preview_stale")
        );
        let p = preview_id(&preview(&mut e, 6, &[]));
        assert_eq!(
            e.d.create(&mut e.store, 3, 8, p, 6, true, 1_000).unwrap(),
            refused(3, "preview_stale"),
            "another slot's preview"
        );
    }

    #[test]
    fn narrowing_only_removes_whole_lines() {
        let mut e = env("narrow");
        install(&mut e);
        let one = enc(|w| {
            w.array(1);
            w.text("seat.key app_id:foot");
        });
        let p = preview_id(&preview(&mut e, 5, &one));
        let out = e.d.create(&mut e.store, 3, 7, p, 5, true, 1_000).unwrap();
        let FromPolicyd::Provision { grant, .. } = &out[1].1 else {
            panic!()
        };
        let g = ec_policy_eval::grant::Grant::verify(
            grant,
            &ed25519_dalek::SigningKey::from_bytes(&[3; 32]).verifying_key(),
            2_000,
        )
        .unwrap();
        assert_eq!(g.capabilities.len(), 1);
        let wider = enc(|w| {
            w.array(1);
            w.text("seat.key app_id:*");
        });
        assert_eq!(preview(&mut e, 5, &wider), refused(2, "narrowing_widens"));
    }

    #[test]
    fn previews_refuse_before_enter_not_after() {
        let mut e = env("refuse");
        assert_eq!(preview(&mut e, 5, &[]), refused(2, "not_installed"));
        install(&mut e);
        assert_eq!(preview(&mut e, 0, &[]), refused(2, "policy_unavailable"));
        assert_eq!(
            e.d.preview(&e.store, 2, 7, "ec-ref-agent", "x", 0, &[], "", "", 5, false),
            refused(2, "agentd_unavailable")
        );
        let long = "x".repeat(MAX_STATEMENT + 1);
        assert_eq!(
            e.d.preview(&e.store, 2, 7, "ec-ref-agent", &long, 0, &[], "", "", 5, true),
            refused(2, "statement_length")
        );
        assert_eq!(
            e.d.preview(
                &e.store,
                2,
                7,
                "ec-ref-agent",
                "x",
                MAX_DEADLINE_MS + 1,
                &[],
                "",
                "",
                5,
                true
            ),
            refused(2, "deadline_too_long")
        );
        std::fs::write(e.pkg.join("manifest.kdl"), REF.replace("Type in it", "Type")).unwrap();
        assert_eq!(
            preview(&mut e, 5, &[]),
            refused(2, "package_changed"),
            "edited after review"
        );
    }

    #[test]
    fn install_is_reviewed_and_survives_a_restart() {
        let mut e = env("install");
        let out = e.d.install_begin(1, "/nonexistent", 0);
        assert_eq!(out, refused(1, "not_found"));
        let out = e.d.install_begin(1, e.pkg.to_str().unwrap(), 0);
        let FromPolicyd::InstallReview { review, .. } = out[0].1 else {
            panic!()
        };
        assert_eq!(
            e.d.install_answer(&mut e.store, review, false, 1).unwrap(),
            refused(1, "refused_by_human")
        );
        assert!(e.d.installed("ec-ref-agent").is_none());
        install(&mut e);
        let again = Dispatch::open(&e.root.join("state"), e.d.roots.clone()).unwrap();
        assert_eq!(again.installed("ec-ref-agent").unwrap().lines.len(), 2);
    }

    #[test]
    fn pause_unpause_cancel_and_exit_move_the_task_and_say_so() {
        let mut e = env("state");
        install(&mut e);
        let p = preview_id(&preview(&mut e, 5, &[]));
        let out = e.d.create(&mut e.store, 3, 7, p, 5, true, 1_000).unwrap();
        let FromPolicyd::TaskCreated { task, .. } = &out[0].1 else {
            panic!()
        };
        let task = task.clone();
        let out = e.d.pause(&mut e.store, 10, &task).unwrap();
        assert_eq!(out[0], (To::Asker, FromPolicyd::Done { req: 10 }));
        assert!(matches!(&out[1].1, FromPolicyd::TaskState { state, .. } if state == "paused"));
        let out = e.d.unpause(&mut e.store, 11, &task).unwrap();
        assert!(matches!(&out[1].1, FromPolicyd::TaskState { state, .. } if state == "active"));
        assert_eq!(
            e.d.unpause(&mut e.store, 12, &task).unwrap(),
            refused(12, "not_paused")
        );
        let out = e.d.exited(&mut e.store, 13, &task, "completed").unwrap();
        assert!(out
            .iter()
            .any(|(_, m)| matches!(m, FromPolicyd::TaskState { state, reason, .. } if state == "closed" && reason == "completed")));
        assert!(out.iter().any(|(_, m)| matches!(m, FromPolicyd::Revoked { .. })));
        assert_eq!(
            e.d.cancel(&mut e.store, 14, &task, CancelMode::Immediate)
                .unwrap(),
            refused(14, "closed")
        );
        assert_eq!(
            e.d.pause(&mut e.store, 15, "nope").unwrap(),
            refused(15, "not_active")
        );
    }

    /// A-08 §13 "Resume does not launder", "Ineligible sessions": resuming
    /// needs a resumable package, a closed task of that package and its
    /// statement verbatim; the new task's chain root collapses the old chain
    /// and the breaker counters come along.
    #[test]
    fn resume_carries_the_chain_and_the_breaker_and_refuses_the_ineligible() {
        let mut e = env("resume");
        let preview_r = |e: &mut Env, statement: &str, resumes: &str| {
            e.d.preview(
                &e.store,
                2,
                7,
                "ec-ref-agent",
                statement,
                0,
                &[],
                "",
                resumes,
                5,
                true,
            )
        };
        // Not resumable: the package does not say so (F-24 default for a
        // non-local publisher).
        install(&mut e);
        let p = preview_id(&preview(&mut e, 5, &[]));
        let out = e.d.create(&mut e.store, 3, 7, p, 5, true, 1_000).unwrap();
        let FromPolicyd::TaskCreated { task: first, .. } = out[0].1.clone() else {
            panic!()
        };
        let id = e
            .store
            .tasks()
            .iter()
            .find(|t| t.id.to_text() == first)
            .unwrap()
            .id;
        e.store
            .update_counters(id, |c| c.denied_irreversible_streak = 2)
            .unwrap();
        assert_eq!(
            preview_r(&mut e, "Say hello", &first),
            refused(2, "not_resumable")
        );

        std::fs::write(
            e.pkg.join("manifest.kdl"),
            REF.replace("  capabilities {", "  resumable #true\n  capabilities {"),
        )
        .unwrap();
        install(&mut e);
        assert_eq!(
            preview_r(&mut e, "Say hello", &first),
            refused(2, "resume_not_closed")
        );
        e.d.exited(&mut e.store, 9, &first, "completed").unwrap();
        assert_eq!(
            preview_r(&mut e, "Say hi", &first),
            refused(2, "resume_statement_changed")
        );
        assert_eq!(
            preview_r(&mut e, "Say hello", "01NOPE"),
            refused(2, "resume_unknown")
        );
        assert_eq!(
            e.d.preview(
                &e.store,
                2,
                7,
                "ec-ref-agent",
                "Say hello",
                0,
                &[],
                &first,
                &first,
                5,
                true
            ),
            refused(2, "continuation_and_resumes")
        );

        let p = preview_id(&preview_r(&mut e, "Say hello", &first));
        let out = e.d.create(&mut e.store, 3, 7, p, 5, true, 2_000).unwrap();
        let FromPolicyd::Provision { resumes, task, .. } = &out[1].1 else {
            panic!()
        };
        assert_eq!(resumes, &first);
        let t = e.store.tasks().iter().find(|t| &t.id.to_text() == task).unwrap();
        assert_eq!(t.chain_root, format!("collapsed:{first}"));
        assert_eq!(
            t.counters.denied_irreversible_streak, 2,
            "the breaker streak carries"
        );
        assert_eq!(t.counters.prompts_shown, 0, "the prompt budget resets");
    }
}
