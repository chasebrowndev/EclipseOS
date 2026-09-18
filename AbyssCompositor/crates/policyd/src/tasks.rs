// SPDX-License-Identifier: AGPL-3.0-only
//! The A-04 task store and the S-01 §4 grant ledger.
//!
//! A task is the unit of accountability: every acting request belongs to one,
//! every grant names one, and closing one ends the authority of all of them.
//! This module is the single writer of that fact.
//!
//! Three properties are deliberate.
//!
//! * **Journal before answer.** Every mutation is appended to the audit store
//!   and fsynced *before* the in-memory state changes and before the caller is
//!   told it happened. A caller who is told "opened" and then loses the
//!   machine must find the task on restart; the reverse order cannot promise
//!   that.
//! * **Revocation on close is one operation.** Closing a task writes a single
//!   `grant_revoked` record naming every grant it covers. A loop that revoked
//!   grants one at a time would have a window in the middle where some of a
//!   dead task's authority still verified.
//! * **One live task per principal.** `active`, `paused` and `draining` all
//!   count. The limit exists so that a request can be attributed to exactly
//!   one task without asking the requester which one it meant — an attacker
//!   who could choose would choose the one with the budget left.

use crate::audit::{Kind, Record, Store, StoreError};
use policy_eval::cbor::{enc, MapBuilder, Reader, Writer};
use policy_eval::grant::{cose_sign1, protected_header, sig_structure};
use policy_eval::{cbor, CloseReason, Counters, Grant, Origin, Task, TaskEvent, TaskState, Ulid};
use std::path::Path;

/// Why a task or grant operation was refused.
///
/// These are policy answers, not failures: each one is a rule from A-04 or
/// S-01 §4 saying no. They are kept apart from [`StoreError`] so that a caller
/// cannot confuse "the disk is broken" with "you may not do that".
#[derive(Debug)]
pub enum TaskError {
    /// A-04 §2: the principal already has an `active`/`paused`/`draining` task.
    AlreadyLive {
        existing: Ulid,
    },
    UnknownTask(Ulid),
    /// A-04 §2: the state machine has no such edge.
    IllegalTransition {
        from: TaskState,
        event: TaskEvent,
    },
    /// S-01 §4: a grant may not outlive the task it names.
    ExpiryBeyondDeadline,
    /// S-01 §4: a grant naming a `closed` task is rejected at issue.
    TaskClosed,
    /// S-01 §4: an unattended prompt-class grant may not exceed one hour.
    UnattendedTooLong,
    Store(StoreError),
}

impl std::fmt::Display for TaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskError::AlreadyLive { existing } => {
                write!(f, "principal already has live task {}", existing.to_text())
            }
            TaskError::UnknownTask(id) => write!(f, "no such task {}", id.to_text()),
            TaskError::IllegalTransition { from, event } => {
                write!(f, "illegal transition {} on {:?}", from.as_str(), event)
            }
            TaskError::ExpiryBeyondDeadline => f.write_str("grant expiry exceeds the task deadline"),
            TaskError::TaskClosed => f.write_str("the task is closed"),
            TaskError::UnattendedTooLong => f.write_str("an unattended grant may not exceed one hour"),
            TaskError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for TaskError {}

impl From<StoreError> for TaskError {
    fn from(e: StoreError) -> Self {
        TaskError::Store(e)
    }
}

type Result<T> = std::result::Result<T, TaskError>;

/// A grant as `policyd` remembers it: the signed bytes it handed out, plus the
/// bookkeeping needed to revoke it.
#[derive(Debug, Clone)]
pub struct Issued {
    pub grant: Grant,
    /// The COSE_Sign1 envelope handed to the requester.
    pub cose: Vec<u8>,
    pub revoked: bool,
}

/// The authority on which tasks exist and which grants are live.
pub struct TaskStore {
    audit: Store,
    tasks: Vec<Task>,
    grants: Vec<Issued>,
    signing: ed25519_dalek::SigningKey,
}

fn task_body(t: &Task, op: &str) -> Vec<u8> {
    let mut m = MapBuilder::new();
    m.insert_opt("agent_note", t.agent_note.as_ref().map(|n| enc(|w| w.text(n))));
    m.insert("chain_root", enc(|w| w.text(&t.chain_root)));
    m.insert_opt(
        "close_reason",
        match t.state {
            TaskState::Closed(r) => Some(enc(|w| w.text(r.as_str()))),
            _ => None,
        },
    );
    m.insert("counters", t.counters.encode());
    m.insert("deadline", enc(|w| w.u64(t.deadline_ms)));
    m.insert("depth", enc(|w| w.u64(t.depth as u64)));
    m.insert("op", enc(|w| w.text(op)));
    m.insert("opened", enc(|w| w.u64(t.opened_ms)));
    m.insert("origin", enc(|w| w.text(t.origin.as_str())));
    m.insert("origin_ref", enc(|w| w.text(&t.origin_ref)));
    m.insert_opt("parent", t.parent_task_id.map(|p| enc(|w| w.bytes(&p.0))));
    m.insert("state", enc(|w| w.text(t.state.as_str())));
    m.insert("statement_hash", enc(|w| w.bytes(&t.statement_hash())));
    m.finish()
}

/// Rebuilds a task from a `task` record body. The record is the truth; the
/// in-memory task is a cache of it.
fn read_task_body(id: Ulid, principal: &str, body: &[u8]) -> cbor::Result<Task> {
    let mut r = Reader::new(body);
    let n = r.map_begin()?;
    let mut t = Task {
        id,
        principal: principal.to_owned(),
        origin: Origin::Human,
        origin_ref: String::new(),
        statement: String::new(),
        agent_note: None,
        parent_task_id: None,
        depth: 0,
        state: TaskState::Active,
        opened_ms: 0,
        deadline_ms: 0,
        chain_root: String::new(),
        counters: Counters::default(),
    };
    let mut state = String::from("active");
    let mut close_reason = None;
    for _ in 0..n {
        match r.key()? {
            "agent_note" => t.agent_note = Some(r.text()?.to_owned()),
            "chain_root" => t.chain_root = r.text()?.to_owned(),
            "close_reason" => close_reason = Some(CloseReason::parse(r.text()?).ok_or(cbor::Error::Type)?),
            "counters" => t.counters = Counters::decode(&mut r)?,
            "deadline" => t.deadline_ms = r.u64()?,
            "depth" => t.depth = u32::try_from(r.u64()?).map_err(|_| cbor::Error::Type)?,
            "op" => {
                r.text()?;
            }
            "opened" => t.opened_ms = r.u64()?,
            "origin" => t.origin = Origin::parse(r.text()?).ok_or(cbor::Error::Type)?,
            "origin_ref" => t.origin_ref = r.text()?.to_owned(),
            "parent" => t.parent_task_id = Some(Ulid(r.byte_array::<16>()?)),
            "state" => state = r.text()?.to_owned(),
            // S-04 §1.1 journals the hash, not the text, so a replayed task
            // has no statement. Nothing in policyd renders one; the text a
            // human saw is recoverable from the m12 `prompt` records.
            "statement_hash" => {
                r.byte_array::<32>()?;
            }
            _ => return Err(cbor::Error::Type),
        }
    }
    r.map_end()?;
    r.finish()?;
    t.state = match (state.as_str(), close_reason) {
        ("active", _) => TaskState::Active,
        ("paused", _) => TaskState::Paused,
        ("draining", _) => TaskState::Draining,
        ("closed", Some(r)) => TaskState::Closed(r),
        _ => return Err(cbor::Error::Type),
    };
    Ok(t)
}

impl TaskStore {
    /// Opens the store at `dir`, replaying its journal.
    pub fn open(dir: &Path, signing: ed25519_dalek::SigningKey) -> Result<TaskStore> {
        let audit = Store::open(dir)?;
        let mut tasks: Vec<Task> = Vec::new();
        let mut revoked: Vec<Ulid> = Vec::new();
        let mut issued: Vec<Issued> = Vec::new();
        // A record this process wrote and cannot now read is not a record to
        // skip past: skipping one drops a task or a grant on the floor and the
        // daemon comes up believing it holds less state than it does. Fail
        // closed and let the operator look at the journal.
        let mut replay_err: Option<cbor::Error> = None;
        audit.for_each(|rec| {
            if replay_err.is_some() {
                return;
            }
            let mut step = || -> cbor::Result<()> {
                match rec.kind {
                    Kind::Task => {
                        let id = rec.task_id.ok_or(cbor::Error::Type)?;
                        let t = read_task_body(id, &rec.principal, &rec.body)?;
                        match tasks.iter_mut().find(|x| x.id.0 == id.0) {
                            Some(slot) => *slot = t,
                            None => tasks.push(t),
                        }
                    }
                    Kind::GrantIssued => issued.push(replay_issued(&rec.body)?),
                    Kind::GrantRevoked => revoked.extend(replay_revoked(&rec.body)?),
                    Kind::Anchor => {}
                }
                Ok(())
            };
            if let Err(e) = step() {
                replay_err = Some(e);
            }
        })?;
        if let Some(e) = replay_err {
            return Err(TaskError::Store(StoreError::Cbor(e)));
        }
        for g in &mut issued {
            // A closed task's grants are revoked whether or not the
            // `grant_revoked` record made it to disk. The close record is the
            // authoritative operation (S-01 §4); treating the second record as
            // required would leave a crash window in which a restarted policyd
            // honours grants against a task it knows is closed.
            let task_closed = tasks
                .iter()
                .any(|t| t.id.0 == g.grant.task_id.0 && !t.state.is_live());
            if task_closed || revoked.iter().any(|r| r.0 == g.grant.id.0) {
                g.revoked = true;
            }
        }
        Ok(TaskStore {
            audit,
            tasks,
            grants: issued,
            signing,
        })
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    pub fn grants(&self) -> &[Issued] {
        &self.grants
    }

    pub fn task(&self, id: Ulid) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id.0 == id.0)
    }

    /// The principal's one live task, if it has one.
    pub fn live_task(&self, principal: &str) -> Option<&Task> {
        self.tasks
            .iter()
            .find(|t| t.principal == principal && t.state.is_live())
    }

    /// Opens a task. Fails if the principal already has a live one (A-04 §2).
    #[allow(clippy::too_many_arguments)]
    pub fn open_task(
        &mut self,
        principal: &str,
        origin: Origin,
        origin_ref: &str,
        statement: &str,
        deadline_ms: u64,
        now_ms: u64,
    ) -> Result<Ulid> {
        if let Some(t) = self.live_task(principal) {
            return Err(TaskError::AlreadyLive { existing: t.id });
        }
        let mut entropy = [0u8; 10];
        getrandom::fill(&mut entropy).expect("entropy");
        let id = Ulid::from_parts(now_ms, entropy);
        let task = Task {
            id,
            principal: principal.to_owned(),
            origin,
            origin_ref: origin_ref.to_owned(),
            statement: statement.to_owned(),
            agent_note: None,
            parent_task_id: None,
            depth: 0,
            state: TaskState::Active,
            opened_ms: now_ms,
            deadline_ms,
            chain_root: String::new(),
            counters: Counters::default(),
        };
        self.journal(&task, "open")?;
        self.tasks.push(task);
        Ok(id)
    }

    /// Applies a state-machine event, journalling the result before returning.
    pub fn apply(&mut self, id: Ulid, event: TaskEvent) -> Result<TaskState> {
        let idx = self
            .tasks
            .iter()
            .position(|t| t.id.0 == id.0)
            .ok_or(TaskError::UnknownTask(id))?;
        let from = self.tasks[idx].state;
        let next = from.transition(event).map_err(|e| TaskError::IllegalTransition {
            from: e.from,
            event: e.event,
        })?;

        // Journal a *copy* carrying the new state: nothing in memory changes
        // until the record is durable.
        let mut next_task = self.tasks[idx].clone();
        next_task.state = next;
        let op = if matches!(next, TaskState::Closed(_)) {
            "close"
        } else {
            "state"
        };
        self.journal(&next_task, op)?;

        if let TaskState::Closed(_) = next {
            self.revoke_for_task(id)?;
        }
        self.tasks[idx].state = next;
        Ok(next)
    }

    /// Records counter movement durably. The counters are the reason a
    /// restart cannot launder a spent budget (A-04 §6), so they take the same
    /// journal-before-answer path as a state change.
    pub fn update_counters(&mut self, id: Ulid, f: impl FnOnce(&mut Counters)) -> Result<()> {
        let idx = self
            .tasks
            .iter()
            .position(|t| t.id.0 == id.0)
            .ok_or(TaskError::UnknownTask(id))?;
        let mut next = self.tasks[idx].clone();
        f(&mut next.counters);
        self.journal(&next, "counters")?;
        self.tasks[idx].counters = next.counters;
        Ok(())
    }

    /// Mints and signs a grant against a live task (S-01 §4).
    pub fn issue_grant(&mut self, mut grant: Grant, now_ms: u64) -> Result<Vec<u8>> {
        let task = self
            .task(grant.task_id)
            .ok_or(TaskError::UnknownTask(grant.task_id))?;
        // `draining` and `paused` both refuse acting requests (A-04 §4), and a
        // grant is nothing but stored permission to act later — minting one
        // against a task that may not act is how a drain gets undone.
        if !task.state.accepts_acting_requests() {
            return Err(TaskError::TaskClosed);
        }
        if grant.expires_ms > task.deadline_ms {
            return Err(TaskError::ExpiryBeyondDeadline);
        }
        // The issuer stamps `issued_ms`; a requester-supplied one could be set
        // forward to make a long unattended window measure short.
        grant.issued_ms = now_ms;
        if grant.unattended && grant.expires_ms.saturating_sub(now_ms) > policy_eval::grant::UNATTENDED_MAX_MS
        {
            return Err(TaskError::UnattendedTooLong);
        }

        let mut entropy = [0u8; 10];
        getrandom::fill(&mut entropy).expect("entropy");
        grant.id = Ulid::from_parts(now_ms, entropy);
        grant.principal = task.principal.clone();

        let key = self.signing.verifying_key();
        let protected = protected_header(&key);
        let payload = grant.encode();
        let sig = {
            use ed25519_dalek::Signer;
            self.signing.sign(&sig_structure(&protected, &payload))
        };
        let cose = cose_sign1(&protected, &payload, &sig.to_bytes());

        let mut m = MapBuilder::new();
        m.insert("cose", enc(|w| w.bytes(&cose)));
        let body = m.finish();
        let rec = Record {
            seq: 0,
            ts: 0,
            mono: 0,
            kind: Kind::GrantIssued,
            principal: grant.principal.clone(),
            grant_id: Some(grant.id),
            task_id: Some(grant.task_id),
            chain_id: None,
            req_id: None,
            serial: None,
            body,
            prev_hash: [0; 32],
            hash: [0; 32],
        };
        self.audit.append(rec)?;
        self.audit.sync().map_err(TaskError::Store)?;
        self.grants.push(Issued {
            grant,
            cose: cose.clone(),
            revoked: false,
        });
        Ok(cose)
    }

    /// True if the grant exists here and has not been revoked. Expiry is not
    /// consulted: that is the verifier's job, at request time, with no grace.
    pub fn is_live_grant(&self, id: Ulid) -> bool {
        self.grants.iter().any(|g| g.grant.id.0 == id.0 && !g.revoked)
    }

    /// Revokes every grant naming `task` in a single journalled operation.
    fn revoke_for_task(&mut self, task: Ulid) -> Result<()> {
        let ids: Vec<Ulid> = self
            .grants
            .iter()
            .filter(|g| g.grant.task_id.0 == task.0 && !g.revoked)
            .map(|g| g.grant.id)
            .collect();
        if ids.is_empty() {
            return Ok(());
        }
        let principal = self.task(task).map(|t| t.principal.clone()).unwrap_or_default();
        let mut w = Writer::new();
        w.array(ids.len());
        for id in &ids {
            w.bytes(&id.0);
        }
        let mut m = MapBuilder::new();
        m.insert("grants", w.finish());
        m.insert("reason", enc(|w| w.text("task_closed")));
        let rec = Record {
            seq: 0,
            ts: 0,
            mono: 0,
            kind: Kind::GrantRevoked,
            principal,
            grant_id: None,
            task_id: Some(task),
            chain_id: None,
            req_id: None,
            serial: None,
            body: m.finish(),
            prev_hash: [0; 32],
            hash: [0; 32],
        };
        self.audit.append(rec)?;
        self.audit.sync()?;
        for g in &mut self.grants {
            if ids.iter().any(|i| i.0 == g.grant.id.0) {
                g.revoked = true;
            }
        }
        Ok(())
    }

    fn journal(&mut self, task: &Task, op: &str) -> Result<()> {
        let rec = Record {
            seq: 0,
            ts: 0,
            mono: 0,
            kind: Kind::Task,
            principal: task.principal.clone(),
            grant_id: None,
            task_id: Some(task.id),
            chain_id: None,
            req_id: None,
            serial: None,
            body: task_body(task, op),
            prev_hash: [0; 32],
            hash: [0; 32],
        };
        self.audit.append(rec)?;
        self.audit.sync()?;
        Ok(())
    }
}

fn replay_issued(body: &[u8]) -> cbor::Result<Issued> {
    let mut r = Reader::new(body);
    let n = r.map_begin()?;
    let mut cose = Vec::new();
    for _ in 0..n {
        match r.key()? {
            "cose" => cose = r.bytes()?.to_vec(),
            _ => return Err(cbor::Error::Type),
        }
    }
    r.map_end()?;
    r.finish()?;
    // Replay reads the payload it signed earlier; the signature is re-checked
    // by whoever presents the grant, not by the process that minted it.
    let mut rr = Reader::new(&cose);
    if rr.array_len()? != 4 {
        return Err(cbor::Error::Type);
    }
    rr.bytes()?;
    rr.map_begin()?;
    rr.map_end()?;
    let payload = rr.bytes()?.to_vec();
    let grant = Grant::decode(&payload)?;
    Ok(Issued {
        grant,
        cose,
        revoked: false,
    })
}

fn replay_revoked(body: &[u8]) -> cbor::Result<Vec<Ulid>> {
    let mut out = Vec::new();
    let mut r = Reader::new(body);
    let n = r.map_begin()?;
    for _ in 0..n {
        match r.key()? {
            "grants" => {
                let len = r.array_len()?;
                for _ in 0..len {
                    out.push(Ulid(r.byte_array::<16>()?));
                }
            }
            _ => r.skip()?,
        }
    }
    r.map_end()?;
    r.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use policy_eval::{Capability, Constraints};
    use std::fs;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("policyd-tasks-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[9u8; 32])
    }

    fn store(dir: &Path) -> TaskStore {
        TaskStore::open(dir, key()).unwrap()
    }

    fn grant_for(task: Ulid, expires_ms: u64) -> Grant {
        Grant {
            id: Ulid([0; 16]),
            principal: String::new(),
            issued_ms: 1_000,
            expires_ms,
            issuer: "policyd".into(),
            task_id: task,
            capabilities: vec![Capability {
                name: "fs.read".into(),
                scopes: vec!["~/notes".into()],
                quota: None,
            }],
            constraints: Constraints::default(),
            unattended: false,
        }
    }

    #[test]
    fn closing_a_task_revokes_every_grant_in_one_operation() {
        let dir = tmp("revoke");
        let mut s = store(&dir);
        let t = s
            .open_task("agent:a", Origin::Human, "chat:1", "tidy notes", 100_000, 1_000)
            .unwrap();
        s.issue_grant(grant_for(t, 50_000), 1_000).unwrap();
        s.issue_grant(grant_for(t, 60_000), 1_001).unwrap();
        s.apply(t, TaskEvent::Drain).unwrap();
        let before = s.audit.next_seq();
        s.apply(t, TaskEvent::Drained).unwrap();
        // One task record plus exactly one revocation record.
        assert_eq!(s.audit.next_seq(), before + 2);
        assert!(s.grants().iter().all(|g| g.revoked));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_rotation_does_not_lose_tasks_counters_or_grants() {
        let dir = tmp("rotate");
        let mut s = store(&dir);
        let t = s
            .open_task("agent:a", Origin::Human, "chat:1", "tidy notes", 100_000, 1_000)
            .unwrap();
        s.issue_grant(grant_for(t, 50_000), 1_000).unwrap();
        s.update_counters(t, |c| c.prompts_shown += 7).unwrap();
        // Rotating mid-task is the ordinary case — segments turn over on size,
        // not on anything a task does — so nothing here may depend on the live
        // task living in the open segment.
        s.audit.rotate().unwrap();
        s.update_counters(t, |c| c.prompts_shown += 1).unwrap();
        drop(s);

        let s = store(&dir);
        let task = s.task(t).expect("task survives the rotation");
        assert_eq!(task.state, TaskState::Active);
        assert_eq!(task.counters.prompts_shown, 8);
        assert_eq!(s.grants().len(), 1);
        assert!(!s.grants()[0].revoked);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_closed_task_revokes_its_grants_on_replay_without_the_revocation_record() {
        let dir = tmp("close-window");
        let mut s = store(&dir);
        let t = s
            .open_task("agent:a", Origin::Human, "chat:1", "tidy", 100_000, 1_000)
            .unwrap();
        s.issue_grant(grant_for(t, 50_000), 1_000).unwrap();
        let id = s.grants()[0].grant.id;
        s.apply(t, TaskEvent::Cancel).unwrap();
        drop(s);

        // Simulate the crash window: the close record is durable, the
        // revocation record is not. The close is the authoritative operation.
        let mut plain = fs::read(dir.join("current.open")).unwrap();
        let cut = last_frame_start(&plain);
        plain.truncate(cut);
        fs::write(dir.join("current.open"), &plain).unwrap();

        let s = store(&dir);
        assert!(!s.is_live_grant(id));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// The offset at which the final framed record begins.
    fn last_frame_start(buf: &[u8]) -> usize {
        let mut pos = 0usize;
        let mut last = 0usize;
        while pos + 4 <= buf.len() {
            let len = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
            let end = pos + 4 + len;
            if len == 0 || end > buf.len() {
                break;
            }
            last = pos;
            pos = end;
        }
        last
    }

    #[test]
    fn a_second_live_task_per_principal_is_rejected() {
        let dir = tmp("second");
        let mut s = store(&dir);
        let t = s
            .open_task("agent:a", Origin::Human, "chat:1", "one", 100_000, 1_000)
            .unwrap();
        assert!(matches!(
            s.open_task("agent:a", Origin::Human, "chat:1", "two", 100_000, 1_001),
            Err(TaskError::AlreadyLive { .. })
        ));
        // Pausing does not free the slot; only closing does.
        s.apply(t, TaskEvent::Pause).unwrap();
        assert!(s
            .open_task("agent:a", Origin::Human, "chat:1", "two", 100_000, 1_002)
            .is_err());
        s.apply(t, TaskEvent::Resume).unwrap();
        s.apply(t, TaskEvent::Drain).unwrap();
        s.apply(t, TaskEvent::Drained).unwrap();
        assert!(s
            .open_task("agent:a", Origin::Human, "chat:1", "two", 100_000, 1_003)
            .is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn counters_survive_a_restart() {
        let dir = tmp("counters");
        let t = {
            let mut s = store(&dir);
            let t = s
                .open_task("agent:a", Origin::Human, "chat:1", "work", 100_000, 1_000)
                .unwrap();
            s.update_counters(t, |c| {
                c.prompts_allowed = 7;
                c.irreversible.record(5_000, 3_600_000);
                c.class_ring_mut("delete").record(5_000, 3_600_000);
            })
            .unwrap();
            t
        };
        let s = store(&dir);
        let task = s.task(t).expect("task survives");
        assert_eq!(task.counters.prompts_allowed, 7);
        assert_eq!(task.counters.irreversible.count(5_001, 3_600_000), 1);
        assert_eq!(task.state, TaskState::Active);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_grant_outliving_its_task_is_refused_at_issue() {
        let dir = tmp("deadline");
        let mut s = store(&dir);
        let t = s
            .open_task("agent:a", Origin::Human, "chat:1", "work", 50_000, 1_000)
            .unwrap();
        assert!(matches!(
            s.issue_grant(grant_for(t, 50_001), 1_000),
            Err(TaskError::ExpiryBeyondDeadline)
        ));
        assert!(s.issue_grant(grant_for(t, 50_000), 1_000).is_ok());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_closed_task_issues_nothing_more() {
        let dir = tmp("closed");
        let mut s = store(&dir);
        let t = s
            .open_task("agent:a", Origin::Human, "chat:1", "work", 50_000, 1_000)
            .unwrap();
        s.apply(t, TaskEvent::Cancel).unwrap();
        assert!(matches!(
            s.issue_grant(grant_for(t, 10_000), 1_000),
            Err(TaskError::TaskClosed)
        ));
        assert!(s.apply(t, TaskEvent::Cancel).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn issued_grants_replay_across_a_restart() {
        let dir = tmp("replay-grants");
        let t = {
            let mut s = store(&dir);
            let t = s
                .open_task("agent:a", Origin::Human, "chat:1", "work", 90_000, 1_000)
                .unwrap();
            s.issue_grant(grant_for(t, 50_000), 1_000).unwrap();
            t
        };
        let s = store(&dir);
        assert_eq!(s.grants().len(), 1);
        let g = &s.grants()[0];
        assert_eq!(g.grant.task_id.0, t.0);
        assert!(s.is_live_grant(g.grant.id));
        assert!(g.grant.allows("fs.read", "~/notes"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
