// SPDX-License-Identifier: AGPL-3.0-only
//! The `eclipse_semantic_v1` tree model (COMP-09 §2, §3; P-01 §1, §2).
//!
//! State-free and Wayland-free: [`Model`] is what a client's requests edit,
//! [`View`] is what a commit publishes. Nothing here knows a window, a
//! client or an agent, so the fuzz loops in `fuzz.rs` drive it directly.
//!
//! Everything the client sends is data (COMP-09 §1):
//!
//! - **Raise only.** `sensitivity`, `ext.irreversible`, `ext.credential`, a
//!   `password` role and an `ext.echo=false` raise are recorded in a
//!   side table keyed by node id, *not* on the node. Removing a node and
//!   adding another with the same id therefore gets the old classification
//!   back: id reuse cannot lower anything. When that table is full the
//!   whole surface is [`Model::tainted`], which reads as `secret`.
//! - **Inheritance.** A node's effective class is the maximum of its own and
//!   its ancestors' ([`effective`]), so moving a node under a secret parent
//!   raises it and moving it out never lowers what it was declared.
//! - **Downgrades wait** (S-05 §5). Echo coming back on holds the raise for
//!   the S-05 grace; removing a secret node keeps the surface "known secret"
//!   for the same grace ([`Model::ghost_until`]).
//! - **Fail closed.** A tree that cannot place a secret node, or that
//!   dropped one to the node budget, is reported as [`SecretFacts::unplaced`]
//!   and the caller covers the whole surface.
//!
//! The class of the *window* is not decided here: it comes from the TCB
//! scene filter and is applied by the caller ([`View::snapshot`]'s `floor`).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ec_policy_eval::Class;

/// COMP-09 §3 / P-01 §10.1: nodes kept per surface.
pub const NODE_BUDGET: usize = 4_000;
/// Nodes a client may have in its model before it is cut off. Well above the
/// budget so a 100k-node publish truncates deterministically (COMP-09 §6)
/// rather than erroring, and bounded so a client cannot grow it forever.
pub const MODEL_CAP: usize = 131_072;
/// Deepest nesting `add_node` and `move_node` accept.
pub const MAX_DEPTH: usize = 512;
/// COMP-09 §7 item 3: 16 keys per node, 256 bytes per value.
pub const EXT_KEYS: usize = 16;
pub const EXT_KEY_MAX: usize = 64;
pub const EXT_VALUE_MAX: usize = 256;
pub const NAME_MAX: usize = 1_024;
pub const DESC_MAX: usize = 2_048;
pub const TEXT_MAX: usize = 3_072;
pub const URL_MAX: usize = 2_048;
pub const LABEL_MAX: usize = 128;
pub const MAX_ACTIONS: usize = 32;
/// Entries in the raise-only table before the surface is tainted.
pub const SEC_CAP: usize = 4_096;
/// Truncated subtree roots reported.
pub const TRUNCATED_MAX: usize = 256;
/// Secret rectangles reported; more than this is "unplaced".
pub const SECRET_RECTS_MAX: usize = 256;
/// What a surface of unknown size is clamped to.
pub const FALLBACK_SIZE: (i32, i32) = (16_384, 16_384);
/// S-05 §5 `downgrade_grace`, from the TCB constant.
pub const GRACE: Duration = crate::policy::classes::DOWNGRADE_GRACE;

/// P-01 §1.1 codes (the order in the protocol XML and in `ec-cataclysm-pub`).
pub const ROLE_PASSWORD: u32 = 30;
pub const ROLE_UNKNOWN: u32 = 57;
/// P-01 §1.2: 23 states; bit `n` is state code `n`.
pub const STATE_COUNT: u32 = 23;
pub const STATE_FOCUSED: u32 = 1;
/// Verb codes below this are P-01 §1.4's closed set; above are custom.
pub const VERB_FIXED: u32 = 12;
pub const VERB_CUSTOM_BASE: u32 = 256;

/// A surface-local rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// Clamp into `[0, size)`; the flag says whether anything changed.
    /// Arithmetic is 64-bit, so extreme or negative inputs cannot overflow.
    pub fn clamped(self, size: (i32, i32)) -> (Rect, bool) {
        let (bw, bh) = (i64::from(size.0.max(0)), i64::from(size.1.max(0)));
        let x0 = i64::from(self.x).clamp(0, bw);
        let y0 = i64::from(self.y).clamp(0, bh);
        let x1 = (i64::from(self.x) + i64::from(self.w.max(0))).clamp(x0, bw);
        let y1 = (i64::from(self.y) + i64::from(self.h.max(0))).clamp(y0, bh);
        let out = Rect {
            x: x0 as i32,
            y: y0 as i32,
            w: (x1 - x0) as i32,
            h: (y1 - y0) as i32,
        };
        (out, out != self)
    }
}

/// P-01 §1.3, minus `Selection` (no request sets it).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Empty,
    Text {
        text: String,
        cursor: i32,
        sel: (i32, i32),
    },
    Number {
        value: f64,
        min: f64,
        max: f64,
        step: f64,
    },
    Url(String),
}

/// A node, as the client described it.
#[derive(Clone, Debug)]
pub struct Node {
    /// 0 only for the root.
    pub parent: u32,
    pub children: Vec<u32>,
    pub role: u32,
    pub name: String,
    pub desc: String,
    pub rect: Option<Rect>,
    /// Bit `n` is state code `n`.
    pub states: u32,
    pub value: Value,
    pub actions: Vec<u32>,
    pub labels: Vec<(u32, String)>,
    pub ext: Vec<(String, String)>,
    /// Set on the published copy when the rectangle was clamped (`ext.clamped`).
    pub clamped: bool,
}

impl Node {
    fn new(parent: u32) -> Self {
        Self {
            parent,
            children: Vec::new(),
            role: ROLE_UNKNOWN,
            name: String::new(),
            desc: String::new(),
            rect: None,
            states: 0,
            value: Value::Empty,
            actions: Vec::new(),
            labels: Vec::new(),
            ext: Vec::new(),
            clamped: false,
        }
    }
}

/// A rejected request. Each is a protocol error; the model is left as it
/// was, and the client is about to be disconnected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Violation {
    InvalidNode,
    InvalidParent,
    Cycle,
    AlreadyExists,
    LowerClassification,
    ExtLimit,
    UncommittedDestroy,
}

impl Violation {
    /// The wire code (`eclipse_semantic_surface_v1.error`, COMP-09 §2 order).
    pub fn code(self) -> u32 {
        match self {
            Self::InvalidNode => 0,
            Self::InvalidParent => 1,
            Self::Cycle => 2,
            Self::AlreadyExists => 3,
            Self::LowerClassification => 5,
            Self::ExtLimit => 6,
            Self::UncommittedDestroy => 7,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::InvalidNode => "unknown node id, or no root at commit",
            Self::InvalidParent => "parent missing, root moved, or tree too deep",
            Self::Cycle => "move would make a node its own ancestor",
            Self::AlreadyExists => "node id in use, or root already set",
            Self::LowerClassification => "classification may only be raised",
            Self::ExtLimit => "ext or node limit exceeded",
            Self::UncommittedDestroy => "destroyed with uncommitted changes",
        }
    }
}

/// What a `set_ext` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtOutcome {
    Applied,
    /// An unknown `ext.irreversible` id: not recorded (COMP-09 §3, A-07).
    Dropped,
}

/// The raise-only record of one node id.
#[derive(Clone, Debug)]
struct Sec {
    class: Class,
    /// The node was ever a `password`: its text is discarded for good.
    password: bool,
    credential: bool,
    irreversible: Option<String>,
    echo_off: bool,
    hold_until: Option<Instant>,
}

impl Sec {
    fn new() -> Self {
        Self {
            class: Class::Private,
            password: false,
            credential: false,
            irreversible: None,
            echo_off: false,
            hold_until: None,
        }
    }

    /// Whether this node is secret by its own record at `now`.
    fn secret_at(&self, now: Instant) -> bool {
        self.class == Class::Secret
            || self.password
            || self.credential
            || self.echo_off
            || self.hold_until.is_some_and(|t| t > now)
    }

    fn local(&self, now: Instant) -> Class {
        if self.secret_at(now) {
            Class::Secret
        } else {
            self.class
        }
    }
}

/// Strip what a client could use to dress its text up as something else:
/// controls (newline and tab kept when `multiline`), bidi overrides and
/// zero-width marks; then cut to `max` bytes on a char boundary.
pub fn clean(s: &str, max: usize, multiline: bool) -> String {
    let mut out = String::with_capacity(s.len().min(max));
    for c in s.chars() {
        let keep = if multiline && (c == '\n' || c == '\t') {
            true
        } else {
            !c.is_control()
                && !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        };
        if !keep {
            continue;
        }
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// The `uint32` codes in a Wayland `array`, native-endian. A trailing
/// partial word is ignored.
pub fn words(bytes: &[u8]) -> impl Iterator<Item = u32> + '_ {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
}

/// Preorder `(id, effective class)` over `nodes` from `root`: the
/// maximum of a node's own class and its ancestors'. `tainted` makes
/// everything secret.
fn effective(
    nodes: &HashMap<u32, Node>,
    root: u32,
    sec: &HashMap<u32, Sec>,
    tainted: bool,
    now: Instant,
) -> Vec<(u32, Class)> {
    let mut out = Vec::with_capacity(nodes.len());
    let mut stack = vec![(root, Class::Private)];
    while let Some((id, inherited)) = stack.pop() {
        let Some(node) = nodes.get(&id) else { continue };
        let own = sec.get(&id).map_or(Class::Private, |s| s.local(now));
        let class = if tainted {
            Class::Secret
        } else {
            inherited.max(own)
        };
        out.push((id, class));
        for &c in node.children.iter().rev() {
            stack.push((c, class));
        }
    }
    out
}

/// What a client edits. One per `eclipse_semantic_surface_v1`.
#[derive(Debug, Default)]
pub struct Model {
    nodes: HashMap<u32, Node>,
    root: u32,
    sec: HashMap<u32, Sec>,
    /// The raise-only table overflowed: every node reads as secret.
    tainted: bool,
    /// A secret node was removed less than the grace ago.
    ghost_until: Option<Instant>,
    dirty_structural: bool,
    dirty_value: bool,
    warned: bool,
    generation: u64,
    value_rev: u64,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn tainted(&self) -> bool {
        self.tainted
    }

    pub fn root(&self) -> u32 {
        self.root
    }

    pub fn contains(&self, id: u32) -> bool {
        self.nodes.contains_key(&id)
    }

    /// `id`'s parent: `Some(0)` for the root, `None` for no such node.
    pub fn parent_of(&self, id: u32) -> Option<u32> {
        self.nodes.get(&id).map(|n| n.parent)
    }

    /// Whether requests have arrived since the last commit.
    pub fn dirty(&self) -> bool {
        self.dirty_structural || self.dirty_value
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether the model holds a secret the published view may not yet
    /// show: uncommitted changes while any node is secret, or a secret node
    /// removed within the grace. The capture path covers the whole surface
    /// for it rather than trust the older view's rectangles.
    pub fn pending_secret(&self, now: Instant) -> bool {
        self.dirty()
            && (self.tainted
                || self.ghost_until.is_some_and(|t| t > now)
                || self
                    .sec
                    .iter()
                    .any(|(id, s)| s.secret_at(now) && self.nodes.contains_key(id)))
    }

    fn node_mut(&mut self, id: u32) -> Result<&mut Node, Violation> {
        self.nodes.get_mut(&id).ok_or(Violation::InvalidNode)
    }

    /// The raise-only record of `id`, created on first raise. `None` when
    /// the table is full, which taints the surface instead.
    fn sec_raise(&mut self, id: u32) -> Option<&mut Sec> {
        if !self.sec.contains_key(&id) {
            if self.sec.len() >= SEC_CAP {
                self.tainted = true;
                return None;
            }
            self.sec.insert(id, Sec::new());
        }
        self.sec.get_mut(&id)
    }

    /// Ancestors of `id`, counting itself, capped at one past [`MAX_DEPTH`].
    fn depth_of(&self, mut id: u32) -> usize {
        let mut d = 0;
        while id != 0 && d <= MAX_DEPTH {
            d += 1;
            id = self.nodes.get(&id).map_or(0, |n| n.parent);
        }
        d
    }

    /// `set_root`. Idempotent for the root already set; any other id with a
    /// root present is `AlreadyExists`.
    pub fn set_root(&mut self, id: u32) -> Result<(), Violation> {
        if id == 0 {
            return Err(Violation::InvalidNode);
        }
        if self.root == id {
            return Ok(());
        }
        if self.root != 0 || self.nodes.contains_key(&id) {
            return Err(Violation::AlreadyExists);
        }
        self.nodes.insert(id, Node::new(0));
        self.root = id;
        self.dirty_structural = true;
        Ok(())
    }

    pub fn add_node(&mut self, id: u32, parent: u32, index: u32) -> Result<(), Violation> {
        if id == 0 {
            return Err(Violation::InvalidNode);
        }
        if self.nodes.contains_key(&id) {
            return Err(Violation::AlreadyExists);
        }
        if !self.nodes.contains_key(&parent) || self.depth_of(parent) >= MAX_DEPTH {
            return Err(Violation::InvalidParent);
        }
        if self.nodes.len() >= MODEL_CAP {
            return Err(Violation::ExtLimit);
        }
        self.nodes.insert(id, Node::new(parent));
        let siblings = &mut self.nodes.get_mut(&parent).expect("checked").children;
        let at = (index as usize).min(siblings.len());
        siblings.insert(at, id);
        self.dirty_structural = true;
        Ok(())
    }

    /// `remove_node`: the node and everything under it. The raise-only
    /// record stays, and a secret node among them holds the surface "known
    /// secret" for the grace.
    pub fn remove_node(&mut self, id: u32, now: Instant) -> Result<(), Violation> {
        let parent = self.nodes.get(&id).ok_or(Violation::InvalidNode)?.parent;
        let mut stack = vec![id];
        let mut had_secret = false;
        while let Some(x) = stack.pop() {
            if let Some(node) = self.nodes.remove(&x) {
                had_secret |= self.sec.get(&x).is_some_and(|s| s.secret_at(now));
                stack.extend(node.children);
            }
        }
        if parent == 0 {
            self.root = 0;
        } else if let Some(p) = self.nodes.get_mut(&parent) {
            p.children.retain(|&c| c != id);
        }
        if had_secret || self.tainted {
            self.ghost_until = Some(now + GRACE);
        }
        self.dirty_structural = true;
        Ok(())
    }

    pub fn move_node(&mut self, id: u32, new_parent: u32, index: u32) -> Result<(), Violation> {
        if !self.nodes.contains_key(&id) {
            return Err(Violation::InvalidNode);
        }
        if !self.nodes.contains_key(&new_parent) || id == self.root {
            return Err(Violation::InvalidParent);
        }
        // `id` must not be `new_parent` or one of its ancestors.
        let mut cur = new_parent;
        for _ in 0..=self.nodes.len() {
            if cur == id {
                return Err(Violation::Cycle);
            }
            cur = self.nodes.get(&cur).map_or(0, |n| n.parent);
            if cur == 0 {
                break;
            }
        }
        if cur != 0 {
            return Err(Violation::Cycle);
        }
        if self.depth_of(new_parent) >= MAX_DEPTH {
            return Err(Violation::InvalidParent);
        }
        let old = self.nodes[&id].parent;
        if let Some(p) = self.nodes.get_mut(&old) {
            p.children.retain(|&c| c != id);
        }
        self.nodes.get_mut(&id).expect("checked").parent = new_parent;
        let siblings = &mut self.nodes.get_mut(&new_parent).expect("checked").children;
        let at = (index as usize).min(siblings.len());
        siblings.insert(at, id);
        self.dirty_structural = true;
        Ok(())
    }

    /// `set_role`. A `password` role raises the node to `secret` for good
    /// and drops any text it holds.
    pub fn set_role(&mut self, id: u32, role: u32) -> Result<(), Violation> {
        let role = role.min(ROLE_UNKNOWN);
        self.node_mut(id)?.role = role;
        if role == ROLE_PASSWORD {
            if let Some(s) = self.sec_raise(id) {
                s.password = true;
                s.class = Class::Secret;
            }
            self.node_mut(id)?.value = Value::Empty;
        }
        self.dirty_structural = true;
        Ok(())
    }

    pub fn set_name(&mut self, id: u32, name: &str) -> Result<(), Violation> {
        self.node_mut(id)?.name = clean(name, NAME_MAX, false);
        self.dirty_value = true;
        Ok(())
    }

    pub fn set_description(&mut self, id: u32, desc: &str) -> Result<(), Violation> {
        self.node_mut(id)?.desc = clean(desc, DESC_MAX, true);
        self.dirty_value = true;
        Ok(())
    }

    pub fn set_rect(&mut self, id: u32, x: i32, y: i32, w: i32, h: i32) -> Result<(), Violation> {
        self.node_mut(id)?.rect = Some(Rect { x, y, w, h });
        self.dirty_structural = true;
        Ok(())
    }

    pub fn set_states(&mut self, id: u32, codes: impl Iterator<Item = u32>) -> Result<(), Violation> {
        let mut mask = 0u32;
        for c in codes.take(256) {
            if c < STATE_COUNT {
                mask |= 1 << c;
            }
        }
        self.node_mut(id)?.states = mask;
        self.dirty_structural = true;
        Ok(())
    }

    /// Whether this node's text must never be kept.
    fn password(&self, id: u32) -> bool {
        self.sec.get(&id).is_some_and(|s| s.password)
            || self.nodes.get(&id).is_some_and(|n| n.role == ROLE_PASSWORD)
    }

    pub fn set_value_text(
        &mut self,
        id: u32,
        text: &str,
        cursor: i32,
        sel_start: i32,
        sel_end: i32,
    ) -> Result<(), Violation> {
        let discard = self.password(id);
        let node = self.node_mut(id)?;
        node.value = if discard {
            Value::Empty
        } else {
            let text = clean(text, TEXT_MAX, true);
            let n = i32::try_from(text.len()).unwrap_or(i32::MAX);
            Value::Text {
                cursor: cursor.clamp(-1, n),
                sel: (sel_start.clamp(-1, n), sel_end.clamp(-1, n)),
                text,
            }
        };
        self.dirty_value = true;
        Ok(())
    }

    pub fn set_value_number(
        &mut self,
        id: u32,
        value: f64,
        min: f64,
        max: f64,
        step: f64,
    ) -> Result<(), Violation> {
        let discard = self.password(id);
        self.node_mut(id)?.value = if discard {
            Value::Empty
        } else {
            Value::Number {
                value,
                min,
                max,
                step,
            }
        };
        self.dirty_value = true;
        Ok(())
    }

    pub fn set_value_url(&mut self, id: u32, href: &str) -> Result<(), Violation> {
        let discard = self.password(id);
        self.node_mut(id)?.value = if discard {
            Value::Empty
        } else {
            Value::Url(clean(href, URL_MAX, false))
        };
        self.dirty_value = true;
        Ok(())
    }

    pub fn clear_value(&mut self, id: u32) -> Result<(), Violation> {
        self.node_mut(id)?.value = Value::Empty;
        self.dirty_value = true;
        Ok(())
    }

    /// `set_actions`: the full set. Codes between the fixed verbs and the
    /// custom base are not verbs and are ignored.
    pub fn set_actions(&mut self, id: u32, verbs: impl Iterator<Item = u32>) -> Result<(), Violation> {
        let node = self.node_mut(id)?;
        let mut set: Vec<u32> = Vec::new();
        for v in verbs.take(4 * MAX_ACTIONS) {
            if !(VERB_FIXED..VERB_CUSTOM_BASE).contains(&v) && !set.contains(&v) && set.len() < MAX_ACTIONS {
                set.push(v);
            }
        }
        node.labels.retain(|(v, _)| set.contains(v));
        node.actions = set;
        self.dirty_structural = true;
        Ok(())
    }

    /// A label for a verb the node offers; for any other verb, ignored.
    pub fn set_action_label(&mut self, id: u32, verb: u32, label: &str) -> Result<(), Violation> {
        let node = self.node_mut(id)?;
        if node.actions.contains(&verb) {
            let label = clean(label, LABEL_MAX, false);
            match node.labels.iter_mut().find(|(v, _)| *v == verb) {
                Some(slot) => slot.1 = label,
                None => node.labels.push((verb, label)),
            }
            self.dirty_value = true;
        }
        Ok(())
    }

    /// `set_sensitivity`: raise only. An unknown class counts as `secret`.
    /// Anything below the node's current class (default `private`) is
    /// `LowerClassification`.
    pub fn set_sensitivity(&mut self, id: u32, class: u32) -> Result<(), Violation> {
        self.node_mut(id)?;
        let want = match class {
            0 => Class::Public,
            1 => Class::Private,
            _ => Class::Secret,
        };
        let have = self.sec.get(&id).map_or(Class::Private, |s| s.class);
        if want < have {
            return Err(Violation::LowerClassification);
        }
        if want > have {
            if let Some(s) = self.sec_raise(id) {
                s.class = want;
            }
            self.dirty_structural = true;
        }
        Ok(())
    }

    /// `set_ext` (COMP-09 §2, A-07). `taxonomy_known` is whether the
    /// compiled taxonomy has `value` as an id; it is read only for
    /// `irreversible`.
    pub fn set_ext(
        &mut self,
        id: u32,
        key: &str,
        value: &str,
        now: Instant,
        taxonomy_known: bool,
    ) -> Result<ExtOutcome, Violation> {
        self.node_mut(id)?;
        let key = key.strip_prefix("ext.").unwrap_or(key);
        if key.is_empty()
            || key.len() > EXT_KEY_MAX
            || key.chars().any(char::is_control)
            || value.len() > EXT_VALUE_MAX
        {
            return Err(Violation::ExtLimit);
        }
        let value = clean(value, EXT_VALUE_MAX, false);
        match key {
            // Compositor-owned.
            "clamped" => return Ok(ExtOutcome::Applied),
            "credential" => {
                let on = !matches!(value.as_str(), "false" | "0");
                if !on {
                    if self.sec.get(&id).is_some_and(|s| s.credential) {
                        return Err(Violation::LowerClassification);
                    }
                } else if let Some(s) = self.sec_raise(id) {
                    s.credential = true;
                    self.dirty_structural = true;
                }
                return Ok(ExtOutcome::Applied);
            }
            "irreversible" => {
                let have = self.sec.get(&id).and_then(|s| s.irreversible.clone());
                return match have {
                    Some(h) if h != value => Err(Violation::LowerClassification),
                    Some(_) => Ok(ExtOutcome::Applied),
                    None if value.is_empty() => Ok(ExtOutcome::Applied),
                    None if !taxonomy_known => {
                        self.warned = true;
                        Ok(ExtOutcome::Dropped)
                    }
                    None => {
                        if let Some(s) = self.sec_raise(id) {
                            s.irreversible = Some(value);
                        }
                        self.dirty_structural = true;
                        Ok(ExtOutcome::Applied)
                    }
                };
            }
            // P-04 §6.1: echo off is a raise to `secret`; echo back on is a
            // downgrade, and waits the grace.
            "echo" => {
                if matches!(value.as_str(), "true" | "1") {
                    if let Some(s) = self.sec.get_mut(&id).filter(|s| s.echo_off) {
                        s.echo_off = false;
                        s.hold_until = Some(now + GRACE);
                        self.dirty_structural = true;
                    }
                } else if let Some(s) = self.sec_raise(id) {
                    s.echo_off = true;
                    s.hold_until = None;
                    self.dirty_structural = true;
                }
            }
            _ => {}
        }
        let node = self.node_mut(id)?;
        if let Some(slot) = node.ext.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else if node.ext.len() >= EXT_KEYS {
            return Err(Violation::ExtLimit);
        } else {
            node.ext.push((key.to_owned(), value));
        }
        self.dirty_value = true;
        Ok(ExtOutcome::Applied)
    }

    /// `commit`: validate and publish. `Ok(None)` when nothing changed
    /// since the last commit; the previous [`View`] stands untouched, so
    /// its recorded size still tells a resize from a no-op.
    pub fn commit(&mut self, size: (i32, i32), now: Instant) -> Result<Option<(Commit, View)>, Violation> {
        if !self.dirty() {
            return Ok(None);
        }
        if self.root == 0 && !self.nodes.is_empty() {
            return Err(Violation::InvalidNode);
        }
        let order = effective(&self.nodes, self.root, &self.sec, self.tainted, now);
        // Every node must be reachable from the root exactly once.
        if order.len() != self.nodes.len() {
            return Err(Violation::Cycle);
        }
        let kept = self.choose(&order);
        let mut nodes = HashMap::with_capacity(kept.len());
        let mut ids = Vec::with_capacity(kept.len());
        let mut truncated = Vec::new();
        let mut secret_dropped = false;
        for &(id, class) in &order {
            if !kept.contains(&id) {
                secret_dropped |= class == Class::Secret;
                continue;
            }
            let mut n = self.nodes[&id].clone();
            n.children.retain(|c| {
                let keep = kept.contains(c);
                if !keep && truncated.len() < TRUNCATED_MAX {
                    truncated.push(*c);
                }
                keep
            });
            if let Some(r) = n.rect {
                let (r, was) = r.clamped(size);
                n.rect = Some(r);
                n.clamped = was;
            }
            ids.push(id);
            nodes.insert(id, n);
        }
        let complete = kept.len() == order.len();
        if self.dirty_structural {
            self.generation += 1;
        } else {
            self.value_rev += 1;
        }
        let commit = Commit {
            generation: self.generation,
            value_rev: self.value_rev,
            structural: self.dirty_structural,
            budget_exceeded: !complete || self.warned,
        };
        let view = View {
            generation: self.generation,
            value_rev: self.value_rev,
            root: self.root,
            nodes,
            order: ids,
            sec: self.sec.clone(),
            tainted: self.tainted,
            ghost_until: self.ghost_until,
            secret_dropped,
            complete,
            truncated,
            size,
        };
        self.dirty_structural = false;
        self.dirty_value = false;
        self.warned = false;
        Ok(Some((commit, view)))
    }

    /// Which nodes survive the budget: everything if it fits, else the path
    /// to the focused node and its subtree (up to half the budget), then
    /// document order until the budget is spent. Deterministic: depends only
    /// on the tree.
    fn choose(&self, order: &[(u32, Class)]) -> HashSet<u32> {
        if order.len() <= NODE_BUDGET {
            return order.iter().map(|&(id, _)| id).collect();
        }
        let mut kept = HashSet::with_capacity(NODE_BUDGET);
        let focus = order
            .iter()
            .map(|&(id, _)| id)
            .find(|id| self.nodes[id].states & STATE_FOCUSED != 0);
        if let Some(f) = focus {
            let mut up = f;
            while up != 0 {
                kept.insert(up);
                up = self.nodes[&up].parent;
            }
            let mut stack = vec![f];
            let mut under = 0;
            while let Some(id) = stack.pop() {
                if under >= NODE_BUDGET / 2 {
                    break;
                }
                if id != f {
                    kept.insert(id);
                }
                under += 1;
                stack.extend(self.nodes[&id].children.iter().rev());
            }
        }
        for &(id, _) in order {
            if kept.len() >= NODE_BUDGET {
                break;
            }
            let parent = self.nodes[&id].parent;
            if !kept.contains(&id) && (parent == 0 || kept.contains(&parent)) {
                kept.insert(id);
            }
        }
        kept
    }

    /// Structure check for tests and the fuzz loops: parent and child links
    /// agree, there is one root, nothing is orphaned or listed twice.
    pub fn check(&self) -> Result<(), String> {
        if self.root == 0 {
            return if self.nodes.is_empty() {
                Ok(())
            } else {
                Err("nodes without a root".into())
            };
        }
        let root = self.nodes.get(&self.root).ok_or("root missing")?;
        if root.parent != 0 {
            return Err("root has a parent".into());
        }
        let mut seen = HashSet::new();
        let mut stack = vec![self.root];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                return Err(format!("node {id} reached twice"));
            }
            let n = self.nodes.get(&id).ok_or(format!("child {id} missing"))?;
            for &c in &n.children {
                let child = self.nodes.get(&c).ok_or(format!("child {c} missing"))?;
                if child.parent != id {
                    return Err(format!("child {c} disagrees about its parent"));
                }
                stack.push(c);
            }
        }
        if seen.len() != self.nodes.len() {
            return Err("orphaned nodes".into());
        }
        Ok(())
    }
}

/// What a commit did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Commit {
    pub generation: u64,
    pub value_rev: u64,
    pub structural: bool,
    /// Send `budget_exceeded`: truncated, or an extension was dropped.
    pub budget_exceeded: bool,
}

/// The committed tree: what agents and the capture path read (COMP-09 §3).
#[derive(Debug, Clone)]
pub struct View {
    pub generation: u64,
    pub value_rev: u64,
    pub root: u32,
    nodes: HashMap<u32, Node>,
    /// Document order.
    order: Vec<u32>,
    sec: HashMap<u32, Sec>,
    tainted: bool,
    ghost_until: Option<Instant>,
    /// The budget dropped a secret node.
    secret_dropped: bool,
    pub complete: bool,
    /// Roots of the subtrees the budget dropped, in document order.
    pub truncated: Vec<u32>,
    /// The surface size the rectangles were clamped to.
    pub size: (i32, i32),
}

/// What the capture path needs to know (COMP-02 §7, A-10).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecretFacts {
    /// A secret node is, or very recently was, in the tree.
    pub known: bool,
    /// Some secret cannot be placed: no rectangle, none left after
    /// clamping, too many, dropped to the budget, or the table overflowed.
    /// The surface is covered whole.
    pub unplaced: bool,
    /// Clamped, surface-local rectangles of the secret nodes.
    pub rects: Vec<Rect>,
}

/// One node as an agent may see it.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapNode {
    pub id: u32,
    pub parent: u32,
    pub role: u32,
    pub name: String,
    pub description: String,
    pub value: Value,
    pub states: u32,
    pub rect: Option<Rect>,
    pub actions: Vec<(u32, String)>,
    /// At least the window's class; never `secret` (those are omitted).
    pub sensitivity: Class,
    pub ext: Vec<(String, String)>,
    pub children: Vec<u32>,
}

/// What an agent may be given of a tree (the read half of `get_tree`).
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub generation: u64,
    pub value_rev: u64,
    pub complete: bool,
    pub truncated: Vec<u32>,
    pub root: u32,
    /// Document order.
    pub nodes: Vec<SnapNode>,
}

impl View {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.get(&id)
    }

    /// Node ids in document order.
    pub fn ids(&self) -> &[u32] {
        &self.order
    }

    /// The effective class of `id` (own, ancestors', and the table's), at
    /// least `private`.
    pub fn class_of(&self, id: u32, now: Instant) -> Option<Class> {
        effective(&self.nodes, self.root, &self.sec, self.tainted, now)
            .into_iter()
            .find(|&(n, _)| n == id)
            .map(|(_, c)| c)
    }

    /// The secret nodes' placement, for the capture path.
    pub fn secret_facts(&self, now: Instant) -> SecretFacts {
        let mut f = SecretFacts::default();
        if self.tainted || self.secret_dropped || self.ghost_until.is_some_and(|t| t > now) {
            f.known = true;
            f.unplaced = true;
        }
        for (id, class) in effective(&self.nodes, self.root, &self.sec, self.tainted, now) {
            if class != Class::Secret {
                continue;
            }
            f.known = true;
            match self.nodes[&id].rect {
                Some(r) if !r.is_empty() => f.rects.push(r),
                _ => f.unplaced = true,
            }
        }
        if f.rects.len() > SECRET_RECTS_MAX {
            f.unplaced = true;
        }
        if f.unplaced {
            f.rects.clear();
        }
        f
    }

    /// The tree an agent may read: no secret node, and no descendant of
    /// one, appears at all (F-07: absent, not redacted), and every node is
    /// at least `floor`, the window's class as the scene filter decided it.
    pub fn snapshot(&self, floor: Class, now: Instant) -> Snapshot {
        let classes: HashMap<u32, Class> = effective(&self.nodes, self.root, &self.sec, self.tainted, now)
            .into_iter()
            .collect();
        let shown = |id: &u32| classes.get(id).is_some_and(|c| *c != Class::Secret);
        let mut nodes = Vec::new();
        for &id in self.order.iter().filter(|id| shown(id)) {
            let n = &self.nodes[&id];
            let mut ext = n.ext.clone();
            if n.clamped {
                ext.push(("clamped".into(), "true".into()));
            }
            if let Some(s) = self.sec.get(&id) {
                if let Some(i) = &s.irreversible {
                    ext.push(("irreversible".into(), i.clone()));
                }
            }
            nodes.push(SnapNode {
                id,
                parent: n.parent,
                role: n.role,
                name: n.name.clone(),
                description: n.desc.clone(),
                value: n.value.clone(),
                states: n.states,
                rect: n.rect,
                actions: n
                    .actions
                    .iter()
                    .map(|v| {
                        let label = n.labels.iter().find(|(l, _)| l == v).map(|(_, s)| s.clone());
                        (*v, label.unwrap_or_default())
                    })
                    .collect(),
                sensitivity: classes[&id].max(floor),
                ext,
                children: n.children.iter().copied().filter(|c| shown(c)).collect(),
            });
        }
        Snapshot {
            generation: self.generation,
            value_rev: self.value_rev,
            complete: self.complete,
            truncated: self.truncated.clone(),
            root: if nodes.is_empty() { 0 } else { self.root },
            nodes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn small() -> Model {
        let mut m = Model::new();
        m.set_root(1).unwrap();
        m.add_node(2, 1, 0).unwrap();
        m.add_node(3, 1, 1).unwrap();
        m
    }

    #[test]
    fn rect_clamp_is_total() {
        let (r, was) = Rect {
            x: -5,
            y: 3,
            w: 10,
            h: 4,
        }
        .clamped((100, 50));
        assert_eq!(
            r,
            Rect {
                x: 0,
                y: 3,
                w: 5,
                h: 4
            }
        );
        assert!(was);
        let (r, was) = Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        }
        .clamped((100, 50));
        assert_eq!(
            (r, was),
            (
                Rect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4
                },
                false
            )
        );
        let (r, _) = Rect {
            x: i32::MAX,
            y: i32::MIN,
            w: i32::MAX,
            h: i32::MIN,
        }
        .clamped((100, 50));
        assert!(r.is_empty());
        assert!(r.x <= 100 && r.y >= 0);
    }

    #[test]
    fn set_root_twice_is_idempotent_but_a_second_root_is_not() {
        let mut m = Model::new();
        m.set_root(7).unwrap();
        m.set_root(7).unwrap();
        assert_eq!(m.set_root(8), Err(Violation::AlreadyExists));
        assert_eq!(m.set_root(0), Err(Violation::InvalidNode));
    }

    #[test]
    fn violations_carry_the_spec_wire_codes() {
        assert_eq!(Violation::InvalidNode.code(), 0);
        assert_eq!(Violation::Cycle.code(), 2);
        // The v0.1 LOWER_SENSITIVITY code, kept by the v0.2 rename.
        assert_eq!(Violation::LowerClassification.code(), 5);
        assert_eq!(Violation::UncommittedDestroy.code(), 7);
    }

    #[test]
    fn a_cycle_is_refused_and_leaves_the_model_sound() {
        let mut m = small();
        m.add_node(4, 2, 0).unwrap();
        assert_eq!(m.move_node(2, 4, 0), Err(Violation::Cycle));
        assert_eq!(m.move_node(2, 2, 0), Err(Violation::Cycle));
        assert_eq!(m.move_node(1, 2, 0), Err(Violation::InvalidParent));
        m.check().unwrap();
    }

    #[test]
    fn password_text_is_discarded_and_stays_discarded() {
        let mut m = small();
        m.set_role(2, ROLE_PASSWORD).unwrap();
        m.set_value_text(2, "hunter2", 0, 0, 0).unwrap();
        assert_eq!(m.nodes[&2].value, Value::Empty);
        // Flipping the role away does not bring the text back.
        m.set_role(2, 28).unwrap();
        m.set_value_text(2, "hunter2", 0, 0, 0).unwrap();
        assert_eq!(m.nodes[&2].value, Value::Empty);
        let (_, v) = m.commit((100, 100), t0()).unwrap().unwrap();
        assert!(v.secret_facts(t0()).known, "a role flip does not lower the class");
    }

    #[test]
    fn sensitivity_is_raise_only_even_across_id_reuse() {
        let mut m = small();
        assert_eq!(m.set_sensitivity(2, 0), Err(Violation::LowerClassification));
        m.set_sensitivity(2, 2).unwrap();
        assert_eq!(m.set_sensitivity(2, 1), Err(Violation::LowerClassification));
        m.set_sensitivity(2, 2).unwrap();
        m.remove_node(2, t0()).unwrap();
        m.add_node(2, 1, 0).unwrap();
        let (_, v) = m.commit((100, 100), t0()).unwrap().unwrap();
        assert_eq!(v.class_of(2, t0()), Some(Class::Secret));
        assert_eq!(v.class_of(3, t0()), Some(Class::Private));
    }

    #[test]
    fn a_secret_parent_raises_its_children() {
        let mut m = small();
        m.set_sensitivity(1, 2).unwrap();
        let (_, v) = m.commit((10, 10), t0()).unwrap().unwrap();
        assert_eq!(v.class_of(3, t0()), Some(Class::Secret));
        assert!(v.snapshot(Class::Private, t0()).nodes.is_empty());
    }

    #[test]
    fn credential_and_irreversible_are_raise_only() {
        let mut m = small();
        let now = t0();
        m.set_ext(2, "ext.credential", "true", now, false).unwrap();
        assert_eq!(
            m.set_ext(2, "credential", "false", now, false),
            Err(Violation::LowerClassification)
        );
        assert_eq!(
            m.set_ext(3, "irreversible", "communication.send", now, false),
            Ok(ExtOutcome::Dropped)
        );
        assert_eq!(
            m.set_ext(3, "irreversible", "communication.send", now, true),
            Ok(ExtOutcome::Applied)
        );
        assert_eq!(
            m.set_ext(3, "irreversible", "", now, true),
            Err(Violation::LowerClassification)
        );
        assert_eq!(
            m.set_ext(3, "irreversible", "other.thing", now, true),
            Err(Violation::LowerClassification)
        );
    }

    #[test]
    fn ext_is_bounded() {
        let mut m = small();
        let now = t0();
        for i in 0..EXT_KEYS {
            m.set_ext(2, &format!("k{i}"), "v", now, false).unwrap();
        }
        assert_eq!(
            m.set_ext(2, "one-more", "v", now, false),
            Err(Violation::ExtLimit)
        );
        assert_eq!(
            m.set_ext(3, "big", &"x".repeat(EXT_VALUE_MAX + 1), now, false),
            Err(Violation::ExtLimit)
        );
        // Rewriting an existing key is not a new key.
        m.set_ext(2, "k0", "w", now, false).unwrap();
    }

    #[test]
    fn echo_off_raises_and_echo_on_waits_the_grace() {
        let mut m = small();
        let now = t0();
        m.set_ext(1, "echo", "false", now, false).unwrap();
        let (_, v) = m.commit((10, 10), now).unwrap().unwrap();
        assert!(v.secret_facts(now).known);
        m.set_ext(1, "echo", "true", now, false).unwrap();
        let (_, v) = m.commit((10, 10), now).unwrap().unwrap();
        assert!(v.secret_facts(now).known, "held through the grace");
        assert!(!v.secret_facts(now + GRACE + Duration::from_millis(1)).known);
    }

    #[test]
    fn removing_a_secret_node_keeps_the_surface_secret_for_the_grace() {
        let mut m = small();
        let now = t0();
        m.set_sensitivity(2, 2).unwrap();
        m.set_rect(2, 0, 0, 5, 5).unwrap();
        m.remove_node(2, now).unwrap();
        let (_, v) = m.commit((10, 10), now).unwrap().unwrap();
        let f = v.secret_facts(now);
        assert!(f.known && f.unplaced);
        assert!(!v.secret_facts(now + GRACE + Duration::from_millis(1)).known);
    }

    #[test]
    fn value_only_commits_bump_value_rev_not_generation() {
        let mut m = small();
        let (c, _) = m.commit((10, 10), t0()).unwrap().unwrap();
        assert_eq!((c.generation, c.value_rev), (1, 0));
        m.set_name(2, "ok").unwrap();
        let (c, _) = m.commit((10, 10), t0()).unwrap().unwrap();
        assert_eq!((c.generation, c.value_rev), (1, 1));
        m.set_rect(2, 0, 0, 1, 1).unwrap();
        let (c, _) = m.commit((10, 10), t0()).unwrap().unwrap();
        assert_eq!((c.generation, c.value_rev), (2, 1));
        assert!(m.commit((10, 10), t0()).unwrap().is_none(), "nothing changed");
    }

    #[test]
    fn secret_facts_place_or_fail_closed() {
        let mut m = small();
        m.set_sensitivity(2, 2).unwrap();
        let (_, v) = m.commit((100, 100), t0()).unwrap().unwrap();
        let f = v.secret_facts(t0());
        assert!(f.known && f.unplaced, "no rectangle: unplaced");
        m.set_rect(2, 10, 10, 20, 5).unwrap();
        let (_, v) = m.commit((100, 100), t0()).unwrap().unwrap();
        let f = v.secret_facts(t0());
        assert_eq!(
            f,
            SecretFacts {
                known: true,
                unplaced: false,
                rects: vec![Rect {
                    x: 10,
                    y: 10,
                    w: 20,
                    h: 5
                }],
            }
        );
        m.set_rect(2, 500, 500, 20, 5).unwrap();
        let (_, v) = m.commit((100, 100), t0()).unwrap().unwrap();
        assert!(v.secret_facts(t0()).unplaced, "off the surface: unplaced");
    }

    #[test]
    fn the_budget_keeps_the_focus_path_and_flags_dropped_secrets() {
        let mut m = Model::new();
        m.set_root(1).unwrap();
        for i in 2..=(NODE_BUDGET as u32 + 500) {
            m.add_node(i, 1, u32::MAX).unwrap();
        }
        // A focused node late in document order survives; a secret one
        // beyond the budget is dropped and says so.
        let late = NODE_BUDGET as u32 + 400;
        m.set_states(late, [0u32].into_iter()).unwrap();
        m.set_sensitivity(NODE_BUDGET as u32 + 500, 2).unwrap();
        let (c, v) = m.commit((10, 10), t0()).unwrap().unwrap();
        assert!(c.budget_exceeded && !v.complete);
        assert_eq!(v.len(), NODE_BUDGET);
        assert!(v.node(late).is_some(), "focus kept");
        assert!(v.node(NODE_BUDGET as u32 + 500).is_none());
        let f = v.secret_facts(t0());
        assert!(f.known && f.unplaced, "a dropped secret covers the surface");
        assert!(!v.truncated.is_empty());
    }

    #[test]
    fn text_is_sanitised() {
        assert_eq!(clean("a\u{0}b\u{202E}c\u{7}d", 100, false), "abcd");
        assert_eq!(clean("a\nb", 100, false), "ab");
        assert_eq!(clean("a\nb", 100, true), "a\nb");
        assert_eq!(clean("ééé", 3, false), "é");
    }

    #[test]
    fn arrays_decode_as_native_words() {
        let bytes: Vec<u8> = [3u32, 9]
            .iter()
            .flat_map(|w| w.to_ne_bytes())
            .chain([1, 2])
            .collect();
        assert_eq!(words(&bytes).collect::<Vec<_>>(), vec![3, 9]);
    }
}
