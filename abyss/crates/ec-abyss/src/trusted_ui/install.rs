// SPDX-License-Identifier: AGPL-3.0-only
//! Agent install review (A-07 §3). **TCB.**
//!
//! `ec-ctl agent install <dir>` reaches [`begin`] over the human socket. abyss
//! asks `policyd`, which validates the manifest (A-07 §7) and answers with
//! what to review. The review is a trusted modal on the human seat: the
//! capability lines with their scopes, what changed since the installed
//! version, what the package does not ask for, and the package's own words
//! (name, justifications) in the untrusted block. Install or Refuse goes back
//! to `policyd`, which stores the approved lines as the install policy.
//!
//! The socket caller learns nothing but that the request was sent: the
//! outcome is the human's, drawn here, and the journal's.

use std::collections::VecDeque;

use ec_policy_eval::cbor::Reader;
use ec_policy_eval::link::ToPolicyd;

use super::modal::{Button, Modal, Role};
use crate::state::AbyssState;

/// Request ids this module sends; above every enforcement and panel id.
pub(crate) const REQ_BASE: u64 = (1 << 62) | (1 << 40);

/// Prompt tokens: below `consent::TOKEN_BASE`, above notices.
const TOKEN_BASE: u64 = (1 << 61) | (1 << 59);

/// Reviews waiting for the seat. More than this and the rest are refused:
/// an install is never urgent.
const MAX_WAITING: usize = 4;

const HEADING: &str = "Install this agent?";
const BODY: &str = "The package asks for the capabilities below. What any of its tasks is granted \
never exceeds what you approve here. Lines marked + are new since the installed version.";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Display {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub previous: Option<String>,
    /// `(line, because, added)`.
    pub caps: Vec<(String, String, bool)>,
    pub removed: Vec<String>,
    pub absent: Vec<String>,
    pub warnings: Vec<String>,
    pub sandbox: Vec<String>,
}

fn texts(r: &mut Reader<'_>) -> Option<Vec<String>> {
    let n = r.array_len().ok()?;
    if n > 256 {
        return None;
    }
    (0..n).map(|_| r.text().ok().map(str::to_owned)).collect()
}

/// policyd's `install_review.display` (`ec-policyd/src/dispatch.rs`).
pub fn decode(b: &[u8]) -> Option<Display> {
    let mut r = Reader::new(b);
    let n = r.map_begin().ok()?;
    let mut d = Display::default();
    for _ in 0..n {
        match r.key().ok()? {
            "absent" => d.absent = texts(&mut r)?,
            "caps" => {
                let k = r.array_len().ok()?;
                if k > 256 {
                    return None;
                }
                for _ in 0..k {
                    let m = r.map_begin().ok()?;
                    let (mut line, mut because, mut added) = (None, None, None);
                    for _ in 0..m {
                        match r.key().ok()? {
                            "added" => added = Some(r.bool().ok()?),
                            "because" => because = Some(r.text().ok()?.to_owned()),
                            "line" => line = Some(r.text().ok()?.to_owned()),
                            _ => return None,
                        }
                    }
                    r.map_end().ok()?;
                    d.caps.push((line?, because?, added?));
                }
            }
            "id" => d.id = r.text().ok()?.to_owned(),
            "name" => d.name = r.text().ok()?.to_owned(),
            "previous" => d.previous = Some(r.text().ok()?.to_owned()),
            "publisher" => d.publisher = r.text().ok()?.to_owned(),
            "removed" => d.removed = texts(&mut r)?,
            "sandbox" => d.sandbox = texts(&mut r)?,
            "version" => d.version = r.text().ok()?.to_owned(),
            "warnings" => d.warnings = texts(&mut r)?,
            _ => return None,
        }
    }
    r.map_end().ok()?;
    r.finish().ok()?;
    Some(d)
}

/// The review modal. `None` when it cannot be drawn whole, which refuses the
/// install: a review that leaves a capability off screen is not one.
pub fn modal(token: u64, d: &Display) -> Option<Modal> {
    let mut untrusted = format!("Name: {}", d.name);
    for (line, because, _) in &d.caps {
        let cap = line.split(' ').next().unwrap_or(line);
        untrusted.push_str(&format!("\n{cap}: {because}"));
    }
    let version = match &d.previous {
        Some(p) => format!("{} (installed: {p})", d.version),
        None => d.version.clone(),
    };
    let mut m = Modal::new(
        token,
        HEADING,
        None,
        BODY,
        "The package says:",
        &untrusted,
        vec![
            Button {
                label: "Install",
                role: Role::Grant,
            },
            Button {
                label: "Refuse",
                role: Role::Safe,
            },
        ],
    )
    .ok()?
    .with_facts(&[
        ("Package", &d.id),
        ("Version", &version),
        ("Publisher", &d.publisher),
    ]);
    if d.caps.is_empty() {
        m = m.with_facts(&[("Asks for", "nothing beyond its own conversation")]);
    }
    for (line, _, added) in &d.caps {
        let shown = if *added { format!("+ {line}") } else { line.clone() };
        m = m.with_whole_fact("Asks for", &shown, 3).ok()?;
    }
    for line in &d.removed {
        m = m.with_whole_fact("No longer", line, 3).ok()?;
    }
    for s in &d.sandbox {
        m = m.with_whole_fact("Sandbox", s, 2).ok()?;
    }
    for w in &d.warnings {
        m = m.with_whole_fact("Would refuse", w, 2).ok()?;
    }
    if !d.absent.is_empty() {
        m = m.with_whole_fact("Does not ask", &d.absent.join(", "), 2).ok()?;
    }
    Some(m)
}

#[derive(Debug)]
struct Waiting {
    review: u64,
    display: Display,
}

#[derive(Debug, Default)]
pub struct Installs {
    next: u64,
    /// Requests sent and not yet finished (`done` or `refused`).
    sent: Vec<u64>,
    waiting: VecDeque<Waiting>,
    /// The review on screen: (token, review id).
    showing: Option<(u64, u64)>,
}

/// From the human socket's `agent_install` (ec-abyss/src/ipc/hooks.rs).
pub fn begin(state: &mut AbyssState, path: &std::path::Path) -> Result<(), &'static str> {
    let path = path.to_str().ok_or("path is not UTF-8")?;
    let i = &mut state.trusted_ui.installs;
    if i.sent.len() >= MAX_WAITING {
        return Err("too many installs waiting for review");
    }
    i.next += 1;
    let req = REQ_BASE | i.next;
    i.sent.push(req);
    crate::policy::link::send(
        state,
        &ToPolicyd::InstallBegin {
            req,
            path: path.to_owned(),
        },
    );
    Ok(())
}

/// Whether `done`/`refused` for `req` is an install's answer.
pub fn owns_req(state: &AbyssState, req: u64) -> bool {
    state.trusted_ui.installs.sent.contains(&req)
}

/// policyd's review. Shown now, or when the seat is free.
pub fn review(state: &mut AbyssState, req: u64, review: u64, display: &[u8]) {
    let i = &mut state.trusted_ui.installs;
    if !i.sent.contains(&req) {
        return;
    }
    let Some(display) = decode(display) else {
        tracing::warn!(req, "install review display malformed; refused");
        crate::policy::link::send(
            state,
            &ToPolicyd::InstallAnswer {
                review,
                approve: false,
            },
        );
        return;
    };
    i.waiting.push_back(Waiting { review, display });
    schedule(state);
}

/// The outcome policyd reports. Logged by code only.
pub fn outcome(state: &mut AbyssState, req: u64, refused: Option<&str>) {
    state.trusted_ui.installs.sent.retain(|&r| r != req);
    match refused {
        None => tracing::info!(req, "agent installed"),
        Some(why) => tracing::info!(req, why, "agent install refused"),
    }
}

pub fn schedule(state: &mut AbyssState) {
    if state.trusted_ui.is_open() || state.trusted_ui.installs.showing.is_some() {
        return;
    }
    let Some(w) = state.trusted_ui.installs.waiting.pop_front() else {
        return;
    };
    state.trusted_ui.installs.next += 1;
    let token = TOKEN_BASE | (state.trusted_ui.installs.next & 0xffff_ffff);
    let opened = modal(token, &w.display).is_some_and(|m| super::open(state, m));
    if opened {
        state.trusted_ui.installs.showing = Some((token, w.review));
    } else {
        tracing::warn!(id = %w.display.id, "install review could not be drawn whole; refused");
        crate::policy::link::send(
            state,
            &ToPolicyd::InstallAnswer {
                review: w.review,
                approve: false,
            },
        );
    }
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    state.trusted_ui.installs.showing.is_some_and(|(t, _)| t == token)
}

pub fn answer(state: &mut AbyssState, choice: super::Choice) {
    let Some((_, review)) = state.trusted_ui.installs.showing.take() else {
        return;
    };
    let approve = choice.role == Role::Grant && !choice.timed_out;
    crate::policy::link::send(state, &ToPolicyd::InstallAnswer { review, approve });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_policy_eval::cbor::{enc, MapBuilder};

    fn display() -> Vec<u8> {
        let mut d = MapBuilder::new();
        d.insert("id", enc(|w| w.text("ec-ref-agent")));
        d.insert("name", enc(|w| w.text("Reference\u{202e} agent")));
        d.insert("publisher", enc(|w| w.text("eclipse")));
        d.insert("version", enc(|w| w.text("0.1.0")));
        d.insert(
            "caps",
            enc(|w| {
                w.array(1);
                let mut c = MapBuilder::new();
                c.insert("line", enc(|w| w.text("seat.key app_id:foot")));
                c.insert("because", enc(|w| w.text("Type in it")));
                c.insert("added", enc(|w| w.bool(true)));
                w.raw(&c.finish());
            }),
        );
        for k in ["removed", "warnings", "sandbox"] {
            d.insert(k, enc(|w| w.array(0)));
        }
        d.insert(
            "absent",
            enc(|w| {
                w.array(2);
                w.text("capture");
                w.text("shell");
            }),
        );
        d.finish()
    }

    #[test]
    fn a_review_shows_every_line_and_puts_package_words_in_the_untrusted_block() {
        let d = decode(&display()).unwrap();
        let m = modal(TOKEN_BASE | 1, &d).unwrap();
        assert!(m
            .facts()
            .iter()
            .any(|(l, v)| *l == "Asks for" && v == "+ seat.key app_id:foot"));
        assert!(m
            .facts()
            .iter()
            .any(|(l, v)| *l == "Does not ask" && v == "capture, shell"));
        assert!(m.untrusted().iter().any(|l| l.contains("Type in it")));
        assert!(
            !m.untrusted().iter().any(|l| l.contains('\u{202e}')),
            "the untrusted block is sanitised"
        );
        assert_eq!(m.buttons()[m.safe()].label, "Refuse", "Refuse is the default");
    }

    #[test]
    fn a_review_that_does_not_fit_is_refused_not_cut() {
        let mut d = decode(&display()).unwrap();
        d.caps = (0..40)
            .map(|i| (format!("scene.list app_id:a{i}"), "x".into(), true))
            .collect();
        assert!(modal(1, &d).is_none());
    }

    #[test]
    fn a_malformed_display_does_not_decode() {
        let mut b = display();
        b.push(0);
        assert!(decode(&b).is_none());
        assert!(decode(&[0xa0]).is_some(), "an empty map is an empty display");
    }
}
