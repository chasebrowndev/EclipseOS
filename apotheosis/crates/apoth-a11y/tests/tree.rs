// SPDX-License-Identifier: AGPL-3.0-only
//! Tree-builder tests (T-20 §8): snapshot to nodes, offsets and extents, read back through
//! `accesskit_consumer` (the same model the AT-SPI adapter serves).

use accesskit::{NodeId, Role};
use accesskit_consumer::{NodeRef, Tree, TreeChangeHandler};
use apoth_a11y::{
    Builder, PaletteModel, PaneModel, RowText, TabModel, TextModel, WindowModel, ids,
};
use apoth_grid::WordChars;

fn pane(id: u64, rows: &[(&str, bool)], cursor: (usize, usize), focused: bool) -> PaneModel {
    PaneModel {
        id,
        label: format!("pane {id}"),
        focused,
        origin: (10.0, 20.0),
        cell: (8.0, 16.0),
        cols: 80,
        rows: rows.iter().map(|(s, w)| RowText::plain(s, *w)).collect(),
        cursor: Some(cursor),
        credential: false,
    }
}

fn window(panes: Vec<PaneModel>) -> WindowModel {
    WindowModel {
        title: "shell".into(),
        tabs: vec![
            TabModel {
                id: 1,
                label: "one".into(),
                selected: true,
                description: None,
            },
            TabModel {
                id: 2,
                label: "two".into(),
                selected: false,
                description: Some("bell".into()),
            },
        ],
        panes,
        palette: None,
        announcement: String::new(),
    }
}

fn builder(m: TextModel) -> Builder {
    Builder::new(m, WordChars::new(""))
}

fn find<'a>(n: NodeRef<'a>, id: NodeId) -> Option<NodeRef<'a>> {
    if n.locate().0 == id {
        return Some(n);
    }
    n.children().find_map(|c| find(c, id))
}

#[test]
fn terminal_text_joins_wrapped_rows_and_ends_hard_lines() {
    let m = window(vec![pane(
        7,
        &[
            ("$ echo hello wor", true),
            ("ld", false),
            ("hello world", false),
            ("$", false),
        ],
        (3, 2),
        true,
    )]);
    let mut b = builder(TextModel::Runs);
    let tree = Tree::new(b.full(&m), true);
    let st = tree.state();
    let focus = st.focus().expect("focus");
    assert_eq!(focus.role(), Role::Terminal);
    assert!(focus.supports_text_ranges());
    assert_eq!(
        focus.document_range().text(),
        "$ echo hello world\nhello world\n$"
    );
    // The caret sits after "$ " on the last row: scalar offset 18+1+11+1+1.
    let caret = focus.text_selection_focus().expect("caret");
    assert_eq!(caret.to_global_usv_index(), 32);
    // One line per visible row, wrapped rows included.
    assert_eq!(
        focus.line_range_from_index(1).map(|r| r.text()).as_deref(),
        Some("ld\n")
    );
}

#[test]
fn wide_and_combining_text_has_cell_extents() {
    let mut row = RowText::new(false);
    row.push(0, "a", 1);
    row.push(1, "界", 2);
    row.push(3, "e\u{301}", 1);
    let mut p = pane(3, &[], (0, 3), true);
    p.rows = vec![row];
    let mut b = builder(TextModel::Runs);
    let u = b.full(&window(vec![p]));
    let (_, run) = u
        .nodes
        .iter()
        .find(|(id, _)| *id == ids::run(3, 0, 0))
        .expect("run");
    assert_eq!(run.value(), Some("a界e\u{301}"));
    assert_eq!(run.character_lengths(), &[1, 3, 3]);
    assert_eq!(run.character_positions(), Some(&[0.0f32, 8.0, 24.0][..]));
    assert_eq!(run.character_widths(), Some(&[8.0f32, 16.0, 8.0][..]));
    let r = run.bounds().expect("bounds");
    assert_eq!((r.x0, r.y0, r.x1, r.y1), (10.0, 20.0, 42.0, 36.0));
    // Caret on the combining cell is grapheme 2.
    let tree = Tree::new(u, true);
    let f = tree.state().focus().expect("focus");
    assert_eq!(
        f.text_selection_focus().map(|p| p.to_global_usv_index()),
        Some(2)
    );
}

#[test]
fn long_rows_split_into_runs_on_one_line() {
    let long: String = "x".repeat(300);
    let m = window(vec![pane(
        1,
        &[(&long, false), ("y", false)],
        (0, 290),
        true,
    )]);
    let mut b = builder(TextModel::Runs);
    let tree = Tree::new(b.full(&m), true);
    let f = tree.state().focus().expect("focus");
    assert_eq!(
        f.line_range_from_index(0).map(|r| r.text()),
        Some(format!("{long}\n"))
    );
    assert_eq!(
        f.text_selection_focus().map(|p| p.to_global_usv_index()),
        Some(290)
    );
}

#[test]
fn words_match_double_click_rules() {
    let m = window(vec![pane(1, &[("ls -la foo.txt", false)], (0, 0), true)]);
    let mut b = builder(TextModel::Runs);
    let tree = Tree::new(b.full(&m), true);
    let f = tree.state().focus().expect("focus");
    let mut p = f.document_range().start();
    let mut starts = vec![p.to_global_usv_index()];
    loop {
        let n = p.forward_to_word_start();
        if n.is_document_end() || n.to_global_usv_index() == p.to_global_usv_index() {
            break;
        }
        starts.push(n.to_global_usv_index());
        p = n;
    }
    // "ls ", "-", "la ", "foo", ".", "txt" (the selection classes: '-' and '.' are other).
    assert_eq!(starts, [0, 3, 4, 7, 10, 11]);
}

#[derive(Default)]
struct Count {
    added: usize,
    updated: Vec<NodeId>,
    removed: usize,
}

impl TreeChangeHandler for Count {
    fn node_added(&mut self, _: &NodeRef) {
        self.added += 1;
    }
    fn node_updated(&mut self, _: &NodeRef, new: &NodeRef) {
        self.updated.push(new.locate().0);
    }
    fn focus_moved(&mut self, _: Option<&NodeRef>, _: Option<&NodeRef>) {}
    fn node_removed(&mut self, _: &NodeRef) {
        self.removed += 1;
    }
}

#[test]
fn updates_carry_only_the_changed_rows() {
    let rows = [("$ ls", false), ("", false), ("", false)];
    let mut m = window(vec![pane(1, &rows, (0, 4), true)]);
    let mut b = builder(TextModel::Runs);
    let mut tree = Tree::new(b.full(&m), true);
    assert!(b.update(&m).is_none(), "nothing changed");

    // Output on row 1, cursor unchanged: exactly that run is sent.
    m.panes[0].rows[1] = RowText::plain("a.txt", false);
    let u = b.update(&m).expect("update");
    assert_eq!(u.nodes.len(), 1);
    assert_eq!(u.nodes[0].0, ids::run(1, 1, 0));
    let mut c = Count::default();
    tree.update_and_process_changes(u, &mut c);
    assert_eq!((c.added, c.removed), (0, 0));
    assert_eq!(c.updated, [ids::run(1, 1, 0)]);

    // A caret move alone updates only the terminal node.
    m.panes[0].cursor = Some((2, 0));
    let u = b.update(&m).expect("update");
    assert_eq!(u.nodes.len(), 1);
    assert_eq!(u.nodes[0].0, ids::terminal(1));
}

#[test]
fn line_model_puts_focus_on_the_cursor_row() {
    let m = window(vec![pane(
        4,
        &[("one", false), ("two", false)],
        (1, 1),
        true,
    )]);
    let mut b = builder(TextModel::Lines);
    let tree = Tree::new(b.full(&m), true);
    let f = tree.state().focus().expect("focus");
    assert_eq!(f.role(), Role::Label);
    assert_eq!(f.value().as_deref(), Some("two"));
    assert!(f.is_read_only());
    assert_eq!(f.parent().map(|p| p.role()), Some(Role::List));
}

#[test]
fn tabs_palette_and_toast_have_standard_roles() {
    let mut m = window(vec![pane(1, &[("x", false)], (0, 0), true)]);
    m.palette = Some(PaletteModel {
        query: "spl".into(),
        placeholder: "Run an action".into(),
        rows: vec!["Split right".into(), "Split down".into()],
        selected: Some(1),
    });
    m.announcement = "Bell".into();
    let mut b = builder(TextModel::Runs);
    let tree = Tree::new(b.full(&m), true);
    let st = tree.state();
    // The field has focus; its active descendant (the highlighted row) is what is read.
    let f = st.focus().expect("focus");
    assert_eq!(f.role(), Role::ListBoxOption);
    assert_eq!(f.label().as_deref(), Some("Split down"));
    let root = st.root();
    let input = find(root, ids::PALETTE_INPUT).expect("input");
    assert_eq!(input.role(), Role::SearchInput);
    assert_eq!(input.value().as_deref(), Some("spl"));
    let tab2 = find(root, ids::tab(2)).expect("tab");
    assert_eq!(tab2.role(), Role::Tab);
    assert!(!tab2.is_selected().unwrap_or(true));
    assert_eq!(tab2.description().as_deref(), Some("bell"));
    let row = find(root, ids::palette_row(1)).expect("row");
    assert_eq!(row.is_selected(), Some(true));
    assert_eq!(row.label().as_deref(), Some("Split down"));
    let toast = find(root, ids::TOAST).expect("toast");
    assert_eq!(toast.label().as_deref(), Some("Bell"));
    assert!(toast.live() != accesskit::Live::Off);
}

#[test]
fn credential_state_is_protected() {
    let mut p = pane(1, &[("Password:", false)], (0, 9), true);
    p.credential = true;
    let mut b = builder(TextModel::Runs);
    let tree = Tree::new(b.full(&window(vec![p])), true);
    let f = tree.state().focus().expect("focus");
    assert!(f.is_password_input());
}
