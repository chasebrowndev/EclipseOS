// SPDX-License-Identifier: AGPL-3.0-only

//! The command palette (Ctrl+P or `:`; FOG §UI and navigation): every
//! built-in action and every custom `action` from fog.kdl, by name, matched
//! fuzzily as you type. Matches keep their fixed order (built-ins in
//! [`Action::ALL`] order, then custom actions as configured); nothing is
//! ranked.

use fog_config::{Action, CustomAction};

use crate::edit::{Edit, LineEdit};
use crate::state::fuzzy;

/// One palette row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Action(Action),
    /// Index into the config's custom actions.
    Custom(usize),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Palette {
    pub line: LineEdit,
    /// Index into [`Self::matches`].
    pub pick: usize,
}

impl Palette {
    pub fn edit(&mut self, e: Edit) {
        self.line.apply(e);
        self.pick = 0;
    }

    /// Rows matching the query. Spaces in the query are ignored, so
    /// `sort size` finds `sort-size`.
    pub fn matches(&self, custom: &[CustomAction]) -> Vec<Item> {
        let q: String = self.line.text().split_whitespace().collect();
        Action::ALL
            .into_iter()
            .filter(|a| fuzzy(&q, a.name().as_bytes()))
            .map(Item::Action)
            .chain(
                custom
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| fuzzy(&q, c.name.as_bytes()))
                    .map(|(i, _)| Item::Custom(i)),
            )
            .collect()
    }

    /// Move the pick through `n` matches, clamped.
    pub fn step(&mut self, delta: isize, n: usize) {
        self.pick = self
            .pick
            .saturating_add_signed(delta)
            .min(n.saturating_sub(1));
    }

    /// The picked row, if anything matches.
    pub fn chosen(&self, custom: &[CustomAction]) -> Option<Item> {
        let m = self.matches(custom);
        m.get(self.pick.min(m.len().checked_sub(1)?)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom() -> Vec<CustomAction> {
        vec![CustomAction {
            name: "Open in Cataclysm".into(),
            key: None,
            run: vec!["cataclysm".into()],
        }]
    }

    #[test]
    fn lists_everything_then_narrows_in_fixed_order() {
        let c = custom();
        let mut p = Palette::default();
        let all = p.matches(&c);
        assert_eq!(all.len(), Action::ALL.len() + 1);
        assert_eq!(all.last(), Some(&Item::Custom(0)));

        for ch in "sort s".chars() {
            p.edit(Edit::Insert(ch.to_string()));
        }
        assert_eq!(
            p.matches(&c),
            [
                Item::Action(Action::SortSize),
                Item::Action(Action::SortReverse),
                Item::Action(Action::SortDirsFirst),
            ]
        );
        let mut p = Palette::default();
        p.edit(Edit::Insert("cata".into()));
        // Case-insensitive, and custom actions match by name.
        assert_eq!(p.matches(&c), [Item::Custom(0)]);
        assert_eq!(p.chosen(&c), Some(Item::Custom(0)));
        p.edit(Edit::Insert("zzz".into()));
        assert_eq!(p.chosen(&c), None);
    }

    #[test]
    fn pick_clamps_and_resets_on_edit() {
        let c = custom();
        let mut p = Palette::default();
        p.step(-1, 5);
        assert_eq!(p.pick, 0);
        p.step(10, 5);
        assert_eq!(p.pick, 4);
        assert_eq!(p.chosen(&c), Some(Item::Action(Action::Top)));
        p.edit(Edit::Insert("t".into()));
        assert_eq!(p.pick, 0);
    }
}
