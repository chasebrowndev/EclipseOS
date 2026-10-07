// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse_semantic_v1` against the in-process harness (COMP-16 M22, COMP-09
//! §6): the tree and action round trips, the protocol errors, the agent read
//! path through the scene filter, and the redaction suite's live-tree arm.
//!
//! The fuzz targets are in `fuzz.rs`; the reference client is `refclient.rs`.

use std::time::{Duration, Instant};

use ec_cataclysm_pub::{Config, Publisher};
use ec_policy_eval::scope::SCENE_LIST;
use ec_policy_eval::{Capability, Class, Constraints, Grant, SceneView, Ulid};
use ec_protocols::semantic::client::eclipse_semantic_surface_v1::EclipseSemanticSurfaceV1;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use super::refclient::{array, forward, Ref, Win, WIN};
use super::tree::{Rect, Value, GRACE, NODE_BUDGET};
use super::*;
use crate::policy::scene;
use crate::render::capture::{resolve_nodes, NodeVerdict, RedactReason};
use crate::shell::focus::state_tests::{harness, Harness};

const ROLE_WINDOW: u32 = 0;
const ROLE_BUTTON: u32 = 20;
const ROLE_TEXTFIELD: u32 = 28;
const ROLE_PASSWORD: u32 = 30;
const ROLE_LABEL: u32 = 36;
const ROLE_PROGRESS: u32 = 35;
const ROLE_LINK: u32 = 25;
const ROLE_TERMINAL: u32 = 48;
const ROLE_TERMINAL_LINE: u32 = 49;
const VERB_ACTIVATE: u32 = 0;
const STATE_FOCUSABLE: u32 = 1;
const CLASS_PRIVATE: u32 = 1;
const CLASS_SECRET: u32 = 2;

struct Rig {
    h: Harness,
    c: Ref,
    win: Win,
    sem: EclipseSemanticSurfaceV1,
}

fn rig() -> Rig {
    scene::test_class::clear();
    let mut h = harness();
    let mut c = Ref::connect(&mut h);
    let win = c.map(&mut h);
    let sem = c.semantic(&mut h, &win);
    Rig { h, c, win, sem }
}

impl Rig {
    fn pump(&mut self) {
        self.c.pump(&mut self.h);
    }

    /// The compositor's `wl_surface` for the (only) toplevel.
    fn surface(&self) -> WlSurface {
        self.h.state.xdg_shell_state.toplevel_surfaces()[0]
            .wl_surface()
            .clone()
    }

    fn entry(&self) -> &Entry {
        &self.h.state.semantic.entries[0]
    }

    fn view(&self) -> &View {
        self.entry().view.as_ref().expect("a committed tree")
    }

    /// A root and three children: a button with an action, a text field and
    /// a label. All inside the window.
    fn base(&mut self) {
        let s = &self.sem;
        s.set_root(1);
        s.set_role(1, ROLE_WINDOW);
        s.set_rect(1, 0, 0, WIN.0, WIN.1);
        s.add_node(2, 1, 0);
        s.set_role(2, ROLE_BUTTON);
        s.set_name(2, "Send".into());
        s.set_rect(2, 10, 10, 50, 20);
        s.set_actions(2, array(&[VERB_ACTIVATE]));
        s.set_states(2, array(&[STATE_FOCUSABLE]));
        s.add_node(3, 1, 1);
        s.set_role(3, ROLE_TEXTFIELD);
        s.set_rect(3, 10, 40, 100, 20);
        s.set_value_text(3, "hello".into(), 5, 0, 5);
        s.add_node(4, 1, 2);
        s.set_role(4, ROLE_LABEL);
        s.set_name(4, "Subject".into());
        s.commit();
        self.pump();
    }

    fn verdict(&self) -> NodeVerdict {
        let (t, g, k) = capture_facts(&self.h.state, Some(&self.surface()));
        resolve_nodes(&t, g, k)
    }

    /// The scene view and handle an agent with `scene.list` over every
    /// human workspace would have, with the window classed `private`.
    fn agent(&mut self, class: Class) -> (SceneView, u64) {
        let window = crate::shell::window_for_surface(&self.h.state, &self.surface()).expect("mapped window");
        scene::test_class::set(&window, class);
        let grant = Grant {
            id: Ulid([1; 16]),
            principal: "agent:test".into(),
            issued_ms: 0,
            expires_ms: u64::MAX,
            issuer: "policyd".into(),
            task_id: Ulid([2; 16]),
            capabilities: vec![Capability {
                name: SCENE_LIST.into(),
                scopes: vec!["workspace:human".into(), "class:private".into()],
                quota: None,
            }],
            constraints: Constraints::default(),
            unattended: false,
        };
        let view = SceneView::compile([&grant]);
        let handle = self.h.state.ipc.handle_for(&window);
        (view, handle)
    }
}

/// Run `f` against a rig with the base tree published; the protocol error
/// code the client was killed with.
fn error_of(f: impl FnOnce(&EclipseSemanticSurfaceV1)) -> Option<u32> {
    let mut r = rig();
    r.base();
    f(&r.sem);
    r.pump();
    r.c.error()
}

// ------------------------------------------------------------ round trips

#[test]
fn the_capabilities_are_announced_on_bind() {
    let r = rig();
    assert_eq!(r.c.seen.caps, Some(7), "ACTIONS | DIFF | SENSITIVITY");
    assert_eq!(r.h.state.semantic.len(), 1);
}

#[test]
fn a_published_tree_round_trips() {
    let mut r = rig();
    r.base();
    {
        let s = &r.sem;
        s.add_node(5, 1, 3);
        s.set_role(5, ROLE_PROGRESS);
        s.set_value_number(5, 0.5, 0.0, 1.0, 0.25);
        s.add_node(6, 1, 4);
        s.set_role(6, ROLE_LINK);
        s.set_value_url(6, "https://example.org/".into());
        s.set_description(6, "a link".into());
        s.set_action_label(2, VERB_ACTIVATE, "Send mail".into());
        s.set_ext(2, "ext.accesskey".into(), "s".into());
        s.commit();
    }
    r.pump();
    assert_eq!(r.c.error(), None);
    let v = r.view();
    assert_eq!((v.generation, v.value_rev), (2, 0));
    assert!(v.complete);
    assert_eq!(v.root, 1);
    assert_eq!(v.ids(), [1, 2, 3, 4, 5, 6], "document order");
    let n = v.node(2).unwrap();
    assert_eq!(n.name, "Send");
    assert_eq!(n.role, ROLE_BUTTON);
    assert_eq!(
        n.rect,
        Some(Rect {
            x: 10,
            y: 10,
            w: 50,
            h: 20
        })
    );
    assert_eq!(n.states, 1 << STATE_FOCUSABLE);
    assert_eq!(n.actions, [VERB_ACTIVATE]);
    assert_eq!(n.labels, [(VERB_ACTIVATE, "Send mail".to_owned())]);
    assert_eq!(n.ext, [("accesskey".to_owned(), "s".to_owned())]);
    assert_eq!(
        v.node(3).unwrap().value,
        Value::Text {
            text: "hello".into(),
            cursor: 5,
            sel: (0, 5)
        }
    );
    assert_eq!(
        v.node(5).unwrap().value,
        Value::Number {
            value: 0.5,
            min: 0.0,
            max: 1.0,
            step: 0.25
        }
    );
    assert_eq!(
        v.node(6).unwrap().value,
        Value::Url("https://example.org/".into())
    );
    assert_eq!(v.node(1).unwrap().children, [2, 3, 4, 5, 6]);
}

#[test]
fn nothing_is_visible_before_commit_and_a_commit_is_atomic() {
    let mut r = rig();
    r.base();
    let before = r.view().generation;
    r.sem.add_node(7, 1, 0);
    r.sem.set_name(7, "new".into());
    r.pump();
    assert_eq!(r.view().generation, before);
    assert!(r.view().node(7).is_none(), "uncommitted");
    r.sem.commit();
    r.pump();
    assert_eq!(r.view().generation, before + 1);
    assert_eq!(r.view().node(7).unwrap().name, "new");
    assert_eq!(r.view().node(1).unwrap().children[0], 7, "index 0");
}

#[test]
fn a_value_only_commit_bumps_the_value_revision_not_the_generation() {
    let mut r = rig();
    r.base();
    let (g, rev) = (r.view().generation, r.view().value_rev);
    r.sem.set_value_text(3, "hello!".into(), 6, 6, 6);
    r.sem.commit();
    r.pump();
    assert_eq!((r.view().generation, r.view().value_rev), (g, rev + 1));
    r.sem.set_rect(3, 10, 40, 120, 20);
    r.sem.commit();
    r.pump();
    assert_eq!((r.view().generation, r.view().value_rev), (g + 1, rev + 1));
    // An empty commit changes nothing.
    r.sem.commit();
    r.pump();
    assert_eq!((r.view().generation, r.view().value_rev), (g + 1, rev + 1));
}

#[test]
fn rects_are_clamped_to_the_surface_and_flagged() {
    let mut r = rig();
    r.base();
    r.sem.set_rect(4, -50, 90, 100, 100);
    r.sem.set_rect(3, i32::MIN, i32::MAX, i32::MAX, i32::MIN);
    r.sem.commit();
    r.pump();
    assert_eq!(r.c.error(), None, "a wild rectangle is data, not an error");
    let size = surface_size(&r.h.state, &r.surface());
    assert_eq!(size, WIN, "the window is the buffer");
    let n = r.view().node(4).unwrap();
    assert_eq!(
        n.rect,
        Some(Rect {
            x: 0,
            y: 90,
            w: 50,
            h: 10
        })
    );
    assert!(n.clamped);
    assert!(r.view().node(3).unwrap().rect.unwrap().is_empty());
    assert!(!r.view().node(2).unwrap().clamped);
}

#[test]
fn a_cataclysm_publisher_round_trips_through_the_server() {
    let mut r = rig();
    let mut p = Publisher::new(Config::default(), 4, 20).unwrap();
    p.set_line(0, b"$ make").unwrap();
    p.set_line(1, b"cc main.c").unwrap();
    {
        let b = p.poll(0).expect("initial publish");
        forward(&r.sem, &b);
    }
    r.pump();
    assert_eq!(r.c.error(), None);
    let v = r.view();
    assert_eq!(v.node(1).unwrap().role, ROLE_TERMINAL);
    let lines: Vec<_> = v
        .node(1)
        .unwrap()
        .children
        .iter()
        .map(|&c| (v.node(c).unwrap().role, v.node(c).unwrap().value.clone()))
        .collect();
    assert_eq!(lines.len(), 4);
    assert!(lines.iter().all(|(role, _)| *role == ROLE_TERMINAL_LINE));
    assert_eq!(
        lines[1].1,
        Value::Text {
            text: "cc main.c".into(),
            cursor: -1,
            sel: (-1, -1)
        }
    );
    let (g, rev) = (v.generation, v.value_rev);
    // A scrolling log rewrites values: no node is added or removed (P-04 §7).
    p.set_line(1, b"cc main.c -O2").unwrap();
    {
        let b = p.poll(300_000_000).expect("value publish");
        forward(&r.sem, &b);
    }
    r.pump();
    assert_eq!((r.view().generation, r.view().value_rev), (g, rev + 1));
    // A command block is a structural change.
    p.osc133(b"A", 400_000_000);
    {
        let b = p.poll(600_000_000).expect("block publish");
        forward(&r.sem, &b);
    }
    r.pump();
    assert_eq!(r.c.error(), None);
    assert!(r.view().generation > g);
    assert!(r.view().len() > 5, "a group node arrived");
}

// ------------------------------------------------------------ protocol errors

#[test]
fn a_second_semantic_surface_for_a_toplevel_is_already_exists() {
    let mut r = rig();
    let _second = r.c.semantic(&mut r.h, &r.win);
    assert_eq!(r.c.error(), Some(3));
}

#[test]
fn every_protocol_error_has_its_spec_code() {
    // INVALID_NODE
    assert_eq!(error_of(|s| s.set_name(99, "x".into())), Some(0));
    assert_eq!(error_of(|s| s.remove_node(99)), Some(0));
    assert_eq!(error_of(|s| s.set_root(0)), Some(0));
    // INVALID_PARENT
    assert_eq!(error_of(|s| s.add_node(9, 99, 0)), Some(1));
    assert_eq!(error_of(|s| s.move_node(2, 99, 0)), Some(1));
    assert_eq!(
        error_of(|s| s.move_node(1, 2, 0)),
        Some(1),
        "the root cannot move"
    );
    // CYCLE
    assert_eq!(
        error_of(|s| {
            s.add_node(9, 2, 0);
            s.move_node(2, 9, 0);
        }),
        Some(2)
    );
    assert_eq!(error_of(|s| s.move_node(2, 2, 0)), Some(2));
    // ALREADY_EXISTS
    assert_eq!(error_of(|s| s.add_node(2, 1, 0)), Some(3));
    assert_eq!(error_of(|s| s.set_root(77)), Some(3));
    // LOWER_CLASSIFICATION (the wire code of v0.1 LOWER_SENSITIVITY)
    assert_eq!(error_of(|s| s.set_sensitivity(2, 0)), Some(5));
    assert_eq!(
        error_of(|s| {
            s.set_sensitivity(2, CLASS_SECRET);
            s.set_sensitivity(2, CLASS_PRIVATE);
        }),
        Some(5)
    );
    assert_eq!(
        error_of(|s| {
            s.set_ext(2, "credential".into(), "true".into());
            s.set_ext(2, "ext.credential".into(), "false".into());
        }),
        Some(5)
    );
    // EXT_LIMIT
    assert_eq!(
        error_of(|s| {
            for i in 0..17 {
                s.set_ext(2, format!("k{i}"), "v".into());
            }
        }),
        Some(6)
    );
    assert_eq!(error_of(|s| s.set_ext(2, "k".into(), "x".repeat(257))), Some(6));
    // UNCOMMITTED_DESTROY
    assert_eq!(
        error_of(|s| {
            s.set_name(2, "changed".into());
            s.destroy();
        }),
        Some(7)
    );
    // A clean destroy is fine.
    assert_eq!(error_of(|s| s.destroy()), None);
}

#[test]
fn a_client_cannot_lower_a_classification_by_reusing_an_id() {
    let mut r = rig();
    r.base();
    r.sem.set_sensitivity(2, CLASS_SECRET);
    r.sem.commit();
    r.sem.remove_node(2);
    r.sem.add_node(2, 1, 0);
    r.sem.commit();
    r.pump();
    assert_eq!(r.c.error(), None);
    assert_eq!(r.view().class_of(2, Instant::now()), Some(Class::Secret));
}

#[test]
fn an_unknown_irreversible_id_is_dropped_with_a_warning() {
    let mut r = rig();
    r.base();
    assert_eq!(r.c.seen.budget, 0);
    r.sem
        .set_ext(2, "ext.irreversible".into(), "communication.send".into());
    r.pump();
    assert_eq!(r.c.error(), None, "dropped, not a protocol error");
    assert_eq!(r.c.seen.budget, 1, "the budget_exceeded-style warning");
    r.sem.commit();
    r.pump();
    let (view, handle) = r.agent(Class::Private);
    match read_tree(&r.h.state, &view, handle, Instant::now()) {
        ReadOutcome::Tree(t) => {
            let n = t.nodes.iter().find(|n| n.id == 2).unwrap();
            assert!(
                n.ext.iter().all(|(k, _)| k != "irreversible"),
                "no category was recorded"
            );
        }
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------ the agent read path

#[test]
fn an_agent_reads_the_tree_through_the_scene_filter() {
    let mut r = rig();
    r.base();
    let (view, handle) = r.agent(Class::Private);
    let ReadOutcome::Tree(t) = read_tree(&r.h.state, &view, handle, Instant::now()) else {
        panic!("a visible window with a tree");
    };
    assert_eq!(t.root, 1);
    assert_eq!(t.nodes.len(), 4);
    assert!(t.complete);
    assert!(
        t.nodes.iter().all(|n| n.sensitivity >= Class::Private),
        "every node is at least the window's class, and the default is private"
    );
    let send = t.nodes.iter().find(|n| n.id == 2).unwrap();
    assert_eq!(send.name, "Send");
    assert_eq!(send.actions, [(VERB_ACTIVATE, String::new())]);
}

#[test]
fn a_window_the_agent_cannot_see_has_no_tree_to_read() {
    let mut r = rig();
    r.base();
    // A `secret` window is invisible to every agent: the same answer as one
    // that never existed.
    let (view, handle) = r.agent(Class::Secret);
    assert_eq!(
        read_tree(&r.h.state, &view, handle, Instant::now()),
        ReadOutcome::NotFound
    );
    assert_eq!(
        read_tree(&r.h.state, &SceneView::default(), handle, Instant::now()),
        ReadOutcome::NotFound
    );
    assert_eq!(
        read_tree(&r.h.state, &view, handle + 1000, Instant::now()),
        ReadOutcome::NotFound
    );
}

#[test]
fn secret_nodes_never_reach_an_agent() {
    let mut r = rig();
    r.base();
    {
        let s = &r.sem;
        s.add_node(10, 1, 3);
        s.set_role(10, ROLE_PASSWORD);
        s.set_name(10, "Password".into());
        s.set_value_text(10, "hunter2".into(), 7, 7, 7);
        s.set_rect(10, 10, 70, 100, 20);
        s.add_node(11, 1, 4);
        s.set_role(11, ROLE_LABEL);
        s.set_name(11, "account number".into());
        s.set_sensitivity(11, CLASS_SECRET);
        s.add_node(12, 11, 0);
        s.set_name(12, "child of a secret".into());
        s.add_node(13, 1, 5);
        s.set_ext(13, "credential".into(), "true".into());
        s.commit();
    }
    r.pump();
    assert_eq!(r.c.error(), None);
    // The compositor holds the nodes; the password's text is gone for good.
    assert_eq!(r.view().node(10).unwrap().value, Value::Empty);
    let (view, handle) = r.agent(Class::Private);
    let ReadOutcome::Tree(t) = read_tree(&r.h.state, &view, handle, Instant::now()) else {
        panic!("tree");
    };
    let ids: Vec<u32> = t.nodes.iter().map(|n| n.id).collect();
    assert_eq!(
        ids,
        [1, 2, 3, 4],
        "no password, no secret node, no child of one, no credential"
    );
    assert_eq!(t.nodes[0].children, [2, 3, 4]);
    let dump = format!("{t:?}");
    for leaked in ["hunter2", "account number", "child of a secret", "Password"] {
        assert!(!dump.contains(leaked), "{leaked} reached an agent");
    }
}

#[test]
fn a_destroyed_publisher_answers_client_gone_for_a_second() {
    let mut r = rig();
    r.base();
    let (view, handle) = r.agent(Class::Private);
    r.sem.destroy();
    r.pump();
    assert_eq!(r.c.error(), None);
    assert_eq!(
        read_tree(&r.h.state, &view, handle, Instant::now()),
        ReadOutcome::ClientGone,
        "retained, but not served"
    );
    let later = Instant::now() + RETENTION + Duration::from_millis(5);
    r.h.state.semantic.sweep(later);
    assert!(r.h.state.semantic.is_empty(), "dropped after the retention");
    assert_eq!(
        read_tree(&r.h.state, &view, handle, Instant::now()),
        ReadOutcome::NoTree
    );
}

// ------------------------------------------------------------ actions

#[test]
fn a_semantic_action_round_trips() {
    let mut r = rig();
    r.base();
    let surface = r.surface();
    let generation = r.view().generation;
    let t = Instant::now();
    let serial = request_action(&mut r.h.state, &surface, 42, 2, VERB_ACTIVATE, "now", generation).unwrap();
    let asked = t.elapsed();
    r.pump();
    assert_eq!(r.c.seen.actions, [(serial, 2, VERB_ACTIVATE, "now".to_owned())]);
    r.sem.ack_action(serial, 0);
    r.pump();
    let t = Instant::now();
    let done = take_done(&mut r.h.state);
    let collected = t.elapsed();
    assert_eq!(done.len(), 1);
    assert_eq!(
        (
            done[0].requester,
            done[0].serial,
            done[0].node,
            done[0].verb,
            done[0].status
        ),
        (42, serial, 2, VERB_ACTIVATE, ActionStatus::Ok)
    );
    assert_eq!(done[0].generation, generation, "the post-action generation");
    // COMP-09 §6: compositor-side latency, the client excluded.
    assert!(
        asked + collected < Duration::from_millis(3),
        "{asked:?} + {collected:?}"
    );
    assert!(take_done(&mut r.h.state).is_empty(), "exactly once");
}

#[test]
fn an_action_is_refused_before_it_reaches_the_client() {
    let mut r = rig();
    r.base();
    {
        let s = &r.sem;
        s.add_node(10, 1, 3);
        s.set_role(10, ROLE_PASSWORD);
        s.set_actions(10, array(&[VERB_ACTIVATE]));
        s.commit();
    }
    r.pump();
    let surface = r.surface();
    let g = r.view().generation;
    let mut ask =
        |node, verb, generation| request_action(&mut r.h.state, &surface, 1, node, verb, "", generation);
    assert_eq!(ask(2, VERB_ACTIVATE, g + 1), Err(ActionError::StaleGeneration));
    assert_eq!(ask(2, VERB_ACTIVATE, g - 1), Err(ActionError::StaleGeneration));
    assert_eq!(
        ask(2, 5, g),
        Err(ActionError::NoSuchAction),
        "the node does not offer it"
    );
    assert_eq!(
        ask(3, VERB_ACTIVATE, g),
        Err(ActionError::NoSuchAction),
        "no actions at all"
    );
    assert_eq!(ask(99, VERB_ACTIVATE, g), Err(ActionError::NoSuchNode));
    assert_eq!(
        ask(10, VERB_ACTIVATE, g),
        Err(ActionError::NoSuchNode),
        "a secret node cannot be named"
    );
    r.pump();
    assert!(r.c.seen.actions.is_empty(), "nothing was sent for any of them");
}

#[test]
fn an_unacked_action_times_out_and_a_late_ack_is_ignored() {
    let mut r = rig();
    r.base();
    let surface = r.surface();
    let g = r.view().generation;
    let serial = request_action(&mut r.h.state, &surface, 7, 2, VERB_ACTIVATE, "", g).unwrap();
    r.pump();
    assert!(take_done(&mut r.h.state).is_empty(), "still in flight");
    r.h.state
        .semantic
        .sweep(Instant::now() + ACTION_TIMEOUT + Duration::from_millis(5));
    let done = take_done(&mut r.h.state);
    assert_eq!(done.len(), 1);
    assert_eq!(
        (done[0].serial, done[0].status),
        (serial, ActionStatus::ClientTimeout)
    );
    r.sem.ack_action(serial, 0);
    r.pump();
    assert_eq!(r.c.error(), None, "a late ack is a race, not an error");
    assert!(take_done(&mut r.h.state).is_empty(), "and does not answer twice");
}

#[test]
fn a_client_ack_cannot_claim_the_compositors_statuses() {
    let mut r = rig();
    r.base();
    let surface = r.surface();
    let g = r.view().generation;
    let serial = request_action(&mut r.h.state, &surface, 7, 2, VERB_ACTIVATE, "", g).unwrap();
    r.sem.ack_action(serial, 4);
    r.pump();
    assert_eq!(take_done(&mut r.h.state)[0].status, ActionStatus::Failed);
}

#[test]
fn a_publisher_that_goes_away_fails_what_it_held() {
    let mut r = rig();
    r.base();
    let surface = r.surface();
    let g = r.view().generation;
    request_action(&mut r.h.state, &surface, 7, 2, VERB_ACTIVATE, "", g).unwrap();
    r.sem.destroy();
    r.pump();
    let done = take_done(&mut r.h.state);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].status, ActionStatus::ClientGone);
    assert_eq!(
        request_action(&mut r.h.state, &surface, 7, 2, VERB_ACTIVATE, "", g),
        Err(ActionError::ClientGone)
    );
}

#[test]
fn a_focus_hint_reaches_the_publisher_for_a_node_agents_may_name() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_role(10, ROLE_PASSWORD);
    r.sem.commit();
    r.pump();
    let surface = r.surface();
    focus_hint(&r.h.state, &surface, 3);
    focus_hint(&r.h.state, &surface, 10);
    focus_hint(&r.h.state, &surface, 99);
    r.pump();
    assert_eq!(r.c.seen.hints, [3]);
}

// ------------------------------------------------------------ redaction, live-tree arm

/// COMP-15 §2's redaction suite, run against a tree a client really
/// published (COMP-16 M22). The fail-closed arms (absent, stale) were
/// asserted in `render/capture.rs` against hand-built trees; these assert
/// the same verdicts when the facts come from [`capture_facts`]. The pixel
/// and pass-list assertions (occlusion, popups) stay in that suite and read
/// the same `semantics_for` once it calls [`capture_facts`] (TCB-HOOK).

#[test]
fn live_arm_absent_tree_covers_nothing_when_nothing_is_secret() {
    let h = harness();
    let (t, g, k) = capture_facts(&h.state, None);
    assert_eq!((t, g, k), (SemanticTree::Absent, 0, false));
    // A surface with no semantic surface at all.
    let mut h = harness();
    let mut c = Ref::connect(&mut h);
    let _w = c.map(&mut h);
    let surface = h.state.xdg_shell_state.toplevel_surfaces()[0]
        .wl_surface()
        .clone();
    let (t, g, k) = capture_facts(&h.state, Some(&surface));
    assert_eq!(resolve_nodes(&t, g, k), NodeVerdict::None);
}

#[test]
fn live_arm_a_tree_without_secrets_covers_nothing() {
    let mut r = rig();
    assert_eq!(r.verdict(), NodeVerdict::None, "no commit yet");
    r.base();
    assert_eq!(r.verdict(), NodeVerdict::None);
}

#[test]
fn live_arm_a_placed_password_is_covered_by_its_rectangle() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_role(10, ROLE_PASSWORD);
    r.sem.set_rect(10, 10, 70, 100, 20);
    r.sem.commit();
    r.pump();
    let rect = Rectangle::<i32, Logical>::new((10, 70).into(), (100, 20).into());
    assert_eq!(r.verdict(), NodeVerdict::Nodes(vec![rect]));
}

#[test]
fn live_arm_a_declared_secret_node_and_its_children_are_covered() {
    let mut r = rig();
    r.base();
    {
        let s = &r.sem;
        s.add_node(10, 1, 3);
        s.set_sensitivity(10, CLASS_SECRET);
        s.set_rect(10, 0, 60, 50, 20);
        s.add_node(11, 10, 0);
        s.set_rect(11, 60, 60, 40, 20);
        s.commit();
    }
    r.pump();
    let a = Rectangle::<i32, Logical>::new((0, 60).into(), (50, 20).into());
    let b = Rectangle::<i32, Logical>::new((60, 60).into(), (40, 20).into());
    assert_eq!(r.verdict(), NodeVerdict::Nodes(vec![a, b]));
}

#[test]
fn live_arm_a_secret_without_a_rectangle_covers_the_whole_surface() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_role(10, ROLE_PASSWORD);
    r.sem.commit();
    r.pump();
    assert_eq!(
        r.verdict(),
        NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
    );
}

#[test]
fn live_arm_a_secret_clamped_to_nothing_covers_the_whole_surface() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_role(10, ROLE_PASSWORD);
    r.sem.set_rect(10, 5_000, 5_000, 100, 20);
    r.sem.commit();
    r.pump();
    assert_eq!(
        r.verdict(),
        NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
    );
}

#[test]
fn live_arm_a_resize_makes_the_tree_stale() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_role(10, ROLE_PASSWORD);
    r.sem.set_rect(10, 10, 70, 100, 20);
    r.sem.commit();
    r.pump();
    assert!(matches!(r.verdict(), NodeVerdict::Nodes(_)));
    r.c.resize(&mut r.h, &r.win, WIN.0 + 40, WIN.1);
    assert_eq!(r.verdict(), NodeVerdict::WholeSurface(RedactReason::StaleTree));
    // The client republishes for the new size: current again.
    r.sem.set_rect(10, 10, 70, 140, 20);
    r.sem.commit();
    r.pump();
    assert!(matches!(r.verdict(), NodeVerdict::Nodes(_)));
}

#[test]
fn live_arm_a_resize_with_nothing_secret_is_not_a_redaction() {
    let mut r = rig();
    r.base();
    r.c.resize(&mut r.h, &r.win, WIN.0 + 40, WIN.1);
    assert_eq!(r.verdict(), NodeVerdict::None);
}

#[test]
fn live_arm_a_secret_declared_but_not_yet_committed_covers_the_whole_surface() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_role(10, ROLE_PASSWORD);
    r.sem.set_rect(10, 10, 70, 100, 20);
    r.pump();
    assert_eq!(
        r.verdict(),
        NodeVerdict::WholeSurface(RedactReason::UnplacedSecret),
        "the older tree's rectangles are not trusted for it"
    );
    r.sem.commit();
    r.pump();
    assert!(matches!(r.verdict(), NodeVerdict::Nodes(_)));
}

#[test]
fn live_arm_removing_a_secret_does_not_uncover_it_inside_the_grace() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_role(10, ROLE_PASSWORD);
    r.sem.set_rect(10, 10, 70, 100, 20);
    r.sem.commit();
    r.sem.remove_node(10);
    r.sem.commit();
    r.pump();
    assert_eq!(
        r.verdict(),
        NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
    );
    // Past the grace (the tree-level test pins the clock) the node is simply
    // gone.
    let v = r.view();
    assert!(
        !v.secret_facts(Instant::now() + GRACE + Duration::from_millis(5))
            .known
    );
}

#[test]
fn live_arm_lowering_attempts_leave_the_redaction_in_place() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_sensitivity(10, CLASS_SECRET);
    r.sem.set_rect(10, 10, 70, 100, 20);
    r.sem.commit();
    r.pump();
    assert!(matches!(r.verdict(), NodeVerdict::Nodes(_)));
    // The lowering is a protocol error (the client is cut off, its window
    // with it) and the compositor still holds the secret.
    let surface = r.surface();
    r.sem.set_sensitivity(10, CLASS_PRIVATE);
    r.pump();
    assert_eq!(r.c.error(), Some(5));
    let (_, _, known) = capture_facts(&r.h.state, Some(&surface));
    assert!(known);
}

#[test]
fn live_arm_a_tree_that_vanishes_holding_a_secret_cannot_uncover_it() {
    let mut r = rig();
    r.base();
    r.sem.add_node(10, 1, 3);
    r.sem.set_role(10, ROLE_PASSWORD);
    r.sem.set_rect(10, 10, 70, 100, 20);
    r.sem.commit();
    r.sem.destroy();
    r.pump();
    r.h.state
        .semantic
        .sweep(Instant::now() + RETENTION + Duration::from_millis(5));
    assert!(r.h.state.semantic.is_empty(), "the tree is gone");
    assert_eq!(
        r.verdict(),
        NodeVerdict::WholeSurface(RedactReason::UnplacedSecret),
        "but the compositor still knows"
    );
}

#[test]
fn live_arm_a_secret_beyond_the_node_budget_covers_the_whole_surface() {
    let mut r = rig();
    r.sem.set_root(1);
    r.sem.set_role(1, ROLE_WINDOW);
    let n = NODE_BUDGET as u32 + 100;
    for i in 2..=n {
        r.sem.add_node(i, 1, u32::MAX);
        if i % 200 == 0 {
            r.pump();
        }
    }
    r.sem.set_role(n, ROLE_PASSWORD);
    r.sem.set_rect(n, 0, 0, 10, 10);
    r.sem.commit();
    r.pump();
    assert_eq!(r.c.error(), None);
    assert!(!r.view().complete);
    assert_eq!(r.c.seen.budget, 1, "budget_exceeded was sent");
    assert_eq!(
        r.verdict(),
        NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
    );
}

#[test]
fn live_arm_echo_off_from_the_terminal_publisher_covers_the_terminal() {
    let mut r = rig();
    let mut p = Publisher::new(Config::default(), 3, 20).unwrap();
    p.set_line(0, b"$ sudo ls").unwrap();
    {
        let b = p.poll(0).unwrap();
        forward(&r.sem, &b);
    }
    r.pump();
    assert_eq!(r.verdict(), NodeVerdict::None, "an ordinary terminal");
    // `sudo` asks for a password: the raise is the very next batch.
    p.set_echo(false, 10_000_000);
    {
        let b = p.poll(10_000_000).expect("the raise leaves ahead of every limit");
        forward(&r.sem, &b);
    }
    r.pump();
    assert_eq!(r.c.error(), None);
    assert_eq!(
        r.verdict(),
        NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
    );
    // The agent reads nothing of it: the terminal root is secret.
    let (view, handle) = r.agent(Class::Private);
    let ReadOutcome::Tree(t) = read_tree(&r.h.state, &view, handle, Instant::now()) else {
        panic!("tree");
    };
    assert!(t.nodes.is_empty(), "no tree, no text, for agents while raised");
    // Echo back on: the raise holds through the grace, then the publisher
    // resumes. The compositor's hold is the same grace.
    p.set_echo(true, 20_000_000);
    assert!(p.poll(20_000_000).is_none(), "nothing leaves while raised");
    assert_eq!(
        r.verdict(),
        NodeVerdict::WholeSurface(RedactReason::UnplacedSecret)
    );
}
