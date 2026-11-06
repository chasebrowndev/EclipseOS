// SPDX-License-Identifier: AGPL-3.0-only
//! Where each icon sits, per output, and how the desktop sorts.
//!
//! Icons live on a grid of [`Cell`]s. Positions are saved by output name and
//! file name in `$XDG_STATE_HOME/eclipse/desktop-layout` (JSON, written
//! atomically). An icon with no saved cell, or one whose cell is gone (the
//! output shrank) or taken, is placed in the first free cell filling top to
//! bottom, then left to right, in the output's sort order: so a fresh desktop
//! is just the sort, and a dragged icon stays where it was dropped.
//!
//! A corrupt file is a default layout and one warning with no content.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::scan::Entry;

/// One grid position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cell {
    pub col: u32,
    pub row: u32,
}

/// The grid an output offers. `rows` is the fill direction's extent: icons run
/// down a column of `rows` cells, then start the next. Columns past `cols`
/// are placed anyway (the desktop never drops an icon), just off the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Name,
    Kind,
    Modified,
}

impl Sort {
    pub fn as_str(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Kind => "kind",
            Sort::Modified => "modified",
        }
    }

    pub fn parse(s: &str) -> Option<Sort> {
        match s {
            "name" => Some(Sort::Name),
            "kind" => Some(Sort::Kind),
            "modified" => Some(Sort::Modified),
            _ => None,
        }
    }
}

/// Sort `entries` in place. Name: case-insensitive on the label. Kind:
/// folders, launchers, files, links, then by name. Modified: newest first.
/// Ties always break on the file name, so the order is total and stable.
pub fn sort_entries(entries: &mut [Entry], sort: Sort) {
    let by_name = |a: &Entry, b: &Entry| {
        a.label()
            .to_lowercase()
            .cmp(&b.label().to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    };
    match sort {
        Sort::Name => entries.sort_by(by_name),
        Sort::Kind => entries.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| by_name(a, b))),
        Sort::Modified => {
            entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.name.cmp(&b.name)))
        }
    }
}

/// One output's sort and saved cells, by file name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputLayout {
    pub sort: Sort,
    pub cells: BTreeMap<String, Cell>,
}

/// Every output's layout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout {
    pub outputs: BTreeMap<String, OutputLayout>,
}

/// `$XDG_STATE_HOME/eclipse/desktop-layout`, else under `~/.local/state`.
pub fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("eclipse/desktop-layout"))
}

/// The first free cell at or after `from` in fill order, skipping `taken`.
fn first_free(from: Cell, grid: Grid, taken: &BTreeSet<Cell>) -> Cell {
    let rows = grid.rows.max(1);
    let mut i = from.col as u64 * rows as u64 + from.row.min(rows - 1) as u64;
    loop {
        let c = Cell {
            col: (i / rows as u64) as u32,
            row: (i % rows as u64) as u32,
        };
        if !taken.contains(&c) {
            return c;
        }
        i += 1;
    }
}

impl Layout {
    pub fn output(&self, name: &str) -> OutputLayout {
        self.outputs.get(name).cloned().unwrap_or_default()
    }

    /// The cell of every entry on `output`, index-aligned with `entries`.
    /// Saved cells hold (first claimant wins a doubly-saved cell, and a cell
    /// beyond the rows is not held); the rest fill in sort order.
    pub fn arrange(&self, output: &str, entries: &[Entry], grid: Grid) -> Vec<Cell> {
        let out = self.output(output);
        let mut cells: Vec<Option<Cell>> = vec![None; entries.len()];
        let mut taken = BTreeSet::new();
        for (i, e) in entries.iter().enumerate() {
            if let Some(c) = out.cells.get(&e.name) {
                if c.row < grid.rows.max(1) && taken.insert(*c) {
                    cells[i] = Some(*c);
                }
            }
        }
        let mut loose: Vec<Entry> = entries
            .iter()
            .zip(&cells)
            .filter(|(_, c)| c.is_none())
            .map(|(e, _)| e.clone())
            .collect();
        sort_entries(&mut loose, out.sort);
        let index: std::collections::HashMap<&str, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.name.as_str(), i))
            .collect();
        let order: Vec<usize> = loose
            .iter()
            .filter_map(|e| index.get(e.name.as_str()).copied())
            .collect();
        let mut next = Cell { col: 0, row: 0 };
        for i in order {
            let c = first_free(next, grid, &taken);
            taken.insert(c);
            cells[i] = Some(c);
            next = c;
        }
        cells
            .into_iter()
            .map(|c| c.unwrap_or(Cell { col: 0, row: 0 }))
            .collect()
    }

    /// Save the current arrangement of `output` as its cells, so what is on
    /// screen stays put.
    fn commit(&mut self, output: &str, entries: &[Entry], grid: Grid) {
        let cells = self.arrange(output, entries, grid);
        let out = self.outputs.entry(output.to_owned()).or_default();
        out.cells = entries.iter().map(|e| e.name.clone()).zip(cells).collect();
    }

    /// Sort `output` by `sort`: every icon goes to its place in that order.
    pub fn sort(&mut self, output: &str, sort: Sort, entries: &[Entry], grid: Grid) {
        let out = self.outputs.entry(output.to_owned()).or_default();
        out.sort = sort;
        out.cells.clear();
        self.commit(output, entries, grid);
    }

    /// Drop icon `name` on `want` of `output`. A taken cell sends it to the
    /// next free one in fill order, and a cell below the grid is clamped to
    /// its last row. Returns where it landed.
    pub fn place(&mut self, output: &str, name: &str, want: Cell, entries: &[Entry], grid: Grid) -> Cell {
        self.commit(output, entries, grid);
        let out = self.outputs.entry(output.to_owned()).or_default();
        let taken: BTreeSet<Cell> = out
            .cells
            .iter()
            .filter(|(n, _)| n.as_str() != name)
            .map(|(_, c)| *c)
            .collect();
        let want = Cell {
            col: want.col,
            row: want.row.min(grid.rows.max(1) - 1),
        };
        let c = first_free(want, grid, &taken);
        out.cells.insert(name.to_owned(), c);
        c
    }

    /// A file was renamed: its position follows it, on every output.
    pub fn rename(&mut self, old: &str, new: &str) {
        for out in self.outputs.values_mut() {
            if let Some(c) = out.cells.remove(old) {
                out.cells.insert(new.to_owned(), c);
            }
        }
    }

    /// Forget positions of files that are no longer on the desktop.
    pub fn prune(&mut self, entries: &[Entry]) {
        let live: BTreeSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        for out in self.outputs.values_mut() {
            out.cells.retain(|n, _| live.contains(n.as_str()));
        }
    }

    pub fn to_json(&self) -> Value {
        let mut outputs = Map::new();
        for (name, out) in &self.outputs {
            let cells: Map<String, Value> = out
                .cells
                .iter()
                .map(|(n, c)| (n.clone(), json!([c.col, c.row])))
                .collect();
            outputs.insert(name.clone(), json!({ "sort": out.sort.as_str(), "cells": cells }));
        }
        json!({ "version": 1, "outputs": outputs })
    }

    /// Lenient: whatever parses is kept, the rest is dropped.
    pub fn from_json(v: &Value) -> Layout {
        let mut layout = Layout::default();
        let Some(outputs) = v.get("outputs").and_then(Value::as_object) else {
            return layout;
        };
        for (name, o) in outputs {
            let sort = o
                .get("sort")
                .and_then(Value::as_str)
                .and_then(Sort::parse)
                .unwrap_or_default();
            let cells = o
                .get("cells")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(n, c)| {
                    let c = c.as_array()?;
                    let col = u32::try_from(c.first()?.as_u64()?).ok()?;
                    let row = u32::try_from(c.get(1)?.as_u64()?).ok()?;
                    Some((n.clone(), Cell { col, row }))
                })
                .collect();
            layout.outputs.insert(name.clone(), OutputLayout { sort, cells });
        }
        layout
    }

    /// Read `path`. A missing file is the default; a corrupt one is the
    /// default and a warning that says so and nothing else.
    pub fn load(path: &Path) -> Layout {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Layout::default();
        };
        match serde_json::from_str::<Value>(&text) {
            Ok(v) => Layout::from_json(&v),
            Err(_) => {
                eprintln!("ec-desktop: warning: desktop-layout is corrupt; using the default layout");
                Layout::default()
            }
        }
    }

    /// Write `path` atomically: a temporary file beside it, flushed to disk,
    /// then renamed over it, so a crash leaves the old file or the new one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".desktop-layout.{}.tmp", std::process::id()));
        let result = (|| {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(self.to_json().to_string().as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Kind;
    use std::time::{Duration, SystemTime};

    const GRID: Grid = Grid { cols: 6, rows: 3 };

    fn e(name: &str, kind: Kind, age_s: u64) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/d").join(name),
            kind,
            mime: None,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_s)),
            size: 0,
            to_dir: false,
            broken: false,
            launcher: None,
        }
    }

    fn set() -> Vec<Entry> {
        vec![
            e("b.txt", Kind::File, 10),
            e("Apps", Kind::Dir, 30),
            e("a.txt", Kind::File, 20),
            e("z", Kind::Dir, 5),
        ]
    }

    fn names(v: &[Entry]) -> Vec<&str> {
        v.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn the_three_sorts() {
        let mut v = set();
        sort_entries(&mut v, Sort::Name);
        assert_eq!(names(&v), ["a.txt", "Apps", "b.txt", "z"]);
        sort_entries(&mut v, Sort::Kind);
        assert_eq!(names(&v), ["Apps", "z", "a.txt", "b.txt"]);
        sort_entries(&mut v, Sort::Modified);
        assert_eq!(names(&v), ["z", "b.txt", "a.txt", "Apps"]);
    }

    #[test]
    fn a_fresh_desktop_fills_down_then_across_in_sort_order() {
        let l = Layout::default();
        let v = set();
        let cells = l.arrange("DP-1", &v, GRID);
        // Name order: a.txt, Apps, b.txt, z -> (0,0) (0,1) (0,2) (1,0).
        let at = |n: &str| cells[v.iter().position(|e| e.name == n).unwrap()];
        assert_eq!(at("a.txt"), Cell { col: 0, row: 0 });
        assert_eq!(at("Apps"), Cell { col: 0, row: 1 });
        assert_eq!(at("b.txt"), Cell { col: 0, row: 2 });
        assert_eq!(at("z"), Cell { col: 1, row: 0 });
    }

    #[test]
    fn a_dropped_icon_stays_and_others_flow_around_it() {
        let mut l = Layout::default();
        let v = set();
        let landed = l.place("DP-1", "z", Cell { col: 2, row: 1 }, &v, GRID);
        assert_eq!(landed, Cell { col: 2, row: 1 });
        let cells = l.arrange("DP-1", &v, GRID);
        assert_eq!(cells[3], landed);
        // Dropping another on the taken cell moves to the next free one.
        let next = l.place("DP-1", "a.txt", Cell { col: 2, row: 1 }, &v, GRID);
        assert_eq!(next, Cell { col: 2, row: 2 });
        // Below the grid clamps to the last row.
        let clamped = l.place("DP-1", "b.txt", Cell { col: 4, row: 99 }, &v, GRID);
        assert_eq!(clamped, Cell { col: 4, row: 2 });
        // No two icons share a cell.
        let all = l.arrange("DP-1", &v, GRID);
        let uniq: BTreeSet<_> = all.iter().collect();
        assert_eq!(uniq.len(), all.len());
    }

    #[test]
    fn outputs_are_independent() {
        let mut l = Layout::default();
        let v = set();
        l.place("DP-1", "z", Cell { col: 3, row: 0 }, &v, GRID);
        assert_eq!(l.arrange("HDMI-A-1", &v, GRID)[3], Cell { col: 1, row: 0 });
    }

    #[test]
    fn sorting_rearranges_everything_and_is_remembered() {
        let mut l = Layout::default();
        let v = set();
        l.place("DP-1", "z", Cell { col: 3, row: 0 }, &v, GRID);
        l.sort("DP-1", Sort::Kind, &v, GRID);
        assert_eq!(l.output("DP-1").sort, Sort::Kind);
        let cells = l.arrange("DP-1", &v, GRID);
        // Kind order: Apps, z, a.txt, b.txt.
        assert_eq!(cells[1], Cell { col: 0, row: 0 });
        assert_eq!(cells[3], Cell { col: 0, row: 1 });
        assert_eq!(cells[2], Cell { col: 0, row: 2 });
        assert_eq!(cells[0], Cell { col: 1, row: 0 });
    }

    #[test]
    fn a_cell_off_a_shrunk_grid_is_replaced_not_lost() {
        let mut l = Layout::default();
        let v = set();
        l.place("DP-1", "z", Cell { col: 0, row: 2 }, &v, GRID);
        let small = Grid { cols: 6, rows: 2 };
        let cells = l.arrange("DP-1", &v, small);
        assert!(cells.iter().all(|c| c.row < 2));
        let uniq: BTreeSet<_> = cells.iter().collect();
        assert_eq!(uniq.len(), cells.len());
    }

    #[test]
    fn rename_and_prune_follow_the_files() {
        let mut l = Layout::default();
        let v = set();
        l.place("DP-1", "z", Cell { col: 3, row: 0 }, &v, GRID);
        l.rename("z", "y");
        assert_eq!(l.output("DP-1").cells["y"], Cell { col: 3, row: 0 });
        l.prune(&v);
        assert!(!l.output("DP-1").cells.contains_key("y"));
    }

    #[test]
    fn json_round_trips_and_garbage_is_dropped() {
        let mut l = Layout::default();
        let v = set();
        l.sort("DP-1", Sort::Modified, &v, GRID);
        assert_eq!(Layout::from_json(&l.to_json()), l);
        let junk = json!({"outputs": {"X": {"sort": "weird", "cells": {"a": [1], "b": "x", "c": [2, 3]}}}});
        let got = Layout::from_json(&junk).output("X");
        assert_eq!(got.sort, Sort::Name);
        assert_eq!(got.cells.len(), 1);
        assert_eq!(got.cells["c"], Cell { col: 2, row: 3 });
        assert_eq!(Layout::from_json(&json!("nope")), Layout::default());
    }

    #[test]
    fn save_is_atomic_and_load_survives_corruption() {
        let d = crate::tmp("layout");
        let path = d.join("eclipse/desktop-layout");
        assert_eq!(Layout::load(&path), Layout::default());
        let mut l = Layout::default();
        l.place("DP-1", "z", Cell { col: 1, row: 1 }, &set(), GRID);
        l.save(&path).unwrap();
        assert_eq!(Layout::load(&path), l);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers, ["desktop-layout"], "no temp file left behind");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Layout::load(&path), Layout::default());
        let _ = std::fs::remove_dir_all(&d);
    }
}
