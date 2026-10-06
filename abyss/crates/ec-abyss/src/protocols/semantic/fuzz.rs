// SPDX-License-Identifier: AGPL-3.0-only
//! COMP-15 §3's `eclipse_semantic_v1` fuzz targets as deterministic
//! property loops: cycles, id reuse, huge trees, negative and out-of-bounds
//! rects, sensitivity lowering (COMP-16 M22).
//!
//! There is no `cargo-fuzz` harness in this tree yet (`abyss/fuzz` does not
//! exist; STATUS lists it), so these run in `cargo test` on every push with
//! fixed seeds, which is the COMP-15 §1 "fuzz smoke" tier's blocking bar. A
//! `cargo-fuzz` target can drive [`Model`] through the same `op` function:
//! it takes no I/O and no clock.
//!
//! Two layers: the pure [`Model`] under thousands of random request
//! sequences with the invariants asserted after every step, and random wire
//! traffic from a real client at the compositor.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use ec_policy_eval::Class;

use super::refclient::{array, Ref};
use super::tree::{
    clean, Model, Rect, Value, View, Violation, MAX_DEPTH, MODEL_CAP, NODE_BUDGET, ROLE_PASSWORD,
    ROLE_UNKNOWN,
};
use crate::shell::focus::state_tests::harness;

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Mostly small, sometimes extreme.
    fn int(&mut self) -> i32 {
        match self.below(8) {
            0 => i32::MIN,
            1 => i32::MAX,
            2 => -1,
            3 => 0,
            4 => self.below(1_000) as i32 - 500,
            _ => self.below(300) as i32,
        }
    }

    fn string(&mut self) -> String {
        match self.below(6) {
            0 => String::new(),
            1 => "x".repeat(self.below(300) as usize),
            2 => "a\u{0}b\u{202E}\n\u{7}c".into(),
            3 => "é€😀".repeat(self.below(60) as usize),
            4 => format!("k{}", self.below(40)),
            _ => "true".into(),
        }
    }
}

/// What a client has legitimately done, to check the model against.
#[derive(Default)]
struct Ctx {
    /// The class a client declared, per id (default private).
    decl: HashMap<u32, Class>,
    /// Ids that were ever raised to secret: password role, secret class,
    /// credential.
    raised: HashSet<u32>,
    /// Ids that were ever a password.
    password: HashSet<u32>,
}

impl Ctx {
    fn declared(&self, id: u32) -> Class {
        self.decl.get(&id).copied().unwrap_or(Class::Private)
    }

    /// The node's declared class is now secret.
    fn raise(&mut self, id: u32) {
        self.decl.insert(id, Class::Secret);
        self.raised.insert(id);
    }

    /// The node is secret by another route (credential): its declared class
    /// is untouched, so `set_sensitivity` is judged against that alone.
    fn flag(&mut self, id: u32) {
        self.raised.insert(id);
    }
}

const IDS: u64 = 20;

/// One random request against `m`, asserting the per-request properties.
fn op(m: &mut Model, g: &mut Rng, c: &mut Ctx, now: Instant) -> Result<(), Violation> {
    let id = g.below(IDS) as u32;
    let other = g.below(IDS) as u32;
    let index = g.below(6) as u32;
    match g.below(24) {
        0 => m.set_root(id),
        1..=3 => m.add_node(id, other, index),
        4 => m.remove_node(id, now),
        5 | 6 => {
            // Property: a move into one's own subtree is a cycle.
            let mut into_own = false;
            let mut cur = other;
            while cur != 0 {
                if cur == id {
                    into_own = true;
                    break;
                }
                cur = m.parent_of(cur).unwrap_or(0);
            }
            let (exists, parent_exists) = (m.contains(id), m.contains(other));
            let r = m.move_node(id, other, index);
            if exists && parent_exists && id != m.root() && into_own {
                assert_eq!(r, Err(Violation::Cycle), "move {id} under {other}");
            }
            r
        }
        7 => {
            let role = g.below(70) as u32;
            let r = m.set_role(id, role);
            if r.is_ok() && role.min(ROLE_UNKNOWN) == ROLE_PASSWORD {
                c.raise(id);
                c.password.insert(id);
            }
            r
        }
        8 => m.set_name(id, &g.string()),
        9 => m.set_description(id, &g.string()),
        10 => m.set_rect(id, g.int(), g.int(), g.int(), g.int()),
        11 => m.set_states(
            id,
            (0..g.below(8))
                .map(|_| g.below(40) as u32)
                .collect::<Vec<_>>()
                .into_iter(),
        ),
        12 => m.set_value_text(id, &g.string(), g.int(), g.int(), g.int()),
        13 => m.set_value_number(id, g.int() as f64 / 256.0, 0.0, 1.0, 0.1),
        14 => m.set_value_url(id, &g.string()),
        15 => m.clear_value(id),
        16 => m.set_actions(
            id,
            (0..g.below(6))
                .map(|_| g.below(300) as u32)
                .collect::<Vec<_>>()
                .into_iter(),
        ),
        17 => m.set_action_label(id, g.below(300) as u32, &g.string()),
        18 | 19 => {
            // Property: sensitivity is raise-only, and says so exactly.
            let want = g.below(4) as u32;
            let class = match want {
                0 => Class::Public,
                1 => Class::Private,
                _ => Class::Secret,
            };
            let exists = m.contains(id);
            let have = c.declared(id);
            let r = m.set_sensitivity(id, want);
            if !exists {
                assert_eq!(r, Err(Violation::InvalidNode));
            } else if class < have {
                assert_eq!(r, Err(Violation::LowerClassification), "{class:?} below {have:?}");
            } else {
                assert_eq!(r, Ok(()));
                if class == Class::Secret {
                    c.raise(id);
                } else {
                    c.decl.insert(id, class.max(have));
                }
            }
            r
        }
        20 => {
            let on = g.below(2) == 0;
            let exists = m.contains(id);
            let r = m.set_ext(id, "credential", if on { "true" } else { "false" }, now, false);
            if exists && on && r.is_ok() {
                c.flag(id);
            }
            r.map(|_| ())
        }
        21 => m
            .set_ext(id, "irreversible", &g.string(), now, g.below(2) == 0)
            .map(|_| ()),
        22 => {
            let key = if g.below(3) == 0 {
                "echo".to_owned()
            } else {
                g.string()
            };
            m.set_ext(id, &key, &g.string(), now, false).map(|_| ())
        }
        _ => m
            .set_ext(id, &format!("ext.k{}", g.below(40)), &g.string(), now, false)
            .map(|_| ()),
    }
}

/// The properties of a published view, whatever the client did.
fn check_view(v: &View, c: &Ctx, size: (i32, i32), now: Instant) {
    assert!(v.len() <= NODE_BUDGET);
    let (bw, bh) = (size.0.max(0), size.1.max(0));
    let mut shown: HashSet<u32> = HashSet::new();
    for &id in v.ids() {
        shown.insert(id);
        let n = v.node(id).expect("ordered id has a node");
        if let Some(r) = n.rect {
            // Out-of-bounds and negative rectangles are clamped, never kept.
            assert!(r.x >= 0 && r.y >= 0 && r.w >= 0 && r.h >= 0, "{r:?}");
            assert!(
                i64::from(r.x) + i64::from(r.w) <= i64::from(bw),
                "{r:?} in {size:?}"
            );
            assert!(
                i64::from(r.y) + i64::from(r.h) <= i64::from(bh),
                "{r:?} in {size:?}"
            );
        }
        for &ch in &n.children {
            assert_eq!(v.node(ch).map(|c| c.parent), Some(id), "child links agree");
        }
    }
    assert_eq!(shown.len(), v.ids().len(), "no node twice");
    for &id in &c.raised {
        if v.node(id).is_some() {
            assert_eq!(v.class_of(id, now), Some(Class::Secret), "node {id} was lowered");
            assert!(v.secret_facts(now).known);
        }
    }
    for &id in &c.password {
        if let Some(n) = v.node(id) {
            assert_eq!(n.value, Value::Empty, "password {id} holds a value");
        }
    }
    let snap = v.snapshot(Class::Private, now);
    let seen: HashSet<u32> = snap.nodes.iter().map(|n| n.id).collect();
    for n in &snap.nodes {
        assert_ne!(n.sensitivity, Class::Secret);
        assert!(
            n.parent == 0 || seen.contains(&n.parent),
            "an agent sees a node without its parent"
        );
        assert!(n.children.iter().all(|ch| seen.contains(ch)));
        assert!(!c.raised.contains(&n.id), "a raised node reached an agent");
        assert_ne!(v.class_of(n.id, now), Some(Class::Secret));
    }
    // Secret rectangles are placed or the surface is covered whole.
    let f = v.secret_facts(now);
    if f.unplaced {
        assert!(f.rects.is_empty() && f.known);
    }
    for r in &f.rects {
        assert!(!r.is_empty());
    }
}

#[test]
fn random_requests_never_break_the_model_or_lower_a_class() {
    for seed in 1..=2_000u64 {
        let mut g = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut m = Model::new();
        let mut c = Ctx::default();
        let now = Instant::now();
        for step in 0..90 {
            let _ = op(&mut m, &mut g, &mut c, now);
            m.check()
                .unwrap_or_else(|e| panic!("seed {seed} step {step}: {e}"));
            if step % 9 == 8 {
                let size = (g.int(), g.int());
                match m.commit(size, now) {
                    Ok(Some((_, v))) => check_view(&v, &c, size, now),
                    Ok(None) => {}
                    Err(e) => panic!("seed {seed} step {step}: commit refused a sound model: {e:?}"),
                }
            }
        }
    }
}

/// A client that never commits is invisible, and one that keeps a secret
/// uncommitted is covered whole by the capture path (`pending_secret`).
#[test]
fn uncommitted_changes_are_never_published() {
    let now = Instant::now();
    let mut m = Model::new();
    m.set_root(1).unwrap();
    m.add_node(2, 1, 0).unwrap();
    assert!(m.commit((10, 10), now).unwrap().is_some());
    m.set_sensitivity(2, 2).unwrap();
    assert!(m.pending_secret(now));
    m.commit((10, 10), now).unwrap();
    assert!(!m.pending_secret(now), "committed: the view carries it now");
}

// ------------------------------------------------------------ cycles

#[test]
fn every_move_into_ones_own_subtree_is_a_cycle() {
    // All trees of up to 7 nodes where each node picks any earlier parent,
    // and every (node, new parent) pair on them.
    let now = Instant::now();
    for shape in 0..(1u32 << 12) {
        let mut m = Model::new();
        m.set_root(1).unwrap();
        let mut parents = vec![0u32];
        for id in 2..=7u32 {
            let p = 1 + (shape >> (2 * (id - 2))) % (id - 1);
            m.add_node(id, p, u32::MAX).unwrap();
            parents.push(p);
        }
        for id in 2..=7u32 {
            for to in 1..=7u32 {
                let mut under = false;
                let mut cur = to;
                while cur != 0 {
                    under |= cur == id;
                    cur = parents[cur as usize - 1];
                }
                let before = m.check();
                assert!(before.is_ok());
                let r = m.move_node(id, to, 0);
                if under {
                    assert_eq!(r, Err(Violation::Cycle), "shape {shape}: {id} under {to}");
                } else {
                    assert_eq!(r, Ok(()), "shape {shape}: {id} under {to}");
                    parents[id as usize - 1] = to;
                }
                m.check().unwrap();
            }
        }
        let (_, v) = m.commit((10, 10), now).unwrap().unwrap();
        assert!(v.complete && v.len() == 7);
    }
}

#[test]
fn a_tree_deeper_than_the_cap_is_refused_not_walked_forever() {
    let mut m = Model::new();
    m.set_root(1).unwrap();
    let mut refused = None;
    for id in 2..(MAX_DEPTH as u32 * 2) {
        if let Err(e) = m.add_node(id, id - 1, 0) {
            refused = Some((id, e));
            break;
        }
    }
    let (id, e) = refused.expect("the chain is cut off");
    assert_eq!(e, Violation::InvalidParent);
    assert!(id as usize <= MAX_DEPTH + 1);
    m.check().unwrap();
}

// ------------------------------------------------------------ id reuse

#[test]
fn id_reuse_cannot_launder_a_classification() {
    let now = Instant::now();
    let mut m = Model::new();
    m.set_root(1).unwrap();
    for id in 2..=6 {
        m.add_node(id, 1, u32::MAX).unwrap();
    }
    m.set_sensitivity(2, 2).unwrap();
    m.set_role(3, ROLE_PASSWORD).unwrap();
    m.set_ext(4, "credential", "true", now, false).unwrap();
    m.set_ext(5, "echo", "false", now, false).unwrap();
    for id in 2..=5 {
        m.remove_node(id, now).unwrap();
        // Reused for something innocent, in a different place.
        m.add_node(id, 6, 0).unwrap();
        m.set_role(id, 36).unwrap();
        m.set_value_text(id, "plain", 0, 0, 0).unwrap();
    }
    let (_, v) = m.commit((10, 10), now).unwrap().unwrap();
    for id in 2..=4 {
        assert_eq!(v.class_of(id, now), Some(Class::Secret), "id {id}");
    }
    assert_eq!(v.class_of(5, now), Some(Class::Secret), "echo stays raised");
    assert_eq!(
        v.node(3).unwrap().value,
        Value::Empty,
        "a former password has no text"
    );
    assert!(v
        .snapshot(Class::Private, now)
        .nodes
        .iter()
        .all(|n| n.id == 1 || n.id == 6));
}

#[test]
fn a_full_raise_table_taints_the_surface_instead_of_forgetting() {
    let now = Instant::now();
    let mut m = Model::new();
    m.set_root(1).unwrap();
    for id in 2..=(super::tree::SEC_CAP as u32 + 10) {
        m.add_node(id, 1, u32::MAX).unwrap();
        m.set_sensitivity(id, 2).unwrap();
    }
    assert!(m.tainted());
    let (_, v) = m.commit((10, 10), now).unwrap().unwrap();
    let f = v.secret_facts(now);
    assert!(f.known && f.unplaced);
    assert!(v.snapshot(Class::Private, now).nodes.is_empty());
}

// ------------------------------------------------------------ huge trees

fn flat(n: u32, focus: Option<u32>) -> Model {
    let mut m = Model::new();
    m.set_root(1).unwrap();
    for id in 2..=n {
        m.add_node(id, 1, u32::MAX).unwrap();
        m.set_rect(id, 0, id as i32, 10, 1).unwrap();
    }
    if let Some(f) = focus {
        m.set_states(f, [0u32].into_iter()).unwrap();
    }
    m
}

#[test]
fn a_hundred_thousand_nodes_truncate_deterministically() {
    let now = Instant::now();
    let publish = || {
        let mut m = flat(100_000, Some(77_777));
        let (c, v) = m.commit((100, 200_000), now).unwrap().unwrap();
        assert!(c.budget_exceeded);
        v
    };
    let (a, b) = (publish(), publish());
    assert_eq!(a.len(), NODE_BUDGET);
    assert!(!a.complete);
    assert_eq!(a.ids(), b.ids(), "same input, same truncation");
    assert_eq!(a.truncated, b.truncated);
    assert!(a.node(77_777).is_some(), "the focused node survives");
    assert!(
        a.node(2).is_some() && a.node(3).is_some(),
        "the first in document order"
    );
    assert!(a.node(99_999).is_none());
    // Connected: every kept node's parent is kept.
    for &id in a.ids() {
        let p = a.node(id).unwrap().parent;
        assert!(p == 0 || a.node(p).is_some());
    }
    assert!(!a.truncated.is_empty() && a.truncated.len() <= super::tree::TRUNCATED_MAX);
}

#[test]
fn a_deep_focused_node_keeps_its_path_under_the_budget() {
    let now = Instant::now();
    let mut m = Model::new();
    m.set_root(1).unwrap();
    // A 300-deep spine, then a wide, unfocused sibling list that overflows.
    for id in 2..=300u32 {
        m.add_node(id, id - 1, 0).unwrap();
    }
    for id in 301..=(NODE_BUDGET as u32 + 2_000) {
        m.add_node(id, 1, u32::MAX).unwrap();
    }
    m.set_states(300, [0u32].into_iter()).unwrap();
    let (_, v) = m.commit((10, 10), now).unwrap().unwrap();
    assert!(!v.complete);
    for id in 1..=300 {
        assert!(v.node(id).is_some(), "the path to the focus is kept: {id}");
    }
}

#[test]
fn the_model_stops_growing_at_its_cap() {
    let mut m = Model::new();
    m.set_root(1).unwrap();
    let mut id = 2u32;
    let err = loop {
        match m.add_node(id, 1, u32::MAX) {
            Ok(()) => id += 1,
            Err(e) => break e,
        }
    };
    assert_eq!(err, Violation::ExtLimit);
    assert_eq!(m.len(), MODEL_CAP);
    m.check().unwrap();
}

#[test]
fn removing_and_re_adding_a_huge_subtree_is_sound() {
    let now = Instant::now();
    let mut m = flat(50_000, None);
    m.add_node(60_000, 2, 0).unwrap();
    m.add_node(60_001, 60_000, 0).unwrap();
    m.remove_node(1, now).unwrap();
    assert!(m.is_empty());
    assert_eq!(m.root(), 0);
    m.check().unwrap();
    m.set_root(1).unwrap();
    assert!(m.commit((10, 10), now).unwrap().is_some());
}

// ------------------------------------------------------------ rects

#[test]
fn rects_are_clamped_whatever_the_client_says() {
    let mut g = Rng(0xFEED_FACE_CAFE_BEEF);
    for _ in 0..50_000 {
        let r = Rect {
            x: g.int(),
            y: g.int(),
            w: g.int(),
            h: g.int(),
        };
        let size = (g.int(), g.int());
        let (c, was) = r.clamped(size);
        let (bw, bh) = (size.0.max(0), size.1.max(0));
        assert!(
            c.x >= 0 && c.y >= 0 && c.w >= 0 && c.h >= 0,
            "{r:?} {size:?} -> {c:?}"
        );
        assert!(i64::from(c.x) + i64::from(c.w) <= i64::from(bw));
        assert!(i64::from(c.y) + i64::from(c.h) <= i64::from(bh));
        assert_eq!(was, c != r);
        // Idempotent: clamping what is already inside changes nothing.
        assert_eq!(c.clamped(size), (c, false));
    }
}

// ------------------------------------------------------------ text

#[test]
fn client_text_is_bounded_and_clean() {
    let mut g = Rng(7);
    for _ in 0..5_000 {
        let s = g.string();
        let out = clean(&s, 64, g.below(2) == 0);
        assert!(out.len() <= 64);
        assert!(out.chars().all(|c| !c.is_control() || c == '\n' || c == '\t'));
        assert!(!out.contains('\u{202E}'));
    }
}

// ------------------------------------------------------------ the wire

/// Random requests from a real client. Protocol errors kill that client and
/// are expected; what must hold is that the compositor survives, every
/// model stays sound, and nothing a client raised is lowered in a view.
#[test]
fn random_wire_traffic_never_breaks_the_compositor() {
    let mut h = harness();
    let mut g = Rng(0xA11C_E5ED);
    for _ in 0..24 {
        let mut c = Ref::connect(&mut h);
        let win = c.map(&mut h);
        let sem = c.semantic(&mut h, &win);
        sem.set_root(1);
        for _ in 0..8 {
            for _ in 0..8 {
                let node = g.below(IDS) as u32;
                let other = g.below(IDS) as u32;
                let s = || {
                    let mut g2 = Rng(node as u64 + 99);
                    let mut t = g2.string();
                    t.retain(|c| c != '\0');
                    t.truncate(200);
                    t
                };
                match g.below(18) {
                    0 => sem.add_node(node, other, g.below(5) as u32),
                    1 => sem.remove_node(node),
                    2 => sem.move_node(node, other, g.below(5) as u32),
                    3 => sem.set_role(node, g.below(70) as u32),
                    4 => sem.set_name(node, s()),
                    5 => sem.set_rect(node, g.int(), g.int(), g.int(), g.int()),
                    6 => sem.set_states(node, array(&[g.below(40) as u32, 0])),
                    7 => sem.set_value_text(node, s(), g.int(), g.int(), g.int()),
                    8 => sem.set_value_number(node, g.int() as f64, 0.0, 1.0, 0.5),
                    9 => sem.set_actions(node, array(&[g.below(14) as u32, 300])),
                    10 => sem.set_sensitivity(node, g.below(4) as u32),
                    11 => sem.set_ext(node, format!("k{}", g.below(20)), s()),
                    12 => sem.set_ext(node, "irreversible".into(), s()),
                    13 => sem.set_ext(node, "credential".into(), "true".into()),
                    14 => sem.ack_action(g.below(5) as u32, g.below(7) as u32),
                    15 => sem.set_action_label(node, 0, s()),
                    16 => sem.clear_value(node),
                    _ => sem.commit(),
                }
            }
            sem.commit();
            c.pump(&mut h);
            if c.error().is_some() {
                break;
            }
        }
        for e in &h.state.semantic.entries {
            if let Some(v) = &e.view {
                let now = Instant::now();
                // Nothing a client can send gives an agent a secret node.
                let snap = v.snapshot(Class::Private, now);
                for n in &snap.nodes {
                    assert_ne!(v.class_of(n.id, now), Some(Class::Secret));
                }
            }
        }
        drop(sem);
        drop(win);
        drop(c);
        h.dispatch();
        // First sweep notices the dead, the second is past their retention.
        h.state.semantic.sweep(Instant::now() + super::RETENTION);
        h.state.semantic.sweep(Instant::now() + super::RETENTION * 3);
    }
    assert!(
        h.state.semantic.is_empty(),
        "every publisher was dropped and swept"
    );
}
