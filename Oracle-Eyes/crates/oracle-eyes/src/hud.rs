// SPDX-License-Identifier: AGPL-3.0-only

//! The compositor half of Oracle-Eyes: one live annotation, addressed over
//! the COMP-13 control socket (COMP-18 §3).
//!
//! Everything about how the panel looks — placement, wrapping, clamping,
//! sanitisation — belongs to the compositor. This module sends a rectangle
//! and a string and keeps the handle it gets back, nothing more.

use serde_json::{json, Value};

/// The four methods COMP-18 §3 grants this daemon, behind a trait so the
/// display logic is testable without a running compositor.
pub trait Control {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
}

impl Control for eclipse_ipc::Client {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        eclipse_ipc::Client::call(self, method, params).map_err(|e| e.to_string())
    }
}

/// A rectangle in compositor-logical coordinates: what the answer is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// At most one annotation is on screen at a time in Phase 1. Automatic mode
/// (spec §2.2) will hold several; the compositor already owns eviction, so
/// that is a change here and nowhere else.
#[derive(Debug, Default)]
pub struct Hud {
    /// The live handle and the region it was created against. The anchor is
    /// kept because `annotation_update` carries text only: a panel can be
    /// re-worded in place but never moved, so an answer about somewhere else
    /// has to be a new panel.
    live: Option<(u64, Anchor)>,
}

impl Hud {
    pub fn new() -> Hud {
        Hud::default()
    }

    /// The handle currently on screen, if any.
    #[cfg(test)]
    pub fn live(&self) -> Option<u64> {
        self.live.map(|(id, _)| id)
    }

    /// Show `text` at `anchor`, replacing whatever was there. An answer about
    /// the same region replaces the text in place, which keeps the panel from
    /// blinking between two answers about one thing; an answer about a
    /// different region gets a fresh panel there, because an annotation that
    /// stayed put would be pointing at the wrong thing.
    pub fn show(
        &mut self,
        c: &mut impl Control,
        anchor: Anchor,
        text: &str,
    ) -> Result<u64, String> {
        match self.live {
            Some((id, at)) if at == anchor => {
                match c.call("annotation_update", json!({"id": id, "text": text})) {
                    Ok(_) => return Ok(id),
                    // The compositor forgot it — we disconnected, or it
                    // restarted. Fall through and create a fresh one rather
                    // than going blind.
                    Err(_) => self.live = None,
                }
            }
            Some((id, _)) => {
                // Moving is destroy-then-create. A failure here is not fatal:
                // the compositor evicts on its own and a stale panel is worse
                // than a duplicate call.
                let _ = c.call("annotation_destroy", json!({"id": id}));
                self.live = None;
            }
            None => {}
        }
        let reply = c.call(
            "annotation_create",
            json!({
                "x": anchor.x, "y": anchor.y, "w": anchor.w, "h": anchor.h,
                "text": text,
            }),
        )?;
        let id = reply
            .get("id")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("annotation_create returned no handle: {reply}"))?;
        self.live = Some((id, anchor));
        Ok(id)
    }

    /// Take everything down. Idempotent: dismissing nothing is not an error,
    /// because the chord is the user's and they may press it any time.
    pub fn dismiss(&mut self, c: &mut impl Control) -> Result<(), String> {
        self.live = None;
        c.call("annotation_clear", json!({})).map(|_| ())
    }
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
    }

    impl Control for Fake {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.seen.push(method.to_owned());
            match method {
                "annotation_create" => {
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
        let first = hud.show(&mut c, A, "one").unwrap();
        let second = hud.show(&mut c, A, "two").unwrap();
        assert_eq!(first, second, "the panel should not blink between answers");
        assert_eq!(c.seen, ["annotation_create", "annotation_update"]);
    }

    #[test]
    fn an_answer_about_somewhere_else_moves_rather_than_staying_put() {
        let mut c = Fake::default();
        let mut hud = Hud::new();
        let first = hud.show(&mut c, A, "one").unwrap();
        let elsewhere = Anchor {
            x: 900,
            y: 600,
            w: 100,
            h: 40,
        };
        let second = hud.show(&mut c, elsewhere, "two").unwrap();
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
        hud.show(&mut c, A, "one").unwrap();
        // The compositor restarted: it no longer knows the handle.
        c.known.clear();
        hud.show(&mut c, A, "two").unwrap();
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
        hud.show(&mut c, A, "one").unwrap();
        hud.dismiss(&mut c).unwrap();
        hud.show(&mut c, A, "two").unwrap();
        assert_eq!(
            c.seen,
            ["annotation_create", "annotation_clear", "annotation_create"]
        );
    }
}
