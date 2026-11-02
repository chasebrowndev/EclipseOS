// SPDX-License-Identifier: AGPL-3.0-only

//! The compositor half of Oracle-Eyes: one live annotation, addressed over
//! the COMP-13 control socket (COMP-18 §3).
//!
//! Everything about how the panel looks — placement, wrapping, clamping,
//! sanitisation — belongs to the compositor. This module sends a rectangle
//! and a string and keeps the handle it gets back, nothing more.

use serde_json::{json, Value};

use crate::fault::Failure;
use crate::logsafe::log_safe;

/// The four methods COMP-18 §3 grants this daemon, behind a trait so the
/// display logic is testable without a running compositor.
pub trait Control {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

impl Control for ec_ipc::Client {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        ec_ipc::Client::call(self, method, params).map_err(|e| e.to_string())
    }
}

/// Every call to the compositor, logged on its way out. Panel text came off
/// the model, which saw redacted screen text, but it goes through
/// [`log_safe`] all the same.
fn traced(c: &mut impl Control, method: &str, params: Value) -> Result<Value, String> {
    if tracing::enabled!(tracing::Level::DEBUG) {
        let int = |k: &str| params.get(k).and_then(Value::as_i64);
        let text = |k: &str| {
            params
                .get(k)
                .and_then(Value::as_str)
                .map(log_safe)
                .unwrap_or_default()
        };
        let id = params.get("id").and_then(Value::as_u64);
        // Counts only: the picks are option labels and rectangles, not text.
        let picks = params
            .get("picks")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        tracing::debug!(
            method,
            id,
            x = int("x"),
            y = int("y"),
            w = int("w"),
            h = int("h"),
            title = %text("title"),
            text = %text("text"),
            picks,
            "hud: send"
        );
    }
    let r = c.call(method, params);
    if let Err(e) = &r {
        tracing::debug!(method, error = %e, "hud: call failed");
    }
    r
}

/// A rectangle in compositor-logical coordinates: what the answer is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl From<crate::frame::Region> for Anchor {
    fn from(r: crate::frame::Region) -> Anchor {
        Anchor {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

/// The option a multiple-choice answer picked: a label and the rect OCR
/// measured for it (ADR 0054). The compositor draws it only inside the
/// anchor, so it can never point anywhere the anchor does not already.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub label: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Whether a panel is an answer or a failure (`kind` on the wire). The
/// compositor marks a failure as one; how is its business.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelKind {
    #[default]
    Answer,
    Error,
}

/// What goes in the panel. `title` is the headline, drawn as the panel's
/// header; empty means a body-only panel. Styling is the compositor's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Panel {
    pub title: String,
    pub text: String,
    /// The first option picked, if any. Sent as the first entry of `picks`.
    pub pick: Option<Pick>,
    /// The rest of a multi-select's picks, in order after `pick`.
    pub more: Vec<Pick>,
    pub kind: PanelKind,
}

impl Panel {
    /// A panel that says something went wrong: a fixed title and the one
    /// sentence for its class. The failure's detail is for the log, never
    /// the screen.
    pub fn failure(f: &Failure) -> Panel {
        Panel {
            title: "Couldn't answer".to_string(),
            text: f.fault.sentence().to_string(),
            pick: None,
            more: Vec::new(),
            kind: PanelKind::Error,
        }
    }

    fn params(&self) -> Value {
        let mut p = json!({"text": self.text});
        if !self.title.is_empty() {
            p["title"] = json!(self.title);
        }
        // `answer` is the default, so only the exception is spelled out.
        if self.kind == PanelKind::Error {
            p["kind"] = json!("error");
        }
        p
    }
}

/// At most one annotation is on screen at a time in Phase 1. Automatic mode
/// (spec §2.2) will hold several; the compositor already owns eviction, so
/// that is a change here and nowhere else.
#[derive(Debug, Default)]
pub struct Hud {
    /// The live handle and what it was created with. The anchor is kept
    /// because `annotation_update` carries text only: a panel can be
    /// re-worded in place but never moved, re-marked or turned from an
    /// answer into a failure, so any of those has to be a new panel.
    /// `None` as the anchor is an unanchored panel.
    live: Option<(u64, Option<Anchor>, Panel)>,
    /// Whether the panels on screen are several (one per question). Those are
    /// replaced as a set, never updated in place.
    several: bool,
}

impl Hud {
    pub fn new() -> Hud {
        Hud::default()
    }

    /// The handle currently on screen, if any.
    #[cfg(test)]
    pub fn live(&self) -> Option<u64> {
        self.live.as_ref().map(|(id, ..)| *id)
    }

    /// Show `panel` at `anchor`, replacing whatever was there. An answer
    /// about the same region with the same pick replaces the text in place,
    /// which keeps the panel from blinking between two answers about one
    /// thing; anything else gets a fresh panel, because `annotation_update`
    /// carries text only and a panel that stayed put would be pointing at the
    /// wrong thing. With no anchor the rect is left out and the compositor
    /// places the panel itself (unanchored).
    pub fn show(
        &mut self,
        c: &mut impl Control,
        anchor: Option<Anchor>,
        panel: &Panel,
    ) -> Result<u64, String> {
        match self.live.take() {
            Some((id, at, shown)) if !self.several && at == anchor && same_marks(&shown, panel) => {
                let mut p = panel.params();
                p["id"] = json!(id);
                if traced(c, "annotation_update", p).is_ok() {
                    self.live = Some((id, at, panel.clone()));
                    return Ok(id);
                }
                // The compositor forgot it — we disconnected, or it
                // restarted. Fall through and create a fresh one rather than
                // going blind.
            }
            Some((id, ..)) => {
                // Moving is destroy-then-create. A failure here is not fatal:
                // the compositor evicts on its own and a stale panel is worse
                // than a duplicate call.
                let _ = traced(c, "annotation_destroy", json!({"id": id}));
            }
            None => {}
        }
        if self.several {
            // The set on screen is replaced as a whole: the previous set's
            // panels go first, or they would sit beside the new ones.
            let _ = traced(c, "annotation_clear", json!({}));
            self.several = false;
        }
        let id = create(c, anchor, panel)?;
        self.live = Some((id, anchor, panel.clone()));
        Ok(id)
    }

    /// Show one panel per question, each at its own anchor. One panel is the
    /// ordinary [`Hud::show`], updated in place when it can be. Several replace
    /// whatever was on screen, as a set.
    pub fn show_all(
        &mut self,
        c: &mut impl Control,
        items: &[(Option<Anchor>, Panel)],
    ) -> Result<(), String> {
        match items {
            [] => Ok(()),
            [(anchor, panel)] if !self.several => self.show(c, *anchor, panel).map(|_| ()),
            _ => {
                let _ = traced(c, "annotation_clear", json!({}));
                self.live = None;
                self.several = true;
                for (anchor, panel) in items {
                    create(c, *anchor, panel)?;
                }
                Ok(())
            }
        }
    }

    /// Take everything down. Idempotent: dismissing nothing is not an error,
    /// because the chord is the user's and they may press it any time.
    pub fn dismiss(&mut self, c: &mut impl Control) -> Result<(), String> {
        self.live = None;
        self.several = false;
        traced(c, "annotation_clear", json!({})).map(|_| ())
    }
}

/// Whether two panels carry the same marks, so an update in place keeps them.
fn same_marks(a: &Panel, b: &Panel) -> bool {
    a.pick == b.pick && a.more == b.more && a.kind == b.kind
}

/// The most picks one annotation may carry: the compositor's cap on `picks`.
const MAX_PICKS: usize = 16;

/// Send one `annotation_create`, and return its handle. Every pick, the first
/// included, goes in the one `picks` list: the compositor refuses `pick` next
/// to `picks`, and marks only the entries inside the anchor.
fn create(c: &mut impl Control, anchor: Option<Anchor>, panel: &Panel) -> Result<u64, String> {
    let mut p = panel.params();
    if let Some(a) = anchor {
        p["x"] = json!(a.x);
        p["y"] = json!(a.y);
        p["w"] = json!(a.w);
        p["h"] = json!(a.h);
    }
    let mark = |k: &Pick| json!({"x": k.x, "y": k.y, "w": k.w, "h": k.h, "label": k.label});
    let all: Vec<Value> = panel
        .pick
        .iter()
        .chain(panel.more.iter())
        .take(MAX_PICKS)
        .map(mark)
        .collect();
    if !all.is_empty() {
        p["picks"] = Value::Array(all);
    }
    let reply = traced(c, "annotation_create", p)?;
    reply
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("annotation_create returned no handle: {reply}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        next: u64,
        /// Method names in the order they were called.
        seen: Vec<String>,
        /// Handles the compositor still knows about.
        known: Vec<u64>,
        /// The params of the last create.
        last: Option<Value>,
    }

    impl Control for Fake {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.seen.push(method.to_owned());
            match method {
                "annotation_create" => {
                    self.last = Some(params);
                    self.next += 1;
                    self.known.push(self.next);
                    Ok(json!({"ok": true, "id": self.next}))
                }
                "annotation_update" => {
                    let id = params["id"].as_u64().unwrap();
                    if self.known.contains(&id) {
                        Ok(json!({"ok": true}))
                    } else {
                        Err("no such annotation".into())
                    }
                }
                "annotation_destroy" => {
                    let id = params["id"].as_u64().unwrap();
                    self.known.retain(|k| *k != id);
                    Ok(json!({"ok": true}))
                }
                "annotation_clear" => {
                    self.known.clear();
                    Ok(json!({"ok": true, "dropped": 0}))
                }
                other => panic!("oracle-eyes called {other}, which it is not granted"),
            }
        }
    }

    fn p(text: &str) -> Panel {
        Panel {
            text: text.into(),
            ..Panel::default()
        }
    }

    const A: Anchor = Anchor {
        x: 10,
        y: 10,
        w: 100,
        h: 40,
    };

    #[test]
    fn a_second_answer_replaces_the_first_in_place() {
        let mut c = Fake::default();
        let mut hud = Hud::new();
        let first = hud.show(&mut c, Some(A), &p("one")).unwrap();
        let second = hud.show(&mut c, Some(A), &p("two")).unwrap();
        assert_eq!(first, second, "the panel should not blink between answers");
        assert_eq!(c.seen, ["annotation_create", "annotation_update"]);
    }

    #[test]
    fn an_answer_about_somewhere_else_moves_rather_than_staying_put() {
        let mut c = Fake::default();
        let mut hud = Hud::new();
        let first = hud.show(&mut c, Some(A), &p("one")).unwrap();
        let elsewhere = Anchor {
            x: 900,
            y: 600,
            w: 100,
            h: 40,
        };
        let second = hud.show(&mut c, Some(elsewhere), &p("two")).unwrap();
        assert_ne!(first, second, "a moved panel is a new panel");
        assert_eq!(
            c.seen,
            [
                "annotation_create",
                "annotation_destroy",
                "annotation_create"
            ],
            "update carries text only, so it cannot follow the region"
        );
    }

    #[test]
    fn a_forgotten_handle_is_recreated_not_lost() {
        let mut c = Fake::default();
        let mut hud = Hud::new();
        hud.show(&mut c, Some(A), &p("one")).unwrap();
        // The compositor restarted: it no longer knows the handle.
        c.known.clear();
        hud.show(&mut c, Some(A), &p("two")).unwrap();
        assert_eq!(
            c.seen,
            [
                "annotation_create",
                "annotation_update",
                "annotation_create"
            ]
        );
        assert_eq!(hud.live(), Some(2));
    }

    #[test]
    fn a_different_pick_is_a_new_panel_and_is_sent() {
        let mut c = Fake::default();
        let mut hud = Hud::new();
        hud.show(&mut c, Some(A), &p("one")).unwrap();
        let picked = Panel {
            title: "Jupiter".into(),
            text: "".into(),
            pick: Some(Pick {
                label: "B".into(),
                x: 12,
                y: 30,
                w: 60,
                h: 10,
            }),
            ..Panel::default()
        };
        hud.show(&mut c, Some(A), &picked).unwrap();
        assert_eq!(
            c.seen,
            [
                "annotation_create",
                "annotation_destroy",
                "annotation_create"
            ]
        );
        let sent = c.last.clone().unwrap();
        assert_eq!(sent["picks"][0]["label"], "B");
        assert!(sent.get("pick").is_none(), "the compositor refuses both");
        assert_eq!(sent["title"], "Jupiter");
    }

    #[test]
    fn every_pick_goes_in_one_list_capped_at_the_compositor_limit() {
        let mut c = Fake::default();
        let mark = |label: &str, y: i32| Pick {
            label: label.into(),
            x: 12,
            y,
            w: 60,
            h: 10,
        };
        let panel = Panel {
            pick: Some(mark("A", 30)),
            more: (0..20).map(|i| mark(&format!("L{i}"), 40 + i)).collect(),
            ..Panel::default()
        };
        create(&mut c, Some(A), &panel).unwrap();
        let sent = c.last.clone().unwrap();
        let picks = sent["picks"].as_array().unwrap();
        assert_eq!(picks.len(), MAX_PICKS);
        assert_eq!(picks[0]["label"], "A", "the first pick leads the list");
        assert_eq!(picks[1]["label"], "L0");
        assert!(sent.get("pick").is_none());
    }

    #[test]
    fn an_empty_title_is_not_sent() {
        let mut c = Fake::default();
        let mut hud = Hud::new();
        hud.show(&mut c, Some(A), &p("x")).unwrap();
        assert!(c.last.unwrap().get("title").is_none());
    }

    #[test]
    fn a_failure_is_an_error_panel_with_its_sentence_and_no_detail() {
        use crate::fault::{Failure, Fault};
        let mut c = Fake::default();
        let mut hud = Hud::new();
        let f = Failure::new(
            Fault::Command,
            "could not run /home/u/bin/claude: No such file",
        );
        hud.show(&mut c, None, &Panel::failure(&f)).unwrap();
        let sent = c.last.clone().unwrap();
        assert_eq!(sent["kind"], "error");
        assert_eq!(sent["title"], "Couldn't answer");
        assert_eq!(sent["text"], Fault::Command.sentence());
        assert!(
            !sent.to_string().contains("/home/u/bin"),
            "no path on screen"
        );
        for k in ["x", "y", "w", "h"] {
            assert!(sent.get(k).is_none(), "unanchored: no {k}");
        }
    }

    #[test]
    fn an_answer_does_not_say_its_kind_and_a_kind_change_is_a_new_panel() {
        use crate::fault::{Failure, Fault};
        let mut c = Fake::default();
        let mut hud = Hud::new();
        hud.show(&mut c, Some(A), &p("one")).unwrap();
        assert!(c.last.clone().unwrap().get("kind").is_none());
        let f = Panel::failure(&Failure::new(Fault::Timeout, "x"));
        hud.show(&mut c, Some(A), &f).unwrap();
        assert_eq!(
            c.seen,
            [
                "annotation_create",
                "annotation_destroy",
                "annotation_create"
            ]
        );
    }

    #[test]
    fn dismissing_nothing_is_not_an_error() {
        let mut c = Fake::default();
        let mut hud = Hud::new();
        hud.dismiss(&mut c).unwrap();
        assert_eq!(hud.live(), None);
    }

    #[test]
    fn dismiss_forgets_the_handle_so_the_next_answer_is_a_fresh_panel() {
        let mut c = Fake::default();
        let mut hud = Hud::new();
        hud.show(&mut c, Some(A), &p("one")).unwrap();
        hud.dismiss(&mut c).unwrap();
        hud.show(&mut c, Some(A), &p("two")).unwrap();
        assert_eq!(
            c.seen,
            ["annotation_create", "annotation_clear", "annotation_create"]
        );
    }
}
