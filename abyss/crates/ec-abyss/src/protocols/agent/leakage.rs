// SPDX-License-Identifier: AGPL-3.0-only
//! The COMP-16 M11 gate: the **scope leakage suite**.
//!
//! COMP-16 M11: "a window outside `scene.list` scope never appears in
//! listings, hit tests, events, captures or `wait_for` results; `no-agent`
//! windows absent for every agent". Driven end to end over the wire: a real
//! `wayland-client` maps the windows on the human display, and a real
//! `wayland-client` agent asks about them on the agent display.
//!
//! The world is a fixed matrix of windows that vary by class (via
//! [`test_class`], the stand-in for the M16 table), app_id, workspace,
//! output and the `no-agent` rule, two of them floating over visible ones.
//! Each grant below states the set it must see, written out by hand from the
//! cited rule rather than computed, so the suite does not share a bug with
//! `policy-eval`.
//!
//! One property the harness cannot separate: every client here is this test
//! process, so every window has the same pid, and "no hidden pid on the
//! wire" cannot be told apart from "the visible window's own pid". The pid
//! field is still checked to be the window's own.

use std::collections::BTreeSet;

use ec_protocols::agent::client::{eclipse_agent_v1::EclipseAgentV1, eclipse_scene_v1};
use policy_eval::{Capability, Class, Constraints, Grant, Ulid};
use smithay::desktop::Window;
use smithay::utils::{Logical, Point};

use super::tests::{hooked, signed, sk, Peer, INVALID_ARGUMENT};
use crate::config::{Matchers, Pattern, RuleAction, WindowRule};
use crate::policy::scene::test_class;
use crate::shell::focus::state_tests::client::{Client, Toplevel};
use crate::shell::focus::state_tests::Harness;

/// `eclipse_agent_v1.result.status` (COMP-08 §2.1).
const NO_CAPABILITY: u32 = 1;
const OUT_OF_SCOPE: u32 = 2;

/// Request ids: one listing, then one `get_toplevel` and one `hit_test` per
/// window, offset by its index, and one bogus handle.
const LIST: u32 = 1;
const BOGUS: u32 = 99;
const GET: u32 = 100;
const HIT: u32 = 200;

/// `(app_id, class)`. `None` is a window the table never classed, which is
/// `secret` (S-05 §8). Names are chosen so none is a substring of another,
/// which lets the wire scan look for a hidden name anywhere.
///
/// Placement, from the rules in [`world`]:
/// - `*A` and `f*`: output `test-a`, workspace 1, tiled except the floats.
/// - `*W2`: output `test-a`, workspace 2 (not shown, so never under a point).
/// - `*B`: output `test-b`, workspace 1.
/// - `nag*`, `fnag`: `no-agent`.
/// - `fsec` floats over `pubA`'s centre, `fnag` over `privA`'s.
const WINDOWS: &[(&str, Option<Class>)] = &[
    ("pubA", Some(Class::Public)),
    ("privA", Some(Class::Private)),
    ("secA", Some(Class::Secret)),
    ("unsetA", None),
    ("nagA", Some(Class::Public)),
    ("pubW2", Some(Class::Public)),
    ("privW2", Some(Class::Private)),
    ("fsec", Some(Class::Secret)),
    ("fnag", Some(Class::Public)),
    ("pubB", Some(Class::Public)),
    ("secB", Some(Class::Secret)),
];

/// Every window an agent could ever see in this world: not `secret`, not
/// `no-agent`.
const SEEABLE: &[&str] = &["pubA", "privA", "pubW2", "privW2", "pubB"];
/// The `public` ones of those.
const PUBLIC: &[&str] = &["pubA", "pubW2", "pubB"];

fn title(name: &str) -> String {
    format!("Title of {name}")
}

struct Win {
    name: &'static str,
    window: Window,
    handle: u32,
}

struct World {
    h: Harness,
    wins: Vec<Win>,
    app: Client,
    toplevels: Vec<(&'static str, Toplevel)>,
    next_grant: u8,
}

fn rule(app_id: &str, action: RuleAction) -> WindowRule {
    WindowRule {
        action,
        matchers: Matchers {
            app_id: Pattern::parse(app_id),
            ..Default::default()
        },
    }
}

fn find(h: &Harness, app_id: &str) -> Window {
    for e in h.state.outputs.iter() {
        for ws in &e.workspaces {
            for w in ws.all_windows() {
                if crate::ipc::methods::identity_of(&w).0.as_deref() == Some(app_id) {
                    return w;
                }
            }
        }
    }
    panic!("{app_id} not placed")
}

fn centre(h: &Harness, w: &Window) -> Option<Point<i32, Logical>> {
    let g = h.state.space.element_geometry(w)?;
    Some((g.loc.x + g.size.w / 2, g.loc.y + g.size.h / 2).into())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

/// The window matrix, mapped, classed and named over IPC (so every window
/// has a handle a `handle:` scope can name), with `policyd`'s key pinned.
fn world(sock: &str) -> World {
    let (mut h, _path) = hooked(sock, true);
    h.state.audit.sink = Some(Vec::new());
    h.state.policy_key = Some(sk().verifying_key());
    test_class::clear();
    h.state.config.window_rules = vec![
        rule("W2$", RuleAction::Workspace(2)),
        rule("B$", RuleAction::Output("test-b".into())),
        rule("(W2|A)$|^f", RuleAction::Output("test-a".into())),
        rule("nag", RuleAction::NoAgent),
        // Relative to test-a's tiling area at (4, 8): centres over pubA
        // at (403, 8) and privA at (4, 302), both 100x100.
        rule("^fsec$", RuleAction::Position(406, 12)),
        rule("^fnag$", RuleAction::Position(26, 322)),
    ];
    let mut app = Client::connect(&mut h);
    let mut toplevels = Vec::new();
    for (name, _) in WINDOWS {
        let t = app.create_toplevel(&mut h);
        t.toplevel.set_app_id((*name).into());
        t.toplevel.set_title(title(name));
        app.commit(&mut h, &t.surface);
        app.attach(&mut h, &t.surface);
        toplevels.push((*name, t));
    }
    let mut wins = Vec::new();
    for (name, class) in WINDOWS {
        let window = find(&h, name);
        if let Some(c) = class {
            test_class::set(&window, *c);
        }
        let handle = u32::try_from(h.state.ipc.handle_for(&window)).expect("small handle");
        wins.push(Win { name, window, handle });
    }
    let w = World {
        h,
        wins,
        app,
        toplevels,
        next_grant: 1,
    };
    w.check_layout();
    w
}

impl World {
    fn win(&self, name: &str) -> &Win {
        self.wins.iter().find(|w| w.name == name).expect("known window")
    }

    fn handle(&self, name: &str) -> u32 {
        self.win(name).handle
    }

    fn by_handle(&self, handle: u32) -> Option<&Win> {
        self.wins.iter().find(|w| w.handle == handle)
    }

    /// What the matrix relies on, so a layout change fails here and not as
    /// a confusing leak.
    fn check_layout(&self) {
        use crate::ipc::methods::location_of;
        let (a, b) = (self.h.a, self.h.b);
        for w in &self.wins {
            let want = match w.name {
                n if n.ends_with("W2") => (a, 2),
                n if n.ends_with('B') => (b, 1),
                _ => (a, 1),
            };
            assert_eq!(location_of(&self.h.state, &w.window), Some(want), "{}", w.name);
            assert_eq!(
                crate::shell::rules::hidden_from_agents(&w.window),
                w.name.contains("nag"),
                "{}",
                w.name
            );
        }
        for (top, beneath) in [("fsec", "pubA"), ("fnag", "privA")] {
            let c = centre(&self.h, &self.win(top).window).expect("float shown");
            let under = self.top_at(c).expect("something at the float's centre");
            assert!(under == self.win(top).window, "{top} is topmost at its centre");
            let g = self
                .h
                .state
                .space
                .element_geometry(&self.win(beneath).window)
                .expect("shown");
            assert!(g.contains(c), "{beneath} lies beneath {top}'s centre");
        }
    }

    /// The window a click at `pos` would really hit, by the compositor's
    /// own stacking.
    fn top_at(&self, pos: Point<i32, Logical>) -> Option<Window> {
        assert!(crate::shell::layer_at(&self.h.state, pos.to_f64()).is_none());
        self.h
            .state
            .space
            .element_under(pos.to_f64())
            .map(|(w, _)| w.clone())
    }

    /// A signed grant for `agent:leak` holding `caps`, expiring at
    /// `expires_ms`.
    fn grant(&mut self, caps: Vec<Capability>, expires_ms: u64) -> Vec<u8> {
        let id = self.next_grant;
        self.next_grant += 1;
        signed(&Grant {
            id: Ulid([id; 16]),
            principal: "agent:leak".into(),
            issued_ms: 0,
            expires_ms,
            issuer: "policyd".into(),
            task_id: Ulid([2; 16]),
            capabilities: caps,
            constraints: Constraints::default(),
            unattended: false,
        })
    }

    /// A fresh agent object admitted with one grant.
    fn agent(&mut self, caps: Vec<Capability>) -> (Peer, EclipseAgentV1, eclipse_scene_v1::EclipseSceneV1) {
        let g = self.grant(caps, u64::MAX);
        let mut p = Peer::inserted(&mut self.h, true);
        let (agent, scene) = p.admit(&mut self.h, g);
        assert!(p.error().is_none(), "admitted");
        (p, agent, scene)
    }

    /// Every query in the matrix: one listing, `get_toplevel` on a bogus
    /// handle and on every window's real handle, and `hit_test` at every
    /// shown window's centre.
    fn ask_everything(&mut self, p: &mut Peer, scene: &eclipse_scene_v1::EclipseSceneV1) {
        scene.list_toplevels(LIST, String::new());
        let bogus = self.wins.iter().map(|w| w.handle).max().unwrap_or(0) + 1000;
        scene.get_toplevel(BOGUS, bogus);
        for (i, w) in self.wins.iter().enumerate() {
            scene.get_toplevel(GET + i as u32, w.handle);
            if let Some(c) = centre(&self.h, &w.window) {
                scene.hit_test(HIT + i as u32, c.x, c.y);
            }
        }
        p.pump(&mut self.h);
        assert!(p.error().is_none(), "agent connection intact");
        assert!(!self.h.state.agents.slots.is_empty(), "agent object still alive");
    }
}

/// One capability line: `name` with `scopes`, AND-combined (S-01 §3).
fn line(name: &str, scopes: &[&str]) -> Capability {
    Capability {
        name: name.into(),
        scopes: scopes.iter().map(|s| s.to_string()).collect(),
        quota: None,
    }
}

/// `scene.list` and `scene.read` over the same lines, so whatever exists
/// may also be read.
fn list_and_read(lines: &[Vec<String>]) -> Vec<Capability> {
    let mut caps = Vec::new();
    for name in ["scene.list", "scene.read"] {
        for l in lines {
            let scopes: Vec<&str> = l.iter().map(String::as_str).collect();
            caps.push(line(name, &scopes));
        }
    }
    caps
}

/// `(label, capability lines, the windows it must see)`.
type Case = (&'static str, Vec<Vec<String>>, Vec<&'static str>);

fn lines(ls: &[&[&str]]) -> Vec<Vec<String>> {
    ls.iter()
        .map(|l| l.iter().map(|s| s.to_string()).collect())
        .collect()
}

/// The facts a scene event carries about one window.
#[derive(Debug, PartialEq)]
struct Carried {
    handle: u32,
    app_id: String,
    title: String,
    pid: u32,
    workspace: u32,
    output: u32,
    rect: (i32, i32, i32, i32),
    parent: u32,
    sensitivity: u32,
}

fn carried(e: &eclipse_scene_v1::Event) -> Option<(u32, Carried)> {
    use eclipse_scene_v1::Event;
    match e {
        Event::Toplevel {
            req_id,
            handle,
            app_id,
            title,
            pid,
            workspace,
            output,
            x,
            y,
            w,
            h,
            parent,
            sensitivity,
            ..
        } => Some((
            *req_id,
            Carried {
                handle: *handle,
                app_id: app_id.clone(),
                title: title.clone(),
                pid: *pid,
                workspace: *workspace,
                output: *output,
                rect: (*x, *y, *w, *h),
                parent: *parent,
                sensitivity: *sensitivity,
            },
        )),
        _ => None,
    }
}

/// `(req_id, handle)` of a `toplevel_detail`, the part of `get_toplevel`'s
/// answer that follows its `toplevel`.
fn detail(e: &eclipse_scene_v1::Event) -> Option<(u32, u32)> {
    match e {
        eclipse_scene_v1::Event::ToplevelDetail { req_id, handle, .. } => Some((*req_id, *handle)),
        _ => None,
    }
}

/// `eclipse_scene_v1.sensitivity` of the class the window was given
/// (COMP-08 §3 `sensitivity`; S-05 §2 classes). An agent never sees a
/// `secret` window, so 2 never reaches the wire.
fn wire_class(name: &str) -> u32 {
    match WINDOWS.iter().find(|(n, _)| *n == name).and_then(|(_, c)| *c) {
        Some(Class::Public) => 0,
        Some(Class::Private) => 1,
        Some(Class::Secret) | None => 2,
    }
}

/// What a window's own event must say about it.
fn own(w: &World, win: &Win, parent: u32) -> Carried {
    let (output, workspace) = crate::ipc::methods::location_of(&w.h.state, &win.window).expect("placed");
    let g = w.h.state.space.element_geometry(&win.window).unwrap_or_default();
    Carried {
        handle: win.handle,
        app_id: win.name.into(),
        title: title(win.name),
        pid: std::process::id(),
        workspace: workspace as u32,
        output: output as u32,
        rect: (g.loc.x, g.loc.y, g.size.w, g.size.h),
        parent,
        sensitivity: wire_class(win.name),
    }
}

fn results_for(p: &Peer, req: u32) -> Vec<(u32, String)> {
    p.seen
        .results
        .iter()
        .filter(|(r, ..)| *r == req)
        .map(|(_, s, d)| (*s, d.clone()))
        .collect()
}

fn hits_for(p: &Peer, req: u32) -> Vec<u32> {
    p.seen
        .hits
        .iter()
        .filter(|(r, _)| *r == req)
        .map(|(_, h)| *h)
        .collect()
}

/// The matrix assertion for one grant whose `scene.read` equals its
/// `scene.list`: the agent sees exactly `expected`, and every other window
/// is absent from every answer, not redacted.
///
/// - Listing (COMP-08 §3, S-01 §3): exactly the expected handles, one event
///   each, all before the one `toplevels_done`, so the done counts nothing
///   hidden.
/// - `get_toplevel` (COMP-08 §3 as amended by F-07, S-01 §3): a visible
///   window answers with exactly one `toplevel` carrying its own facts and
///   one `toplevel_detail` with the same handle, and no `result`; a hidden
///   window's real handle answers exactly as the bogus handle does,
///   `invalid_argument` with detail `handle`, with neither event.
/// - `hit_test` (COMP-08 §3): at each shown window's centre, the handle of
///   the real topmost window there if it is visible, else 0, never a window
///   beneath an invisible one.
/// - The wire (COMP-08 §3 "nothing outside scope appears"): every
///   `toplevel` carries its own window's facts, its sensitivity the class
///   the window was given, and nothing else; every `toplevel_detail` names
///   a visible handle; no hidden app_id or title appears in any event.
fn assert_sees_exactly(w: &World, p: &Peer, expected: &[&str], case: &str) {
    let want: BTreeSet<u32> = expected.iter().map(|n| w.handle(n)).collect();
    let visible = |h: u32| want.contains(&h);

    // Listing.
    let listed: Vec<u32> = p
        .seen
        .toplevels
        .iter()
        .filter(|(r, _)| *r == LIST)
        .map(|(_, h)| *h)
        .collect();
    assert_eq!(listed.len(), want.len(), "{case}: listing count {listed:?}");
    assert_eq!(
        listed.iter().copied().collect::<BTreeSet<_>>(),
        want,
        "{case}: listed set"
    );
    assert_eq!(
        p.seen.done.iter().filter(|r| **r == LIST).count(),
        1,
        "{case}: one toplevels_done"
    );
    let done_at = p
        .seen
        .scene
        .iter()
        .position(|e| matches!(e, eclipse_scene_v1::Event::ToplevelsDone { req_id } if *req_id == LIST))
        .expect("done");
    let late = p.seen.scene[done_at..]
        .iter()
        .any(|e| matches!(e, eclipse_scene_v1::Event::Toplevel { req_id, .. } if *req_id == LIST));
    assert!(!late, "{case}: a toplevel after its toplevels_done");

    // get_toplevel.
    let bogus = results_for(p, BOGUS);
    assert_eq!(
        bogus,
        vec![(INVALID_ARGUMENT, "handle".to_string())],
        "{case}: bogus"
    );
    for (i, win) in w.wins.iter().enumerate() {
        let req = GET + i as u32;
        let facts: Vec<Carried> = p
            .seen
            .scene
            .iter()
            .filter_map(carried)
            .filter(|(r, _)| *r == req)
            .map(|(_, c)| c)
            .collect();
        let details: Vec<u32> = p
            .seen
            .scene
            .iter()
            .filter_map(detail)
            .filter(|(r, _)| *r == req)
            .map(|(_, h)| h)
            .collect();
        if visible(win.handle) {
            assert_eq!(facts, vec![own(w, win, 0)], "{case}: get {}'s toplevel", win.name);
            assert_eq!(details, vec![win.handle], "{case}: get {}'s detail", win.name);
            assert!(results_for(p, req).is_empty(), "{case}: get {} result", win.name);
        } else {
            assert!(facts.is_empty(), "{case}: toplevel of hidden {}", win.name);
            assert!(details.is_empty(), "{case}: detail of hidden {}", win.name);
            assert_eq!(
                results_for(p, req),
                bogus,
                "{case}: hidden {} unlike bogus",
                win.name
            );
        }
    }

    // hit_test.
    for (i, win) in w.wins.iter().enumerate() {
        let Some(c) = centre(&w.h, &win.window) else {
            continue;
        };
        let req = HIT + i as u32;
        let top = w.top_at(c).and_then(|t| w.wins.iter().find(|x| x.window == t));
        let expect = top.map(|t| t.handle).filter(|h| visible(*h)).unwrap_or(0);
        assert_eq!(
            hits_for(p, req),
            vec![expect],
            "{case}: hit at {}'s centre (top {:?})",
            win.name,
            top.map(|t| t.name)
        );
        assert!(
            results_for(p, req).is_empty(),
            "{case}: hit result at {}",
            win.name
        );
    }

    // The wire.
    let hidden: Vec<&Win> = w.wins.iter().filter(|x| !visible(x.handle)).collect();
    for e in &p.seen.scene {
        if let Some((_, c)) = carried(e) {
            assert!(
                visible(c.handle),
                "{case}: event names hidden handle {}",
                c.handle
            );
            assert!(
                c.parent == 0 || visible(c.parent),
                "{case}: hidden parent {}",
                c.parent
            );
            let win = w.by_handle(c.handle).expect("a real window");
            assert_eq!(
                c,
                own(w, win, c.parent),
                "{case}: {} carries foreign facts",
                win.name
            );
        }
        if let Some((_, handle)) = detail(e) {
            assert!(visible(handle), "{case}: detail names hidden {handle}");
        }
        if let eclipse_scene_v1::Event::Hit { handle, .. } = e {
            assert!(
                *handle == 0 || visible(*handle),
                "{case}: hit names hidden {handle}"
            );
        }
        let text = format!("{e:?}");
        for x in &hidden {
            assert!(!text.contains(x.name), "{case}: {} on the wire: {text}", x.name);
        }
    }
    // The scan reads what the strings carry: every visible name is in it.
    let all: String = p.seen.scene.iter().map(|e| format!("{e:?}")).collect();
    for n in expected {
        assert!(
            all.contains(&title(n)),
            "{case}: {n}'s title not seen by the scan"
        );
    }
}

/// COMP-16 M11 gate, COMP-08 §3, S-01 §2.1/§3/§4, F-07: for every grant in
/// the matrix and every window in the world, the agent sees exactly the
/// windows the scope grammar puts in scope, and every other window is
/// absent from listing, `get_toplevel` and `hit_test` alike (see
/// [`assert_sees_exactly`] for each rule). Scopes AND within a line and OR
/// across lines, and one line never loosens another (S-01 §3, §4). A line
/// with no `class:` reaches `public` only (S-01 §2.1 default "`public`").
/// `workspace:<n>` is the 1-based number on whichever output (COMP-05,
/// `get_windows`). `title:` names the whole title (S-01 §3 `title:<regex>`,
/// anchored so a substring does not widen it).
#[test]
fn every_grant_sees_exactly_its_scope() {
    let mut w = world("matrix.sock");
    let b = w.h.b.to_string();
    let h = |w: &World, n: &str| format!("handle:{}", w.handle(n));
    let cases: Vec<Case> = vec![
        ("workspace:human", lines(&[&["workspace:human"]]), PUBLIC.to_vec()),
        (
            "workspace:human class:private",
            lines(&[&["workspace:human", "class:private"]]),
            SEEABLE.to_vec(),
        ),
        (
            "workspace:human class:public",
            lines(&[&["workspace:human", "class:public"]]),
            PUBLIC.to_vec(),
        ),
        (
            "workspace:2 class:private",
            lines(&[&["workspace:2", "class:private"]]),
            vec!["pubW2", "privW2"],
        ),
        ("workspace:1", lines(&[&["workspace:1"]]), vec!["pubA", "pubB"]),
        (
            "workspace:own",
            lines(&[&["workspace:own", "class:private"]]),
            vec![],
        ),
        (
            "app_id:priv* class:private",
            lines(&[&["app_id:priv*", "class:private"]]),
            vec!["privA", "privW2"],
        ),
        ("app_id:pubA", lines(&[&["app_id:pubA"]]), vec!["pubA"]),
        ("app_id:priv* no class", lines(&[&["app_id:priv*"]]), vec![]),
        (
            "app_id:* class:secret",
            lines(&[&["app_id:*", "class:secret"]]),
            SEEABLE.to_vec(),
        ),
        ("output:test-b", lines(&[&["output:test-b"]]), vec!["pubB"]),
        (
            "output:<id of b>",
            vec![vec![format!("output:{b}")]],
            vec!["pubB"],
        ),
        (
            "output:virtual class:private",
            lines(&[&["output:virtual", "class:private"]]),
            SEEABLE.to_vec(),
        ),
        (
            "output:physical",
            lines(&[&["output:physical", "class:private"]]),
            vec![],
        ),
        (
            "output:test-a workspace:1 class:private",
            lines(&[&["output:test-a", "workspace:1", "class:private"]]),
            vec!["pubA", "privA"],
        ),
        (
            "title:Title of pubA",
            lines(&[&["title:Title of pubA"]]),
            vec!["pubA"],
        ),
        ("title:pubA (substring)", lines(&[&["title:pubA"]]), vec![]),
        (
            "handle:privA class:private",
            vec![vec![h(&w, "privA"), "class:private".into()]],
            vec!["privA"],
        ),
        ("handle:pubB", vec![vec![h(&w, "pubB")]], vec!["pubB"]),
        (
            "two lines, OR, neither loosens the other",
            vec![
                vec!["app_id:pubA".into()],
                vec!["app_id:privW2".into(), "class:private".into()],
            ],
            vec!["pubA", "privW2"],
        ),
        (
            "class:private on another line does not reach privA",
            vec![
                vec!["app_id:privA".into()],
                vec!["app_id:pubB".into(), "class:private".into()],
            ],
            vec!["pubB"],
        ),
        (
            "content scopes match no window",
            lines(&[&["url:*", "class:private"], &["path:*", "class:private"]]),
            vec![],
        ),
        (
            "an unparseable scope voids its line",
            lines(&[&["workspace:human", "class:private", "bogus:x"]]),
            vec![],
        ),
    ];
    for (case, ls, expected) in cases {
        let (mut p, _agent, scene) = w.agent(list_and_read(&ls));
        w.ask_everything(&mut p, &scene);
        assert_sees_exactly(&w, &p, &expected, case);
    }
}

/// S-05 §2 (`secret`: "Not delivered to any agent without a `*.secret`
/// capability **and** a per-request prompt"), S-01 §2.1 (`scene.list` has no
/// secret form; `scene.read.secret` "never without per-request prompt"),
/// S-05 §8 (no table entry: `secret`): no grant, not even one scoped
/// `class:secret` or naming the secret window by handle or app_id, and not
/// one holding `scene.read.secret`, sees `secA`, `secB`, `fsec` or the
/// never-classed `unsetA`. There is no prompt path, so nothing can satisfy
/// the "and".
#[test]
fn secret_windows_are_never_visible() {
    let mut w = world("secret.sock");
    let hs = |w: &World, n: &str| vec![format!("handle:{}", w.handle(n)), "class:secret".to_string()];
    let cases: Vec<Case> = vec![
        (
            "class:secret workspace:human",
            lines(&[&["class:secret", "workspace:human"]]),
            SEEABLE.to_vec(),
        ),
        (
            "class:secret alone",
            lines(&[&["class:secret"]]),
            SEEABLE.to_vec(),
        ),
        (
            "app_id:sec* class:secret",
            lines(&[&["app_id:sec*", "class:secret"]]),
            vec![],
        ),
        (
            "app_id:fsec class:secret",
            lines(&[&["app_id:fsec", "class:secret"]]),
            vec![],
        ),
        ("handle:secA", vec![hs(&w, "secA")], vec![]),
        ("handle:secB", vec![hs(&w, "secB")], vec![]),
        ("handle:unsetA (never classed)", vec![hs(&w, "unsetA")], vec![]),
        (
            "output:test-b class:secret",
            lines(&[&["output:test-b", "class:secret"]]),
            vec!["pubB"],
        ),
    ];
    for (case, ls, expected) in cases {
        let mut caps = list_and_read(&ls);
        for l in &ls {
            let scopes: Vec<&str> = l.iter().map(String::as_str).collect();
            caps.push(line("scene.read.secret", &scopes));
        }
        let (mut p, _agent, scene) = w.agent(caps);
        w.ask_everything(&mut p, &scene);
        assert_sees_exactly(&w, &p, &expected, case);
    }
}

/// COMP-16 M11 gate ("`no-agent` windows absent for every agent"), COMP-05
/// §4 (`no-agent` rule), COMP-08 §3 (a `no-agent` handle answers as an
/// unknown one): `nagA` and `fnag`, both `public`, are absent under every
/// grant, including ones naming them by handle or app_id and the broadest
/// scope the grammar has, and `fnag` on top of `privA` hides it from
/// `hit_test` rather than exposing what lies beneath.
#[test]
fn no_agent_windows_are_never_visible() {
    let mut w = world("noagent.sock");
    let h = |w: &World, n: &str| vec![format!("handle:{}", w.handle(n))];
    let cases: Vec<Case> = vec![
        ("handle:nagA", vec![h(&w, "nagA")], vec![]),
        ("handle:fnag", vec![h(&w, "fnag")], vec![]),
        (
            "app_id:*nag*",
            lines(&[&["app_id:*nag*", "class:secret"]]),
            vec![],
        ),
        ("title:Title of nagA", lines(&[&["title:Title of nagA"]]), vec![]),
        (
            "every scope at once",
            lines(&[
                &["workspace:human", "class:secret"],
                &["output:virtual", "class:secret"],
                &["app_id:*", "class:secret"],
                &["principal:any", "class:secret"],
            ]),
            SEEABLE.to_vec(),
        ),
    ];
    for (case, ls, expected) in cases {
        let (mut p, _agent, scene) = w.agent(list_and_read(&ls));
        w.ask_everything(&mut p, &scene);
        assert_sees_exactly(&w, &p, &expected, case);
    }
}

/// COMP-08 §3 ("Nothing outside scope appears, including in `hit_test`
/// (returns handle=0)") and S-01 §3 (same, "including in events and
/// `hit_test`"): where an invisible window (`fsec` secret, `fnag`
/// `no-agent`) is on top of a visible one (`pubA`, `privA`), the hit is
/// handle 0, never the visible window beneath. A visible window on top
/// (`pubB`) is hit by its own handle.
#[test]
fn hit_test_never_reaches_beneath_an_invisible_top() {
    let mut w = world("hit.sock");
    let (mut p, _agent, scene) = w.agent(list_and_read(&lines(&[&["workspace:human", "class:private"]])));
    let probes = [("fsec", 0), ("fnag", 0), ("pubB", w.handle("pubB"))];
    for (i, (name, _)) in probes.iter().enumerate() {
        let c = centre(&w.h, &w.win(name).window).expect("shown");
        scene.hit_test(i as u32 + 1, c.x, c.y);
    }
    p.pump(&mut w.h);
    for (i, (name, want)) in probes.iter().enumerate() {
        assert_eq!(hits_for(&p, i as u32 + 1), vec![*want], "{name}");
    }
    assert!(p.seen.results.is_empty());
}

/// S-01 §2.1 (`scene.list` grants listing, metadata only; `get_toplevel`
/// and `hit_test` need `scene.read`), COMP-08 §2.1 (`no_capability`, detail
/// the capability name), and COMP-08 §3 (absence comes first: a hidden
/// window is still `invalid_argument "handle"`, a hidden top still hit 0).
#[test]
fn scene_list_without_scene_read_lists_but_cannot_read() {
    let mut w = world("noread.sock");
    let (mut p, _agent, scene) = w.agent(vec![line("scene.list", &["workspace:human", "class:private"])]);
    scene.list_toplevels(LIST, String::new());
    scene.get_toplevel(2, w.handle("privA"));
    scene.get_toplevel(3, w.handle("pubW2"));
    scene.get_toplevel(4, w.handle("secA"));
    scene.get_toplevel(5, w.handle("nagA"));
    let c = centre(&w.h, &w.win("pubB").window).expect("shown");
    scene.hit_test(6, c.x, c.y);
    let c = centre(&w.h, &w.win("fsec").window).expect("shown");
    scene.hit_test(7, c.x, c.y);
    p.pump(&mut w.h);

    let listed: BTreeSet<u32> = p.seen.toplevels.iter().map(|(_, h)| *h).collect();
    let want: BTreeSet<u32> = SEEABLE.iter().map(|n| w.handle(n)).collect();
    assert_eq!(listed, want);
    let no_read = vec![(NO_CAPABILITY, "scene.read".to_string())];
    let unknown = vec![(INVALID_ARGUMENT, "handle".to_string())];
    assert_eq!(results_for(&p, 2), no_read, "privA");
    assert_eq!(results_for(&p, 3), no_read, "pubW2");
    assert_eq!(results_for(&p, 4), unknown, "secA");
    assert_eq!(results_for(&p, 5), unknown, "nagA");
    assert_eq!(results_for(&p, 6), no_read, "hit pubB");
    assert!(hits_for(&p, 6).is_empty(), "no hit event without scene.read");
    assert_eq!(hits_for(&p, 7), vec![0], "hidden top is handle 0");
    assert!(
        !p.seen
            .scene
            .iter()
            .any(|e| matches!(e, eclipse_scene_v1::Event::ToplevelDetail { .. })),
        "no detail without scene.read"
    );
}

/// COMP-08 §3 as amended by F-07 (2026-10-01): "`out_of_scope` is only for a
/// visible target that a held capability's own scope excludes", and S-01 §3
/// ("a visible target outside the acting capability's scope fails with
/// `out_of_scope`"). `scene.read` is held, scoped to `privA`; `privW2` and
/// `pubB` are visible through `scene.list` but outside that scope, so
/// `get_toplevel` and `hit_test` on them are `out_of_scope` (COMP-08 §2.1
/// status 2) with detail `scene.read`, not `no_capability`, which is for a
/// capability not held at all; and neither sends a window's facts.
#[test]
fn a_visible_window_outside_scene_read_scope_is_out_of_scope() {
    let mut w = world("oos.sock");
    let (mut p, _agent, scene) = w.agent(vec![
        line("scene.list", &["workspace:human", "class:private"]),
        line("scene.read", &["app_id:privA", "class:private"]),
    ]);
    scene.get_toplevel(1, w.handle("privW2"));
    let c = centre(&w.h, &w.win("pubB").window).expect("shown");
    scene.hit_test(2, c.x, c.y);
    p.pump(&mut w.h);
    let oos = vec![(OUT_OF_SCOPE, "scene.read".to_string())];
    assert_eq!(results_for(&p, 1), oos, "get_toplevel privW2");
    assert_eq!(results_for(&p, 2), oos, "hit pubB");
    assert!(p.seen.toplevels.is_empty(), "no toplevel for an unreadable get");
    assert!(!p.seen.scene.iter().any(|e| detail(e).is_some()), "no detail");
    assert!(p.seen.hits.is_empty(), "no hit event for an unreadable hit");
}

/// S-01 §8 open item 2 (unscoped `scene.list` proposed "no": it "leaks
/// window titles across the machine"; `policy-eval` denies it until
/// decided, failing closed per F-07 §6), and S-01 §2.1 (`scene.list`
/// default "`public`"; `scene.read` on `public`+`private`): an unscoped
/// `scene.list`/`scene.read` sees nothing at all, and a `private` window
/// needs `class:private` even when named by handle.
#[test]
fn unscoped_sees_nothing_and_private_needs_class_private() {
    let mut w = world("unscoped.sock");
    let (mut p, _agent, scene) = w.agent(vec![line("scene.list", &[]), line("scene.read", &[])]);
    w.ask_everything(&mut p, &scene);
    assert_sees_exactly(&w, &p, &[], "unscoped");

    let hp = format!("handle:{}", w.handle("privA"));
    let (mut p, _agent, scene) = w.agent(list_and_read(&[vec![hp]]));
    w.ask_everything(&mut p, &scene);
    assert_sees_exactly(&w, &p, &[], "handle:privA without class:private");
}

/// S-01 §4: "Expiry is checked at request time in the compositor; no
/// grace", and grants are additive within a principal, each with its own
/// scopes. With a broad grant about to expire beside a narrow one that
/// lasts, the agent sees the broad set before expiry and only the narrow
/// one after, with the formerly visible windows absent (not redacted) from
/// every answer.
#[test]
fn an_expired_grant_stops_seeing_windows() {
    let mut w = world("expiry.sock");
    let broad = list_and_read(&lines(&[&["workspace:human", "class:private"]]));
    let short = w.grant(broad, now_ms() + 400);
    let narrow = list_and_read(&lines(&[&["app_id:pubA"]]));
    let narrow = w.grant(narrow, u64::MAX);
    let mut p = Peer::inserted(&mut w.h, true);
    let (agent, scene) = p.admit(&mut w.h, short);
    agent.add_grant(narrow);
    p.pump(&mut w.h);
    assert!(p.error().is_none());

    w.ask_everything(&mut p, &scene);
    assert_sees_exactly(&w, &p, SEEABLE, "before expiry");

    std::thread::sleep(std::time::Duration::from_millis(500));
    let mut p2 = p;
    p2.seen = Default::default();
    w.ask_everything(&mut p2, &scene);
    assert_sees_exactly(&w, &p2, &["pubA"], "after expiry");
}

/// S-01 §4 (expiry at request time, no grace) and COMP-08 §3: once the
/// agent's only grant has expired it sees nothing, mid-session, on the same
/// objects: no `toplevel`, no `toplevel_detail`, and no hit naming a window.
#[test]
fn the_last_grant_expiring_leaves_nothing_visible() {
    let mut w = world("expiry-all.sock");
    let caps = list_and_read(&lines(&[&["workspace:human", "class:private"]]));
    let g = w.grant(caps, now_ms() + 400);
    let mut p = Peer::inserted(&mut w.h, true);
    let (_agent, scene) = p.admit(&mut w.h, g);
    assert!(p.error().is_none());
    w.ask_everything(&mut p, &scene);
    assert_sees_exactly(&w, &p, SEEABLE, "before expiry");

    std::thread::sleep(std::time::Duration::from_millis(500));
    p.seen = Default::default();
    w.ask_everything(&mut p, &scene);
    assert!(p.seen.toplevels.is_empty(), "listed after expiry");
    assert!(
        !p.seen
            .scene
            .iter()
            .any(|e| matches!(e, eclipse_scene_v1::Event::ToplevelDetail { .. })),
        "detail after expiry"
    );
    assert!(p.seen.hits.iter().all(|(_, h)| *h == 0), "hit after expiry");
    for e in &p.seen.scene {
        let text = format!("{e:?}");
        for win in &w.wins {
            assert!(!text.contains(win.name), "{} after expiry: {text}", win.name);
        }
    }
}

/// COMP-08 §3 ("every response is filtered to the agent's `scene.list`
/// scope ... Nothing outside scope appears"): with the narrowest grant
/// (`app_id:pubA`, ten of eleven windows hidden), every byte the agent
/// receives is about `pubA` or is a refusal: one `toplevel` in the listing
/// and one answering `get_toplevel(pubA)`, then that get's one
/// `toplevel_detail`; nothing for the ten hidden gets; every hit 0 or
/// `pubA`; no hidden window's app_id, title, handle or geometry on the wire. `pubA`
/// lies under `fsec`, so even its own centre hits 0.
#[test]
fn an_in_scope_listing_carries_nothing_of_hidden_windows() {
    let mut w = world("wire.sock");
    let (mut p, _agent, scene) = w.agent(list_and_read(&lines(&[&["app_id:pubA"]])));
    w.ask_everything(&mut p, &scene);
    assert_sees_exactly(&w, &p, &["pubA"], "app_id:pubA");

    use eclipse_scene_v1::Event;
    let pub_a = w.handle("pubA");
    let mut toplevels = 0;
    let mut details = 0;
    for e in &p.seen.scene {
        match e {
            Event::Toplevel { handle, .. } => {
                assert_eq!(*handle, pub_a);
                toplevels += 1;
            }
            Event::ToplevelDetail { handle, .. } => {
                assert_eq!(*handle, pub_a);
                details += 1;
            }
            Event::Hit { handle, .. } => assert_eq!(*handle, 0, "pubA is covered by fsec"),
            Event::ToplevelsDone { .. } | Event::Result { .. } => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!((toplevels, details), (2, 1));
    // Geometry: no hidden shown window's rectangle is carried.
    let carried_rects: Vec<_> = p
        .seen
        .scene
        .iter()
        .filter_map(carried)
        .map(|(_, c)| c.rect)
        .collect();
    for win in w.wins.iter().filter(|x| x.handle != pub_a) {
        if let Some(g) = w.h.state.space.element_geometry(&win.window) {
            let r = (g.loc.x, g.loc.y, g.size.w, g.size.h);
            assert!(!carried_rects.contains(&r), "{}'s geometry on the wire", win.name);
        }
    }
}

/// S-01 §3 (`title:<regex>` scope) and `policy-eval`'s `WindowFacts`
/// ("built fresh per check, never cached"): a window that leaves its scope
/// by renaming itself is absent from the very next request.
#[test]
fn a_window_renamed_out_of_scope_disappears() {
    let mut w = world("rename.sock");
    let (mut p, _agent, scene) = w.agent(list_and_read(&lines(&[&["title:Title of pubB"]])));
    w.ask_everything(&mut p, &scene);
    assert_sees_exactly(&w, &p, &["pubB"], "before rename");

    let t = &w.toplevels.iter().find(|(n, _)| *n == "pubB").expect("pubB").1;
    t.toplevel.set_title("Renamed".into());
    let surface = t.surface.clone();
    w.app.commit(&mut w.h, &surface);
    p.seen = Default::default();
    w.ask_everything(&mut p, &scene);
    assert_sees_exactly(&w, &p, &[], "after rename");
}

/// COMP-16 M11 gate ("never appears in ... events"), S-01 §2.1
/// (`scene.events`: "filtered to visible set"), S-01 §3.
#[test]
#[ignore = "gap: eclipse_scene_v1 has no subscribe request or scene events yet"]
fn events_never_name_a_hidden_window() {
    panic!("gap: no scene.events subscription in eclipse-agent-v1.xml");
}

/// COMP-16 M11 gate ("never appears in ... `wait_for` results"), COMP-08 §3
/// (`wait_for`, `waited`).
#[test]
#[ignore = "gap: eclipse_scene_v1 has no wait_for request yet"]
fn wait_for_never_resolves_on_a_hidden_window() {
    panic!("gap: no wait_for in eclipse-agent-v1.xml");
}

/// COMP-16 M11 gate ("never appears in ... captures"), S-05 §2 (`secret`
/// redacted from output capture).
#[test]
#[ignore = "gap: no agent capture path (capture.* capabilities) exists yet"]
fn captures_never_contain_a_hidden_window() {
    panic!("gap: no agent capture request");
}

/// COMP-08 §3 (`list_toplevels(filter)`, "KDL filter (S-01 §3 scope
/// grammar)"): a filter narrows within `scene.list` scope and can never
/// name a hidden window into view.
#[test]
#[ignore = "gap: the list_toplevels filter grammar is not parsed; any non-empty filter is invalid_argument"]
fn a_filter_never_widens_the_listing() {
    panic!("gap: list_toplevels filter not implemented");
}

/// COMP-08 §3 (`get_toplevel` answers `toplevel_detail`) and S-01 §2.1
/// (`scene.read` reaches `public`+`private`): a visible, readable window's
/// handle answers with exactly one `toplevel` carrying that window's own
/// facts and class, then one `toplevel_detail` with the same handle, and
/// the agent stays connected.
#[test]
fn get_toplevel_on_a_visible_window_answers_with_its_own_detail() {
    let mut w = world("detail.sock");
    let (mut p, _agent, scene) = w.agent(list_and_read(&lines(&[&["workspace:human", "class:private"]])));
    for (i, name) in SEEABLE.iter().enumerate() {
        scene.get_toplevel(GET + i as u32, w.handle(name));
    }
    p.pump(&mut w.h);
    assert!(p.error().is_none());
    assert!(
        !w.h.state.agents.slots.is_empty(),
        "agent disconnected by get_toplevel"
    );
    for (i, name) in SEEABLE.iter().enumerate() {
        let req = GET + i as u32;
        let details: Vec<Carried> = p
            .seen
            .scene
            .iter()
            .filter_map(carried)
            .filter(|(r, _)| *r == req)
            .map(|(_, c)| c)
            .collect();
        let win = w.win(name);
        assert_eq!(details, vec![own(&w, win, 0)], "{name}");
        let slim: Vec<u32> = p
            .seen
            .scene
            .iter()
            .filter_map(detail)
            .filter(|(r, _)| *r == req)
            .map(|(_, h)| h)
            .collect();
        assert_eq!(slim, vec![win.handle], "{name}: one toplevel_detail, same handle");
        let at_toplevel = p
            .seen
            .scene
            .iter()
            .position(|e| carried(e).is_some_and(|(r, _)| r == req));
        let at_detail = p
            .seen
            .scene
            .iter()
            .position(|e| detail(e).is_some_and(|(r, _)| r == req));
        assert!(at_toplevel < at_detail, "{name}: toplevel before toplevel_detail");
        assert!(results_for(&p, req).is_empty(), "{name}");
    }
}
