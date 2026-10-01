// SPDX-License-Identifier: AGPL-3.0-only
//! A-04 task objects: identity, state machine, and journalled counters.
//!
//! A task is the unit whose authority can be reasoned about. Every grant
//! names one (A-04 §2), a principal has at most one live task (§5), and
//! closing a task revokes its grants in a single operation (§2). The type
//! here is shared by `policyd`, which owns the store, and `abyss`, which
//! reads task state off a grant it is verifying — one definition, because
//! two would be two answers to "is this task still live".
//!
//! Two properties are enforced by construction rather than by review:
//!
//! * [`TaskState::transition`] is total and returns an error for every edge
//!   A-04 §4 does not draw, so an illegal transition cannot be spelled.
//! * `closed` has no outgoing edge at all. There is no reopen, because
//!   resuming work means a new origin, which means a human (§4).

use crate::cbor::{self, enc, MapBuilder, Reader, Writer};

/// A ULID. Stored as the raw 128 bits rather than the 26-character text,
/// because the text form is a projection and comparing projections is how
/// two ids that are equal end up looking different.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ulid(pub [u8; 16]);

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

impl Ulid {
    /// Builds a ULID from a millisecond timestamp and 80 bits of entropy.
    /// Minting — and therefore the entropy source — belongs to `policyd`;
    /// this crate only assembles and parses.
    pub fn from_parts(ms: u64, entropy: [u8; 10]) -> Self {
        let mut b = [0u8; 16];
        b[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
        b[6..].copy_from_slice(&entropy);
        Ulid(b)
    }

    /// The 48-bit timestamp, in milliseconds since the Unix epoch.
    pub fn timestamp_ms(&self) -> u64 {
        let mut t = [0u8; 8];
        t[2..].copy_from_slice(&self.0[..6]);
        u64::from_be_bytes(t)
    }

    /// Crockford base32, 26 characters — the form the spec shows in KDL and
    /// the only form that should ever reach a human or a log line.
    pub fn to_text(self) -> String {
        let mut out = [0u8; 26];
        let n = u128::from_be_bytes(self.0);
        for (i, slot) in out.iter_mut().enumerate() {
            let shift = 125 - i * 5;
            let v = if i == 0 {
                (n >> 125) as usize
            } else {
                ((n >> shift) & 0x1f) as usize
            };
            *slot = CROCKFORD[v];
        }
        // Every byte came from CROCKFORD, which is ASCII.
        String::from_utf8(out.to_vec()).expect("crockford alphabet is ascii")
    }
}

impl std::fmt::Display for Ulid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_text())
    }
}

/// Why a task reached its terminal state (A-04 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Completed,
    Cancelled,
    Expired,
    Failed,
}

impl CloseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            CloseReason::Completed => "completed",
            CloseReason::Cancelled => "cancelled",
            CloseReason::Expired => "expired",
            CloseReason::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "completed" => CloseReason::Completed,
            "cancelled" => CloseReason::Cancelled,
            "expired" => CloseReason::Expired,
            "failed" => CloseReason::Failed,
            _ => return None,
        })
    }
}

/// The A-04 §4 state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Active,
    Paused,
    Draining,
    Closed(CloseReason),
}

/// The events that drive it. Naming the edges rather than the target states
/// is what makes `active → closed{completed}` impossible to reach without
/// draining first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskEvent {
    Pause,
    Resume,
    Drain,
    /// The drain finished, or `drain_timeout` elapsed.
    Drained,
    Cancel,
    Deadline,
    Fault,
}

/// A transition A-04 §4 does not draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: TaskState,
    pub event: TaskEvent,
}

impl std::fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "illegal task transition {:?} on {:?}", self.from, self.event)
    }
}

impl std::error::Error for IllegalTransition {}

impl TaskState {
    /// True while the task occupies its principal's single live slot
    /// (A-04 §5). `closed` is the only state that frees it.
    pub fn is_live(self) -> bool {
        !matches!(self, TaskState::Closed(_))
    }

    /// True when acting requests may flow. `paused` and `draining` both
    /// refuse new acting requests; only `active` permits them.
    pub fn accepts_acting_requests(self) -> bool {
        matches!(self, TaskState::Active)
    }

    /// The whole of A-04 §4 in one total function.
    pub fn transition(self, event: TaskEvent) -> Result<TaskState, IllegalTransition> {
        use CloseReason::*;
        use TaskEvent::*;
        use TaskState::*;
        let next = match (self, event) {
            (Active, Pause) => Paused,
            (Paused, Resume) => Active,
            (Active, Drain) | (Paused, Drain) => Draining,
            (Draining, Drained) => Closed(Completed),
            (Active, Cancel) | (Paused, Cancel) | (Draining, Cancel) => Closed(Cancelled),
            (Active, Deadline) | (Paused, Deadline) | (Draining, Deadline) => Closed(Expired),
            (Active, Fault) | (Paused, Fault) | (Draining, Fault) => Closed(Failed),
            // `closed` is terminal and unconditional (§4). Every event on it
            // falls through to the error, including a second `cancel`.
            _ => return Err(IllegalTransition { from: self, event }),
        };
        Ok(next)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Active => "active",
            TaskState::Paused => "paused",
            TaskState::Draining => "draining",
            TaskState::Closed(_) => "closed",
        }
    }
}

/// Where a task came from (A-04 §3). There are exactly two, and neither is
/// "the agent asked": a task with no human somewhere up its chain is the
/// laundering hole A-04 §5 closes structurally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Human,
    ParentTask,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::ParentTask => "parent_task",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "human" => Some(Origin::Human),
            "parent_task" => Some(Origin::ParentTask),
            _ => None,
        }
    }
}

/// How many events a rate-windowed counter remembers.
///
/// The ceilings in A-04 §6 top out at 20 per hour, so 32 slots hold a full
/// window with room to spare. When the ring is full the oldest entry is
/// dropped — which can only ever *understate* the count, and an understated
/// count is one that was already over its ceiling and refused.
pub const RATE_RING: usize = 32;

/// A rate-windowed counter, stored as a ring of timestamps rather than an
/// integer (A-04 §6).
///
/// The integer form loses the shape of the window across a restart: a
/// partially-spent hour either resets to zero, which an attacker induces by
/// crashing us, or never decays. Timestamps decay correctly and replay
/// correctly, at the cost of 256 bytes per counter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RateRing {
    stamps: Vec<u64>,
}

impl RateRing {
    pub fn new() -> Self {
        RateRing { stamps: Vec::new() }
    }

    /// Records an event at `now_ms` and returns the number of events inside
    /// `window_ms`, the new one included.
    pub fn record(&mut self, now_ms: u64, window_ms: u64) -> usize {
        self.prune(now_ms, window_ms);
        if self.stamps.len() == RATE_RING {
            self.stamps.remove(0);
        }
        self.stamps.push(now_ms);
        self.stamps.len()
    }

    /// The count inside the window without recording anything — the form a
    /// check uses, since a refused action must not consume budget.
    pub fn count(&self, now_ms: u64, window_ms: u64) -> usize {
        self.stamps
            .iter()
            .filter(|&&t| now_ms.saturating_sub(t) < window_ms)
            .count()
    }

    fn prune(&mut self, now_ms: u64, window_ms: u64) {
        self.stamps.retain(|&t| now_ms.saturating_sub(t) < window_ms);
    }

    pub fn stamps(&self) -> &[u64] {
        &self.stamps
    }

    pub fn from_stamps(stamps: Vec<u64>) -> Self {
        RateRing { stamps }
    }
}

/// One hour, the window every rate-limited ceiling in A-04 §6 uses.
pub const HOUR_MS: u64 = 3_600_000;

/// The A-04 §6 counter block. Every field is journalled before the caller
/// hears the answer; see `policyd`'s task store for that ordering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counters {
    pub prompts_shown: u32,
    pub prompts_allowed: u32,
    /// Rate-windowed: 20 per hour across all classes (A-04 §6).
    pub irreversible: RateRing,
    /// Rate-windowed per class. Kept as a sorted association list rather
    /// than a hash map so the CBOR encoding is canonical without a sort at
    /// write time.
    pub irreversible_by_class: Vec<(String, RateRing)>,
    pub denied_irreversible_streak: u32,
    pub retries: u32,
    pub egress_bytes_up: u64,
    pub channel_messages: u32,
}

/// The overall irreversible-action ceiling, per hour (A-04 §6, S-06 §8).
pub const IRREVERSIBLE_PER_HOUR: usize = 20;
/// Consecutive denied irreversible actions before the task is paused.
pub const DENIED_IRREVERSIBLE_STREAK_PAUSE: u32 = 3;
/// Prompts in one grant before `policyd` flags it as mis-sized (S-02 §5).
pub const PROMPT_BUDGET: u32 = 3;

impl Counters {
    pub fn class_ring_mut(&mut self, class: &str) -> &mut RateRing {
        if let Some(i) = self.irreversible_by_class.iter().position(|(c, _)| c == class) {
            return &mut self.irreversible_by_class[i].1;
        }
        self.irreversible_by_class
            .push((class.to_owned(), RateRing::new()));
        // Canonical CBOR orders map keys by their *encoded* bytes, which for
        // text strings means shorter first and only then lexicographic. Keep
        // the list in that order so `encode` never has to sort.
        self.irreversible_by_class
            .sort_by(|a, b| (a.0.len(), &a.0).cmp(&(b.0.len(), &b.0)));
        let i = self
            .irreversible_by_class
            .iter()
            .position(|(c, _)| c == class)
            .expect("just inserted");
        &mut self.irreversible_by_class[i].1
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut m = MapBuilder::new();
        m.insert("prompts_shown", enc(|w| w.u64(self.prompts_shown as u64)));
        m.insert("prompts_allowed", enc(|w| w.u64(self.prompts_allowed as u64)));
        m.insert("irreversible", encode_ring(&self.irreversible));
        let mut by_class = MapBuilder::new();
        for (class, ring) in &self.irreversible_by_class {
            by_class.insert(class, encode_ring(ring));
        }
        m.insert("irreversible_by_class", by_class.finish());
        m.insert(
            "denied_irreversible_streak",
            enc(|w| w.u64(self.denied_irreversible_streak as u64)),
        );
        m.insert("retries", enc(|w| w.u64(self.retries as u64)));
        m.insert("egress_bytes_up", enc(|w| w.u64(self.egress_bytes_up)));
        m.insert("channel_messages", enc(|w| w.u64(self.channel_messages as u64)));
        m.finish()
    }

    pub fn decode(r: &mut Reader<'_>) -> cbor::Result<Counters> {
        let n = r.map_begin()?;
        let mut c = Counters::default();
        for _ in 0..n {
            match r.key()? {
                "prompts_shown" => c.prompts_shown = small(r.u64()?)?,
                "prompts_allowed" => c.prompts_allowed = small(r.u64()?)?,
                "irreversible" => c.irreversible = decode_ring(r)?,
                "irreversible_by_class" => {
                    let k = r.map_begin()?;
                    for _ in 0..k {
                        let class = r.key()?.to_owned();
                        c.irreversible_by_class.push((class, decode_ring(r)?));
                    }
                    r.map_end()?;
                }
                "denied_irreversible_streak" => c.denied_irreversible_streak = small(r.u64()?)?,
                "retries" => c.retries = small(r.u64()?)?,
                "egress_bytes_up" => c.egress_bytes_up = r.u64()?,
                "channel_messages" => c.channel_messages = small(r.u64()?)?,
                // Milestone 12 may add counters to a store milestone 10
                // wrote. Skipping validates without understanding.
                _ => r.skip()?,
            }
        }
        r.map_end()?;
        Ok(c)
    }
}

fn small(v: u64) -> cbor::Result<u32> {
    u32::try_from(v).map_err(|_| cbor::Error::Type)
}

fn encode_ring(ring: &RateRing) -> Vec<u8> {
    let mut w = Writer::new();
    w.array(ring.stamps().len());
    for &t in ring.stamps() {
        w.u64(t);
    }
    w.finish()
}

fn decode_ring(r: &mut Reader<'_>) -> cbor::Result<RateRing> {
    let n = r.array_len()?;
    if n as usize > RATE_RING {
        return Err(cbor::Error::Type);
    }
    let mut stamps = Vec::with_capacity(n as usize);
    for _ in 0..n {
        stamps.push(r.u64()?);
    }
    Ok(RateRing::from_stamps(stamps))
}

/// The A-04 §2 task object.
///
/// `statement` is trusted — it comes from the origin and is what a prompt
/// shows. `agent_note` is agent-supplied and must never be rendered where a
/// human could read it as the statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: Ulid,
    pub principal: String,
    pub origin: Origin,
    pub origin_ref: String,
    pub statement: String,
    pub agent_note: Option<String>,
    pub parent_task_id: Option<Ulid>,
    pub depth: u32,
    pub state: TaskState,
    pub opened_ms: u64,
    /// Hard deadline. Every grant's `expires` must be ≤ this (S-01 §4).
    pub deadline_ms: u64,
    pub chain_root: String,
    pub counters: Counters,
}

impl Task {
    /// BLAKE3 of the trusted statement, for the `task` audit body. The
    /// statement itself is human text about human work; the hash is what
    /// links a record to it without copying it into every record.
    pub fn statement_hash(&self) -> [u8; 32] {
        *blake3::hash(self.statement.as_bytes()).as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_is_terminal_for_every_event() {
        let closed = TaskState::Closed(CloseReason::Completed);
        for e in [
            TaskEvent::Pause,
            TaskEvent::Resume,
            TaskEvent::Drain,
            TaskEvent::Drained,
            TaskEvent::Cancel,
            TaskEvent::Deadline,
            TaskEvent::Fault,
        ] {
            assert!(closed.transition(e).is_err(), "{e:?} reopened a closed task");
        }
    }

    #[test]
    fn completion_requires_draining_first() {
        assert!(TaskState::Active.transition(TaskEvent::Drained).is_err());
        let s = TaskState::Active.transition(TaskEvent::Drain).unwrap();
        assert_eq!(s, TaskState::Draining);
        assert_eq!(
            s.transition(TaskEvent::Drained).unwrap(),
            TaskState::Closed(CloseReason::Completed)
        );
    }

    #[test]
    fn only_active_accepts_acting_requests() {
        assert!(TaskState::Active.accepts_acting_requests());
        assert!(!TaskState::Paused.accepts_acting_requests());
        assert!(!TaskState::Draining.accepts_acting_requests());
        assert!(!TaskState::Closed(CloseReason::Cancelled).accepts_acting_requests());
    }

    #[test]
    fn only_closed_frees_the_principal_slot() {
        assert!(TaskState::Active.is_live());
        assert!(TaskState::Paused.is_live());
        assert!(TaskState::Draining.is_live());
        assert!(!TaskState::Closed(CloseReason::Expired).is_live());
    }

    #[test]
    fn ulid_text_is_26_crockford_chars_and_keeps_the_timestamp() {
        let u = Ulid::from_parts(0x0192_3456_789a, [0xff; 10]);
        let t = u.to_text();
        assert_eq!(t.len(), 26);
        assert!(t.bytes().all(|b| CROCKFORD.contains(&b)));
        assert_eq!(u.timestamp_ms(), 0x0192_3456_789a);
    }

    #[test]
    fn ulids_sort_in_mint_order() {
        let a = Ulid::from_parts(1000, [0; 10]);
        let b = Ulid::from_parts(1001, [0; 10]);
        assert!(a < b);
        assert!(a.to_text() < b.to_text());
    }

    #[test]
    fn rate_ring_decays_and_survives_a_round_trip() {
        let mut r = RateRing::new();
        assert_eq!(r.record(0, HOUR_MS), 1);
        assert_eq!(r.record(1000, HOUR_MS), 2);
        // An hour later the old entries are outside the window.
        assert_eq!(r.count(HOUR_MS + 2000, HOUR_MS), 0);

        let enc = encode_ring(&r);
        let mut rd = Reader::new(&enc);
        assert_eq!(decode_ring(&mut rd).unwrap(), r);
    }

    #[test]
    fn rate_ring_never_overstates_when_full() {
        let mut r = RateRing::new();
        for i in 0..RATE_RING as u64 + 10 {
            r.record(i, HOUR_MS);
        }
        assert!(r.count(RATE_RING as u64 + 10, HOUR_MS) <= RATE_RING);
    }

    #[test]
    fn counters_round_trip() {
        let mut c = Counters {
            prompts_shown: 2,
            egress_bytes_up: 9_000_000_000,
            ..Counters::default()
        };
        c.irreversible.record(500, HOUR_MS);
        c.class_ring_mut("delete").record(600, HOUR_MS);
        c.class_ring_mut("send").record(700, HOUR_MS);
        let bytes = c.encode();
        let mut r = Reader::new(&bytes);
        let back = Counters::decode(&mut r).unwrap();
        r.finish().unwrap();
        assert_eq!(back, c);
    }
}
