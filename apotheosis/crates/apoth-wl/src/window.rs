// SPDX-License-Identifier: AGPL-3.0-only
//! Pure window logic (T-09 §4; UI-45, UI-48, UI-49): configure states, decoration
//! negotiation, CSD geometry and hit-testing, app id policy.

use std::str::FromStr;

use wayland_protocols::xdg::shell::client::xdg_toplevel::ResizeEdge as WireEdge;

/// Default app id.
pub const DEFAULT_APP_ID: &str = "org.eclipseos.Apotheosis";

/// Result of the app id policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppIdChoice {
    /// The id to set.
    pub id: String,
    /// An override was given and refused (log a diagnostic).
    pub rejected: bool,
}

/// The id is `DEFAULT_APP_ID` or a dotted sub-id of it (`...Apotheosis.Foo`); a bare string
/// prefix (`...ApotheosisEvil`) does not count.
fn has_default_prefix(id: &str) -> bool {
    id.strip_prefix(DEFAULT_APP_ID)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// Largest window dimension honoured (logical px). Compositor sizes beyond it are clamped so
/// geometry arithmetic cannot overflow `i32`.
pub const MAX_DIM: u32 = 65_535;

/// Clamp a size to [`MAX_DIM`] per axis.
pub fn clamp_size(size: (u32, u32)) -> (u32, u32) {
    (size.0.min(MAX_DIM), size.1.min(MAX_DIM))
}

/// Largest title sent in one request (bytes); Wayland messages are limited to 4096 bytes.
pub const MAX_TITLE_BYTES: usize = 1024;

/// Truncate `s` to at most `max` bytes on a UTF-8 boundary.
pub fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A title safe to put on the wire.
pub fn clamp_title(s: &str) -> String {
    truncate_utf8(s, MAX_TITLE_BYTES).to_string()
}

/// `--app-id` / profile override rules (T-09 §4, T-17 §7): on Abyss an override must start
/// with `org.eclipseos.Apotheosis`, otherwise it is ignored and the default is used.
pub fn resolve_app_id(requested: Option<&str>, on_abyss: bool) -> AppIdChoice {
    match requested.map(str::trim) {
        None | Some("") => AppIdChoice {
            id: DEFAULT_APP_ID.into(),
            rejected: false,
        },
        Some(r) if on_abyss && !has_default_prefix(r) => AppIdChoice {
            id: DEFAULT_APP_ID.into(),
            rejected: true,
        },
        Some(r) => AppIdChoice {
            id: r.into(),
            rejected: false,
        },
    }
}

/// Are we under Abyss? From `XDG_CURRENT_DESKTOP` (a colon-separated list). The spec does
/// not say how a client recognises Abyss; callers may override with an explicit value.
pub fn detect_abyss(xdg_current_desktop: Option<&str>) -> bool {
    xdg_current_desktop.is_some_and(|d| {
        d.split(':')
            .any(|p| p.eq_ignore_ascii_case("abyss") || p.eq_ignore_ascii_case("eclipse"))
    })
}

/// `xdg_toplevel` configure states (`xdg_toplevel.state`, wire values 1..=9).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct WindowStates {
    pub maximized: bool,
    pub fullscreen: bool,
    pub resizing: bool,
    pub activated: bool,
    pub tiled_left: bool,
    pub tiled_right: bool,
    pub tiled_top: bool,
    pub tiled_bottom: bool,
    pub suspended: bool,
}

impl WindowStates {
    /// Parse the `states` array (native-endian u32s). Unknown values are ignored.
    pub fn from_raw(raw: &[u8]) -> WindowStates {
        let mut s = WindowStates::default();
        for c in raw.chunks_exact(4) {
            match u32::from_ne_bytes([c[0], c[1], c[2], c[3]]) {
                1 => s.maximized = true,
                2 => s.fullscreen = true,
                3 => s.resizing = true,
                4 => s.activated = true,
                5 => s.tiled_left = true,
                6 => s.tiled_right = true,
                7 => s.tiled_top = true,
                8 => s.tiled_bottom = true,
                9 => s.suspended = true,
                _ => {}
            }
        }
        s
    }

    /// Any tiled edge.
    pub fn is_tiled(&self) -> bool {
        self.tiled_left || self.tiled_right || self.tiled_top || self.tiled_bottom
    }

    /// `tiled_*`, `maximized` and `fullscreen` drop shadow and corner radius.
    pub fn flat(&self) -> bool {
        self.maximized || self.fullscreen || self.is_tiled()
    }

    /// `suspended` stops rendering entirely.
    pub fn should_render(&self) -> bool {
        !self.suspended
    }
}

/// `window.decorations`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecorationPref {
    /// Ask for server-side; accept what the compositor says.
    #[default]
    Auto,
    /// Draw our own frame.
    Client,
    /// Insist on server-side.
    Server,
    /// No frame at all.
    None,
}

impl FromStr for DecorationPref {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "auto" => Ok(Self::Auto),
            "client" => Ok(Self::Client),
            "server" => Ok(Self::Server),
            "none" => Ok(Self::None),
            _ => Err(()),
        }
    }
}

/// `zxdg_toplevel_decoration_v1.mode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationMode {
    /// We draw.
    Client,
    /// The compositor draws.
    Server,
}

/// Who draws the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// The compositor.
    Server,
    /// Us (CSD, single surface).
    Client,
    /// Nobody (`window.decorations none`).
    Bare,
}

/// The mode to request from the compositor, if a decoration object exists.
pub fn requested_mode(pref: DecorationPref) -> DecorationMode {
    match pref {
        DecorationPref::Auto | DecorationPref::Server => DecorationMode::Server,
        DecorationPref::Client | DecorationPref::None => DecorationMode::Client,
    }
}

/// Resolve the frame owner. `configure(mode)` is authoritative; without the global it is CSD.
pub fn resolve_frame(
    pref: DecorationPref,
    decoration_global: bool,
    configured: Option<DecorationMode>,
) -> FrameKind {
    if pref == DecorationPref::None {
        // `configure(mode)` is authoritative: a compositor that insists on server-side
        // decorations draws the frame even though we asked for none.
        return if configured == Some(DecorationMode::Server) {
            FrameKind::Server
        } else {
            FrameKind::Bare
        };
    }
    if !decoration_global {
        return FrameKind::Client;
    }
    match configured.unwrap_or_else(|| requested_mode(pref)) {
        DecorationMode::Server => FrameKind::Server,
        DecorationMode::Client => FrameKind::Client,
    }
}

/// Integer rectangle in surface-local logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    /// Contains the point.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= f64::from(self.x)
            && y >= f64::from(self.y)
            && x < f64::from(self.x) + f64::from(self.w)
            && y < f64::from(self.y) + f64::from(self.h)
    }
}

/// CSD sizes in logical pixels. T-09 gives no numbers; these are conservative defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CsdMetrics {
    /// Client-drawn shadow margin around the geometry (off Abyss only).
    pub shadow_margin: u32,
    /// Width of the resize band.
    pub resize_margin: u32,
    /// Title bar height (drag area).
    pub title_height: u32,
}

impl Default for CsdMetrics {
    fn default() -> Self {
        CsdMetrics {
            shadow_margin: 24,
            resize_margin: 8,
            title_height: 32,
        }
    }
}

/// Layout of a CSD window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CsdLayout {
    /// Surface size (logical): geometry plus shadow margins.
    pub surface: (u32, u32),
    /// Window geometry (`xdg_surface.set_window_geometry`): excludes the shadow margin.
    pub geometry: Rect,
    /// Input region: geometry plus the resize margin, clipped to the surface.
    pub input: Rect,
}

/// Compute the CSD layout for a window of `window` logical size (the configured size).
/// On Abyss, and in flat states, no shadow margin is drawn; flat states also drop the
/// resize band beyond the geometry (UI-48, UI-49).
pub fn csd_layout(
    on_abyss: bool,
    window: (u32, u32),
    m: &CsdMetrics,
    st: WindowStates,
) -> CsdLayout {
    let shadow = (if on_abyss || st.flat() {
        0
    } else {
        m.shadow_margin.min(MAX_DIM)
    }) as i32;
    let resize = (if st.flat() {
        0
    } else {
        m.resize_margin.min(MAX_DIM)
    }) as i32;
    let (w, h) = (window.0.min(MAX_DIM) as i32, window.1.min(MAX_DIM) as i32);
    let surface = (w + 2 * shadow, h + 2 * shadow);
    let geometry = Rect {
        x: shadow,
        y: shadow,
        w,
        h,
    };
    let x0 = (geometry.x - resize).max(0);
    let y0 = (geometry.y - resize).max(0);
    let x1 = (geometry.x + w + resize).min(surface.0);
    let y1 = (geometry.y + h + resize).min(surface.1);
    CsdLayout {
        surface: (surface.0 as u32, surface.1 as u32),
        geometry,
        input: Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        },
    }
}

/// Resize edge for `xdg_toplevel.resize`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum ResizeEdge {
    Top,
    Bottom,
    Left,
    Right,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl ResizeEdge {
    /// The protocol value.
    pub fn to_wire(self) -> WireEdge {
        match self {
            ResizeEdge::Top => WireEdge::Top,
            ResizeEdge::Bottom => WireEdge::Bottom,
            ResizeEdge::Left => WireEdge::Left,
            ResizeEdge::Right => WireEdge::Right,
            ResizeEdge::TopLeft => WireEdge::TopLeft,
            ResizeEdge::TopRight => WireEdge::TopRight,
            ResizeEdge::BottomLeft => WireEdge::BottomLeft,
            ResizeEdge::BottomRight => WireEdge::BottomRight,
        }
    }
}

/// Where a pointer position lands in a CSD window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsdHit {
    /// Outside the input region (shadow clicks fall through).
    Outside,
    /// The content area.
    Client,
    /// The title bar: drag calls `move`, right-click `show_window_menu`.
    Title,
    /// A resize band: calls `resize`.
    Edge(ResizeEdge),
}

/// Hit-test a surface-local position.
pub fn hit_test(x: f64, y: f64, layout: &CsdLayout, m: &CsdMetrics, st: WindowStates) -> CsdHit {
    if !layout.input.contains(x, y) {
        return CsdHit::Outside;
    }
    let g = layout.geometry;
    let band = if st.flat() {
        0.0
    } else {
        f64::from(m.resize_margin)
    };
    let left = x < f64::from(g.x) + band;
    let right = x >= f64::from(g.x) + f64::from(g.w) - band;
    let top = y < f64::from(g.y) + band;
    let bottom = y >= f64::from(g.y) + f64::from(g.h) - band;
    let edge = match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeEdge::TopLeft),
        (_, true, true, _) => Some(ResizeEdge::TopRight),
        (true, _, _, true) => Some(ResizeEdge::BottomLeft),
        (_, true, _, true) => Some(ResizeEdge::BottomRight),
        (true, ..) => Some(ResizeEdge::Left),
        (_, true, ..) => Some(ResizeEdge::Right),
        (_, _, true, _) => Some(ResizeEdge::Top),
        (_, _, _, true) => Some(ResizeEdge::Bottom),
        _ => None,
    };
    if let Some(e) = edge {
        return CsdHit::Edge(e);
    }
    if y < f64::from(g.y) + f64::from(m.title_height) {
        CsdHit::Title
    } else {
        CsdHit::Client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_requires_dot_boundary() {
        for bad in [
            "org.eclipseos.ApotheosisEvil",
            "org.eclipseos.Apotheosis-spoof",
        ] {
            assert!(resolve_app_id(Some(bad), true).rejected, "{bad}");
        }
        assert!(!resolve_app_id(Some("org.eclipseos.Apotheosis"), true).rejected);
        assert!(!resolve_app_id(Some("org.eclipseos.Apotheosis.X"), true).rejected);
    }

    #[test]
    fn none_pref_yields_to_configured_server() {
        assert_eq!(
            resolve_frame(DecorationPref::None, true, Some(DecorationMode::Server)),
            FrameKind::Server
        );
        assert_eq!(
            resolve_frame(DecorationPref::None, true, Some(DecorationMode::Client)),
            FrameKind::Bare
        );
        assert_eq!(
            resolve_frame(DecorationPref::None, true, None),
            FrameKind::Bare
        );
    }

    #[test]
    fn huge_sizes_do_not_overflow() {
        let m = CsdMetrics {
            shadow_margin: u32::MAX,
            resize_margin: u32::MAX,
            title_height: 32,
        };
        let l = csd_layout(false, (u32::MAX, u32::MAX), &m, WindowStates::default());
        assert!(l.surface.0 <= 3 * MAX_DIM);
        let r = Rect {
            x: i32::MAX,
            y: i32::MAX,
            w: i32::MAX,
            h: i32::MAX,
        };
        assert!(!r.contains(0.0, 0.0));
        assert_eq!(clamp_size((u32::MAX, 5)), (MAX_DIM, 5));
    }

    #[test]
    fn title_truncates_on_boundary() {
        let t = "€".repeat(1000);
        let c = clamp_title(&t);
        assert!(c.len() <= MAX_TITLE_BYTES && t.starts_with(&c));
    }

    fn raw(states: &[u32]) -> Vec<u8> {
        states.iter().flat_map(|s| s.to_ne_bytes()).collect()
    }

    #[test]
    fn app_id_policy() {
        assert_eq!(resolve_app_id(None, true).id, DEFAULT_APP_ID);
        assert!(!resolve_app_id(None, true).rejected);
        // on Abyss: prefix required
        let ok = resolve_app_id(Some("org.eclipseos.Apotheosis.Scratch"), true);
        assert_eq!(
            (ok.id.as_str(), ok.rejected),
            ("org.eclipseos.Apotheosis.Scratch", false)
        );
        let bad = resolve_app_id(Some("com.evil.Terminal"), true);
        assert_eq!((bad.id.as_str(), bad.rejected), (DEFAULT_APP_ID, true));
        // off Abyss anything goes
        assert_eq!(resolve_app_id(Some("my.term"), false).id, "my.term");
        assert!(!resolve_app_id(Some("  "), true).rejected);
    }

    #[test]
    fn abyss_detection() {
        assert!(detect_abyss(Some("Abyss")));
        assert!(detect_abyss(Some("GNOME:Eclipse")));
        assert!(!detect_abyss(Some("KDE")));
        assert!(!detect_abyss(None));
    }

    #[test]
    fn states_parse() {
        let s = WindowStates::from_raw(&raw(&[1, 4, 9, 99]));
        assert!(s.maximized && s.activated && s.suspended);
        assert!(!s.fullscreen && !s.is_tiled());
        assert!(s.flat());
        assert!(!s.should_render());
        let t = WindowStates::from_raw(&raw(&[5, 8]));
        assert!(t.is_tiled() && t.flat() && t.should_render());
        // a trailing partial word is ignored
        let mut r = raw(&[2]);
        r.extend([1, 2]);
        assert!(WindowStates::from_raw(&r).fullscreen);
        assert_eq!(WindowStates::from_raw(&[]), WindowStates::default());
    }

    #[test]
    fn decoration_resolution() {
        use DecorationMode as M;
        use DecorationPref as P;
        // absent global means CSD
        assert_eq!(resolve_frame(P::Auto, false, None), FrameKind::Client);
        // configure is authoritative, whatever we asked for
        assert_eq!(
            resolve_frame(P::Auto, true, Some(M::Client)),
            FrameKind::Client
        );
        assert_eq!(
            resolve_frame(P::Client, true, Some(M::Server)),
            FrameKind::Server
        );
        // before the first configure, our request decides
        assert_eq!(resolve_frame(P::Auto, true, None), FrameKind::Server);
        assert_eq!(resolve_frame(P::Server, true, None), FrameKind::Server);
        assert_eq!(resolve_frame(P::Client, true, None), FrameKind::Client);
        // `none`: bare, unless the compositor insists on drawing the frame
        assert_eq!(
            resolve_frame(P::None, true, Some(M::Server)),
            FrameKind::Server
        );
        assert_eq!(requested_mode(P::Auto), M::Server);
        assert_eq!(requested_mode(P::None), M::Client);
        assert_eq!("server".parse::<P>(), Ok(P::Server));
        assert!("bogus".parse::<P>().is_err());
    }

    #[test]
    fn csd_layout_off_abyss_has_shadow_margin() {
        let m = CsdMetrics::default();
        let l = csd_layout(false, (800, 600), &m, WindowStates::default());
        assert_eq!(l.surface, (848, 648));
        assert_eq!(
            l.geometry,
            Rect {
                x: 24,
                y: 24,
                w: 800,
                h: 600
            }
        );
        // input = geometry + 8 px band; the rest of the shadow falls through
        assert_eq!(
            l.input,
            Rect {
                x: 16,
                y: 16,
                w: 816,
                h: 616
            }
        );
        assert!(!l.input.contains(5.0, 5.0));
    }

    #[test]
    fn csd_layout_on_abyss_and_flat_have_no_shadow() {
        let m = CsdMetrics::default();
        let a = csd_layout(true, (800, 600), &m, WindowStates::default());
        assert_eq!(a.surface, (800, 600));
        assert_eq!(
            a.geometry,
            Rect {
                x: 0,
                y: 0,
                w: 800,
                h: 600
            }
        );
        assert_eq!(
            a.input,
            Rect {
                x: 0,
                y: 0,
                w: 800,
                h: 600
            }
        ); // clipped to the surface
        let flat = WindowStates {
            maximized: true,
            ..Default::default()
        };
        let f = csd_layout(false, (800, 600), &m, flat);
        assert_eq!(f.surface, (800, 600));
        assert_eq!(f.input, f.geometry);
    }

    #[test]
    fn hit_testing() {
        let m = CsdMetrics::default();
        let st = WindowStates::default();
        let l = csd_layout(false, (800, 600), &m, st);
        let h = |x, y| hit_test(x, y, &l, &m, st);
        assert_eq!(h(3.0, 3.0), CsdHit::Outside);
        assert_eq!(h(20.0, 300.0), CsdHit::Edge(ResizeEdge::Left)); // in the margin outside
        assert_eq!(h(26.0, 300.0), CsdHit::Edge(ResizeEdge::Left)); // inside the band
        assert_eq!(h(24.0 + 799.0, 300.0), CsdHit::Edge(ResizeEdge::Right));
        assert_eq!(h(400.0, 18.0), CsdHit::Edge(ResizeEdge::Top));
        assert_eq!(h(400.0, 24.0 + 599.0), CsdHit::Edge(ResizeEdge::Bottom));
        assert_eq!(h(18.0, 18.0), CsdHit::Edge(ResizeEdge::TopLeft));
        assert_eq!(
            h(24.0 + 799.0, 24.0 + 599.0),
            CsdHit::Edge(ResizeEdge::BottomRight)
        );
        assert_eq!(h(400.0, 24.0 + 20.0), CsdHit::Title);
        assert_eq!(h(400.0, 24.0 + 100.0), CsdHit::Client);
    }

    #[test]
    fn flat_windows_have_no_resize_band() {
        let m = CsdMetrics::default();
        let st = WindowStates {
            fullscreen: true,
            ..Default::default()
        };
        let l = csd_layout(false, (800, 600), &m, st);
        assert_eq!(hit_test(1.0, 300.0, &l, &m, st), CsdHit::Client);
    }

    #[test]
    fn edge_wire_mapping() {
        assert_eq!(ResizeEdge::BottomLeft.to_wire(), WireEdge::BottomLeft);
        assert_eq!(ResizeEdge::Top.to_wire(), WireEdge::Top);
    }
}
