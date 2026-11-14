// SPDX-License-Identifier: AGPL-3.0-only
//! Tree shape of the glass surfaces (T-20 §5): bars and sheets, search, tab badges, tooltip.

use accesskit::{Action, NodeId, Rect, Role, Toggled};
use accesskit_consumer::{NodeRef, Tree};
use apoth_a11y::ids::{self, Target};
use apoth_a11y::{
    BarButtonModel, BarModel, Builder, ChipModel, ProgressKind, ProgressModel, SearchModel,
    TabModel, TextModel, TooltipModel, WindowModel,
};
use apoth_grid::WordChars;

fn r(x: f64) -> Rect {
    Rect {
        x0: x,
        y0: 1.0,
        x1: x + 10.0,
        y1: 11.0,
    }
}

fn find<'a>(n: NodeRef<'a>, id: NodeId) -> Option<NodeRef<'a>> {
    if n.locate().0 == id {
        return Some(n);
    }
    n.children().find_map(|c| find(c, id))
}

fn tree(m: &WindowModel) -> Tree {
    let mut b = Builder::new(TextModel::Runs, WordChars::new(""));
    Tree::new(b.full(m), true)
}

fn bounds(n: &NodeRef<'_>) -> Rect {
    n.data().bounds().expect("bounds")
}

fn button(label: &str, key: &str, disabled: bool, x: f64) -> BarButtonModel {
    BarButtonModel {
        label: label.into(),
        key: key.into(),
        disabled,
        bounds: Some(r(x)),
    }
}

#[test]
fn consent_bar_is_a_group_with_keyed_buttons_and_does_not_take_focus() {
    let m = WindowModel {
        title: "t".into(),
        bar: Some(BarModel {
            headline: "Allow clipboard read?".into(),
            detail: Some("title".into()),
            buttons: vec![
                button("Allow once", "O", true, 0.0),
                button("Always for this pane", "A", true, 20.0),
                button("Deny", "D", true, 40.0),
            ],
            bounds: Some(r(0.0)),
            ..BarModel::default()
        }),
        ..WindowModel::default()
    };
    let t = tree(&m);
    let st = t.state();
    assert_eq!(st.focus().expect("focus").locate().0, ids::WINDOW);
    let bar = find(st.root(), ids::BAR).expect("bar");
    assert_eq!(bar.role(), Role::Group);
    assert_eq!(bar.label().as_deref(), Some("Allow clipboard read?"));
    assert_eq!(bar.description().as_deref(), Some("title"));
    assert!(!bar.is_modal());
    assert_eq!(bar.children().count(), 3);
    for (i, key) in ["O", "A", "D"].iter().enumerate() {
        let b = find(st.root(), ids::bar_button(i)).expect("button");
        assert_eq!(b.role(), Role::Button);
        assert!(b.is_disabled(), "guard running");
        assert_eq!(b.data().keyboard_shortcut(), Some(*key));
        assert!(b.data().supports_action(Action::Click));
        assert_eq!(bounds(&b), r(i as f64 * 20.0));
        assert_eq!(ids::target(ids::bar_button(i)), Target::BarButton(i));
    }
}

#[test]
fn guard_end_enables_the_buttons() {
    let mut m = WindowModel {
        bar: Some(BarModel {
            headline: "Open this link?".into(),
            buttons: vec![
                button("Open", "O", true, 0.0),
                button("Cancel", "C", true, 20.0),
            ],
            ..BarModel::default()
        }),
        ..WindowModel::default()
    };
    let mut b = Builder::new(TextModel::Runs, WordChars::new(""));
    let _ = b.full(&m);
    for x in &mut m.bar.as_mut().unwrap().buttons {
        x.disabled = false;
    }
    let u = b.update(&m).expect("changed");
    assert!(u.nodes.iter().any(|(id, _)| *id == ids::bar_button(0)));
    assert!(u.nodes.iter().all(|(_, n)| !n.is_disabled()));
}

#[test]
fn error_bar_is_an_alert() {
    let m = WindowModel {
        bar: Some(BarModel {
            headline: "Config error at a:1".into(),
            alert: true,
            buttons: vec![button("Dismiss", "Esc", false, 0.0)],
            ..BarModel::default()
        }),
        ..WindowModel::default()
    };
    let t = tree(&m);
    assert_eq!(
        find(t.state().root(), ids::BAR).expect("bar").role(),
        Role::Alert
    );
}

#[test]
fn rule_sheet_is_a_modal_dialog_and_takes_focus() {
    let m = WindowModel {
        bar: Some(BarModel {
            headline: "Allow rule \u{201c}x\u{201d}?".into(),
            detail: Some("Triggered by bell.".into()),
            body: Some("notify hello".into()),
            buttons: vec![
                button("Allow this time", "O", false, 0.0),
                button("Always allow", "A", false, 20.0),
                button("Disable rule", "D", false, 40.0),
            ],
            bounds: Some(r(0.0)),
            body_bounds: Some(r(5.0)),
            ..BarModel::default()
        }),
        ..WindowModel::default()
    };
    let t = tree(&m);
    let st = t.state();
    let sheet = find(st.root(), ids::BAR).expect("sheet");
    assert_eq!(sheet.role(), Role::Dialog);
    assert!(sheet.is_modal());
    assert_eq!(st.focus().expect("focus").locate().0, ids::BAR);
    let body = find(st.root(), ids::BAR_BODY).expect("body");
    assert_eq!(body.value().as_deref(), Some("notify hello"));
    assert!(body.is_read_only());
    assert_eq!(bounds(&body), r(5.0));
    assert_eq!(sheet.children().count(), 4);
    assert!(
        find(st.root(), ids::bar_button(2))
            .expect("button")
            .data()
            .supports_action(Action::Click)
    );
}

fn chip(name: &str, on: bool, x: f64) -> ChipModel {
    ChipModel {
        name: name.into(),
        description: None,
        on,
        bounds: Some(r(x)),
    }
}

fn search(focused: bool) -> SearchModel {
    SearchModel {
        query: "needle".into(),
        placeholder: "Find".into(),
        count: "3/12".into(),
        error: false,
        focused,
        can_step: true,
        chips: vec![
            chip("Case", false, 0.0),
            chip("Regular expression", true, 20.0),
            chip("Whole word", false, 40.0),
            chip("Screen only", true, 60.0),
        ],
        bounds: Some(r(0.0)),
        field: Some(r(1.0)),
        prev: Some(r(80.0)),
        next: Some(r(100.0)),
        close: Some(r(120.0)),
    }
}

#[test]
fn search_bar_has_field_count_chips_and_buttons() {
    let m = WindowModel {
        search: Some(search(true)),
        ..WindowModel::default()
    };
    let t = tree(&m);
    let st = t.state();
    let root = st.root();
    let f = st.focus().expect("focus");
    assert_eq!(f.locate().0, ids::SEARCH_FIELD);
    assert_eq!(f.role(), Role::SearchInput);
    assert_eq!(f.value().as_deref(), Some("needle"));
    assert!(f.data().supports_action(Action::Focus));
    assert_eq!(bounds(&f), r(1.0));
    let c = find(root, ids::SEARCH_COUNT).expect("count");
    assert_eq!(c.value().as_deref(), Some("3/12"));
    for (i, (name, on)) in [
        ("Case", false),
        ("Regular expression", true),
        ("Whole word", false),
        ("Screen only", true),
    ]
    .iter()
    .enumerate()
    {
        let n = find(root, ids::chip(i)).expect("chip");
        assert_eq!(n.role(), Role::Button);
        assert_eq!(n.label().as_deref(), Some(*name));
        assert_eq!(n.data().toggled(), Some(Toggled::from(*on)));
        assert!(n.data().supports_action(Action::Click));
        assert_eq!(bounds(&n), r(i as f64 * 20.0));
        assert_eq!(ids::target(ids::chip(i)), Target::SearchChip(i));
    }
    for (id, label) in [
        (ids::SEARCH_PREV, "Previous match"),
        (ids::SEARCH_NEXT, "Next match"),
        (ids::SEARCH_CLOSE, "Close search"),
    ] {
        let n = find(root, id).expect("button");
        assert_eq!(n.label().as_deref(), Some(label));
        assert!(!n.is_disabled());
        assert!(n.data().supports_action(Action::Click));
    }
    let group = find(root, ids::SEARCH).expect("group");
    assert_eq!(group.children().count(), 2 + 4 + 3);
}

#[test]
fn search_without_matches_disables_stepping_and_flags_errors() {
    let mut s = search(false);
    s.can_step = false;
    s.error = true;
    s.count = "Pattern too long".into();
    let m = WindowModel {
        search: Some(s),
        ..WindowModel::default()
    };
    let t = tree(&m);
    let st = t.state();
    assert_eq!(st.focus().expect("focus").locate().0, ids::WINDOW);
    let root = st.root();
    assert!(find(root, ids::SEARCH_PREV).expect("prev").is_disabled());
    assert!(find(root, ids::SEARCH_NEXT).expect("next").is_disabled());
    assert!(!find(root, ids::SEARCH_CLOSE).expect("close").is_disabled());
    let f = find(root, ids::SEARCH_FIELD).expect("field");
    assert_eq!(f.data().invalid(), Some(accesskit::Invalid::True));
}

#[test]
fn tab_badges_progress_profile_and_buttons() {
    let m = WindowModel {
        strip: Some(r(0.0)),
        new_tab: Some(r(200.0)),
        tabs: vec![
            TabModel {
                id: 1,
                label: "one".into(),
                selected: true,
                description: Some("bell".into()),
                bounds: Some(r(0.0)),
                close: Some(r(5.0)),
                progress: Some(ProgressModel {
                    kind: ProgressKind::Error,
                    value: 42,
                }),
                profile: Some("work".into()),
            },
            TabModel {
                id: 2,
                label: "two".into(),
                progress: Some(ProgressModel {
                    kind: ProgressKind::Busy,
                    value: 0,
                }),
                ..TabModel::default()
            },
        ],
        ..WindowModel::default()
    };
    let t = tree(&m);
    let root = t.state().root();
    let tab = find(root, ids::tab(1)).expect("tab");
    assert_eq!(bounds(&tab), r(0.0));
    assert_eq!(tab.children().count(), 3);
    let p = find(root, ids::tab_progress(1)).expect("progress");
    assert_eq!(p.role(), Role::ProgressIndicator);
    assert_eq!(p.numeric_value(), Some(42.0));
    assert_eq!(p.description().as_deref(), Some("error"));
    let pr = find(root, ids::tab_profile(1)).expect("profile");
    assert_eq!(pr.value().as_deref(), Some("work"));
    let c = find(root, ids::tab_close(1)).expect("close");
    assert_eq!(c.label().as_deref(), Some("Close tab"));
    assert!(c.data().supports_action(Action::Click));
    assert_eq!(ids::target(ids::tab_close(1)), Target::TabClose(1));
    let nt = find(root, ids::NEW_TAB).expect("new tab");
    assert_eq!(nt.label().as_deref(), Some("New tab"));
    assert_eq!(ids::target(ids::NEW_TAB), Target::NewTab);
    // Indeterminate: no value. A tab that did not fit has no bounds and no close button.
    let busy = find(root, ids::tab_progress(2)).expect("busy");
    assert_eq!(busy.numeric_value(), None);
    assert!(find(root, ids::tab_close(2)).is_none());
    assert!(
        find(root, ids::tab(2))
            .expect("tab 2")
            .data()
            .bounds()
            .is_none()
    );
}

#[test]
fn tooltip_is_a_labelled_tooltip_with_bounds() {
    let m = WindowModel {
        tooltip: Some(TooltipModel {
            text: "https://example.org/".into(),
            bounds: Some(r(3.0)),
        }),
        ..WindowModel::default()
    };
    let t = tree(&m);
    let n = find(t.state().root(), ids::TOOLTIP).expect("tooltip");
    assert_eq!(n.role(), Role::Tooltip);
    assert_eq!(n.label().as_deref(), Some("https://example.org/"));
    assert_eq!(bounds(&n), r(3.0));
}

#[test]
fn closed_surfaces_leave_no_nodes() {
    let t = tree(&WindowModel::default());
    let root = t.state().root();
    for id in [ids::BAR, ids::SEARCH, ids::TOOLTIP, ids::NEW_TAB] {
        assert!(find(root, id).is_none());
    }
}
