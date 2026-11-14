// SPDX-License-Identifier: AGPL-3.0-only
//! The key rows. One macro invocation produces `TABLE` and `META` in the same order
//! (T-10 §1), so the pair cannot drift. Edit rows here only.

use crate::types::{Dv, Key, Meta, OverrideClass, Reload, Scope, Ty};

const LIGATURES: &[&str] = &["always", "cursor", "never"];
const WIDTH_METHODS: &[&str] = &["unicode", "wcswidth"];
const AMBIGUOUS: &[&str] = &["narrow", "wide"];
const UNICODE_VERSIONS: &[&str] = &["latest", "15.1", "16.0"];
const ALPHA_MODES: &[&str] = &["auto", "premultiplied", "postmultiplied", "opaque"];
const BLUR_FALLBACK: &[&str] = &["tint", "opaque"];
const DECORATIONS: &[&str] = &["auto", "client", "server", "none"];
const MOTION: &[&str] = &["full", "reduced", "none"];
const TABS_SHOW: &[&str] = &["multiple", "always", "never"];
const TABS_STYLE: &[&str] = &["floating", "flat", "hidden"];
const TABS_POS: &[&str] = &["top", "bottom"];
const NEW_TAB_POS: &[&str] = &["after-current", "end"];
const PANES_FRAME: &[&str] = &["full", "titles", "none"];
const REMAIN: &[&str] = &["never", "on-failure", "always"];
const CLOSE_CONFIRM: &[&str] = &["never", "if-running", "always"];
const SCROLLBAR_MODE: &[&str] = &["auto", "always", "never"];
const CURSOR_STYLE: &[&str] = &["block", "underline", "bar"];
const CURSOR_UNFOCUSED: &[&str] = &["hollow", "block", "underline", "bar", "none"];
const MODS: &[&str] = &["shift", "ctrl", "alt", "super"];
const MOK: &[&str] = &["off", "1", "2", "3"];
const FOK: &[&str] = &["xterm", "csi-u"];
const COMPOSE: &[&str] = &["xkb", "builtin", "off"];
const XKB_BACKEND: &[&str] = &["kbvm", "libxkbcommon"];
const BACKSPACE: &[&str] = &["del", "bs"];
const WORD_MODE: &[&str] = &["delimiters", "chars", "unicode"];
const TRIPLE: &[&str] = &["line", "output"];
const NOTIFY_WHEN: &[&str] = &["unfocused", "invisible", "always", "never"];
const TERMINFO_MODE: &[&str] = &["path", "direct", "none"];
const FILE_TX: &[&str] = &["off", "local-only"];
const HWDEC: &[&str] = &["auto", "vaapi", "vulkan", "off"];
const AUDIO_OUT: &[&str] = &["auto", "pulse", "off"];
const SYNC_MASTER: &[&str] = &["audio", "video"];
const AUTOPLAY: &[&str] = &["focused", "never", "always"];
const OFFSCREEN: &[&str] = &["pause", "stop"];
const SHM_TX: &[&str] = &["local-only", "off"];
const BLENDING: &[&str] = &["linear-corrected", "linear", "native"];
const POWER: &[&str] = &["auto", "low", "high"];
const GUARD: &[&str] = &["auto", "always", "never"];
const PASTE_NL: &[&str] = &["as-is", "cr", "confirm"];
const A11Y: &[&str] = &["auto", "on", "off"];
const OFF_ON: &[&str] = &["off", "on"];
const RESTORE: &[&str] = &["ask", "always", "never"];
const QT_ANIM: &[&str] = &["slide", "fade", "none"];
const QT_POS: &[&str] = &["top", "bottom", "left", "right"];
const OPEN_PLACEMENT: &[&str] = &["float", "inline", "window"];

const MAP_RANGE_FONT: Ty = Ty::Map {
    key: &Ty::CodepointRanges,
    value: &Ty::Font,
};
const MAP_RANGE_WIDTH: Ty = Ty::Map {
    key: &Ty::CodepointRanges,
    value: &Ty::Int { min: 1, max: 2 },
};
const MAP_STR_STR: Ty = Ty::Map {
    key: &Ty::Str,
    value: &Ty::Str,
};
const MAP_STR_STRSET: Ty = Ty::Map {
    key: &Ty::Str,
    value: &Ty::StringSet,
};
const MAP_STR_STRLIST: Ty = Ty::Map {
    key: &Ty::Str,
    value: &Ty::StrList,
};
const MAP_PATH_STRSET: Ty = Ty::Map {
    key: &Ty::Path,
    value: &Ty::StringSet,
};

macro_rules! rows {
    ( $( ($path:literal, $ty:expr, $dv:expr, $scope:ident, $prof:literal, $rl:ident, $cl:ident, $doc:literal; $label:literal, $kw:expr) ),* $(,)? ) => {
        /// Every key, in schema order.
        pub static TABLE: &[Key] = &[ $( Key { path: $path, ty: $ty, default: $dv, scope: Scope::$scope, profilable: $prof, reload: Reload::$rl, class: OverrideClass::$cl, doc: $doc } ),* ];
        /// Presentation rows, one per `TABLE` row, same order.
        pub static META: &[Meta] = &[ $( Meta::new($path, $label, $kw) ),* ];
    };
}

rows! {
    ("font.family", Ty::Font, Dv::Str("monospace"), Pane, true, Live, Appearance, "Primary font family."; "Font family", &["typeface"]),
    ("font.size", Ty::float(4.0, 96.0), Dv::Float(12.0), Pane, true, Live, Appearance, "Font size in points."; "Font size", &["zoom","text size"]),
    ("font.bold-family", Ty::Font, Dv::Null, Pane, true, Live, Appearance, "Bold font family; unset derives it from the primary family."; "Bold font", &[]),
    ("font.italic-family", Ty::Font, Dv::Null, Pane, true, Live, Appearance, "Italic font family; unset derives it from the primary family."; "Italic font", &[]),
    ("font.bold-italic-family", Ty::Font, Dv::Null, Pane, true, Live, Appearance, "Bold italic font family; unset derives it from the primary family."; "Bold italic font", &[]),
    ("font.emoji-family", Ty::Font, Dv::Null, Pane, true, Live, Appearance, "Emoji font family; unset derives it from the primary family."; "Emoji font", &[]),
    ("font.fallback", Ty::StrList, Dv::Empty, Pane, true, Live, Appearance, "Fallback families tried in order."; "Fallback fonts", &[]),
    ("font.features", Ty::FontFeatures, Dv::Empty, Pane, true, Live, Appearance, "OpenType feature tags applied to every run (+liga, -calt, ss01)."; "Font features", &[]),
    ("font.ligatures", Ty::Enum(LIGATURES), Dv::Str("always"), Pane, true, Live, Appearance, "Ligature mode: always, only away from the cursor, or never."; "Ligatures", &["ligature","calt"]),
    ("font.synthetic", Ty::Bool, Dv::Bool(true), Pane, true, Live, Appearance, "Synthesise bold and italic when the family lacks them."; "Synthetic styles", &[]),
    ("font.thicken", Ty::Bool, Dv::Bool(false), Pane, true, Live, Appearance, "Embolden glyph rasterisation slightly."; "Thicken glyphs", &[]),
    ("font.symbol-fallback", Ty::Bool, Dv::Bool(true), Pane, true, Live, Appearance, "Fall back to a symbols font for private-use codepoints."; "Symbols fallback", &[]),
    ("font.zoom-step", Ty::float(0.1, 8.0), Dv::Float(1.0), Window, false, Live, Behaviour, "Points added or removed by font.size actions."; "Zoom step", &[]),
    ("font.adjust-cell-width", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Cell width adjustment with an explicit unit: 2px or -5%."; "Adjust cell width", &["metrics"]),
    ("font.adjust-cell-height", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Cell height adjustment with an explicit unit: 2px or -5%."; "Adjust cell height", &["metrics"]),
    ("font.adjust-baseline", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Baseline adjustment with an explicit unit: 2px or -5%."; "Adjust baseline", &["metrics"]),
    ("font.adjust-underline-position", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Underline position adjustment with an explicit unit: 2px or -5%."; "Adjust underline position", &["metrics"]),
    ("font.adjust-underline-thickness", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Underline thickness adjustment with an explicit unit: 2px or -5%."; "Adjust underline thickness", &["metrics"]),
    ("font.adjust-strikethrough-position", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Strikethrough position adjustment with an explicit unit: 2px or -5%."; "Adjust strikethrough position", &["metrics"]),
    ("font.adjust-strikethrough-thickness", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Strikethrough thickness adjustment with an explicit unit: 2px or -5%."; "Adjust strikethrough thickness", &["metrics"]),
    ("font.adjust-overline-position", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Overline position adjustment with an explicit unit: 2px or -5%."; "Adjust overline position", &["metrics"]),
    ("font.adjust-overline-thickness", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Overline thickness adjustment with an explicit unit: 2px or -5%."; "Adjust overline thickness", &["metrics"]),
    ("font.adjust-cursor-thickness", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Cursor thickness adjustment with an explicit unit: 2px or -5%."; "Adjust cursor thickness", &["metrics"]),
    ("font.adjust-cursor-height", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Cursor height adjustment with an explicit unit: 2px or -5%."; "Adjust cursor height", &["metrics"]),
    ("font.adjust-box-thickness", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Box-drawing thickness adjustment with an explicit unit: 2px or -5%."; "Adjust box-drawing thickness", &["metrics"]),
    ("font.adjust-icon-height", Ty::Adjust, Dv::Str("0px"), Pane, true, Live, Appearance, "Icon height adjustment with an explicit unit: 2px or -5%."; "Adjust icon height", &["metrics"]),
    ("font.map", MAP_RANGE_FONT, Dv::Empty, Global, false, Live, Security, "Codepoint ranges drawn with a named font (symbol map)."; "Symbol map", &["nerd font","icons"]),
    ("font.cell-width-override", MAP_RANGE_WIDTH, Dv::Empty, Global, false, Live, Security, "Codepoint ranges forced to 1 or 2 cells (narrow symbols)."; "Cell width overrides", &[]),
    ("font.builtin-glyphs.box", Ty::Bool, Dv::Bool(true), Global, false, Live, Appearance, "Draw box glyphs procedurally instead of from the font."; "Built-in box glyphs", &[]),
    ("font.builtin-glyphs.block", Ty::Bool, Dv::Bool(true), Global, false, Live, Appearance, "Draw block glyphs procedurally instead of from the font."; "Built-in block glyphs", &[]),
    ("font.builtin-glyphs.braille", Ty::Bool, Dv::Bool(true), Global, false, Live, Appearance, "Draw braille glyphs procedurally instead of from the font."; "Built-in braille glyphs", &[]),
    ("font.builtin-glyphs.powerline", Ty::Bool, Dv::Bool(true), Global, false, Live, Appearance, "Draw powerline glyphs procedurally instead of from the font."; "Built-in powerline glyphs", &[]),
    ("font.builtin-glyphs.legacy", Ty::Bool, Dv::Bool(true), Global, false, Live, Appearance, "Draw legacy glyphs procedurally instead of from the font."; "Built-in legacy glyphs", &[]),
    ("font.builtin-glyphs.octants", Ty::Bool, Dv::Bool(true), Global, false, Live, Appearance, "Draw octants glyphs procedurally instead of from the font."; "Built-in octants glyphs", &[]),
    ("colors.foreground", Ty::Color, Dv::Null, Pane, true, Live, Appearance, "Foreground; unset takes it from the theme."; "Foreground", &[]),
    ("colors.background", Ty::Color, Dv::Null, Pane, true, Live, Appearance, "Background; unset takes it from the theme."; "Background", &[]),
    ("colors.cursor", Ty::Color, Dv::Null, Pane, true, Live, Appearance, "Cursor colour; unset takes it from the theme."; "Cursor colour", &[]),
    ("colors.cursor-text", Ty::Color, Dv::Null, Pane, true, Live, Appearance, "Cursor text; unset takes it from the theme."; "Cursor text", &[]),
    ("colors.selection-foreground", Ty::Color, Dv::Null, Pane, true, Live, Appearance, "Selection foreground; unset takes it from the theme."; "Selection foreground", &[]),
    ("colors.selection-background", Ty::Color, Dv::Null, Pane, true, Live, Appearance, "Selection background; unset takes it from the theme."; "Selection background", &[]),
    ("colors.link", Ty::Color, Dv::Null, Pane, true, Live, Appearance, "Link colour; unset takes it from the theme."; "Link colour", &[]),
    ("colors.bold", Ty::Color, Dv::Null, Pane, true, Live, Appearance, "Bold colour; unset takes it from the theme."; "Bold colour", &[]),
    ("colors.palette", Ty::Palette16, Dv::Null, Pane, true, Live, Appearance, "The 16 ANSI colours; unset takes them from the theme."; "ANSI palette", &[]),
    ("colors.minimum-contrast", Ty::float(1.0, 21.0), Dv::Float(1.0), Pane, true, Live, Appearance, "Nudge foreground lightness until this contrast ratio holds; 1 disables."; "Minimum contrast", &["wcag","readability"]),
    ("colors.minimum-contrast-builtin-glyphs", Ty::Bool, Dv::Bool(false), Pane, true, Live, Appearance, "Apply minimum contrast to built-in glyphs too."; "Contrast for built-in glyphs", &[]),
    ("colors.dim-factor", Ty::float(0.0, 1.0), Dv::Float(0.6), Pane, true, Live, Appearance, "Brightness of faint (SGR 2) text."; "Faint text strength", &[]),
    ("colors.bold-is-bright", Ty::Bool, Dv::Bool(false), Pane, true, Live, Appearance, "Draw bold text with the bright palette variant."; "Bold is bright", &[]),
    ("theme", Ty::ThemeRef, Dv::Computed { id: "theme", doc: "System theme on Abyss, otherwise the stock dark/light pair." }, Window, true, Live, Appearance, "Colour source: system, a named scheme, or light=X dark=Y."; "Theme", &["scheme","dark","light"]),
    ("unicode.width-method", Ty::Enum(WIDTH_METHODS), Dv::Str("unicode"), Pane, true, Live, Behaviour, "How cluster width is computed (mode 2027 default)."; "Width method", &[]),
    ("unicode.ambiguous-width", Ty::Enum(AMBIGUOUS), Dv::Str("narrow"), Pane, true, Live, Behaviour, "Width of East Asian ambiguous characters."; "Ambiguous width", &[]),
    ("unicode.version", Ty::Enum(UNICODE_VERSIONS), Dv::Str("latest"), Pane, true, Live, Behaviour, "Unicode width table version."; "Unicode version", &[]),
    ("unicode.bidi.swap-arrows", Ty::Bool, Dv::Bool(false), Pane, true, Live, Behaviour, "Swap arrow-key meaning on right-to-left lines."; "Swap arrows in RTL", &[]),
    ("window.opacity", Ty::float(0.0, 1.0), Dv::Computed { id: "window.opacity", doc: "0.82 while a blur is available, otherwise 1." }, Window, true, Live, Appearance, "Background opacity of default-background cells."; "Opacity", &["transparency","blur"]),
    ("window.alpha-mode", Ty::Enum(ALPHA_MODES), Dv::Str("auto"), Window, false, Live, Appearance, "Surface alpha compositing mode."; "Alpha mode", &[]),
    ("window.blur-fallback", Ty::Enum(BLUR_FALLBACK), Dv::Str("tint"), Window, false, Live, Appearance, "What to draw when the compositor offers no blur."; "Blur fallback", &[]),
    ("window.decorations", Ty::Enum(DECORATIONS), Dv::Str("auto"), Window, false, NewWindow, Appearance, "Window frame: auto, client, server or none."; "Decorations", &[]),
    ("window.hide-chrome", Ty::Bool, Dv::Bool(false), Window, true, Live, Appearance, "Hide tab strip and frame chrome."; "Hide chrome", &[]),
    ("window.resize-by-cells", Ty::Bool, Dv::Bool(true), Window, false, Live, Behaviour, "Hint the compositor to resize in whole cells."; "Resize by cells", &[]),
    ("window.padding-x", Ty::int(0, 200), Dv::Int(6), Window, true, Live, Appearance, "Horizontal padding in pixels."; "Horizontal padding", &[]),
    ("window.padding-y", Ty::int(0, 200), Dv::Int(6), Window, true, Live, Appearance, "Vertical padding in pixels."; "Vertical padding", &[]),
    ("window.padding-balance", Ty::Bool, Dv::Bool(false), Window, true, Live, Appearance, "Spread the leftover pixels evenly around the grid."; "Balance padding", &[]),
    ("window.remember-size", Ty::Bool, Dv::Bool(true), Window, false, Live, Behaviour, "Reopen windows at their last size."; "Remember size", &[]),
    ("window.title", Ty::Template, Dv::Str("{title}"), Window, true, Live, Appearance, "Window title template."; "Window title", &[]),
    ("interface.font", Ty::Font, Dv::Null, Window, false, Live, Appearance, "Font of the terminal's own interface; unset follows the system."; "Interface font", &[]),
    ("interface.font-size", Ty::float(8.0, 32.0), Dv::Float(13.0), Window, false, Live, Appearance, "Size of the interface font."; "Interface font size", &[]),
    ("interface.motion", Ty::Enum(MOTION), Dv::Computed { id: "interface.motion", doc: "Follows the portal reduce-motion setting." }, Window, false, Live, Appearance, "Animation level of the interface."; "Motion", &[]),
    ("interface.scale", Ty::float(0.5, 3.0), Dv::Float(1.0), Window, false, Live, Appearance, "Scale of the terminal's own interface."; "Interface scale", &[]),
    ("interface.glass", Ty::Bool, Dv::Bool(true), Window, false, Live, Appearance, "Glass material on interface surfaces."; "Glass surfaces", &[]),
    ("interface.tooltips", Ty::Bool, Dv::Bool(true), Window, false, Live, Appearance, "Show tooltips."; "Tooltips", &[]),
    ("tabs.show", Ty::Enum(TABS_SHOW), Dv::Str("multiple"), Window, true, Live, Appearance, "When the tab strip is visible."; "Show tab strip", &[]),
    ("tabs.style", Ty::Enum(TABS_STYLE), Dv::Str("floating"), Window, true, Live, Appearance, "Tab strip style."; "Tab style", &[]),
    ("tabs.position", Ty::Enum(TABS_POS), Dv::Str("top"), Window, true, Live, Appearance, "Tab strip edge."; "Tab position", &[]),
    ("tabs.title", Ty::Template, Dv::Str("{title}"), Tab, true, Live, Appearance, "Tab title template."; "Tab title", &[]),
    ("tabs.show-context", Ty::Bool, Dv::Bool(false), Window, true, Live, Appearance, "Show shell/container/ssh context on tabs."; "Show context", &[]),
    ("tabs.new-tab-position", Ty::Enum(NEW_TAB_POS), Dv::Str("after-current"), Window, true, Live, Behaviour, "Where a new tab opens."; "New tab position", &[]),
    ("tabs.close-button", Ty::Bool, Dv::Bool(true), Window, true, Live, Appearance, "Show a close button on tabs."; "Close button", &[]),
    ("tabs.badges", Ty::Bool, Dv::Bool(true), Window, true, Live, Appearance, "Show unseen-output, bell and progress badges."; "Badges", &[]),
    ("tabs.max-title-width", Ty::int(8, 80), Dv::Int(28), Window, true, Live, Appearance, "Longest tab title in cells."; "Maximum title width", &[]),
    ("panes.frame", Ty::Enum(PANES_FRAME), Dv::Str("full"), Window, true, Live, Appearance, "Pane frame: full, titles or none."; "Pane frame", &[]),
    ("panes.inactive-dim", Ty::float(0.0, 1.0), Dv::Float(0.12), Window, true, Live, Appearance, "Dim amount of unfocused panes; 0 disables."; "Dim inactive panes", &[]),
    ("panes.remain-on-exit", Ty::Enum(REMAIN), Dv::Str("never"), Pane, true, Live, Behaviour, "Keep a pane open after its process exits."; "Remain on exit", &[]),
    ("panes.split-handle-size", Ty::int(1, 16), Dv::Int(4), Window, true, Live, Appearance, "Split handle thickness in pixels."; "Split handle size", &[]),
    ("panes.focus-follows-mouse", Ty::Bool, Dv::Bool(false), Window, true, Live, Behaviour, "Focus the pane under the pointer."; "Focus follows mouse", &[]),
    ("panes.zoom-indicator", Ty::Bool, Dv::Bool(true), Window, true, Live, Appearance, "Show an indicator on a zoomed pane."; "Zoom indicator", &[]),
    ("close.confirm", Ty::Enum(CLOSE_CONFIRM), Dv::Str("if-running"), Window, true, Live, Behaviour, "When closing asks first."; "Confirm close", &[]),
    ("close.undo-timeout", Ty::dur(0, 600000), Dv::Int(5000), Window, false, Live, Behaviour, "How long a closed tab or pane can be restored."; "Undo close window", &[]),
    ("scrollbar.mode", Ty::Enum(SCROLLBAR_MODE), Dv::Str("auto"), Window, true, Live, Appearance, "Scrollbar visibility."; "Scrollbar", &[]),
    ("scrollbar.marks", Ty::Bool, Dv::Bool(true), Window, true, Live, Appearance, "Draw prompt and search marks on the scrollbar."; "Scrollbar marks", &[]),
    ("scrollbar.width", Ty::int(2, 24), Dv::Int(8), Window, true, Live, Appearance, "Scrollbar width in pixels."; "Scrollbar width", &[]),
    ("cursor.style", Ty::Enum(CURSOR_STYLE), Dv::Str("block"), Pane, true, Live, Appearance, "Default cursor shape."; "Cursor style", &[]),
    ("cursor.blink", Ty::Bool, Dv::Bool(false), Pane, true, Live, Appearance, "Blink the cursor by default."; "Cursor blink", &[]),
    ("cursor.blink-interval", Ty::dur(100, 5000), Dv::Int(530), Pane, true, Live, Appearance, "Cursor blink half-period."; "Blink interval", &[]),
    ("cursor.thickness", Ty::float(0.05, 1.0), Dv::Float(0.15), Pane, true, Live, Appearance, "Bar and underline thickness as a fraction of the cell."; "Cursor thickness", &[]),
    ("cursor.trail", Ty::Bool, Dv::Bool(false), Pane, true, Live, Appearance, "Animate a short trail behind cursor jumps."; "Cursor trail", &[]),
    ("cursor.unfocused-style", Ty::Enum(CURSOR_UNFOCUSED), Dv::Str("hollow"), Pane, true, Live, Appearance, "Cursor shape in an unfocused pane."; "Unfocused cursor", &[]),
    ("scrollback.lines", Ty::int(0, 1000000000), Dv::Int(0), Pane, true, Live, Behaviour, "Line limit; 0 is unlimited. The byte limit applies too."; "Scrollback lines", &[]),
    ("scrollback.bytes", Ty::bytes(1048576, 68719476736), Dv::Int(67108864), Pane, true, Live, Behaviour, "Memory limit per pane; whichever limit hits first wins."; "Scrollback size", &["history","memory"]),
    ("scrollback.persist", Ty::Bool, Dv::Bool(false), Global, false, Live, Behaviour, "Let the session daemon spill history to encrypted disk."; "Persist scrollback", &[]),
    ("scrollback.reflow", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Reflow wrapped lines on resize."; "Reflow", &[]),
    ("scrollback.snap-on-output", Ty::Bool, Dv::Bool(false), Pane, true, Live, Behaviour, "Scroll to the bottom on output."; "Snap on output", &[]),
    ("scrollback.snap-on-input", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Scroll to the bottom on keypress."; "Snap on input", &[]),
    ("blocks.enabled", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Track command blocks from shell integration."; "Command blocks", &[]),
    ("blocks.max", Ty::int(10, 100000), Dv::Int(1000), Pane, true, Live, Behaviour, "Command records kept per pane."; "Block history", &[]),
    ("blocks.show-markers", Ty::Bool, Dv::Bool(true), Pane, true, Live, Appearance, "Show prompt markers in the gutter."; "Block markers", &[]),
    ("mouse.bypass-modifier", Ty::Enum(MODS), Dv::Str("shift"), Window, false, Live, Behaviour, "Modifier that bypasses application mouse reporting."; "Mouse bypass modifier", &[]),
    ("mouse.selection-override", Ty::StringSet, Dv::List(&["shift"]), Window, false, Live, Behaviour, "Modifier set that forces a local selection."; "Selection override", &[]),
    ("mouse.scroll-lines", Ty::int(1, 50), Dv::Int(3), Window, true, Live, Behaviour, "Lines per wheel notch."; "Scroll lines", &[]),
    ("mouse.scroll-multiplier", Ty::float(0.1, 20.0), Dv::Float(1.0), Window, true, Live, Behaviour, "Touchpad scroll speed."; "Scroll multiplier", &[]),
    ("mouse.alternate-scroll-mode", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Wheel sends cursor keys on the alternate screen."; "Alternate scroll", &[]),
    ("mouse.multi-click-interval", Ty::dur(100, 2000), Dv::Int(400), Window, false, Live, Behaviour, "Maximum gap between clicks of a multi-click."; "Multi-click interval", &[]),
    ("mouse.hide-while-typing", Ty::Bool, Dv::Bool(true), Window, true, Live, Behaviour, "Hide the pointer while typing."; "Hide pointer while typing", &[]),
    ("touch.enabled", Ty::Bool, Dv::Bool(true), Window, false, Live, Behaviour, "Enable touch gestures."; "Touch gestures", &[]),
    ("touch.long-press-delay", Ty::dur(100, 2000), Dv::Int(500), Window, false, Live, Behaviour, "Long-press time before a touch drag selects."; "Long-press delay", &[]),
    ("keyboard.sequence-timeout", Ty::dur(100, 10000), Dv::Int(1000), Window, false, Live, Behaviour, "Time allowed between keys of a multi-key bind."; "Sequence timeout", &[]),
    ("keyboard.repeat-delay", Ty::dur(50, 2000), Dv::Int(300), Window, false, Live, Behaviour, "Delay before key repeat."; "Repeat delay", &[]),
    ("keyboard.repeat-rate", Ty::int(1, 100), Dv::Int(30), Window, false, Live, Behaviour, "Key repeats per second."; "Repeat rate", &[]),
    ("keyboard.alt-sends-escape", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Alt prefixes ESC."; "Alt sends escape", &[]),
    ("keyboard.kitty-protocol", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Honour the kitty keyboard protocol."; "Kitty keyboard protocol", &[]),
    ("keyboard.modify-other-keys", Ty::Enum(MOK), Dv::Str("off"), Pane, true, Live, Behaviour, "Default xterm modifyOtherKeys level."; "modifyOtherKeys", &[]),
    ("keyboard.format-other-keys", Ty::Enum(FOK), Dv::Str("xterm"), Pane, true, Live, Behaviour, "modifyOtherKeys output form."; "formatOtherKeys", &[]),
    ("keyboard.layout-independent-binds", Ty::Bool, Dv::Bool(true), Window, false, Live, Behaviour, "Match binds on the base-layout key too."; "Layout-independent binds", &[]),
    ("keyboard.compose", Ty::Enum(COMPOSE), Dv::Str("xkb"), Window, false, Live, Behaviour, "Compose source."; "Compose", &[]),
    ("keyboard.compose-file", Ty::Path, Dv::Null, Window, false, Live, Behaviour, "Extra compose file; unset uses the locale and ~/.XCompose."; "Compose file", &[]),
    ("keyboard.ime", Ty::Bool, Dv::Bool(true), Window, false, Live, Behaviour, "Enable input methods."; "Input method", &[]),
    ("keyboard.xkb-backend", Ty::Enum(XKB_BACKEND), Dv::Str("kbvm"), Global, false, Restart, Behaviour, "Keymap backend."; "XKB backend", &[]),
    ("keyboard.backspace", Ty::Enum(BACKSPACE), Dv::Str("del"), Pane, true, Live, Behaviour, "Byte sent by Backspace."; "Backspace sends", &[]),
    ("selection.copy-on-select", Ty::Bool, Dv::Bool(false), Window, true, Live, Behaviour, "Copy to the primary selection when selecting."; "Copy on select", &[]),
    ("selection.clear-on-copy", Ty::Bool, Dv::Bool(false), Window, true, Live, Behaviour, "Clear the selection after copying."; "Clear on copy", &[]),
    ("selection.clear-on-typing", Ty::Bool, Dv::Bool(true), Window, true, Live, Behaviour, "Clear the selection when typing."; "Clear on typing", &[]),
    ("selection.trim-trailing-spaces", Ty::Bool, Dv::Bool(true), Window, true, Live, Behaviour, "Trim trailing spaces from copied lines."; "Trim trailing spaces", &[]),
    ("selection.word-mode", Ty::Enum(WORD_MODE), Dv::Str("delimiters"), Window, true, Live, Behaviour, "Word boundary model."; "Word mode", &[]),
    ("selection.word-delimiters", Ty::Str, Dv::Str(" \t'`|:;,()[]{}<>$\""), Window, true, Live, Behaviour, "Characters that end a word."; "Word delimiters", &[]),
    ("selection.word-chars", Ty::Str, Dv::Str("@-./_~?&=%+#"), Window, true, Live, Behaviour, "Extra word characters in word-character mode."; "Word characters", &[]),
    ("selection.word-chars-forward", Ty::Str, Dv::Null, Window, true, Live, Behaviour, "Extra word characters when extending forward."; "Forward word characters", &[]),
    ("selection.triple-click", Ty::Enum(TRIPLE), Dv::Str("line"), Window, true, Live, Behaviour, "What triple click selects."; "Triple click", &[]),
    ("hints.alphabet", Ty::Str, Dv::Str("asdfghjklqwertyuiopzxcvbnm"), Window, false, Live, Behaviour, "Letters used for hint labels."; "Hint alphabet", &[]),
    ("hints.max-matches", Ty::int(1, 1000), Dv::Int(100), Window, false, Live, Behaviour, "Most hints shown at once."; "Maximum hints", &[]),
    ("bell.audible", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Ring the system bell."; "Audible bell", &[]),
    ("bell.urgent", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Request attention when unfocused."; "Urgency hint", &[]),
    ("bell.visual", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Tab badge and a brief rim brightening."; "Visual bell", &[]),
    ("bell.notify", Ty::Bool, Dv::Bool(false), Pane, true, Live, Behaviour, "Send a desktop notification on bell."; "Bell notification", &[]),
    ("bell.rate-limit", Ty::dur(0, 10000), Dv::Int(100), Pane, true, Live, Behaviour, "Minimum gap between bell effects."; "Bell rate limit", &[]),
    ("bell.command", Ty::Argv, Dv::Empty, Pane, true, Live, Security, "argv run on bell."; "Bell command", &[]),
    ("notifications.when", Ty::Enum(NOTIFY_WHEN), Dv::Str("unfocused"), Pane, true, Live, Behaviour, "When terminal notifications are shown."; "Show notifications", &[]),
    ("notifications.min-duration", Ty::dur(0, 3600000), Dv::Int(0), Pane, true, Live, Behaviour, "Minimum command duration before a finish notification."; "Minimum duration", &[]),
    ("notifications.osc-9", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Accept OSC 9 notifications."; "OSC 9", &[]),
    ("notifications.osc-99", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Accept OSC 99 notifications."; "OSC 99", &[]),
    ("notifications.osc-777", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Accept OSC 777 notifications."; "OSC 777", &[]),
    ("notifications.max-per-minute", Ty::int(1, 600), Dv::Int(30), Pane, true, Live, Behaviour, "Notifications accepted per pane per minute."; "Rate limit", &[]),
    ("progress.enabled", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Show OSC 9;4 progress."; "Progress", &[]),
    ("progress.timeout", Ty::dur(1000, 600000), Dv::Int(15000), Pane, true, Live, Behaviour, "Clear stale progress after this long."; "Progress timeout", &[]),
    ("shell.program", Ty::Path, Dv::Null, Pane, true, NewPane, Security, "Shell program; unset resolves $SHELL then passwd."; "Shell", &[]),
    ("shell.args", Ty::Argv, Dv::Empty, Pane, true, NewPane, Security, "Shell arguments."; "Shell arguments", &[]),
    ("shell.login", Ty::Bool, Dv::Bool(false), Pane, true, NewPane, Security, "Start the shell as a login shell."; "Login shell", &[]),
    ("shell.cwd", Ty::Path, Dv::Null, Pane, true, NewPane, Security, "Starting directory; unset inherits."; "Start directory", &[]),
    ("shell.env", MAP_STR_STR, Dv::Empty, Pane, true, NewPane, Security, "Extra environment for the shell."; "Environment", &[]),
    ("shell-integration.enabled", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Honour OSC 133 / OSC 7 shell integration."; "Shell integration", &[]),
    ("shell-integration.inject", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Inject the integration script into supported shells."; "Inject script", &[]),
    ("terminal.term", Ty::Str, Dv::Str("xterm-apotheosis"), Pane, true, NewPane, Behaviour, "Value of $TERM."; "TERM", &[]),
    ("terminal.terminfo-mode", Ty::Enum(TERMINFO_MODE), Dv::Str("path"), Pane, true, NewPane, Behaviour, "How the terminfo entry is found."; "Terminfo mode", &[]),
    ("graphics.kitty", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Kitty graphics protocol."; "Kitty graphics", &[]),
    ("graphics.sixel", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Sixel graphics."; "Sixel", &[]),
    ("graphics.iterm2", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "iTerm2 inline images."; "iTerm2 images", &[]),
    ("graphics.quota", Ty::bytes(0, 17179869184), Dv::Int(335544320), Pane, true, Live, Behaviour, "Image memory per pane."; "Image quota per pane", &[]),
    ("graphics.quota-total", Ty::bytes(0, 68719476736), Dv::Int(1073741824), Global, false, Live, Behaviour, "Image memory per process."; "Image quota total", &[]),
    ("graphics.animation-quota", Ty::bytes(0, 17179869184), Dv::Int(67108864), Pane, true, Live, Behaviour, "Animation frame memory per pane."; "Animation quota", &[]),
    ("graphics.sandbox-decode", Ty::Bool, Dv::Bool(true), Global, false, Live, Behaviour, "Decode images in the sandboxed helper."; "Sandboxed decode", &[]),
    ("graphics.max-pixels", Ty::int(65536, 1073741824), Dv::Int(100000000), Pane, true, Live, Behaviour, "Largest image in pixels."; "Maximum image size", &[]),
    ("graphics.file-transmission", Ty::Enum(FILE_TX), Dv::Str("local-only"), Pane, true, Live, Security, "Allow images read from files, temp files or shared memory."; "File transmission", &[]),
    ("media.enabled", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Native media protocol."; "Media", &[]),
    ("media.allow-paths", Ty::StringSet, Dv::Empty, Global, false, Live, Security, "Directories media may be opened from."; "Allowed media paths", &[]),
    ("media.hwdec", Ty::Enum(HWDEC), Dv::Str("auto"), Global, false, Live, Behaviour, "Hardware decode backend."; "Hardware decode", &[]),
    ("media.av1-software", Ty::Bool, Dv::Bool(true), Global, false, Live, Behaviour, "Allow software AV1 decode."; "Software AV1", &[]),
    ("media.sw-codecs", Ty::Bool, Dv::Bool(true), Global, false, Live, Behaviour, "Allow software H.264/H.265 decode."; "Software codecs", &[]),
    ("media.audio-output", Ty::Enum(AUDIO_OUT), Dv::Str("auto"), Global, false, Live, Behaviour, "Audio backend."; "Audio output", &[]),
    ("media.sync-master", Ty::Enum(SYNC_MASTER), Dv::Str("audio"), Global, false, Live, Behaviour, "Playback clock master."; "Sync master", &[]),
    ("media.autoplay-audio", Ty::Enum(AUTOPLAY), Dv::Str("focused"), Pane, true, Live, Behaviour, "When audio may start."; "Autoplay audio", &[]),
    ("media.inhibit-idle", Ty::Bool, Dv::Bool(true), Global, false, Live, Behaviour, "Inhibit idle while media plays."; "Inhibit idle", &[]),
    ("media.offscreen", Ty::Enum(OFFSCREEN), Dv::Str("pause"), Pane, true, Live, Behaviour, "Playback in a scrolled-out pane."; "Offscreen playback", &[]),
    ("media.passthrough", Ty::Bool, Dv::Bool(true), Pane, true, Live, Behaviour, "Accept tmux/screen passthrough envelopes."; "Multiplexer passthrough", &[]),
    ("media.shm-transmission", Ty::Enum(SHM_TX), Dv::Str("local-only"), Pane, true, Live, Behaviour, "Shared-memory transmission."; "Shared-memory transmission", &[]),
    ("render.allow-software", Ty::Bool, Dv::Bool(false), Global, false, Restart, Behaviour, "Accept a software adapter when no GPU is found."; "Allow software rendering", &[]),
    ("render.sync-timeout", Ty::dur(0, 10000), Dv::Int(1000), Global, false, Live, Behaviour, "Longest synchronized-output hold."; "Sync timeout", &[]),
    ("render.echo-window", Ty::dur(0, 100), Dv::Int(10), Global, false, Live, Behaviour, "Output within this of a keypress is drawn inline."; "Echo window", &[]),
    ("render.text-blending", Ty::Enum(BLENDING), Dv::Str("linear-corrected"), Global, false, Live, Appearance, "Glyph alpha blending."; "Text blending", &[]),
    ("render.low-latency", Ty::Bool, Dv::Bool(false), Global, false, Live, Behaviour, "Favour latency over throughput."; "Low latency", &[]),
    ("render.max-fps", Ty::int(0, 1000), Dv::Int(0), Global, false, Live, Behaviour, "Frame cap; 0 follows the display."; "Frame cap", &[]),
    ("render.power-preference", Ty::Enum(POWER), Dv::Str("auto"), Global, false, Live, Behaviour, "GPU selection preference."; "GPU preference", &[]),
    ("security.remote-keys", Ty::Bool, Dv::Bool(false), Pane, true, Live, Security, "Deliver key-stream events to socket clients in remote panes."; "Remote key stream", &[]),
    ("security.sequence", MAP_STR_STRSET, Dv::Empty, Global, false, Live, Security, "Sequence policy overrides: class local remote, each allow, ask or deny."; "Sequence policy", &["osc52","consent"]),
    ("remote.clients", Ty::StringSet, Dv::List(&["ssh", "mosh-client", "telnet", "docker", "podman", "kubectl", "tmux", "screen"]), Global, false, Live, Security, "Foreground programs that mark a pane remote (extends the default)."; "Remote clients", &[]),
    ("clipboard.osc52", Ty::Bool, Dv::Bool(true), Pane, true, Live, Security, "Accept OSC 52."; "OSC 52", &[]),
    ("clipboard.osc52-max", Ty::bytes(1024, 67108864), Dv::Int(1048576), Pane, true, Live, Security, "Largest OSC 52 payload."; "OSC 52 size limit", &[]),
    ("clipboard.buffer-limit", Ty::int(1, 1000), Dv::Int(50), Window, false, Live, Security, "Paste buffers kept."; "Paste buffers", &[]),
    ("clipboard.primary-selection", Ty::Bool, Dv::Bool(true), Window, false, Live, Security, "Use the primary selection."; "Primary selection", &[]),
    ("paste.guard", Ty::Enum(GUARD), Dv::Str("auto"), Pane, true, Live, Security, "When the paste guard asks first."; "Paste guard", &[]),
    ("paste.confirm-bytes", Ty::bytes(0, 1073741824), Dv::Int(4096), Pane, true, Live, Security, "Pastes larger than this are confirmed."; "Confirm above", &[]),
    ("paste.max-bytes", Ty::bytes(1024, 1073741824), Dv::Int(5242880), Pane, true, Live, Security, "Hard paste limit."; "Paste limit", &[]),
    ("paste.newline", Ty::Enum(PASTE_NL), Dv::Str("cr"), Pane, true, Live, Security, "Newline handling without bracketed paste."; "Newlines", &[]),
    ("paste.trim-trailing-newline", Ty::Bool, Dv::Bool(false), Pane, true, Live, Security, "Strip one trailing newline."; "Trim trailing newline", &[]),
    ("paste.sanitize", Ty::Bool, Dv::Bool(true), Pane, true, Live, Security, "Strip control characters."; "Sanitise", &[]),
    ("paste.confirm-multiline", Ty::Bool, Dv::Bool(true), Pane, true, Live, Security, "Confirm multi-line pastes at a prompt."; "Confirm multi-line", &[]),
    ("links.detect", Ty::Bool, Dv::Bool(true), Pane, true, Live, Security, "Detect plain-text URLs."; "Detect links", &[]),
    ("links.open-modifier", Ty::Enum(MODS), Dv::Str("ctrl"), Window, false, Live, Security, "Modifier for opening a link by click."; "Open modifier", &[]),
    ("links.schemes", Ty::StringSet, Dv::List(&["http", "https", "mailto", "ftp", "file"]), Window, false, Live, Security, "Allowed URL schemes."; "Allowed schemes", &[]),
    ("links.explicit-precedence", Ty::Bool, Dv::Bool(true), Pane, true, Live, Security, "OSC 8 links win over detected ones."; "OSC 8 precedence", &[]),
    ("links.open-command", Ty::Argv, Dv::List(&["xdg-open", "{url}"]), Window, false, Live, Security, "argv that opens a link."; "Open command", &[]),
    ("open.placement", Ty::Enum(OPEN_PLACEMENT), Dv::Str("float"), Window, false, Live, Behaviour, "Where a terminal handler (apoth icat, apoth play) shows when its rule names no placement: float, inline (a split pane) or window (--ext)."; "Terminal handler placement", &["open with","icat","play"]),
    ("open.handlers", MAP_STR_STRLIST, Dv::Empty, Global, false, Live, Security, "Open-with rules: MIME type or type/* (x-scheme-handler/<scheme> for URLs) to a handler (apoth-icat, apoth-play, <id>.desktop or opener) and an optional placement. Unlisted types: image/* apoth-icat, video/* and audio/* apoth-play, else the freedesktop default."; "Open-with rules", &["open with","mime","default app","mimeapps"]),
    ("accessibility.enabled", Ty::Enum(A11Y), Dv::Str("auto"), Global, false, Live, Behaviour, "Accessibility tree: auto, on or off."; "Accessibility", &[]),
    ("accessibility.announce", Ty::Bool, Dv::Bool(true), Global, false, Live, Behaviour, "Announce output and events."; "Announcements", &[]),
    ("accessibility.semantic", Ty::Bool, Dv::Bool(true), Global, false, Live, Behaviour, "Expose prompt and block structure."; "Semantic structure", &[]),
    ("accessibility.media-announcements", Ty::Bool, Dv::Bool(true), Global, false, Live, Behaviour, "Announce media state."; "Media announcements", &[]),
    ("accessibility.update-interval", Ty::dur(10, 5000), Dv::Int(100), Global, false, Live, Behaviour, "Tree update coalescing."; "Update interval", &[]),
    ("accessibility.exposed-rows", Ty::int(0, 1000), Dv::Int(0), Global, false, Live, Behaviour, "Rows beyond the viewport exposed; 0 is the viewport."; "Exposed rows", &[]),
    ("socket.grants", MAP_PATH_STRSET, Dv::Empty, Global, false, Live, Grant, "Per-program capability grants (never keys)."; "Socket grants", &[]),
    ("socket.key-stream", Ty::Bool, Dv::Bool(false), Pane, true, Live, Grant, "Allow clients the key-stream channel in this pane."; "Key stream", &[]),
    ("socket.interpreters", Ty::StringSet, Dv::Empty, Global, false, Live, Grant, "Extra interpreter names refused as grant paths (extends the built-in list)."; "Extra interpreters", &[]),
    ("sessions.server", Ty::Enum(OFF_ON), Dv::Str("off"), Global, false, Restart, Behaviour, "Run the session daemon."; "Session daemon", &[]),
    ("sessions.restore", Ty::Enum(RESTORE), Dv::Str("ask"), Global, false, Live, Behaviour, "Restore the last session at start."; "Restore session", &[]),
    ("quick-terminal.animation", Ty::Enum(QT_ANIM), Dv::Str("slide"), Global, false, Live, Appearance, "Show/hide animation."; "Quick terminal animation", &[]),
    ("quick-terminal.autohide", Ty::Bool, Dv::Bool(true), Global, false, Live, Behaviour, "Hide on focus loss."; "Autohide", &[]),
    ("quick-terminal.position", Ty::Enum(QT_POS), Dv::Str("top"), Global, false, Live, Appearance, "Screen edge."; "Position", &[]),
    ("quick-terminal.size", Ty::float(0.1, 1.0), Dv::Float(0.4), Global, false, Live, Appearance, "Fraction of the screen."; "Size", &[]),
    ("binds.clear-defaults", Ty::Bool, Dv::Bool(false), Global, false, Live, Security, "Start from an empty key map."; "Clear default binds", &[]),
    ("mouse-binds.clear-defaults", Ty::Bool, Dv::Bool(false), Global, false, Live, Security, "Start from an empty mouse map."; "Clear default mouse binds", &[]),
    ("key-table.default-timeout", Ty::dur(100, 60000), Dv::Int(3000), Global, false, Live, Security, "Idle time that pops a key table."; "Key table timeout", &[]),
    ("rule.rate-limit", Ty::int(1, 1000), Dv::Int(10), Global, false, Live, Security, "Firings per second per rule."; "Rule rate limit", &[]),
    ("rule.loop-guard", Ty::int(10, 100000), Dv::Int(1000), Global, false, Live, Security, "Queued rule events before the guard trips."; "Rule loop guard", &[]),
    ("profile.default", Ty::Str, Dv::Null, Global, false, Live, Security, "Profile for new panes."; "Default profile", &[]),
    ("profile.auto-switch", Ty::Bool, Dv::Bool(true), Global, false, Live, Security, "Switch profiles by match blocks."; "Automatic profiles", &[]),
    ("layout.default", Ty::Str, Dv::Null, Global, false, Live, Security, "Layout applied to new windows."; "Default layout", &[]),
    ("menu.context-menu", Ty::Bool, Dv::Bool(true), Global, false, Live, Security, "Show the context menu on right click."; "Context menu", &[]),
    ("tabs.wheel-switch", Ty::Bool, Dv::Bool(true), Window, true, Live, Behaviour, "Switch tabs with the wheel over the strip."; "Wheel switches tabs", &[]),
    ("tabs.middle-click-close", Ty::Bool, Dv::Bool(true), Window, true, Live, Behaviour, "Close a tab with middle click."; "Middle click closes", &[]),
    ("panes.drag-resize", Ty::Bool, Dv::Bool(true), Window, true, Live, Behaviour, "Resize panes by dragging split handles."; "Drag to resize", &[]),
    ("cursor.color-follows-text", Ty::Bool, Dv::Bool(false), Pane, true, Live, Appearance, "Cursor takes the colour of the cell under it."; "Cursor follows text colour", &[]),
}
