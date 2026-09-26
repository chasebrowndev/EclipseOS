// SPDX-License-Identifier: AGPL-3.0-only
//! The declarative description of every config key (COMP-13 §1.4, F-01 §4).
//!
//! F-01 §4 requires every setting to be reachable without a text editor. A GUI
//! cannot generate a control for a key it does not know the type, range and
//! owning file of — so that knowledge has to exist as data, in one place, and
//! it has to be impossible for the parser and the table to drift apart.
//!
//! This is a static table, deliberately, not a derive macro: `abyss` carries no
//! proc-macro dependency, and a macro would hide ~50 rows that a human reviewer
//! needs to read one by one — the `owner` column decides which keys live in the
//! file the control socket may never write.
//!
//! Drift is caught from three sides, in `tests` below:
//! 1. schema → parser: every key in [`TABLE`] is accepted by `Config::apply`.
//! 2. parser → schema: the parser's `unknown …` fallthrough consults the table,
//!    so a key the parser handles but the table omits is reported as a bug.
//! 3. schema → defaults: [`get`] on a default `Config` returns exactly the
//!    `default` column for every row.

use super::{BarPopupAnchor, BarPosition, Config, FloatingPlacement, LayoutKind};

/// The type of a key's value, and whatever constrains it. A GUI maps this
/// straight onto a control: `Bool` is a toggle, `Int{min,max}` a slider,
/// `Enum` a row of pills.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ty {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Float {
        min: f64,
        max: f64,
    },
    Str,
    Enum(&'static [&'static str]),
    /// `#rrggbb` / `#rrggbbaa`.
    Color,
    /// A single node carrying any number of string arguments.
    StrList,
}

/// Which file a key lives in (COMP-13 §1.3). `Policy` keys are the security
/// surface: the control socket refuses to write them at all, and a GUI shows
/// them read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Abyss,
    Policy,
}

/// Whether a written value takes effect on the next reload or only on restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    Live,
    NeedsRestart,
}

/// A key's value, as read back out of a live [`Config`]. `Null` is a key that
/// is genuinely unset, not an error.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<String>),
    Color([f32; 4]),
}

/// A default, in a form that can be written in a `const`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dv {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(&'static str),
    /// An allow-list defaults to empty; a non-empty default there would be an
    /// entry nobody asked for.
    EmptyList,
    /// A non-empty list default. Only for ordering keys (`bar.widgets.*`),
    /// never for a policy-owned key; a test below pins that.
    List(&'static [&'static str]),
    Color([f32; 4]),
}

/// One row. `path` is dotted and mirrors the KDL nesting exactly, so
/// `decoration.blur.size` is `decoration { blur { size … } }`.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub path: &'static str,
    pub ty: Ty,
    pub default: Dv,
    pub owner: Owner,
    pub reload: Reload,
    pub doc: &'static str,
}

const fn k(path: &'static str, ty: Ty, default: Dv, owner: Owner, reload: Reload, doc: &'static str) -> Key {
    Key {
        path,
        ty,
        default,
        owner,
        reload,
        doc,
    }
}

use Dv::*;
use Owner::{Abyss, Policy};
use Reload::{Live, NeedsRestart};

const PCT: Ty = Ty::Float { min: 0.0, max: 1.0 };
const fn int(min: i64, max: i64) -> Ty {
    Ty::Int { min, max }
}

/// Every scalar key `abyss` understands.
///
/// `xwayland.enable` is `Abyss` and not `Policy` even though disabling X11 is
/// the strongest isolation available (ADR 0026): splitting one two-key block
/// across two files costs more in confusion than it buys, and the X11 trust
/// domain is already clamped per-window by the `app-trust`/`seat-compat`
/// actions, which *are* policy-owned. Stated so the choice is visible rather
/// than inferred.
pub const TABLE: &[Key] = &[
    // general
    k(
        "general.gaps-in",
        int(0, 512),
        Int(5),
        Abyss,
        Live,
        "Gap between tiled windows, logical px.",
    ),
    k(
        "general.gaps-out",
        int(0, 512),
        Int(10),
        Abyss,
        Live,
        "Gap between the tiling area and the screen edge, logical px.",
    ),
    k(
        "general.border-size",
        int(0, 512),
        Int(2),
        Abyss,
        Live,
        "Window border thickness, logical px. 0 disables borders.",
    ),
    k(
        "general.layout",
        Ty::Enum(&["radiant", "dwindle", "master"]),
        Str("radiant"),
        Abyss,
        Live,
        "Default tiling layout for workspaces without their own. `radiant` is a weighted tree with drag-to-tile drop zones and per-window priority; `dwindle` is classic dwindle; `master` puts the first window on the left.",
    ),
    k(
        "general.floating-placement",
        Ty::Enum(&["centered", "pointer", "cascade"]),
        Str("centered"),
        Abyss,
        Live,
        "Where a new floating window lands when no window rule places it.",
    ),
    k(
        "general.focus-follows-mouse",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Move keyboard focus to the window under the pointer.",
    ),
    k(
        "general.focus-follows-mouse-across-outputs",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Let pointer motion move focus to another output. Off keeps focus on the current output until you click.",
    ),
    k(
        "general.cursor-follows-moved-window",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Move the pointer onto a window when a keybind moves it, instead of leaving the cursor behind.",
    ),
    k(
        "general.follow-window-to-workspace",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Follow a window sent to another workspace or display, instead of staying where you are.",
    ),
    k(
        "general.unfocus-on-empty-workspace",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Clear keyboard focus when the pointer enters an output whose workspace has no focusable window. Off keeps the previous focus.",
    ),
    k(
        "general.focus-follows-mouse-layers",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Let pointer motion take focus back from an on-demand layer surface such as the bar. Exclusive layer surfaces are never affected.",
    ),
    k(
        "general.refocus-on-scene-change",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Re-evaluate focus when windows appear or disappear under a stationary pointer.",
    ),
    k(
        "general.col-active-border",
        Ty::Color,
        Color([0.91, 0.64, 0.24, 1.0]),
        Abyss,
        Live,
        "Border colour of the focused window.",
    ),
    k(
        "general.col-inactive-border",
        Ty::Color,
        Color([0.09, 0.09, 0.09, 1.0]),
        Abyss,
        Live,
        "Border colour of every unfocused window.",
    ),
    k(
        "general.drop-guides",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Draw the drop zones and a ghost of where a dragged window will land (radiant layout).",
    ),
    k(
        "general.drop-guide-color",
        Ty::Color,
        Color([0.91, 0.64, 0.24, 1.0]),
        Abyss,
        Live,
        "Colour of the drop guides.",
    ),
    k(
        "general.drop-edge-band",
        int(0, 512),
        Int(40),
        Abyss,
        Live,
        "Width of the screen-edge band that drops a window as a full-height column or full-width row, logical px. 0 disables it.",
    ),
    // render
    k(
        "render.direct-scanout",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Let the DRM backend hand buffers straight to a KMS plane. Composition \
       is forced anyway for any frame holding a sensitive surface.",
    ),
    // bar
    k(
        "bar.fold-when-inactive",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Shrink the taskbar to a thin strip on outputs the pointer is not on.",
    ),
    k(
        "bar.fold-height",
        int(2, 16),
        Int(4),
        Abyss,
        Live,
        "Height in logical pixels of the folded taskbar strip.",
    ),
    k(
        "bar.fold-when-idle",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Also fold the taskbar once the session has been idle. Independent of \
       fold-when-inactive: either one folds the bar, and any input unfolds it.",
    ),
    k(
        "bar.idle-seconds",
        int(5, 600),
        Int(30),
        Abyss,
        Live,
        "Seconds without any human input, pointer motion included, before the \
       taskbar folds when fold-when-idle is on.",
    ),
    k(
        "bar.fold-duration-ms",
        int(0, 1000),
        Int(150),
        Abyss,
        Live,
        "How long the taskbar takes to slide open or shut. Height and exclusive \
       zone animate together, so tiled windows reflow with it. Zero snaps.",
    ),
    k(
        "bar.fold-curve",
        Ty::Enum(ANIMATION_CURVES),
        Str("ease-out"),
        Abyss,
        Live,
        "Easing applied to the taskbar's fold slide.",
    ),
    k(
        "bar.position",
        Ty::Enum(&["top", "bottom"]),
        Str("top"),
        Abyss,
        NeedsRestart,
        "Which edge of every output the taskbar is anchored to. Takes effect \
       the next time the taskbar starts, not on a live reload.",
    ),
    k(
        "bar.tray.pinned",
        Ty::StrList,
        Null,
        Abyss,
        Live,
        "StatusNotifierItem ids shown on the taskbar itself, in this order; an \
       app goes by its own id. Unset means the taskbar's built-in order; an \
       empty list pins nothing. Anything neither pinned nor hidden sits in \
       the overflow drawer. The built-in applets are widgets now \
       (`bar.widgets.order`): their old ids here (network, bluetooth, \
       battery, volume) still load, with a deprecation warning, and \
       `eclipse-ctl config migrate` moves them.",
    ),
    k(
        "bar.tray.hidden",
        Ty::StrList,
        EmptyList,
        Abyss,
        Live,
        "StatusNotifierItem ids never shown, on the taskbar or in its \
       overflow drawer. Hidden wins over pinned when an id is in both. A \
       built-in applet id here is deprecated the same way as in `pinned`: \
       leave it out of `bar.widgets.order` instead.",
    ),
    k(
        "bar.rounding",
        int(0, 64),
        Int(20),
        Abyss,
        Live,
        "Corner radius in logical px for the taskbar's own blur backdrop.",
    ),
    k(
        "bar.clock.hour-12",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Show the taskbar clock in 12-hour time with AM/PM; off is 24-hour.",
    ),
    k(
        "bar.clock.date-mdy",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Write the taskbar date month/day/year; off is ISO year-month-day \
       (2026-09-23).",
    ),
    k(
        "bar.popup-anchor",
        Ty::Enum(&["cell", "pointer"]),
        Str("cell"),
        Abyss,
        Live,
        "Where taskbar popups open: under the cell that was clicked, or at \
       the pointer.",
    ),
    k(
        "bar.eye",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Show the Oracle-Eyes status eye on the taskbar's eclipse mark. The \
       compositor only stores this; the taskbar reads Oracle-Eyes' own status \
       socket (ADR 0055).",
    ),
    k(
        "bar.widgets.order",
        Ty::StrList,
        List(BAR_WIDGET_DEFAULT_ORDER),
        Abyss,
        Live,
        "Widgets drawn after the task strip, left to right (ADR 0065). \
       Built-in ids: now-playing, system-usage, volume, network, bluetooth, \
       battery, tray (the StatusNotifierItems and their overflow drawer) and \
       clock; `custom:<name>` names a `widget` block in `bar`. A widget left \
       out is not drawn. An unknown id, a repeated one, or a `custom:` \
       naming no `widget` block is refused.",
    ),
    k(
        "bar.widgets.important",
        Ty::StrList,
        List(BAR_WIDGET_DEFAULT_IMPORTANT),
        Abyss,
        Live,
        "Widgets that never compress: when the bar runs short of room they \
       keep their full size and everything else gives way first. Same ids \
       as `bar.widgets.order`.",
    ),
    k(
        "bar.widgets.now-playing.art",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Show album art in the Now Playing widget. Art the player keeps on \
       this machine is read from disk; `https` art is fetched only while \
       `bar.widgets.now-playing.remote-art` is on.",
    ),
    k(
        "bar.widgets.now-playing.visualizer",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Draw the audio visualizer in Now Playing. While media plays and the \
       widget is on screen it reads the default output's monitor, reduced to \
       levels in memory and never stored, logged or sent. Off closes the \
       stream.",
    ),
    k(
        "bar.widgets.now-playing.remote-art",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Fetch Now Playing cover art the player publishes as an `https` URL, \
       over https via `curl`. This reveals the playing track to the image \
       host. Off shows the fallback glyph instead; `http` is never fetched.",
    ),
    k(
        "bar.widgets.system-usage.interval-ms",
        int(250, 10_000),
        Int(1000),
        Abyss,
        Live,
        "How often System Usage samples CPU, memory, GPU and disk, in ms.",
    ),
    k(
        "bar.widgets.system-usage.gpu",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Show GPU load in System Usage, where the driver reports it.",
    ),
    k(
        "bar.widgets.system-usage.disk",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Show how full a disk is in System Usage.",
    ),
    k(
        "bar.widgets.system-usage.disk-path",
        Ty::Str,
        Str("/"),
        Abyss,
        Live,
        "Absolute path on the filesystem whose fill level System Usage shows.",
    ),
    k(
        "bar.widgets.volume.step",
        int(1, 25),
        Int(5),
        Abyss,
        Live,
        "Percent the volume moves per scroll notch on the Volume widget.",
    ),
    k(
        "bar.widgets.volume.scroll",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Scrolling over the Volume widget changes the volume.",
    ),
    k(
        "bar.widgets.volume.max-percent",
        int(100, 150),
        Int(100),
        Abyss,
        Live,
        "Highest volume the Volume widget's slider and scroll reach. Above \
       100 boosts past the output's nominal level.",
    ),
    k(
        "bar.motion.enabled",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Animate the taskbar's chips and widgets to their new place, width \
       and opacity. Off snaps.",
    ),
    k(
        "bar.motion.duration-ms",
        int(0, 2000),
        Int(220),
        Abyss,
        Live,
        "How long a taskbar movement takes; with `spring`, roughly how long \
       the spring takes to settle. Zero snaps.",
    ),
    k(
        "bar.motion.curve",
        Ty::Enum(BAR_MOTION_CURVES),
        Str("spring"),
        Abyss,
        Live,
        "Curve for taskbar movement: one of the `animations` easings, or \
       `spring`, which carries its speed through an interrupted move instead \
       of jumping.",
    ),
    // decoration
    k(
        "decoration.rounding",
        int(0, 512),
        Int(13),
        Abyss,
        Live,
        "Corner radius in logical px; 0 disables. Also rounds the blur backdrop \
       behind a layer-shell surface, except one that spans an output edge to \
       edge (three anchors, or two opposite ones, with no positive exclusive \
       zone), which stays square; the taskbar uses `bar.rounding`.",
    ),
    k(
        "decoration.active-opacity",
        PCT,
        Float(1.0),
        Abyss,
        Live,
        "Alpha applied to the focused window.",
    ),
    k(
        "decoration.inactive-opacity",
        PCT,
        Float(1.0),
        Abyss,
        Live,
        "Alpha applied to every unfocused window.",
    ),
    k(
        "decoration.dim-inactive",
        PCT,
        Float(0.0),
        Abyss,
        Live,
        "Strength of the darkening overlay on unfocused windows.",
    ),
    k(
        "decoration.blur.enabled",
        Ty::Bool,
        Bool(true),
        Abyss,
        Live,
        "Dual-Kawase blur behind translucent windows and layer-shell surfaces. \
       A layer blurs only where its opaque region leaves it uncovered.",
    ),
    k(
        "decoration.blur.size",
        int(1, 64),
        Int(8),
        Abyss,
        Live,
        "Blur kernel offset. Larger is softer and costs more.",
    ),
    k(
        "decoration.blur.passes",
        int(1, 6),
        Int(2),
        Abyss,
        Live,
        "Down/up-sample pairs in the blur chain.",
    ),
    k(
        "decoration.shadow.enabled",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Drop shadow behind windows.",
    ),
    k(
        "decoration.shadow.range",
        int(0, 128),
        Int(20),
        Abyss,
        Live,
        "Shadow falloff distance, logical px.",
    ),
    // animations
    k(
        "animations.enabled",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Master switch for geometry animations.",
    ),
    // xwayland
    k(
        "xwayland.enable",
        Ty::Bool,
        Bool(true),
        Abyss,
        NeedsRestart,
        "Run an X server. Off is the strongest isolation available (ADR 0026).",
    ),
    k(
        "xwayland.scaling",
        Ty::Enum(&["compositor", "client"]),
        Str("compositor"),
        Abyss,
        NeedsRestart,
        "`compositor` upscales X11 clients — blurry but always correct. \
       `client` hands them DPI hints and lets them render natively.",
    ),
    // idle
    k(
        "idle.dpms-timeout-seconds",
        int(0, 86_400),
        Null,
        Abyss,
        Live,
        "Inactivity before outputs power off. Unset or 0 disables.",
    ),
    k(
        "idle.lock-timeout-seconds",
        int(0, 86_400),
        Null,
        Abyss,
        Live,
        "Inactivity before `idle.lock-command` runs. Unset or 0 disables.",
    ),
    k(
        "idle.lock-command",
        Ty::Str,
        Null,
        Abyss,
        Live,
        "Locker to spawn on the lock timeout. Without one the timeout is inert.",
    ),
    // input
    k("input.kb-layout", Ty::Str, Str("us"), Abyss, Live, "XKB layout."),
    k("input.kb-variant", Ty::Str, Str(""), Abyss, Live, "XKB variant."),
    k(
        "input.kb-options",
        Ty::Str,
        Null,
        Abyss,
        Live,
        "XKB options string.",
    ),
    k(
        "input.repeat-rate",
        int(0, 255),
        Int(40),
        Abyss,
        Live,
        "Key repeats per second.",
    ),
    k(
        "input.repeat-delay",
        int(0, 5_000),
        Int(300),
        Abyss,
        Live,
        "Milliseconds held before a key starts repeating.",
    ),
    k(
        "input.accel-profile",
        Ty::Enum(&["adaptive", "flat"]),
        Str("adaptive"),
        Abyss,
        Live,
        "Pointer acceleration profile.",
    ),
    k(
        "input.touchpad.natural-scroll",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Invert touchpad scroll direction.",
    ),
    k(
        "input.touchpad.tap-to-click",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Treat a tap as a click.",
    ),
    k(
        "input.touchpad.dwt",
        Ty::Bool,
        Bool(false),
        Abyss,
        Live,
        "Disable the touchpad while typing.",
    ),
    // misc
    k(
        "misc.render-device",
        Ty::Str,
        Null,
        Abyss,
        NeedsRestart,
        "`auto`, a /dev/dri/… path, or `pci:DDDD:BB:DD.F`. The CLI flag and \
       ECLIPSE_RENDER_DEVICE both override it.",
    ),
    k(
        "misc.terminal-command",
        Ty::Str,
        Null,
        Abyss,
        Live,
        "Terminal emulator used to launch `Terminal=true` .desktop entries \
       (`$term -e <argv>`). Unset: those entries are dropped from the app \
       index rather than shown and refused.",
    ),
    // --- policy-owned: the security surface -------------------------------
    k(
        "misc.scripted-input",
        Ty::Bool,
        Bool(false),
        Policy,
        Live,
        "Let the control socket synthesise human input (COMP-13 §2.2).",
    ),
    k(
        "clipboard.data-control-allow",
        Ty::StrList,
        EmptyList,
        Policy,
        Live,
        "Process names allowed to bind zwlr_data_control_manager_v1. Empty \
       denies everyone — data control reads every selection.",
    ),
    k(
        "capture.allow",
        Ty::StrList,
        EmptyList,
        Policy,
        Live,
        "Process names allowed to bind zwlr_screencopy_manager_v1. Empty \
       denies everyone.",
    ),
    k(
        "capture.redact-app-id",
        Ty::StrList,
        EmptyList,
        Policy,
        Live,
        "app_ids whose windows are `secret`: never composited into a capture \
       target, only a solid placeholder (COMP-02 §7).",
    ),
    k(
        "capture.hide-layer",
        Ty::StrList,
        EmptyList,
        Policy,
        Live,
        "exe:namespace pairs whose layer surfaces are omitted from every \
       capture: shown on screen, absent from screenshots and screen shares, \
       with whatever is beneath showing instead. Only surfaces up to 64x64 \
       logical px qualify (ADR 0056). Empty hides nothing.",
    ),
];

/// Owner of a whole top-level node, when every key under it agrees.
///
/// `None` means either "not in the schema" or "mixed" — `misc` holds
/// `render-device` (abyss) beside `scripted-input` (policy), and `windowrule`
/// is decided per action. Both are checked one key at a time instead.
pub fn node_owner(node: &str) -> Option<Owner> {
    if let Some(c) = COLLECTIONS.iter().find(|c| c.node == node) {
        // windowrule's owner is its action's; see RULE_ACTIONS.
        return if c.node == "windowrule" {
            None
        } else {
            Some(c.owner)
        };
    }
    let prefix = format!("{node}.");
    let mut owners = TABLE
        .iter()
        .filter(|k| k.path.starts_with(&prefix))
        .map(|k| k.owner);
    let first = owners.next()?;
    owners.all(|o| o == first).then_some(first)
}

pub fn get_key(path: &str) -> Option<&'static Key> {
    TABLE.iter().find(|k| k.path == path)
}

/// A repeating construct: several nodes of the same name, each with its own
/// identity. These are not settable through `set_config_value` v1 — editing one
/// means naming *which* one, which is list-identity semantics and its own work
/// item — so they are listed here for the GUI to render and for
/// `ci/gui-coverage-exceptions.txt` to point at.
#[derive(Debug, Clone, Copy)]
pub struct Collection {
    pub node: &'static str,
    pub owner: Owner,
    pub doc: &'static str,
}

pub const COLLECTIONS: &[Collection] = &[
    Collection { node: "bind", owner: Abyss, doc: "A key binding: `bind [\"<modifiers>\"] \"<keysym>\" { <action>; }`, e.g. `bind \"SUPER SHIFT\" \"Return\" { spawn \"foot\"; }`. `Super+Escape` and `Super+space` are reserved and cannot be bound." },
    Collection { node: "gesture", owner: Abyss, doc: "A touchpad swipe binding: `gesture \"swipe\" <fingers> \"<direction>\" { <action>; }`. `fingers` is 3 or 4, `direction` is `left`, `right`, `up` or `down`, and the action is anything `bind` accepts. A bound finger count is the compositor's for the whole swipe; unbound swipes, pinches and holds reach the app. Defaults: 3-finger `left` runs `workspace-next`, 3-finger `right` runs `workspace-prev`. A `gesture` for the same fingers and direction replaces the default." },
    Collection { node: "mousebind", owner: Abyss, doc: "A modifier + mouse-button binding: `mousebind \"<modifiers>\" \"<button>\" { <action>; }`. `button` is `left`, `right` or `middle`, and the action is `move-window` or `resize-window`. With exactly those modifiers held, pressing the button over a window drags it (move) or drags its nearest corner (resize); a tiled window is floated first. The press never reaches the client. At least one modifier is required. Defaults: `Alt` + `left` runs `move-window`, `Alt` + `right` runs `resize-window`. A `mousebind` for the same modifiers and button replaces the default." },
    Collection { node: "output", owner: Abyss, doc: "Per-output settings: `output \"<glob>\" { … }`. The glob (`*` only) matches the connector name or the persistent identity; later blocks override earlier ones key by key." },
    Collection { node: "workspace", owner: Abyss, doc: "Per-workspace layout override." },
    Collection { node: "widget", owner: Abyss, doc: "A custom taskbar widget, written inside `bar { }` (ADR 0065): `widget \"<name>\" { exec \"<argv0>\" \"<arg>\"…; interval-ms <ms>; }`; or `stream #true` instead of `interval-ms`, for a command that keeps running and prints one update per line; or `source \"<source>\"` with a `format` instead of `exec`. `bar.widgets.order` draws it as `custom:<name>`. Commands run argv-exec, never through a shell, off the draw path, with a timeout and a 4 KiB line cap, and are killed on reload or removal. A line of output is plain text or JSON `{text, detail, tooltip, state}`; it is shown as plain text, never markup, and never logged. A command has exactly your authority and gains nothing from the taskbar. A later block with the same name replaces an earlier one. `get_config` lists every block, in file order, under `collections.widget` as `{\"name\", \"kind\": \"exec\" | \"stream\" | \"source\", \"exec\": [argv] | null, \"interval-ms\": int | null, \"source\": string | null, \"format\": string | null, \"icon\": string | null, \"on-click\": [argv] | null, \"on-scroll-up\": [argv] | null, \"on-scroll-down\": [argv] | null}`, every field always present: `exec` is set for `exec` and `stream`, `interval-ms` for `exec` only, `source` and `format` for `source` only." },
    Collection { node: "windowrule", owner: Abyss, doc: "A rule matched against windows at map time. Its *action* decides the owning file." },
];

/// `windowrule` is the one construct whose criticality is mixed: `float` is
/// cosmetic, `sensitivity` is the redaction class. So ownership is per-action
/// data rather than per-node, and `apply_windowrule` refuses in both directions.
///
/// `no-agent` is policy-owned: it decides whether an agent can see a window at
/// all, which is the same kind of decision as `sensitivity`.
pub const RULE_ACTIONS: &[(&str, Owner)] = &[
    ("float", Abyss),
    ("tile", Abyss),
    ("fullscreen", Abyss),
    ("size", Abyss),
    ("position", Abyss),
    ("output", Abyss),
    ("opacity", Abyss),
    ("blur", Abyss),
    ("workspace", Abyss),
    ("no-focus-steal", Abyss),
    ("idle-inhibit", Abyss),
    ("sensitivity", Policy),
    ("app-trust", Policy),
    ("seat-compat", Policy),
    ("no-agent", Policy),
];

pub fn rule_owner(action: &str) -> Option<Owner> {
    RULE_ACTIONS.iter().find(|(a, _)| *a == action).map(|(_, o)| *o)
}

/// One sub-node or argument form a construct accepts. `names[0]` is canonical,
/// the rest are aliases; `args` is the argument syntax as a human writes it;
/// every entry in `examples` is fed through `Config::apply` by the tests below,
/// so the syntax `docs/CONFIG.md` shows is syntax the parser takes.
///
/// The parser looks a name up here before dispatching on it, so a form it
/// handles but this list omits is refused rather than silently undocumented.
#[derive(Debug, Clone, Copy)]
pub struct Form {
    pub names: &'static [&'static str],
    pub args: &'static str,
    pub examples: &'static [&'static str],
    pub doc: &'static str,
}

const fn form(
    names: &'static [&'static str],
    args: &'static str,
    examples: &'static [&'static str],
    doc: &'static str,
) -> Form {
    Form {
        names,
        args,
        examples,
        doc,
    }
}

/// The form in `forms` answering to `name`, alias or not.
pub fn find(forms: &'static [Form], name: &str) -> Option<&'static Form> {
    forms.iter().find(|f| f.names.contains(&name))
}

/// Easing curves, shared by `animations` and `bar.fold-curve`.
pub const ANIMATION_CURVES: &[&str] = &["linear", "ease-in", "ease-out", "ease-in-out"];
pub const ANIMATION_DEFAULT_CURVE: &str = "ease-out";
pub const ANIMATION_DEFAULT_MS: u32 = 150;
/// A longer `duration` is refused, not clamped.
pub const ANIMATION_MAX_MS: u32 = 10_000;

/// `bar.motion.curve`: the [`ANIMATION_CURVES`] easings plus `spring`. Its
/// own list so `animations` keeps refusing `spring`; a test pins the prefix.
pub const BAR_MOTION_CURVES: &[&str] = &["linear", "ease-in", "ease-out", "ease-in-out", "spring"];

/// Built-in taskbar widget ids (ADR 0065). `custom:<name>` names a `widget`
/// block instead.
pub const BAR_WIDGET_IDS: &[&str] = &[
    "now-playing",
    "system-usage",
    "volume",
    "network",
    "bluetooth",
    "battery",
    "tray",
    "clock",
];
/// The prefix a custom widget's id carries in `bar.widgets.order`.
pub const BAR_WIDGET_CUSTOM_PREFIX: &str = "custom:";
pub const BAR_WIDGET_DEFAULT_ORDER: &[&str] = &[
    "now-playing",
    "volume",
    "network",
    "bluetooth",
    "battery",
    "tray",
    "clock",
];
pub const BAR_WIDGET_DEFAULT_IMPORTANT: &[&str] = &["clock", "battery"];
/// Built-in applet ids `bar.tray.pinned`/`hidden` carried before they became
/// widgets. Still accepted there, with a deprecation warning;
/// `eclipse-ctl config migrate` moves them to `bar.widgets.order`.
pub const LEGACY_TRAY_BUILTINS: &[&str] = &["network", "bluetooth", "battery", "volume"];

/// Shipped data sources a declarative `widget { source … }` can show. The
/// taskbar resolves them from its own feeds; `{}` in `format` becomes the
/// value (a whole percent for `usage.*` and `audio.volume`, text for
/// `media.*`).
pub const WIDGET_SOURCES: &[&str] = &[
    "usage.cpu",
    "usage.mem",
    "usage.gpu",
    "usage.disk",
    "audio.volume",
    "media.title",
    "media.artist",
];
/// `widget { interval-ms … }`: default and bounds.
pub const WIDGET_DEFAULT_INTERVAL_MS: u32 = 5_000;
pub const WIDGET_MIN_INTERVAL_MS: u32 = 250;
pub const WIDGET_MAX_INTERVAL_MS: u32 = 86_400_000;

/// Sub-nodes of `bar { widget "<name>" { … } }` (ADR 0065). Exactly one of
/// `exec` and `source`.
pub const WIDGET_KEYS: &[Form] = &[
    form(
        &["exec"],
        "\"<argv0>\" [\"<arg>\"…]",
        &[r#"exec "curl" "-s" "wttr.in/?format=1""#],
        "Run this command, argv-exec with no shell. Each run's output is one update: a line of plain text, or JSON `{text, detail, tooltip, state}`.",
    ),
    form(
        &["interval-ms"],
        "<250..86400000>",
        &["interval-ms 600000", r#"interval-ms "10m""#],
        "How often the `exec` command runs: milliseconds, or a string with a unit. Default 5000. Not with `stream`.",
    ),
    form(
        &["stream"],
        "[<bool>]",
        &["stream #true", "stream"],
        "Run the `exec` command once and keep it running; every line it prints is one update. Bare means `#true`.",
    ),
    form(
        &["source"],
        "\"<source>\"",
        &[r#"source "usage.cpu""#],
        "Show a shipped data source instead of running a command.",
    ),
    form(
        &["format"],
        "\"<text>\"",
        &[r#"format "{}%""#],
        "How a `source` value is shown; `{}` becomes the value. Default `{}`.",
    ),
    form(
        &["icon"],
        "\"<icon name or path>\"",
        &[r#"icon "weather-clear""#],
        "Icon drawn before the text.",
    ),
    form(
        &["on-click"],
        "\"<argv0>\" [\"<arg>\"…]",
        &[r#"on-click "xdg-open" "https://wttr.in""#],
        "Command run on a click, argv-exec.",
    ),
    form(
        &["on-scroll-up"],
        "\"<argv0>\" [\"<arg>\"…]",
        &[r#"on-scroll-up "brightnessctl" "set" "+5%""#],
        "Command run on a scroll up, argv-exec.",
    ),
    form(
        &["on-scroll-down"],
        "\"<argv0>\" [\"<arg>\"…]",
        &[r#"on-scroll-down "brightnessctl" "set" "5%-""#],
        "Command run on a scroll down, argv-exec.",
    ),
];

/// `animations { animation "<name>" duration=… curve=… }` (COMP-02 §9). Each
/// animation is off until named; an unknown name drops its node.
pub const ANIMATIONS: &[Form] = &[
    form(
        &["windows"],
        "",
        &[r#"animation "windows" duration="150ms" curve="ease-out""#],
        "A tiled or floating window sliding to its new position.",
    ),
    form(
        &["workspaces"],
        "",
        &[r#"animation "workspaces" duration=200"#],
        "The arriving workspace's windows sliding in from the side the switch came from.",
    ),
    form(
        &["fade"],
        "",
        &[r#"animation "fade" duration="1s" curve="linear""#],
        "A newly mapped window fading in from zero alpha.",
    ),
    form(
        &["border"],
        "",
        &[r#"animation "border" curve="ease-in-out""#],
        "A border crossfading between its active and inactive colour when focus changes.",
    ),
];

/// `windowrule` matchers (COMP-05 §4). All given matchers must match; a rule
/// with none is refused, since it would hit every window.
pub const RULE_MATCHERS: &[Form] = &[
    form(
        &["app-id"],
        "\"<regex>\"",
        &[r#"app-id "pavucontrol|org.gnome.Calculator""#],
        "Regex against the xdg-shell app id.",
    ),
    form(
        &["title"],
        "\"<regex>\"",
        &[r#"title "^Picture-in-Picture$""#],
        "Regex against the window title.",
    ),
    form(
        &["pid"],
        "<int>",
        &["pid 4242"],
        "The client's process id, positive.",
    ),
    form(
        &["xwayland"],
        "[<bool>]",
        &["xwayland", "xwayland #false"],
        "Whether the window is an X11 client. Bare means `#true`.",
    ),
    form(
        &["output"],
        "\"<glob>\"",
        &[r#"output "DP-*""#],
        "Glob (`*` only) against the connector or persistent identity of the output the window is on.",
    ),
    form(
        &["cgroup"],
        "\"<regex>\"",
        &[r#"cgroup "app-firefox""#],
        "Regex against the client's cgroup path from `/proc/<pid>/cgroup`.",
    ),
    form(
        &["workspace"],
        "<1..10>",
        &["workspace 3"],
        "The active workspace number of the window's output.",
    ),
];

/// Matchers COMP-05 §4 names that this build refuses, with the reason. The
/// whole rule is dropped, so it never applies to more windows than written.
pub const REFUSED_MATCHERS: &[(&str, &str)] = &[(
    "launching-principal",
    "needs COMP-08 launch tracking, which does not exist yet",
)];

/// Argument syntax of every [`RULE_ACTIONS`] entry. The action and its argument
/// are one string: `windowrule "size 800x600" { … }`.
pub const RULE_ACTION_FORMS: &[Form] = &[
    form(&["float"], "", &["float"], "Map floating."),
    form(
        &["tile"],
        "",
        &["tile"],
        "Map tiled, overriding the default float of a dialog or fixed-size window.",
    ),
    form(
        &["fullscreen"],
        "",
        &["fullscreen"],
        "Map fullscreen, after every other placement rule.",
    ),
    form(
        &["size"],
        "<W>x<H>",
        &["size 800x600"],
        "Floating size in logical px. Implies `float`.",
    ),
    form(
        &["position"],
        "<X>,<Y>",
        &["position 100,-40"],
        "Floating position in logical px. Implies `float`.",
    ),
    form(
        &["output"],
        "<glob>",
        &["output HDMI-A-1"],
        "Map on the output whose connector or identity matches.",
    ),
    form(
        &["opacity"],
        "<0.0..1.0>",
        &["opacity 0.85"],
        "Alpha for this window, replacing `decoration.active-opacity`/`inactive-opacity`.",
    ),
    form(
        &["blur"],
        "true | false",
        &["blur true", "blur false"],
        "Force blur on or off, overriding `decoration.blur.enabled`. An opaque window never blurs.",
    ),
    form(
        &["workspace"],
        "<1..10>",
        &["workspace 2"],
        "Map on this workspace.",
    ),
    form(
        &["no-focus-steal"],
        "",
        &["no-focus-steal"],
        "Do not take keyboard focus on map.",
    ),
    form(
        &["idle-inhibit"],
        "",
        &["idle-inhibit"],
        "Hold the idle timers off while the window is mapped.",
    ),
    form(
        &["sensitivity"],
        "secret | private",
        &["sensitivity secret", "sensitivity private"],
        "Raise the capture sensitivity class. Raise-only: `public` is refused.",
    ),
    form(
        &["app-trust"],
        "standard | trusted",
        &["app-trust standard", "app-trust trusted"],
        "Trust level (COMP-07 §2). Clamped to `standard` for X11 windows.",
    ),
    form(
        &["seat-compat"],
        "lock | multi",
        &["seat-compat lock", "seat-compat multi"],
        "Seat concurrency (COMP-07 §6). Clamped to `lock` for X11 windows.",
    ),
    form(&["no-agent"], "", &["no-agent"], "Hide the window from agents."),
];

/// Values `output … { lid-close … }` takes.
pub const LID_CLOSE: &[&str] = &["off", "suspend", "ignore"];
/// Values `output … { transform … }` takes; `0` is an alias of `normal`.
pub const OUTPUT_TRANSFORMS: &[&str] = &[
    "normal",
    "90",
    "180",
    "270",
    "flipped",
    "flipped-90",
    "flipped-180",
    "flipped-270",
];

/// Sub-keys of `output "<glob>" { … }` (COMP-03). The glob matches the
/// connector or the persistent identity; later blocks override earlier ones
/// key by key.
pub const OUTPUT_KEYS: &[Form] = &[
    form(&["mode"], "\"<W>x<H>[@<refresh>]\"", &[r#"mode "1920x1080@60""#, r#"mode "2560x1440""#], "Mode to set. Refresh in Hz or mHz."),
    form(&["position"], "<x> <y>", &["position 1920 0"], "Top-left corner in the global layout, logical px."),
    form(&["scale"], "<float>", &["scale 1.5", "scale 2"], "Scale factor, positive."),
    form(&["transform"], "\"<transform>\"", &[r#"transform "90""#, r#"transform "flipped-270""#], "Rotation and flip."),
    form(&["overscan"], "<px> | top= bottom= left= right=", &["overscan 30", "overscan top=20 left=40"], "Per-edge inset in physical px for a panel that crops the signal. Bare applies to all four edges. Wins over saved calibration."),
    form(&["enabled", "disabled"], "[<bool>]", &["enabled", "disabled", "enabled #false"], "Turn the output on or off. Bare `disabled` is off."),
    form(&["lid-close"], "\"<lid-close>\"", &[r#"lid-close "suspend""#, r#"lid-close "ignore""#, r#"lid-close "off""#], "On an internal panel: `off` turns the panel off (never the last output), `suspend` runs `systemctl suspend`, `ignore` does nothing."),
    form(&["vrr", "adaptive-sync"], "[<bool>]", &["vrr", "adaptive-sync #false"], "Variable refresh rate. Bare means `#true`."),
    form(&["number"], "<1..255>", &["number 2"], "Display number used by `move-to-output` (ADR 0049). Defaults to connection order."),
];

/// Actions a `bind` or `gesture` block accepts (COMP-13 §3).
pub const BIND_ACTIONS: &[Form] = &[
    form(
        &["spawn", "exec"],
        "<command>",
        &[r#"spawn "foot""#],
        "Run a command.",
    ),
    form(
        &["close-window", "killactive"],
        "",
        &["close-window"],
        "Ask the focused window to close.",
    ),
    form(
        &["toggle-floating"],
        "",
        &["toggle-floating"],
        "Float or tile the focused window.",
    ),
    form(&["minimize"], "", &["minimize"], "Send the focused window away."),
    form(
        &["unminimize", "restore"],
        "",
        &["unminimize"],
        "Bring back the last window sent away on the active workspace.",
    ),
    form(
        &["toggle-layout"],
        "",
        &["toggle-layout"],
        "Cycle the workspace layout: radiant, dwindle, master.",
    ),
    form(
        &["focus-left"],
        "",
        &["focus-left"],
        "Focus the neighbour to the left.",
    ),
    form(
        &["focus-right"],
        "",
        &["focus-right"],
        "Focus the neighbour to the right.",
    ),
    form(&["focus-up"], "", &["focus-up"], "Focus the neighbour above."),
    form(&["focus-down"], "", &["focus-down"], "Focus the neighbour below."),
    form(
        &["move-left"],
        "",
        &["move-left"],
        "Swap with the neighbour to the left (tiled windows only).",
    ),
    form(
        &["move-right"],
        "",
        &["move-right"],
        "Swap with the neighbour to the right.",
    ),
    form(&["move-up"], "", &["move-up"], "Swap with the neighbour above."),
    form(
        &["move-down"],
        "",
        &["move-down"],
        "Swap with the neighbour below.",
    ),
    form(
        &["priority-up"],
        "",
        &["priority-up"],
        "Give the focused tiled window a bigger share of its row or column (radiant layout).",
    ),
    form(
        &["priority-down"],
        "",
        &["priority-down"],
        "Give the focused tiled window a smaller share of its row or column (radiant layout).",
    ),
    form(
        &["workspace"],
        "<1..10>",
        &["workspace 3"],
        "Switch to a workspace.",
    ),
    form(
        &["workspace-next"],
        "",
        &["workspace-next"],
        "The workspace after the active one on the focused output.",
    ),
    form(
        &["workspace-prev"],
        "",
        &["workspace-prev"],
        "The workspace before the active one on the focused output.",
    ),
    form(
        &["move-to-workspace"],
        "<1..10>",
        &["move-to-workspace 3"],
        "Send the focused window to a workspace.",
    ),
    form(
        &["move-to-output"],
        "<1..255>",
        &["move-to-output 2"],
        "Send the focused window to display `number`'s active workspace.",
    ),
    form(
        &["agent-override"],
        "",
        &["agent-override"],
        "The reserved override chord (COMP-13 §1.1). Nothing to revoke until agent seats exist.",
    ),
    form(
        &["agent-attention"],
        "",
        &["agent-attention"],
        "The pending-decision-queue chord (COMP-10 §3.10). The queue arrives with the trusted UI.",
    ),
    form(
        &["annotation-select"],
        "",
        &["annotation-select"],
        "Start a region selection (COMP-18 §1.3).",
    ),
    form(
        &["annotation-dismiss"],
        "",
        &["annotation-dismiss"],
        "Forwarded to the annotation addon on the `keybind` event stream.",
    ),
    form(
        &["annotation-expand"],
        "",
        &["annotation-expand"],
        "Forwarded to the annotation addon.",
    ),
    form(
        &["annotation-auto-toggle"],
        "",
        &["annotation-auto-toggle"],
        "Forwarded to the annotation addon.",
    ),
    form(&["quit", "exit"], "", &["quit"], "Exit the compositor."),
];

/// Read a key back out of a live `Config`. `None` means the path is not in the
/// schema at all; `Some(Value::Null)` means it is a key that is currently unset.
///
/// This is the other half of the write path: `get_config` answers with it, and
/// the drift test below asserts it agrees with the `default` column on a
/// default `Config` for every single row.
pub fn get(c: &Config, path: &str) -> Option<Value> {
    use Value as V;
    let s = |o: &Option<String>| o.clone().map_or(V::Null, V::Str);
    let n = |o: &Option<u64>| o.map_or(V::Null, |v| V::Int(v as i64));
    let list = |v: &[String]| V::List(v.to_vec());
    Some(match path {
        "general.gaps-in" => V::Int(c.general.gaps_in as i64),
        "general.gaps-out" => V::Int(c.general.gaps_out as i64),
        "general.border-size" => V::Int(c.general.border_size as i64),
        "general.layout" => V::Str(layout_name(c.general.layout).into()),
        "general.floating-placement" => V::Str(floating_placement_name(c.general.floating_placement).into()),
        "general.focus-follows-mouse" => V::Bool(c.general.focus_follows_mouse),
        "general.focus-follows-mouse-across-outputs" => V::Bool(c.general.focus_follows_mouse_across_outputs),
        "general.cursor-follows-moved-window" => V::Bool(c.general.cursor_follows_moved_window),
        "general.follow-window-to-workspace" => V::Bool(c.general.follow_window_to_workspace),
        "general.unfocus-on-empty-workspace" => V::Bool(c.general.unfocus_on_empty_workspace),
        "general.focus-follows-mouse-layers" => V::Bool(c.general.focus_follows_mouse_layers),
        "general.refocus-on-scene-change" => V::Bool(c.general.refocus_on_scene_change),
        "general.col-active-border" => V::Color(c.general.col_active),
        "general.col-inactive-border" => V::Color(c.general.col_inactive),
        "general.drop-guides" => V::Bool(c.general.drop_guides),
        "general.drop-guide-color" => V::Color(c.general.drop_guide_color),
        "general.drop-edge-band" => V::Int(c.general.drop_edge_band as i64),
        "render.direct-scanout" => V::Bool(c.render.direct_scanout),
        "bar.fold-when-inactive" => V::Bool(c.bar.fold_when_inactive),
        "bar.fold-height" => V::Int(c.bar.fold_height as i64),
        "bar.fold-when-idle" => V::Bool(c.bar.fold_when_idle),
        "bar.idle-seconds" => V::Int(c.bar.idle_seconds as i64),
        "bar.fold-duration-ms" => V::Int(c.bar.fold_duration_ms as i64),
        "bar.fold-curve" => V::Str(c.bar.fold_curve.clone()),
        "bar.position" => V::Str(position_name(c.bar.position).into()),
        "bar.tray.pinned" => c.bar.tray.pinned.as_deref().map_or(V::Null, list),
        "bar.tray.hidden" => list(&c.bar.tray.hidden),
        "bar.rounding" => V::Int(c.bar.rounding as i64),
        "bar.clock.hour-12" => V::Bool(c.bar.clock.hour_12),
        "bar.clock.date-mdy" => V::Bool(c.bar.clock.date_mdy),
        "bar.eye" => V::Bool(c.bar.eye),
        "bar.widgets.order" => list(&c.bar.widgets.order),
        "bar.widgets.important" => list(&c.bar.widgets.important),
        "bar.widgets.now-playing.art" => V::Bool(c.bar.widgets.now_playing.art),
        "bar.widgets.now-playing.visualizer" => V::Bool(c.bar.widgets.now_playing.visualizer),
        "bar.widgets.now-playing.remote-art" => V::Bool(c.bar.widgets.now_playing.remote_art),
        "bar.widgets.system-usage.interval-ms" => V::Int(c.bar.widgets.system_usage.interval_ms as i64),
        "bar.widgets.system-usage.gpu" => V::Bool(c.bar.widgets.system_usage.gpu),
        "bar.widgets.system-usage.disk" => V::Bool(c.bar.widgets.system_usage.disk),
        "bar.widgets.system-usage.disk-path" => V::Str(c.bar.widgets.system_usage.disk_path.clone()),
        "bar.widgets.volume.step" => V::Int(c.bar.widgets.volume.step as i64),
        "bar.widgets.volume.scroll" => V::Bool(c.bar.widgets.volume.scroll),
        "bar.widgets.volume.max-percent" => V::Int(c.bar.widgets.volume.max_percent as i64),
        "bar.motion.enabled" => V::Bool(c.bar.motion.enabled),
        "bar.motion.duration-ms" => V::Int(c.bar.motion.duration_ms as i64),
        "bar.motion.curve" => V::Str(c.bar.motion.curve.clone()),
        "bar.popup-anchor" => V::Str(
            match c.bar.popup_anchor {
                BarPopupAnchor::Cell => "cell",
                BarPopupAnchor::Pointer => "pointer",
            }
            .into(),
        ),
        "decoration.rounding" => V::Int(c.decoration.rounding as i64),
        "decoration.active-opacity" => V::Float(c.decoration.active_opacity as f64),
        "decoration.inactive-opacity" => V::Float(c.decoration.inactive_opacity as f64),
        "decoration.dim-inactive" => V::Float(c.decoration.dim_inactive as f64),
        "decoration.blur.enabled" => V::Bool(c.decoration.blur.enabled),
        "decoration.blur.size" => V::Int(c.decoration.blur.size as i64),
        "decoration.blur.passes" => V::Int(c.decoration.blur.passes as i64),
        "decoration.shadow.enabled" => V::Bool(c.decoration.shadow.enabled),
        "decoration.shadow.range" => V::Int(c.decoration.shadow.range as i64),
        "animations.enabled" => V::Bool(c.animations.enabled),
        "xwayland.enable" => V::Bool(c.xwayland.enable),
        "xwayland.scaling" => V::Str(
            if c.xwayland.scaling_client {
                "client"
            } else {
                "compositor"
            }
            .into(),
        ),
        "idle.dpms-timeout-seconds" => n(&c.idle.dpms_timeout),
        "idle.lock-timeout-seconds" => n(&c.idle.lock_timeout),
        "idle.lock-command" => s(&c.idle.lock_command),
        "input.kb-layout" => V::Str(c.input.kb_layout.clone()),
        "input.kb-variant" => V::Str(c.input.kb_variant.clone()),
        "input.kb-options" => s(&c.input.kb_options),
        "input.repeat-rate" => V::Int(c.input.repeat_rate as i64),
        "input.repeat-delay" => V::Int(c.input.repeat_delay as i64),
        "input.accel-profile" => V::Str(c.input.accel_profile.clone()),
        "input.touchpad.natural-scroll" => V::Bool(c.input.touchpad.natural_scroll),
        "input.touchpad.tap-to-click" => V::Bool(c.input.touchpad.tap_to_click),
        "input.touchpad.dwt" => V::Bool(c.input.touchpad.dwt),
        "misc.render-device" => s(&c.misc.render_device),
        "misc.terminal-command" => s(&c.misc.terminal_command),
        "misc.scripted-input" => V::Bool(c.misc.scripted_input),
        "clipboard.data-control-allow" => list(&c.clipboard.data_control_allow),
        "capture.allow" => list(&c.capture.allow),
        "capture.redact-app-id" => list(&c.capture.redact_app_id),
        "capture.hide-layer" => V::List(
            c.capture
                .hide_layer
                .iter()
                .map(|(exe, ns)| format!("{exe}:{ns}"))
                .collect(),
        ),
        _ => return None,
    })
}

fn layout_name(l: LayoutKind) -> &'static str {
    match l {
        LayoutKind::Radiant => "radiant",
        LayoutKind::Dwindle => "dwindle",
        LayoutKind::Master => "master",
    }
}

fn floating_placement_name(p: FloatingPlacement) -> &'static str {
    match p {
        FloatingPlacement::Centered => "centered",
        FloatingPlacement::Pointer => "pointer",
        FloatingPlacement::Cascade => "cascade",
    }
}

fn position_name(p: BarPosition) -> &'static str {
    match p {
        BarPosition::Top => "top",
        BarPosition::Bottom => "bottom",
    }
}

impl Dv {
    /// The default as a [`Value`], so it can be compared with [`get`].
    pub fn as_value(self) -> Value {
        match self {
            Dv::Null => Value::Null,
            Dv::Bool(b) => Value::Bool(b),
            Dv::Int(i) => Value::Int(i),
            Dv::Float(f) => Value::Float(f),
            Dv::Str(s) => Value::Str(s.to_string()),
            Dv::EmptyList => Value::List(Vec::new()),
            Dv::List(l) => Value::List(l.iter().map(|s| s.to_string()).collect()),
            Dv::Color(c) => Value::Color(c),
        }
    }

    /// The default written as KDL source, for `config describe` and for
    /// synthesising a config in the drift test.
    pub fn to_kdl(self) -> Option<String> {
        Some(match self {
            Dv::Null => return None,
            Dv::Bool(b) => format!("#{b}"),
            Dv::Int(i) => i.to_string(),
            Dv::Float(f) => format!("{f:?}"),
            Dv::Str(s) => format!("{s:?}"),
            Dv::EmptyList => String::new(),
            Dv::List(l) => l.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(" "),
            Dv::Color(c) => format!(
                "\"#{:02x}{:02x}{:02x}{:02x}\"",
                (c[0] * 255.0).round() as u8,
                (c[1] * 255.0).round() as u8,
                (c[2] * 255.0).round() as u8,
                (c[3] * 255.0).round() as u8
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::edit;
    use kdl::KdlDocument;

    /// Build a config file that sets every scalar key to a value of the right
    /// shape, using the real write path to do it.
    fn synthesised() -> String {
        let mut text = String::new();
        for key in TABLE {
            let Some(v) = sample(key) else { continue };
            text = edit::set_value(&text, key.path, &v).unwrap_or_else(|e| panic!("{}: {e}", key.path));
        }
        text
    }

    /// A legal, non-default value for a key — used to prove the parser accepts
    /// the key *and* that it lands where `get` says it does.
    fn sample(key: &Key) -> Option<kdl::KdlValue> {
        use kdl::KdlValue as K;
        Some(match key.ty {
            Ty::Bool => K::Bool(!matches!(key.default, Dv::Bool(true))),
            Ty::Int { min, max } => K::Integer((min + 1).min(max) as i128),
            Ty::Float { min, max } => K::Float((min + max) / 2.0),
            Ty::Enum(vs) => K::String(vs[vs.len() - 1].to_string()),
            Ty::Color => K::String("#0a0b0c0d".into()),
            Ty::Str => K::String(match key.path {
                // Not free-form: each of these is validated on its own terms.
                // "auto" is normalised to `None` by the parser — it means
                // "ask smithay", which is the unset state, not a value.
                "misc.render-device" => "/dev/dri/card1".into(),
                "input.kb-layout" => "de".into(),
                "bar.widgets.system-usage.disk-path" => "/home".into(),
                _ => "x".to_string(),
            }),
            // A list node is not a scalar assignment; covered by its own row on
            // the GUI-coverage exception list, not by the splice path.
            Ty::StrList => return None,
        })
    }

    /// Side 1: every key the schema claims exists is a key the parser accepts.
    /// A typo in `path`, or a key deleted from the parser, fails here.
    #[test]
    fn every_schema_key_is_accepted_by_the_parser() {
        let text = synthesised();
        let doc: KdlDocument = text.parse().expect("synthesised config parses");
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(
            cfg.errors.is_empty(),
            "parser rejected schema-generated config:\n{}\n{:#?}",
            text,
            cfg.errors
        );
    }

    /// …and the value lands in the field `get` reads, so the two halves of the
    /// round trip name the same thing.
    #[test]
    fn a_written_value_reads_back_through_get() {
        let text = synthesised();
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        for key in TABLE {
            let Some(want) = sample(key) else { continue };
            let got = get(&cfg, key.path).unwrap_or_else(|| panic!("get({}) is None", key.path));
            let ok = match (&got, &want) {
                (Value::Bool(a), kdl::KdlValue::Bool(b)) => a == b,
                (Value::Int(a), kdl::KdlValue::Integer(b)) => *a as i128 == *b,
                (Value::Float(a), kdl::KdlValue::Float(b)) => (a - b).abs() < 1e-6,
                (Value::Str(a), kdl::KdlValue::String(b)) => a == b,
                (Value::Color(_), kdl::KdlValue::String(_)) => true,
                _ => false,
            };
            assert!(ok, "{}: wrote {want}, read back {got:?}", key.path);
        }
    }

    /// Side 3: `get` agrees with the `default` column on a default `Config`.
    /// This is what lets `get_config` report `source: null` for a key at its
    /// default without consulting any file.
    #[test]
    fn get_agrees_with_the_default_column() {
        let cfg = Config::default();
        for key in TABLE {
            let got = get(&cfg, key.path).unwrap_or_else(|| panic!("{} missing from get", key.path));
            assert_eq!(got, key.default.as_value(), "default drift at {}", key.path);
        }
    }

    /// Paths are unique, dotted, and never empty-segmented — `get` and the
    /// splice path both index on them.
    #[test]
    fn paths_are_well_formed_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for key in TABLE {
            assert!(seen.insert(key.path), "duplicate path {}", key.path);
            assert!(
                key.path.split('.').all(|s| !s.is_empty()),
                "malformed path {}",
                key.path
            );
        }
        for (a, _) in RULE_ACTIONS {
            assert!(seen.insert(a), "rule action collides with a key path: {a}");
        }
    }

    fn accepts(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert!(cfg.errors.is_empty(), "parser rejected {text}: {:#?}", cfg.errors);
        cfg
    }

    /// Every documented form is one the parser takes: each example, wrapped in
    /// its construct, parses with no error and produces what it should.
    #[test]
    fn every_documented_form_parses() {
        for f in ANIMATIONS {
            for ex in f.examples {
                let cfg = accepts(&format!("animations {{ enabled #true; {ex}; }}"));
                assert!(cfg.animations.get(f.names[0]).is_some(), "{ex}");
            }
        }
        for f in RULE_MATCHERS {
            for ex in f.examples {
                assert!(ex.starts_with(f.names[0]), "{ex}");
                let cfg = accepts(&format!("windowrule \"float\" {{ {ex}; }}"));
                assert_eq!(cfg.window_rules.len(), 1, "{ex}");
            }
        }
        for f in RULE_ACTION_FORMS {
            for ex in f.examples {
                assert!(ex.starts_with(f.names[0]), "{ex}");
                let cfg = accepts(&format!("windowrule \"{ex}\" {{ app-id \"a\"; }}"));
                assert_eq!(cfg.window_rules.len(), 1, "{ex}");
            }
        }
        for f in OUTPUT_KEYS {
            for ex in f.examples {
                assert!(f.names.iter().any(|n| ex.starts_with(n)), "{ex}");
                let cfg = accepts(&format!("output \"DP-1\" {{ {ex}; }}"));
                assert_eq!(cfg.outputs.len(), 1, "{ex}");
            }
        }
        for t in OUTPUT_TRANSFORMS {
            accepts(&format!("output \"DP-1\" {{ transform \"{t}\"; }}"));
        }
        for v in LID_CLOSE {
            accepts(&format!("output \"DP-1\" {{ lid-close \"{v}\"; }}"));
        }
        for f in BIND_ACTIONS {
            let arg = f.examples[0]
                .strip_prefix(f.names[0])
                .expect("example starts with its name");
            for name in f.names {
                let text = format!("bind \"SUPER\" \"F9\" {{ {name}{arg}; }}");
                let doc: KdlDocument = text.parse().unwrap();
                let mut cfg = Config::default();
                let mut binds = Vec::new();
                cfg.apply(&doc, &mut binds);
                assert!(cfg.errors.is_empty(), "{text}: {:#?}", cfg.errors);
                assert!(
                    binds
                        .iter()
                        .any(|b| b.key == smithay::input::keyboard::Keysym::F9),
                    "{text}"
                );
            }
        }
        for f in WIDGET_KEYS {
            // Each example inside a block that is valid around it: exec and
            // source stand alone, format needs a source, the rest an exec.
            let base = match f.names[0] {
                "exec" | "source" => "",
                "format" => "source \"usage.cpu\"; ",
                _ => "exec \"true\"; ",
            };
            for ex in f.examples {
                assert!(ex.starts_with(f.names[0]), "{ex}");
                let cfg = accepts(&format!("bar {{ widget \"w\" {{ {base}{ex}; }} }}"));
                assert_eq!(cfg.bar.custom_widgets.len(), 1, "{ex}");
            }
        }
        for s in WIDGET_SOURCES {
            accepts(&format!("bar {{ widget \"w\" {{ source \"{s}\"; }} }}"));
        }
    }

    /// `bar.motion.curve` is the `animations` easings plus `spring`, and
    /// `animations` itself still refuses `spring`.
    #[test]
    fn bar_motion_curves_extend_animation_curves() {
        assert_eq!(&BAR_MOTION_CURVES[..ANIMATION_CURVES.len()], ANIMATION_CURVES);
        assert_eq!(&BAR_MOTION_CURVES[ANIMATION_CURVES.len()..], ["spring"]);
        assert!(!ANIMATION_CURVES.contains(&"spring"));
    }

    /// A non-empty list default is an ordering, never an allow-list entry.
    #[test]
    fn no_policy_key_has_a_non_empty_default() {
        for key in TABLE {
            if key.owner == Policy {
                assert!(!matches!(key.default, Dv::List(_)), "{}", key.path);
            }
        }
    }

    /// The default order and important set name only built-in widgets.
    #[test]
    fn widget_defaults_are_built_in_ids() {
        for id in BAR_WIDGET_DEFAULT_ORDER
            .iter()
            .chain(BAR_WIDGET_DEFAULT_IMPORTANT)
        {
            assert!(BAR_WIDGET_IDS.contains(id), "{id}");
        }
        for id in LEGACY_TRAY_BUILTINS {
            assert!(BAR_WIDGET_DEFAULT_ORDER.contains(id), "{id}");
        }
    }

    /// Every transform the output layer can name is documented.
    #[test]
    fn output_transforms_are_complete() {
        use smithay::utils::Transform;
        for t in [
            Transform::Normal,
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
            Transform::Flipped90,
            Transform::Flipped180,
            Transform::Flipped270,
        ] {
            assert!(OUTPUT_TRANSFORMS.contains(&crate::outputs::transform_name(t)));
        }
    }

    /// Every rule action has exactly one syntax entry, in the same order.
    #[test]
    fn rule_action_forms_cover_rule_actions() {
        let forms: Vec<_> = RULE_ACTION_FORMS.iter().map(|f| f.names[0]).collect();
        let actions: Vec<_> = RULE_ACTIONS.iter().map(|(a, _)| *a).collect();
        assert_eq!(forms, actions);
    }

    /// A refused matcher is refused, and says why.
    #[test]
    fn refused_matchers_drop_their_rule_with_the_reason() {
        for (m, why) in REFUSED_MATCHERS {
            assert!(find(RULE_MATCHERS, m).is_none());
            let doc: KdlDocument = format!("windowrule \"float\" {{ {m} \"x\"; }}").parse().unwrap();
            let mut cfg = Config::default();
            cfg.apply(&doc, &mut Vec::new());
            assert!(cfg.window_rules.is_empty());
            assert!(
                cfg.errors.iter().any(|e| e.message.contains(why)),
                "{:#?}",
                cfg.errors
            );
        }
    }

    /// The security surface is small and enumerated. A key moving into or out
    /// of `policy.kdl` is a decision with a review attached, not a diff nobody
    /// notices — so the list is pinned here.
    #[test]
    fn the_policy_owned_set_is_exactly_this() {
        let policy: Vec<_> = TABLE
            .iter()
            .filter(|k| k.owner == Policy)
            .map(|k| k.path)
            .collect();
        assert_eq!(
            policy,
            [
                "misc.scripted-input",
                "clipboard.data-control-allow",
                "capture.allow",
                "capture.redact-app-id",
                "capture.hide-layer",
            ]
        );
        let rules: Vec<_> = RULE_ACTIONS
            .iter()
            .filter(|(_, o)| *o == Policy)
            .map(|(a, _)| *a)
            .collect();
        assert_eq!(rules, ["sensitivity", "app-trust", "seat-compat", "no-agent"]);
    }
}
