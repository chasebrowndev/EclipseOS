// SPDX-License-Identifier: AGPL-3.0-only

//! Every colour, radius, size, font and motion curve fog-ui draws with
//! (FOG §Visual design). Nothing outside this module names a literal.
//!
//! **Where the values come from.** EclipseOS has no colour or font KDL: the
//! desktop's palette and faces are the style spec transcribed in
//! `AbyssCompositor/crates/eclipse-ui/src/tokens.rs`, and [`color`] and
//! [`font`] mirror it value for value (each constant names its source).
//! What the user can set, corner rounding, blur, window opacity, shadow and
//! the animation switch, is read from `abyss.kdl` by
//! [`fog_config::theme`] and turned into a [`Look`] at start-up, together
//! with Fog's own `appearance` keys. Fog adds tokens of its own only for
//! what a file manager has and a settings pane does not: row height, the
//! icon stand-ins and the drag ghost.

use std::sync::{OnceLock, PoisonError, RwLock};
use std::time::Duration;

use fog_config::theme::Theme;
use fog_config::Appearance;
use fog_widgets::contrast::{self, Space, Surface};
use fog_widgets::glass::Style;
use fog_widgets::{Blur, Params};
use iced::{Color, Font, Vector};

/// `#rrggbb` at full opacity.
const fn rgb(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// White at `a`.
const fn white(a: f32) -> Color {
    Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a,
    }
}

/// Black at `a`.
const fn black(a: f32) -> Color {
    Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a,
    }
}

/// `c` at alpha `a`.
pub const fn alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

pub mod color {
    use super::{black, rgb, white, Color};

    /// tokens `BASE`: the window tint. Warm, never blue-grey.
    pub const BASE: Color = rgb(0x0b0906);
    /// tokens `SURFACE_1`: a panel's ground when transparency is reduced.
    pub const SURFACE: Color = rgb(0x1a1712);
    /// tokens `GLASS_DEEP`: floating glass over live content.
    pub const GLASS: Color = Color {
        a: 0.62,
        ..rgb(0x17140f)
    };
    /// A modal sheet: [`GLASS`] thickened, it floats over busy rows.
    pub const SHEET: Color = Color {
        a: 0.74,
        ..rgb(0x17140f)
    };

    /// tokens `HAIRLINE`: rules inside the window.
    pub const RULE: Color = white(0.055);
    /// tokens `BORDER`: an outline around an inset field.
    pub const BORDER: Color = white(0.10);
    /// tokens `BORDER_STRONG`: the drag ghost's edge.
    pub const BORDER_STRONG: Color = white(0.16);
    /// tokens `HIGHLIGHT_STRONG`: a glass panel's lit rim.
    pub const RIM: Color = white(0.20);
    /// tokens `SIDEBAR` (STYLE.md "Left sidebar: rgba(0,0,0,.4)"): the
    /// places column, a shade over the window's own tint, no edge.
    pub const SIDEBAR: Color = black(0.4);
    /// tokens `GLASS`: the path field's fill, one step off the window.
    pub const FIELD: Color = white(0.045);
    /// tokens `MENU_GROUND`: the drag ghost's nearly opaque ground, legible
    /// over rows, places or the desktop.
    pub const MENU_GROUND: Color = rgb(0x17140f);

    /// tokens `TEXT`, `TEXT_SECONDARY`, `TEXT_TERTIARY`.
    pub const TEXT: Color = white(1.0);
    pub const TEXT_SECONDARY: Color = white(0.64);
    pub const TEXT_TERTIARY: Color = white(0.40);

    /// tokens `ACCENT`. The single gold: the focused cursor's fill, the
    /// focus caret and the active tab's marker, nothing else.
    pub const ACCENT: Color = rgb(0xf2c33c);
    /// tokens `ACCENT_TEXT`: accent as text, lifted for small sizes.
    pub const ACCENT_TEXT: Color = rgb(0xf5cf5c);
    /// tokens `ACCENT_FILL` (STYLE.md: accent tints ".09 fill"): the
    /// focused cursor's wash, flat, with no edge and no glow. The name on
    /// it takes [`ACCENT_TEXT`], which carries the gold.
    pub const ACCENT_FILL: Color = Color {
        a: 0.09,
        ..rgb(0xf2c33c)
    };

    /// tokens `DANGER`, `OK`, `NEUTRAL`.
    pub const DANGER: Color = rgb(0xe0553f);
    /// Fog's own: a picked destructive choice's wash and hairline, the
    /// accent pill's weights in danger, so the pick never reads as gold.
    pub const DANGER_FILL: Color = Color {
        a: 0.16,
        ..rgb(0xe0553f)
    };
    pub const DANGER_BORDER: Color = Color {
        a: 0.40,
        ..rgb(0xe0553f)
    };
    pub const OK: Color = rgb(0x7fae5e);
    /// Warm neutral: iced's "primary", which it paints on the hovered
    /// scrollbar. Not the accent.
    pub const NEUTRAL: Color = rgb(0x96918a);

    /// tokens `LIFT`: an unfocused cursor's pill, marked rows, the current
    /// place, and a drop target under the pointer. Neutral, so a selection
    /// never competes with the one gold value.
    pub const MARK_FILL: Color = white(0.07);
    /// tokens `LIFT_SOFT`: whatever is under the pointer: a row, a place, a
    /// crumb, a column label, a button.
    pub const HOVER: Color = white(0.035);
    /// Icon stand-ins (STYLE.md "small rounded rects stand in for icons"):
    /// a folder in the warm neutral swatch, a file as a quiet outline.
    pub const GLYPH_FOLDER: Color = rgb(0x96918a);
    pub const GLYPH_FILE: Color = white(0.13);
    pub const GLYPH_EDGE: Color = white(0.30);
    /// A progress bar's track and its filled part. Neutral: a job's
    /// progress never takes the gold.
    pub const TRACK: Color = white(0.08);
    pub const PROGRESS: Color = white(0.64);
    /// Dims the window under a sheet: light, the blur does the separating.
    pub const SCRIM: Color = black(0.28);
    /// tokens `PANEL_SHADOW`, softened for panels docked in the window.
    pub const SHADOW: Color = black(0.42);
    /// A sheet's shadow: it floats higher.
    pub const SHEET_SHADOW: Color = black(0.62);
}

/// The iced theme, for the widgets fog-ui does not style itself (the list's
/// scrollbar): generated from the colours above.
pub fn iced_theme() -> iced::Theme {
    iced::Theme::custom(
        "Fog",
        iced::theme::Palette {
            background: color::BASE,
            text: color::TEXT,
            primary: color::NEUTRAL,
            success: color::OK,
            warning: color::DANGER,
            danger: color::DANGER,
        },
    )
}

/// tokens `font`: Instrument Sans for the interface and file names,
/// JetBrains Mono for data (sizes, dates, paths, chords).
pub mod font {
    use iced::font::{Family, Weight};
    use iced::Font;

    const fn sans(weight: Weight) -> Font {
        Font {
            family: Family::Name("Instrument Sans"),
            weight,
            ..Font::DEFAULT
        }
    }

    const fn mono(weight: Weight) -> Font {
        Font {
            family: Family::Name("JetBrains Mono"),
            weight,
            ..Font::DEFAULT
        }
    }

    pub const UI: Font = sans(Weight::Normal);
    pub const UI_MEDIUM: Font = sans(Weight::Medium);
    pub const UI_SEMIBOLD: Font = sans(Weight::Semibold);
    pub const DATA: Font = mono(Weight::Normal);

    /// The faces, embedded from the desktop's vendored copies (Fog builds
    /// inside the EclipseOS tree; see the PKGBUILD).
    pub const BYTES: &[&[u8]] = &[
        include_bytes!("../../../../AbyssCompositor/assets/fonts/InstrumentSans-Regular.ttf"),
        include_bytes!("../../../../AbyssCompositor/assets/fonts/InstrumentSans-Medium.ttf"),
        include_bytes!("../../../../AbyssCompositor/assets/fonts/InstrumentSans-SemiBold.ttf"),
        include_bytes!("../../../../AbyssCompositor/assets/fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../../../../AbyssCompositor/assets/fonts/JetBrainsMono-Medium.ttf"),
    ];
}

pub mod size {
    /// Fog's own: row height of the file list, in logical pixels. Compact:
    /// a list you scan, not a settings list.
    pub const ROW_H: f32 = 24.0;
    /// Body and data text.
    pub const TEXT: f32 = 13.0;
    /// The status line and the kind tags: smaller, quieter.
    pub const TEXT_SMALL: f32 = 11.5;
    /// Section captions in the sidebar and headings in sheets.
    pub const CAPTION: f32 = 10.5;
    /// tokens `size::MONO`: data (sizes, dates, paths, chords).
    pub const MONO: f32 = 11.5;
    /// tokens `size::MICRO`: the sidebar's uppercase mono section labels.
    pub const MICRO: f32 = 10.0;
    /// Horizontal padding of every row and chrome line.
    pub const PAD_X: f32 = 14.0;
    /// Vertical padding of the status line.
    pub const CHROME_Y: f32 = 7.0;
    /// Gap between the window edge and a floating panel, and between
    /// panels.
    pub const INSET: f32 = 8.0;
    /// Inner padding of a floating panel.
    pub const PANEL_PAD: f32 = 8.0;
    /// Path field height.
    pub const CAPSULE_H: f32 = 28.0;
    /// Horizontal inset of a pill inside its row.
    pub const PILL_X: f32 = 6.0;
    /// One device-independent pixel: rules and hairlines.
    pub const HAIRLINE: f32 = 1.0;
    /// The right-aligned data columns of the list.
    pub const SIZE_W: f32 = 80.0;
    pub const DATE_W: f32 = 132.0;
    pub const TYPE_W: f32 = 88.0;
    /// Horizontal padding inside each list column and its label, so a
    /// label's text sits over its column's.
    pub const COL_PAD: f32 = 8.0;
    /// Horizontal padding of a crumb and of a small text button: enough for
    /// the hover wash to hold its text.
    pub const CRUMB_X: f32 = 3.0;
    /// iced's embedded scrollbar width: the column header leaves it free so
    /// its labels sit over the list's columns.
    pub const SCROLLBAR_W: f32 = 10.0;
    /// Column header height: tighter than a row, it is a caption.
    pub const HEADER_H: f32 = 24.0;
    /// tokens `space::SIDEBAR_W`, and the gap above each of its sections.
    pub const SIDEBAR_W: f32 = 214.0;
    pub const SECTION_GAP: f32 = 14.0;
    /// A sidebar row.
    pub const PLACE_H: f32 = 24.0;
    /// tokens `space::BAR_W`: the gold bar at the left of the focused place.
    pub const NAV_BAR_W: f32 = 3.0;
    /// Icon stand-ins: a place's square, a folder's landscape rect and a
    /// file's portrait one, their corner, and the gap after them.
    pub const PLACE_GLYPH: f32 = 12.0;
    pub const GLYPH_LONG: f32 = 14.0;
    pub const GLYPH_SHORT: f32 = 11.0;
    pub const GLYPH_R: f32 = 3.0;
    pub const GLYPH_GAP: f32 = 8.0;
    /// The drag ghost: its height, widest, and the gap it hangs below the
    /// row it was taken from.
    pub const GHOST_H: f32 = 28.0;
    pub const GHOST_MAX_W: f32 = 280.0;
    pub const GHOST_GAP: f32 = 6.0;
    /// Tab strip height, the widest a tab grows and its gold marker.
    pub const TAB_H: f32 = 30.0;
    pub const TAB_MAX_W: f32 = 200.0;
    pub const TAB_MARK_W: f32 = 18.0;
    pub const TAB_MARK_H: f32 = 2.0;
    /// The palette sheet: width, distance from the top, rows shown.
    pub const PALETTE_W: f32 = 560.0;
    pub const PALETTE_TOP: f32 = 72.0;
    pub const PALETTE_ROWS: usize = 12;
    /// A sheet's input line.
    pub const INPUT_H: f32 = 34.0;
    /// Text caret in the path bar and palette.
    pub const CARET_W: f32 = 2.0;
    /// Advance of one monospace glyph, in ems: turns a width into a
    /// character budget for eliding the path bar. Slightly generous, so an
    /// estimate never clips the current folder.
    pub const MONO_ADVANCE: f32 = 0.62;
    /// Gap between status line segments.
    pub const GAP: f32 = 18.0;
    /// The job tray: rows shown before it scrolls with its cursor, the
    /// kind column, the byte or file count column, the ETA column, and the
    /// height of each job's progress bar.
    pub const TRAY_ROWS: usize = 4;
    pub const KIND_W: f32 = 72.0;
    pub const AMOUNT_W: f32 = 168.0;
    pub const ETA_W: f32 = 72.0;
    pub const PROGRESS_H: f32 = 3.0;
    /// The trash view's "from" column: the folder an item was deleted from.
    pub const FROM_W: f32 = 220.0;
    /// The conflict and delete sheets, and the name sheet.
    pub const DIALOG_W: f32 = 560.0;
    pub const NAME_W: f32 = 420.0;
    /// Padding inside a sheet.
    pub const SHEET_PAD: f32 = 18.0;
    /// A dialog's choice button.
    pub const CHOICE_PAD_Y: f32 = 5.0;
    pub const CHOICE_PAD_X: f32 = 12.0;
    /// Initial window size.
    pub const WINDOW_W: f32 = 1040.0;
    pub const WINDOW_H: f32 = 680.0;
}

/// Glass geometry and light, in logical pixels.
mod glass {
    /// Rim width.
    pub const RIM_W: f32 = 1.0;
    /// White pooled at a panel's top edge: none. STYLE.md lights a panel
    /// with a 1px top-edge highlight (the rim), never a gradient.
    pub const SHEEN: f32 = 0.0;
    /// Grain amplitude: none on docked panels, which blur nothing.
    pub const GRAIN: f32 = 0.0;
    /// Sheets float over a dark scrim, where grain reads as static: a
    /// breath of it keeps the blur from banding, no more.
    pub const SHEET_GRAIN: f32 = 0.003;
    /// Docked panel shadow: offset and blur.
    pub const SHADOW_Y: f32 = 6.0;
    pub const SHADOW_BLUR: f32 = 22.0;
    /// Sheet shadow.
    pub const SHEET_SHADOW_Y: f32 = 22.0;
    pub const SHEET_SHADOW_BLUR: f32 = 56.0;
}

/// Motion: how long each spring takes to settle.
pub mod motion {
    use std::time::Duration;

    /// Sheets scale and fade in: a slight overshoot.
    pub const SHEET: Duration = Duration::from_millis(260);
    /// Sidebar and tray slide.
    pub const PANEL: Duration = Duration::from_millis(220);
    /// The selection pill moving between rows.
    pub const PILL: Duration = Duration::from_millis(140);
    /// Hover wash.
    pub const HOVER: Duration = Duration::from_millis(120);
    /// A sheet's scale when it starts to open.
    pub const SHEET_FROM: f32 = 0.94;
}

/// The design's window tint alpha: raised by the contrast floor if it must
/// be, never lowered.
const WINDOW_ALPHA: f32 = 0.72;

/// What the theme, the compositor and the `appearance` keys make of the
/// tokens: built at start-up, rebuilt when `appearance` is reloaded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    /// The window tint.
    pub window: Color,
    /// Panel and sheet radius, an inset field's, and a chip's or pill's.
    pub radius: f32,
    pub inset_radius: f32,
    pub chip_radius: f32,
    /// Blur for panels over Fog's own content; `None` draws them solid.
    pub blur: Option<Blur>,
    /// Glass is see-through; `false` under `reduce-transparency`.
    pub translucent: bool,
    /// Springs run; `false` under `reduce-motion` or with the compositor's
    /// animations off: every value snaps.
    pub motion: bool,
}

impl Default for Look {
    fn default() -> Look {
        Look::new(&Theme::default(), Appearance::default())
    }
}

/// Text alphas fog-ui draws on the window tint.
const TEXTS: [(contrast::Rgb, f32); 3] = [
    (contrast::WHITE, color::TEXT.a),
    (contrast::WHITE, color::TEXT_SECONDARY.a),
    (contrast::WHITE, color::TEXT_TERTIARY.a),
];

fn rgb3(c: Color) -> contrast::Rgb {
    [c.r, c.g, c.b]
}

/// The window surface at tint alpha `a` under `theme`.
fn surface(theme: &Theme, a: f32) -> Surface {
    Surface {
        tint: rgb3(color::BASE),
        alpha: a,
        opacity: theme.active_opacity,
        // abyss blends toplevels in sRGB.
        space: Space::Srgb,
    }
}

/// The least window tint alpha, at least the design's, that keeps every
/// text alpha at WCAG AA over the worst-case backdrop.
pub fn window_alpha(theme: &Theme) -> f32 {
    contrast::min_alpha(
        surface(theme, 0.0),
        WINDOW_ALPHA,
        &TEXTS,
        &contrast::WORST,
        contrast::AA,
    )
    .unwrap_or(1.0)
}

impl Look {
    pub fn new(theme: &Theme, appearance: Appearance) -> Look {
        // Without the compositor's blur the desktop would show through
        // sharp: the window is opaque then, as it is when asked to be.
        let translucent = !appearance.reduce_transparency && theme.compositor_blurs();
        let window = if translucent {
            alpha(color::BASE, window_alpha(theme))
        } else {
            color::BASE
        };
        let r = theme.rounding as f32;
        Look {
            window,
            radius: r,
            inset_radius: (r - 3.0).max(0.0),
            chip_radius: (r / 2.0).max(0.0),
            blur: (theme.blur && !appearance.reduce_transparency).then_some(Blur {
                size: theme.blur_size,
                passes: theme.blur_passes,
            }),
            translucent: !appearance.reduce_transparency,
            motion: !appearance.reduce_motion && theme.animations,
        }
    }

    /// A panel docked in the window: sidebar, path capsule, tray.
    pub fn panel(&self) -> Style {
        Style {
            tint: if self.translucent {
                color::GLASS
            } else {
                color::SURFACE
            },
            radius: self.radius,
            rim: color::RIM,
            rim_width: glass::RIM_W,
            sheen: if self.translucent { glass::SHEEN } else { 0.0 },
            grain: if self.translucent { glass::GRAIN } else { 0.0 },
            shadow: color::SHADOW,
            shadow_offset: Vector::new(0.0, glass::SHADOW_Y),
            shadow_blur: glass::SHADOW_BLUR,
        }
    }

    /// A sheet over the content: palette, dialogs, the name sheet.
    pub fn sheet(&self) -> Style {
        Style {
            tint: if self.translucent {
                color::SHEET
            } else {
                color::SURFACE
            },
            grain: if self.translucent {
                glass::SHEET_GRAIN
            } else {
                0.0
            },
            shadow: color::SHEET_SHADOW,
            shadow_offset: Vector::new(0.0, glass::SHEET_SHADOW_Y),
            shadow_blur: glass::SHEET_SHADOW_BLUR,
            ..self.panel()
        }
    }

    /// Spring parameters, or `None` to snap.
    pub fn spring(&self, settle: Duration) -> Option<Params> {
        self.motion.then(|| Params::critical(settle))
    }

    pub fn bouncy(&self, settle: Duration) -> Option<Params> {
        self.motion.then(|| Params::bouncy(settle))
    }
}

/// The desktop theme, read once before iced starts.
static THEME: OnceLock<Theme> = OnceLock::new();
/// The look now: rebuilt when fog.kdl's `appearance` changes.
static LOOK: RwLock<Option<Look>> = RwLock::new(None);

/// Fix the desktop theme and the first look. Before iced starts; the first
/// call's theme wins.
pub fn init(theme: Theme, appearance: Appearance) {
    let _ = THEME.set(theme);
    set_appearance(appearance);
}

/// Rebuild the look for new `appearance` keys over the theme [`init`] read
/// (fog.kdl hot reload). Returns whether it changed. The contrast floor is
/// recomputed with it, so a live toggle never drops text below AA.
pub fn set_appearance(appearance: Appearance) -> bool {
    let theme = THEME.get_or_init(Theme::default);
    let next = Look::new(theme, appearance);
    let mut slot = LOOK.write().unwrap_or_else(PoisonError::into_inner);
    let changed = *slot != Some(next);
    *slot = Some(next);
    changed
}

/// The look, or the defaults' (tests, and before [`init`]). A copy: it can
/// change between frames.
pub fn look() -> Look {
    if let Some(l) = *LOOK.read().unwrap_or_else(PoisonError::into_inner) {
        return l;
    }
    let mut slot = LOOK.write().unwrap_or_else(PoisonError::into_inner);
    *slot.get_or_insert_with(Look::default)
}

/// The default font: interface text, names included, is Instrument Sans;
/// data (sizes, dates, paths, chords) asks for [`font::DATA`] by name.
pub const DEFAULT_FONT: Font = font::UI;

#[cfg(test)]
mod tests {
    use super::*;
    use fog_widgets::contrast::{over, ratio, AA, WORST};

    fn theme(opacity: f32) -> Theme {
        Theme {
            active_opacity: opacity,
            ..Theme::default()
        }
    }

    #[test]
    fn window_tint_meets_aa_over_every_worst_backdrop() {
        for op in [0.87, 0.95, 1.0] {
            let t = theme(op);
            let a = window_alpha(&t);
            assert!((WINDOW_ALPHA..=1.0).contains(&a));
            let s = surface(&t, a);
            assert!(s.worst(&TEXTS, &WORST) >= AA, "opacity {op}: alpha {a}");
        }
    }

    #[test]
    fn an_unreachable_floor_saturates_the_tint() {
        // abyss fades text with the window: at half opacity the backdrop
        // shows through even an opaque tint, so the best is fully opaque.
        for op in [0.5, 0.56] {
            let t = theme(op);
            assert_eq!(window_alpha(&t), 1.0, "opacity {op}");
            assert!(surface(&t, 1.0).worst(&TEXTS, &WORST) < AA);
        }
    }

    #[test]
    fn the_floor_raises_the_tint_only_when_it_must() {
        // A thin window over a bright backdrop needs more tint than the
        // design's.
        let t = theme(0.3);
        assert!(surface(&t, WINDOW_ALPHA).worst(&TEXTS, &WORST) < AA);
        let a = window_alpha(&t);
        assert!(a > WINDOW_ALPHA);
        // One step less fails: the least alpha is found, not any.
        let below = surface(&t, a - 1.0 / 256.0);
        assert!(below.worst(&TEXTS, &WORST) < AA);
    }

    #[test]
    fn sheet_text_meets_aa_over_the_window_it_floats_on() {
        let t = theme(0.87);
        let s = surface(&t, window_alpha(&t));
        for tint in [color::GLASS, color::SHEET] {
            for b in WORST {
                // The sheet over the window's ground, in iced's linear light;
                // its text over that.
                let ground = over(rgb3(tint), tint.a, s.ground(b), Space::Linear);
                let text = over(
                    contrast::WHITE,
                    color::TEXT_TERTIARY.a,
                    ground,
                    Space::Linear,
                );
                assert!(ratio(text, ground) >= AA, "{tint:?} over {b:?}");
            }
        }
    }

    #[test]
    fn opaque_unless_the_compositor_blurs_or_when_asked() {
        let blurs = theme(0.87);
        assert!(Look::new(&blurs, Appearance::default()).window.a < 1.0);
        assert_eq!(Look::new(&theme(1.0), Appearance::default()).window.a, 1.0);
        let reduce = Appearance {
            reduce_transparency: true,
            ..Appearance::default()
        };
        let l = Look::new(&blurs, reduce);
        assert_eq!(l.window.a, 1.0);
        assert!(l.blur.is_none() && !l.translucent);
        assert_eq!(l.panel().tint.a, 1.0);
        assert_eq!(l.sheet().tint.a, 1.0);
    }

    #[test]
    fn reduce_motion_and_disabled_animations_snap() {
        let still = Appearance {
            reduce_motion: true,
            ..Appearance::default()
        };
        assert!(Look::new(&Theme::default(), still)
            .spring(motion::PILL)
            .is_none());
        let off = Theme {
            animations: false,
            ..Theme::default()
        };
        assert!(Look::new(&off, Appearance::default())
            .spring(motion::PILL)
            .is_none());
        assert!(Look::default().spring(motion::PILL).is_some());
    }

    #[test]
    fn radii_follow_the_compositor_rounding() {
        let l = Look::new(
            &Theme {
                rounding: 9,
                ..Theme::default()
            },
            Appearance::default(),
        );
        assert_eq!((l.radius, l.inset_radius, l.chip_radius), (9.0, 6.0, 4.5));
    }

    #[test]
    fn every_vendored_face_is_a_font_file() {
        for face in font::BYTES {
            assert!(face.starts_with(&[0, 1, 0, 0]) || face.starts_with(b"OTTO"));
        }
    }
}
