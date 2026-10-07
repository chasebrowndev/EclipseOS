// SPDX-License-Identifier: AGPL-3.0-only
//! The personal secret phrase (COMP-10 §2, DA-03). **TCB.**
//!
//! Every genuine prompt draws the phrase in its fixed frame; a client cannot,
//! because nothing outside the compositor knows it. So the phrase must never
//! leave abyss:
//!
//! - **Storage.** `$XDG_CONFIG_HOME/eclipse/phrase`, mode 0600, written
//!   atomically. It is a file of its own, not a key in `abyss.kdl`, so the
//!   config RPC (`get_config`, the `CONFIG_FILES` gate table) cannot read it
//!   back to any client. A settings client learns only whether one is set,
//!   from the `phrase {set}` event.
//! - **No logging.** [`Phrase`] and the typed text ([`super::modal::Typed`])
//!   print as lengths only. Nothing here logs the text.
//! - **Entry is compositor-drawn.** The `agent-attention` chord (SUPER+space)
//!   opens phrase entry while none is set. The entry screen says in fixed
//!   text that it only ever appears after that chord, because it cannot show
//!   a phrase yet: a screen asking for one unprompted is a fake.
//!
//! Rules: 4 to 48 printable ASCII characters, trimmed, rendered verbatim (§2,
//! "length-clamped and rendered verbatim; no markup"). The panel's font is
//! printable ASCII, so nothing else could be drawn.
//!
//! Changing a phrase that is set needs the human seat and a prompt (§2): it
//! is offered only from the emergency panel, opened by the human-only
//! override chord, and asks for confirmation on a prompt that itself shows
//! the current phrase before the entry screen comes up.

use std::{io::Write, path::PathBuf};

use super::{
    modal::{Button, Modal, Role},
    Choice,
};
use crate::state::AbyssState;

/// The token phrase entry is drawn with. Below `erase::TOKEN_BASE`, so the
/// pointer may answer it.
pub(super) const TOKEN: u64 = 1 << 61;
/// The change confirmation.
const CONFIRM: u64 = TOKEN | 1;

pub const MIN: usize = 4;
pub const MAX: usize = 48;

/// The personal secret. `Debug` prints only its length.
#[derive(Clone, PartialEq, Eq)]
pub struct Phrase(String);

impl Phrase {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Phrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Phrase(<{} chars>)", self.0.chars().count())
    }
}

/// Why a phrase was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    Short,
    Long,
    /// A character the trusted font cannot draw.
    Unprintable,
}

/// Validate typed or stored text as a phrase: trimmed, 4 to 48 printable
/// ASCII characters.
pub fn validate(text: &str) -> Result<Phrase, Refused> {
    let t = text.trim();
    if !t.chars().all(|c| (' '..='~').contains(&c)) {
        return Err(Refused::Unprintable);
    }
    match t.chars().count() {
        n if n < MIN => Err(Refused::Short),
        n if n > MAX => Err(Refused::Long),
        _ => Ok(Phrase(t.to_owned())),
    }
}

/// Phrase entry's state: whether it is up and whether the last try was
/// refused, so the reopened prompt can say why.
#[derive(Debug, Default)]
pub(super) struct Entering {
    up: bool,
    refused: Option<Refused>,
    /// The entry replaces a phrase that is set (confirmed from the panel).
    replace: bool,
    confirming: bool,
}

fn path() -> Option<PathBuf> {
    ec_abyss_config::user_config_base().map(|b| b.join("eclipse").join("phrase"))
}

/// Read the stored phrase at startup. A missing, unreadable or invalid file
/// is no phrase, and prompts then say anti-spoofing is unconfigured.
pub fn load() -> Option<Phrase> {
    let p = path()?;
    let text = std::fs::read_to_string(&p).ok()?;
    match validate(&text) {
        Ok(phrase) => Some(phrase),
        Err(why) => {
            tracing::warn!(?why, path = %p.display(), "stored personal secret is invalid; ignored");
            None
        }
    }
}

/// Write the phrase: a 0600 temporary file in the same directory, renamed
/// over the old one, so a crash leaves either the old phrase or the new.
fn store(phrase: &Phrase) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    // Tests never touch a real config directory.
    if cfg!(test) {
        return Ok(());
    }
    let p = path().ok_or_else(|| std::io::Error::other("no config directory"))?;
    let dir = p.parent().ok_or_else(|| std::io::Error::other("no parent"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(".phrase.tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(phrase.as_str().as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    std::fs::rename(&tmp, &p)
}

const HEADING: &str = "Set your personal secret";
const BODY: &str = "This screen only ever appears after you press Super+Space, or choose \
Change secret phrase in the emergency panel. \
If it appears any other way, it is not EclipseOS: do not type here. \
Every genuine EclipseOS prompt will show this phrase at the top. \
Pick something you will recognise. It is not a password: it shows whenever \
a prompt does, including while sharing your screen, so do not reuse one.";
const WELL: &str = "Rules:";
const RULES: &str = "4 to 48 characters: letters, digits, spaces and punctuation.";
const SHORT: &str = "That was too short. Use at least 4 characters.";
const LONG: &str = "That was too long. Use at most 48 characters.";
const UNPRINTABLE: &str = "Use only letters, digits, spaces and punctuation.";

fn modal(refused: Option<Refused>) -> Option<Modal> {
    let warning = refused.map(|r| match r {
        Refused::Short => SHORT,
        Refused::Long => LONG,
        Refused::Unprintable => UNPRINTABLE,
    });
    Modal::new(
        TOKEN,
        HEADING,
        warning,
        BODY,
        WELL,
        RULES,
        vec![
            Button {
                label: "Later",
                role: Role::Safe,
            },
            // Setting a phrase grants nothing, so Enter may submit it.
            Button {
                label: "Set phrase",
                role: Role::Other,
            },
        ],
    )
    .ok()
    .map(|m| m.with_entry(MAX))
}

/// The `agent-attention` chord with no phrase set: open entry. Returns
/// whether entry was opened (or is already up), so the caller does not also
/// open its normal target.
pub fn prompt_if_unset(state: &mut AbyssState) -> bool {
    if state.trusted_ui.phrase.is_some() {
        return false;
    }
    open_entry(state)
}

fn open_entry(state: &mut AbyssState) -> bool {
    if state.trusted_ui.entering.up {
        return true;
    }
    let Some(m) = modal(state.trusted_ui.entering.refused) else {
        return false;
    };
    if super::open(state, m) {
        state.trusted_ui.entering.up = true;
        true
    } else {
        // Another prompt holds the seat; the human presses the chord again.
        false
    }
}

const CHANGE_HEADING: &str = "Change your personal secret?";
const CHANGE_BODY: &str =
    "The phrase at the top of this prompt is the one in use. Change it only from here: \
EclipseOS never asks for it anywhere else.";

/// The panel's "Change secret phrase": confirm first, then entry. With no
/// phrase set this is plain first entry.
pub fn prompt_change(state: &mut AbyssState) -> bool {
    if state.trusted_ui.phrase.is_none() {
        return prompt_if_unset(state);
    }
    let m = Modal::new(
        CONFIRM,
        CHANGE_HEADING,
        None,
        CHANGE_BODY,
        "Next:",
        "A new phrase replaces this one on every prompt.",
        vec![
            Button {
                label: "Keep it",
                role: Role::Safe,
            },
            Button {
                label: "Change it",
                role: Role::Other,
            },
        ],
    );
    let Ok(m) = m else { return false };
    if super::open(state, m) {
        state.trusted_ui.entering.confirming = true;
        true
    } else {
        false
    }
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    (token == TOKEN && state.trusted_ui.entering.up)
        || (token == CONFIRM && state.trusted_ui.entering.confirming)
}

pub fn answer(state: &mut AbyssState, choice: Choice) {
    if choice.token == CONFIRM {
        state.trusted_ui.entering.confirming = false;
        if choice.role == Role::Other {
            state.trusted_ui.entering.replace = true;
            open_entry(state);
        }
        return;
    }
    state.trusted_ui.entering.up = false;
    if choice.role == Role::Safe {
        state.trusted_ui.entering.refused = None;
        state.trusted_ui.entering.replace = false;
        return;
    }
    if state.trusted_ui.phrase.is_some() && !state.trusted_ui.entering.replace {
        // Only a confirmed change replaces a set phrase.
        return;
    }
    let typed = choice.typed.as_ref().map(|t| t.as_str()).unwrap_or("");
    match validate(typed) {
        Ok(phrase) => {
            if let Err(e) = store(&phrase) {
                // Still used for this session, and every prompt shows it; the
                // owner sets it again after the next start.
                tracing::error!(error = %e, "storing the personal secret failed; kept for this session only");
            }
            tracing::info!("personal secret set");
            state.trusted_ui.entering.refused = None;
            state.trusted_ui.entering.replace = false;
            state.trusted_ui.phrase = Some(phrase);
            super::commit::phrase_changed(state);
            crate::ipc::emit(state, "phrase", serde_json::json!({ "set": true }));
        }
        Err(why) => {
            state.trusted_ui.entering.refused = Some(why);
            open_entry(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phrase_is_four_to_forty_eight_printable_characters() {
        assert_eq!(validate("abc"), Err(Refused::Short));
        assert_eq!(validate("   ab   "), Err(Refused::Short));
        assert_eq!(validate(&"x".repeat(49)), Err(Refused::Long));
        assert_eq!(validate("blue\u{202e}heron"), Err(Refused::Unprintable));
        assert_eq!(validate("tab\there"), Err(Refused::Unprintable));
        assert_eq!(validate("  blue heron 42!  ").unwrap().as_str(), "blue heron 42!");
        assert!(validate(&"x".repeat(48)).is_ok());
    }

    #[test]
    fn neither_the_phrase_nor_the_typed_text_is_ever_printed() {
        let p = validate("blue heron").unwrap();
        let shown = format!("{p:?}");
        assert!(!shown.contains("heron"), "{shown}");
        let mut m = modal(None).unwrap();
        for c in "blue heron".chars() {
            super::super::modal::apply(&mut m, 1, super::super::modal::Key::Char(c));
        }
        assert_eq!(m.typed().unwrap().as_str(), "blue heron");
        assert!(!format!("{m:?}").contains("heron"));
        assert!(!format!("{:?}", super::super::modal::Key::Char('z')).contains('z'));
    }

    #[test]
    fn entry_takes_space_as_text_and_stops_at_the_limit() {
        use super::super::modal::{apply, key, Outcome};
        use smithay::input::keyboard::Keysym;
        let mut m = modal(None).unwrap();
        let k = key(Keysym::space, m.takes_text());
        assert_eq!(apply(&mut m, 1, k), Outcome::Edited);
        for _ in 0..MAX + 5 {
            apply(&mut m, 1, key(Keysym::a, true));
        }
        assert_eq!(m.typed().unwrap().as_str().chars().count(), MAX);
        assert_eq!(apply(&mut m, 1, key(Keysym::BackSpace, true)), Outcome::Edited);
        // Enter submits: setting a phrase grants nothing.
        assert_eq!(apply(&mut m, 1, key(Keysym::Return, true)), Outcome::Choose(1));
        // Escape is Later, from anywhere.
        assert_eq!(apply(&mut m, 1, key(Keysym::Escape, true)), Outcome::Choose(0));
    }

    #[test]
    fn the_entry_screen_says_it_only_follows_the_chord() {
        assert!(BODY.contains("only ever appears after you press Super+Space"));
    }

    fn type_in(s: &mut crate::state::AbyssState, text: &str) {
        use smithay::input::keyboard::Keysym;
        super::super::arm_now(s);
        for c in text.chars() {
            super::super::key(s, Keysym::from_char(c));
        }
        // Focus starts on Later, the safe button; Tab to Set phrase.
        super::super::key(s, Keysym::Tab);
        super::super::key(s, Keysym::Return);
    }

    #[test]
    fn a_set_phrase_changes_only_through_the_confirmation() {
        use smithay::input::keyboard::Keysym;
        let mut h = crate::shell::focus::state_tests::harness();
        let s = &mut h.state;
        s.trusted_ui.phrase = Some(validate("old heron").unwrap());
        // First entry is not offered once a phrase is set.
        assert!(!prompt_if_unset(s));
        // The panel's change: confirm first; Escape keeps the phrase.
        assert!(prompt_change(s));
        super::super::key(s, Keysym::Escape);
        assert_eq!(s.trusted_ui.phrase.as_ref().unwrap().as_str(), "old heron");
        assert!(!s.trusted_ui.is_open());
        // Confirm, then type a new one.
        assert!(prompt_change(s));
        super::super::arm_now(s);
        super::super::key(s, Keysym::Tab);
        super::super::key(s, Keysym::space);
        assert_eq!(
            s.trusted_ui.token(),
            Some(TOKEN),
            "entry follows the confirmation"
        );
        type_in(s, "new egret");
        assert_eq!(s.trusted_ui.phrase.as_ref().unwrap().as_str(), "new egret");
    }
}
