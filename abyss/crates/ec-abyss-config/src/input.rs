// SPDX-License-Identifier: AGPL-3.0-only
//! Plain-data input bindings the config produces (COMP-04, COMP-13 §3). The
//! dispatch that acts on them stays in `ec-abyss`'s `input/`; nothing here
//! touches smithay, so the keysym type is xkbcommon's (smithay re-exports it).

use xkbcommon::xkb::Keysym;

use crate::outputs::Step;

/// Modifier set of a binding. Compared against the held xkb modifier state,
/// which `ec-abyss` converts to a `Mods` (equality is the match rule).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub logo: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// Direction for focus/move actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// What a key means while the region selector owns the seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectKey {
    /// Leave selection mode and emit nothing.
    Cancel,
    /// A key with no meaning here. Swallowed anyway — the selector owns the
    /// seat outright, so nothing bound elsewhere may fire underneath it.
    Ignored,
}

/// Compositor-level action bound to a chord (COMP-13 §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    /// Ctrl+Alt+F1..F12 on the DRM backend.
    SwitchVt(i32),
    Spawn(String),
    Close,
    ToggleFloating,
    /// Send the focused window away (COMP-05 §4 `minimized`).
    Minimize,
    /// Bring back the last window sent away on the active workspace.
    Unminimize,
    ToggleLayout,
    Focus(Direction),
    /// Swap with the neighbour in this direction (tiled windows only; a
    /// floating window is moved with the mouse, see `MouseBind`).
    Move(Direction),
    /// Raise (positive) or lower the focused tiled window's weight in its
    /// Radiant container. A no-op for floating windows and other layouts.
    Priority(i32),
    /// 1-based workspace index.
    SwitchWorkspace(usize),
    /// The workspace after / before the active one on the focused output.
    WorkspaceNext,
    WorkspacePrev,
    /// 1-based workspace index.
    MoveToWorkspace(usize),
    /// Move the focused window to display `number`'s currently active
    /// workspace (ADR 0049). `number` is the compositor-assigned/configured
    /// display number, not a workspace index or an output handle.
    MoveToOutputWorkspace(u8),
    /// The override chord reserved by COMP-13 §1.1. Accepted and dispatched
    /// today so a config naming it is valid; it pauses every agent (COMP-04 §6).
    AgentOverride,
    /// The second reserved chord (COMP-04 §6): pause every agent and end
    /// them via `policyd`. Built in on Super+Shift+Escape, never rebindable.
    AgentTerminate,
    /// The pending-decision-queue chord reserved by COMP-13 §1.1 (COMP-10
    /// §3.10). Dispatched today for the same reason as `AgentOverride`; the
    /// queue it opens arrives with the trusted UI (COMP-16 milestone 14).
    AgentAttention,
    /// One keypress consumed by the overscan calibration overlay (COMP-03 §2).
    /// Not bindable from config — the input filter synthesises it while a
    /// calibration session owns the seat.
    Calibrate(Step),
    /// One keypress consumed by the region selector (COMP-18 §1.3). Not
    /// bindable from config — the input filter synthesises it while a
    /// selection owns the seat.
    RegionSelect(SelectKey),
    /// One keypress consumed by a trusted prompt (COMP-10 §4). Not bindable
    /// from config -- the input filter synthesises it while a prompt holds
    /// the seat.
    Prompt(Keysym),
    /// Chords an addon owns (COMP-18 §4). The compositor does not act on
    /// these itself; it forwards the name on the `keybind` event stream, so
    /// nothing happens when no addon is listening.
    ///
    /// `AnnotationSelect` is the exception: a rectangle is authority over what
    /// gets read, so the compositor runs the selection itself (COMP-18 §1.3)
    /// and forwards only the result.
    AnnotationSelect,
    AnnotationDismiss,
    AnnotationExpand,
    AnnotationAutoToggle,
}

/// A configured key binding.
#[derive(Debug, Clone)]
pub struct Bind {
    pub mods: Mods,
    pub key: Keysym,
    pub action: Action,
}

/// A configured touchpad swipe binding (COMP-04 §2): `fingers` moving in
/// `direction` runs `action` instead of reaching the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GestureBind {
    pub fingers: u32,
    pub direction: Direction,
    pub action: Action,
}

/// Mouse button a [`MouseBind`] listens for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// `linux/input-event-codes.h`.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

impl MouseButton {
    /// The `linux/input-event-codes.h` code libinput reports for this button.
    pub fn code(self) -> u32 {
        match self {
            MouseButton::Left => BTN_LEFT,
            MouseButton::Right => BTN_RIGHT,
            MouseButton::Middle => BTN_MIDDLE,
        }
    }
}

/// What a held-modifier mouse drag does to the window under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    MoveWindow,
    ResizeWindow,
}

/// A configured modifier + mouse-button binding (COMP-04 §5, ADR 0057): with
/// exactly `mods` held, pressing `button` over a window starts `action` on it,
/// and the press never reaches the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseBind {
    pub mods: Mods,
    pub button: MouseButton,
    pub action: MouseAction,
}

/// A configured touchpad window drag (COMP-04 §2, amended C-12): with exactly
/// `mods` held, a `fingers`-finger touchpad drag moves the window under the
/// pointer. Two fingers arrive as finger scroll, three and four as a swipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragGesture {
    pub fingers: u32,
    pub mods: Mods,
}

/// Distance, in libinput's touchpad-scaled pointer units, a claimed swipe has
/// to travel along its dominant axis before it counts. Below it the swipe was
/// a hesitation, not a command, and nothing runs.
pub const SWIPE_THRESHOLD: f64 = 100.0;

/// The direction a finished swipe went: whichever axis moved furthest, if it
/// moved at least `threshold`. Screen coordinates, so `dy > 0` is down.
pub fn swipe_direction(dx: f64, dy: f64, threshold: f64) -> Option<Direction> {
    let (ax, ay) = (dx.abs(), dy.abs());
    if ax.max(ay) < threshold {
        return None;
    }
    Some(if ax >= ay {
        if dx < 0.0 {
            Direction::Left
        } else {
            Direction::Right
        }
    } else if dy < 0.0 {
        Direction::Up
    } else {
        Direction::Down
    })
}
