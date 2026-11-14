// SPDX-License-Identifier: AGPL-3.0-only
//! The tree builder (T-20 §2): a pure function of the interface model and pane text
//! snapshots to AccessKit [`TreeUpdate`]s. It knows nothing about D-Bus.
//!
//! Two text models sit behind one switch (T-20 §8): `Runs` (a `Terminal` node of text runs,
//! one line per visible row, with a caret) and the `Lines` fallback (a `List` of read-only
//! static-text rows, focus and caret by line).

use std::collections::HashMap;

use accesskit::{
    Action, Live, Node, NodeId, Rect, Role, TextPosition, TextSelection, TreeId, TreeInfo,
    TreeUpdate,
};
use apoth_grid::{WordChars, WordClass};

use crate::ids;
use crate::text::{RowText, word_starts};

/// Graphemes per text run: `character_lengths` and `word_starts` index with a byte, and the
/// line break needs one slot.
pub const RUN_GRAPHEMES: usize = 254;

/// Which text model panes use (T-20 §8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextModel {
    /// One text run per visible row (or per 254 graphemes of it), with a caret.
    #[default]
    Runs,
    /// One read-only static-text node per visible row inside a list.
    Lines,
}

/// A tab of the window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabModel {
    /// Mux tab id.
    pub id: u64,
    /// Shown title.
    pub label: String,
    /// The active tab.
    pub selected: bool,
    /// Badges (bell, activity) as words.
    pub description: Option<String>,
}

/// A pane of the shown tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneModel {
    /// Mux pane id.
    pub id: u64,
    /// Pane title.
    pub label: String,
    /// Has keyboard focus within the window.
    pub focused: bool,
    /// Grid origin, window-relative.
    pub origin: (f64, f64),
    /// Cell size in the same units.
    pub cell: (f64, f64),
    /// Grid columns.
    pub cols: u16,
    /// Visible rows, top to bottom.
    pub rows: Vec<RowText>,
    /// Cursor (row, column) when visible.
    pub cursor: Option<(usize, usize)>,
    /// The credential state (T-17 §5): exposed as protected.
    pub credential: bool,
}

/// The open command palette.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaletteModel {
    /// What was typed.
    pub query: String,
    /// Placeholder of the empty field.
    pub placeholder: String,
    /// Result titles in view.
    pub rows: Vec<String>,
    /// The highlighted row.
    pub selected: Option<usize>,
}

/// Everything the tree shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowModel {
    /// Window title.
    pub title: String,
    /// Tabs in strip order.
    pub tabs: Vec<TabModel>,
    /// The shown tab's panes.
    pub panes: Vec<PaneModel>,
    /// The palette, when open (it takes focus).
    pub palette: Option<PaletteModel>,
    /// The latest polite announcement (bell, notification, command finished).
    pub announcement: String,
}

/// Builds full and incremental updates; remembers what it last sent.
#[derive(Debug)]
pub struct Builder {
    model: TextModel,
    words: WordChars,
    prev: HashMap<NodeId, Node>,
    focus: Option<NodeId>,
}

impl Builder {
    /// A builder for `model` with selection word rules `words`.
    #[must_use]
    pub fn new(model: TextModel, words: WordChars) -> Builder {
        Builder {
            model,
            words,
            prev: HashMap::new(),
            focus: None,
        }
    }

    /// The text model in use.
    #[must_use]
    pub fn text_model(&self) -> TextModel {
        self.model
    }

    /// Switch the text model or word rules; the next update is full.
    pub fn configure(&mut self, model: TextModel, words: WordChars) {
        self.model = model;
        self.words = words;
        self.reset();
    }

    /// Forget what was sent (the adapter deactivated).
    pub fn reset(&mut self) {
        self.prev.clear();
        self.focus = None;
    }

    /// A full tree (initial or after a reset).
    pub fn full(&mut self, m: &WindowModel) -> TreeUpdate {
        let (nodes, focus) = self.build(m);
        self.prev = nodes.iter().cloned().collect();
        self.focus = Some(focus);
        TreeUpdate {
            nodes,
            tree: Some(TreeInfo {
                root: ids::WINDOW,
                toolkit_name: Some("apotheosis".into()),
                toolkit_version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
            tree_id: TreeId::ROOT,
            focus,
        }
    }

    /// Only the nodes that changed since the last update, or `None` when nothing did.
    /// A builder that has sent nothing yet returns a full tree.
    pub fn update(&mut self, m: &WindowModel) -> Option<TreeUpdate> {
        if self.prev.is_empty() {
            return Some(self.full(m));
        }
        let (nodes, focus) = self.build(m);
        let mut changed = Vec::new();
        let mut next = HashMap::with_capacity(nodes.len());
        for (id, n) in nodes {
            if self.prev.get(&id) != Some(&n) {
                changed.push((id, n.clone()));
            }
            next.insert(id, n);
        }
        self.prev = next;
        let moved = self.focus != Some(focus);
        self.focus = Some(focus);
        if changed.is_empty() && !moved {
            return None;
        }
        Some(TreeUpdate {
            nodes: changed,
            tree: None,
            tree_id: TreeId::ROOT,
            focus,
        })
    }

    fn build(&self, m: &WindowModel) -> (Vec<(NodeId, Node)>, NodeId) {
        let mut out = Vec::new();
        let mut focus = ids::WINDOW;
        let mut win = Node::new(Role::Window);
        win.set_label(m.title.as_str());
        let mut kids = vec![ids::TAB_LIST];

        let mut list = Node::new(Role::TabList);
        list.set_label("Tabs");
        let mut tabs = Vec::with_capacity(m.tabs.len());
        for t in &m.tabs {
            let id = ids::tab(t.id);
            tabs.push(id);
            let mut n = Node::new(Role::Tab);
            n.set_label(t.label.as_str());
            n.set_selected(t.selected);
            if let Some(d) = &t.description {
                n.set_description(d.as_str());
            }
            n.add_action(Action::Click);
            n.add_action(Action::Focus);
            out.push((id, n));
        }
        list.set_children(tabs);
        out.push((ids::TAB_LIST, list));

        for p in &m.panes {
            kids.push(ids::pane(p.id));
            if let Some(f) = self.pane(p, &mut out) {
                focus = f;
            }
        }

        if let Some(pal) = &m.palette {
            kids.push(ids::PALETTE);
            focus = ids::PALETTE_INPUT;
            palette(pal, &mut out);
        }

        kids.push(ids::TOAST);
        let mut toast = Node::new(Role::Status);
        toast.set_live(Live::Polite);
        toast.set_label(m.announcement.as_str());
        out.push((ids::TOAST, toast));

        win.set_children(kids);
        out.push((ids::WINDOW, win));
        (out, focus)
    }

    /// A pane's nodes; returns the node that has focus when the pane does.
    fn pane(&self, p: &PaneModel, out: &mut Vec<(NodeId, Node)>) -> Option<NodeId> {
        let (cw, ch) = p.cell;
        let (ox, oy) = p.origin;
        let rows = p.rows.len();
        let bounds = Rect {
            x0: ox,
            y0: oy,
            x1: ox + f64::from(p.cols) * cw,
            y1: oy + rows as f64 * ch,
        };
        let mut group = Node::new(Role::Group);
        group.set_label(p.label.as_str());
        group.set_bounds(bounds);
        group.add_action(Action::Focus);
        let mut focus = None;
        match self.model {
            TextModel::Runs => {
                let tid = ids::terminal(p.id);
                group.set_children(vec![tid]);
                let mut term = Node::new(if p.credential {
                    Role::PasswordInput
                } else {
                    Role::Terminal
                });
                term.set_label(p.label.as_str());
                term.set_bounds(bounds);
                term.add_action(Action::Focus);
                let mut runs = Vec::new();
                let mut before: Option<WordClass> = None;
                for (y, row) in p.rows.iter().enumerate() {
                    let last = y + 1 == rows;
                    let classes = row.classes(&self.words);
                    let starts = word_starts(&classes, before);
                    before = if row.wrapped {
                        Some(classes.last().copied().unwrap_or(WordClass::Space))
                    } else {
                        None
                    };
                    self.row_runs(p, y, row, last, &starts, &mut runs, out);
                }
                if let Some(sel) = caret(p) {
                    term.set_text_selection(sel);
                }
                term.set_children(runs);
                out.push((tid, term));
                if p.focused {
                    focus = Some(tid);
                }
            }
            TextModel::Lines => {
                let mut kids = Vec::with_capacity(rows);
                for (y, row) in p.rows.iter().enumerate() {
                    let id = ids::line(p.id, y);
                    kids.push(id);
                    let mut n = Node::new(Role::Label);
                    n.set_value(row.text.as_str());
                    n.set_read_only();
                    n.set_bounds(Rect {
                        x0: ox,
                        y0: oy + y as f64 * ch,
                        x1: ox + f64::from(p.cols) * cw,
                        y1: oy + (y + 1) as f64 * ch,
                    });
                    n.add_action(Action::Focus);
                    out.push((id, n));
                }
                let lid = ids::terminal(p.id);
                let mut list = Node::new(Role::List);
                list.set_label(p.label.as_str());
                if p.credential {
                    list.set_description("password entry");
                }
                list.set_bounds(bounds);
                list.set_children(kids);
                out.push((lid, list));
                group.set_children(vec![lid]);
                if p.focused {
                    let row = p.cursor.map_or(0, |c| c.0).min(rows.saturating_sub(1));
                    focus = Some(if rows == 0 { lid } else { ids::line(p.id, row) });
                }
            }
        }
        out.push((ids::pane(p.id), group));
        focus
    }

    #[allow(clippy::too_many_arguments)]
    fn row_runs(
        &self,
        p: &PaneModel,
        y: usize,
        row: &RowText,
        last: bool,
        starts: &[usize],
        runs: &mut Vec<NodeId>,
        out: &mut Vec<(NodeId, Node)>,
    ) {
        let (cw, ch) = p.cell;
        let (ox, oy) = p.origin;
        let n = chunks(row.len());
        // Byte offset of each grapheme.
        let mut byte = Vec::with_capacity(row.len() + 1);
        let mut at = 0usize;
        for g in &row.graphemes {
            byte.push(at);
            at += usize::from(g.bytes);
        }
        byte.push(at);
        for k in 0..n {
            let a = (k * RUN_GRAPHEMES).min(row.len());
            let b = ((k + 1) * RUN_GRAPHEMES).min(row.len());
            let gs = &row.graphemes[a..b];
            let col0 = gs.first().map_or(0, |g| usize::from(g.col));
            let col_end = gs
                .last()
                .map_or(col0, |g| usize::from(g.col) + usize::from(g.width));
            let mut value = row.text[byte[a]..byte[b]].to_owned();
            let mut lens: Vec<u8> = gs.iter().map(|g| g.bytes).collect();
            let mut pos: Vec<f32> = gs
                .iter()
                .map(|g| ((usize::from(g.col) - col0) as f64 * cw) as f32)
                .collect();
            let mut wid: Vec<f32> = gs
                .iter()
                .map(|g| (f64::from(g.width) * cw) as f32)
                .collect();
            if k + 1 == n && !row.wrapped && !last {
                value.push('\n');
                lens.push(1);
                pos.push(((col_end - col0) as f64 * cw) as f32);
                wid.push(0.0);
            }
            let ws: Vec<u8> = starts
                .iter()
                .filter(|&&s| s >= a && s < b)
                .map(|&s| (s - a) as u8)
                .collect();
            let id = ids::run(p.id, y, k);
            let mut r = Node::new(Role::TextRun);
            r.set_value(value);
            r.set_character_lengths(lens);
            r.set_character_positions(pos);
            r.set_character_widths(wid);
            r.set_word_starts(ws);
            r.set_bounds(Rect {
                x0: ox + col0 as f64 * cw,
                y0: oy + y as f64 * ch,
                x1: ox + col_end as f64 * cw,
                y1: oy + (y + 1) as f64 * ch,
            });
            if k > 0 {
                r.set_previous_on_line(ids::run(p.id, y, k - 1));
            }
            if k + 1 < n {
                r.set_next_on_line(ids::run(p.id, y, k + 1));
            }
            runs.push(id);
            out.push((id, r));
        }
    }
}

/// Text runs a row of `len` graphemes needs (an empty row still has one).
fn chunks(len: usize) -> usize {
    len.div_ceil(RUN_GRAPHEMES).max(1)
}

/// The caret as a degenerate selection at the cursor cell.
fn caret(p: &PaneModel) -> Option<TextSelection> {
    let (y, x) = p.cursor?;
    let row = p.rows.get(y)?;
    let gi = row.index_of_col(x);
    let n = chunks(row.len());
    let k = if gi >= row.len() {
        n - 1
    } else {
        gi / RUN_GRAPHEMES
    };
    let at = TextPosition {
        node: ids::run(p.id, y, k),
        character_index: gi - k * RUN_GRAPHEMES,
    };
    Some(TextSelection {
        anchor: at,
        focus: at,
    })
}

fn palette(p: &PaletteModel, out: &mut Vec<(NodeId, Node)>) {
    let mut sheet = Node::new(Role::Dialog);
    sheet.set_label("Command palette");
    sheet.set_modal();
    sheet.set_children(vec![ids::PALETTE_INPUT, ids::PALETTE_LIST]);
    out.push((ids::PALETTE, sheet));
    let mut input = Node::new(Role::SearchInput);
    input.set_value(p.query.as_str());
    input.set_placeholder(p.placeholder.as_str());
    input.add_action(Action::Focus);
    if let Some(s) = p.selected {
        input.set_active_descendant(ids::palette_row(s));
    }
    out.push((ids::PALETTE_INPUT, input));
    let mut list = Node::new(Role::ListBox);
    list.set_label("Results");
    let mut kids = Vec::with_capacity(p.rows.len());
    for (i, r) in p.rows.iter().enumerate() {
        let id = ids::palette_row(i);
        kids.push(id);
        let mut n = Node::new(Role::ListBoxOption);
        n.set_label(r.as_str());
        n.set_selected(p.selected == Some(i));
        n.add_action(Action::Click);
        out.push((id, n));
    }
    list.set_children(kids);
    out.push((ids::PALETTE_LIST, list));
}
