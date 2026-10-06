// SPDX-License-Identifier: AGPL-3.0-only
//! The console's side of COMP-19: the window is a protected surface, and the
//! one card the compositor draws inside it is the only way a task starts or
//! resumes.
//!
//! Protection covers the whole window (A-08 §10 allows it): the conversation
//! and both input boxes are in it, so none of it is captured and none of it
//! takes anything but physical input.
//!
//! This module never draws. It asks for a slot where the view measured a hole,
//! keeps the draft the composer produced in step with it, and reports what the
//! compositor said back.

use ec_console_client::protected::{Draft, Event, Protected, SlotKind, SlotReason, SlotState};

use crate::model::{next_step, Held, Step, Want};

/// Whether this window is protected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Guard {
    /// The window's handles have not been fetched yet.
    Waiting,
    On,
    /// Not protected, with the reason. The console then has no slot and says
    /// so; it does not pretend.
    Off(String),
}

/// What the compositor told us that the screen should react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Committed(String),
    Cancelled,
    InputRefused(u32),
}

pub struct Shield {
    pub guard: Guard,
    prot: Option<Protected>,
    pub held: Held,
    /// The hole's top-left as the view last measured it, surface-local
    /// logical px.
    pub hole: Option<(i32, i32)>,
    /// The compositor's slot size (`geometry`), or `None` before it speaks.
    pub size: Option<(i32, i32)>,
    pub state: Option<(SlotState, SlotReason)>,
    sent_draft: Option<Draft>,
    sent_unpause: Option<String>,
    /// The last request the glue refused, for a notice.
    pub error: Option<String>,
}

impl Default for Shield {
    fn default() -> Self {
        Self {
            guard: Guard::Waiting,
            prot: None,
            held: Held::default(),
            hole: None,
            size: None,
            state: None,
            sent_draft: None,
            sent_unpause: None,
            error: None,
        }
    }
}

/// The `wl_display` and `wl_surface` pointers of a window, as winit exposes
/// them through raw-window-handle. Only Wayland has them.
pub fn raw_handles(w: &dyn iced::window::Window) -> Result<(usize, usize), String> {
    use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
    let display = w.display_handle().map_err(|e| e.to_string())?.as_raw();
    let surface = w.window_handle().map_err(|e| e.to_string())?.as_raw();
    match (display, surface) {
        (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(s)) => {
            Ok((d.display.as_ptr() as usize, s.surface.as_ptr() as usize))
        }
        _ => Err("this window is not a Wayland surface".to_owned()),
    }
}

impl Shield {
    /// Protect the window. `display` and `surface` are the addresses
    /// [`raw_handles`] returned.
    pub fn attach(&mut self, handles: Result<(usize, usize), String>) {
        let (display, surface) = match handles {
            Ok(h) => h,
            Err(e) => {
                self.guard = Guard::Off(e);
                return;
            }
        };
        // SAFETY: both pointers came from winit through raw-window-handle for
        // the one window this process owns, so they are live `wl_display*` and
        // `wl_surface*` on that display. The window outlives this value: the
        // app drops the shield (`detach`) on the close request, before iced
        // tears the window down, and `Protected` destroys its objects then.
        match unsafe { Protected::new(display as *mut _, surface as *mut _) } {
            Ok(p) => {
                self.prot = Some(p);
                self.guard = Guard::On;
            }
            Err(e) => self.guard = Guard::Off(e.to_string()),
        }
    }

    /// Release the protection before the window goes.
    pub fn detach(&mut self) {
        self.prot = None;
        self.held = Held::default();
        if self.guard == Guard::On {
            self.guard = Guard::Off("closing".to_owned());
        }
    }

    pub fn protecting(&self) -> bool {
        self.prot.is_some()
    }

    /// Hand on what the host's loop has already read, and collect the
    /// compositor's answers. Never blocks.
    pub fn pump(&mut self) -> Vec<Outcome> {
        let mut out = Vec::new();
        let Some(p) = self.prot.as_mut() else {
            return out;
        };
        if let Err(e) = p.pump() {
            self.error = Some(e.to_string());
            return out;
        }
        let events: Vec<Event> = p.events().try_iter().collect();
        for ev in events {
            match ev {
                Event::Geometry { width, height } => {
                    self.size = Some((width.clamp(1, 4096), height.clamp(1, 4096)));
                }
                Event::State { state, reason } => self.state = Some((state, reason)),
                Event::Committed { task_id } => {
                    self.slot_gone();
                    out.push(Outcome::Committed(task_id));
                }
                Event::Cancelled => {
                    self.slot_gone();
                    out.push(Outcome::Cancelled);
                }
                Event::InputRefused { count } => out.push(Outcome::InputRefused(count)),
            }
        }
        out
    }

    fn slot_gone(&mut self) {
        self.held = Held::default();
        self.state = None;
        self.size = None;
        self.sent_draft = None;
        self.sent_unpause = None;
    }

    /// Bring the compositor's slot in line with what the screen needs: create,
    /// move or cancel it, then keep its draft (or its unpause target) current.
    /// Called from the heartbeat, so the draft goes out a few times a second at
    /// most, inside the compositor's own ten-a-second limit.
    pub fn reconcile(&mut self, want: Want, unpause: Option<&str>, draft: Option<&Draft>) {
        let Some(p) = self.prot.as_mut() else {
            return;
        };
        match next_step(want, self.held, self.hole) {
            Step::Idle => {}
            Step::Create(kind, (x, y)) => match p.get_slot(kind, x, y) {
                Ok(()) => {
                    self.held = Held {
                        kind: Some(kind),
                        at: Some((x, y)),
                        ending: false,
                    };
                    self.sent_draft = None;
                    self.sent_unpause = None;
                    self.state = None;
                }
                Err(e) => self.error = Some(e.to_string()),
            },
            Step::Move((x, y)) => match p.move_to(x, y) {
                Ok(()) => {
                    self.held.at = Some((x, y));
                    // A move disarms; the compositor re-previews the draft it
                    // already holds, so nothing is re-sent.
                }
                Err(e) => self.error = Some(e.to_string()),
            },
            Step::Cancel => match p.cancel() {
                Ok(()) => self.held.ending = true,
                Err(e) => self.error = Some(e.to_string()),
            },
        }
        if self.held.ending {
            return;
        }
        match (self.held.kind, want) {
            (Some(SlotKind::TaskCommit), Want::Commit) => {
                if let Some(d) = draft {
                    if self.sent_draft.as_ref() != Some(d) {
                        match p.set_draft(d) {
                            Ok(()) => self.sent_draft = Some(d.clone()),
                            Err(e) => self.error = Some(e.to_string()),
                        }
                    }
                }
            }
            (Some(SlotKind::TaskUnpause), Want::Unpause) => {
                if let Some(id) = unpause {
                    if self.sent_unpause.as_deref() != Some(id) {
                        match p.set_unpause(id) {
                            Ok(()) => self.sent_unpause = Some(id.to_owned()),
                            Err(e) => self.error = Some(e.to_string()),
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
