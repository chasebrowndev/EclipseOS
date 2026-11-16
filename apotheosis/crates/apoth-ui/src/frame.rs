// SPDX-License-Identifier: AGPL-3.0-only
//! Window frame (T-08 §4): where the tab strip and the content area go.
//!
//! On Abyss the compositor draws backdrop, rim, bezel and shadow, so the client lays
//! no frame of its own; the window frame only decides the strip and content rects.
//! CSD (off Abyss) paints a tint, a hairline rim and, with blur off, the analytic shadow.

use crate::draw::{DrawList, Rim};
use crate::geom::Rect;
use crate::tokens::{Palette, metrics, radius};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabStyle {
    Floating,
    Flat,
    Hidden,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabPosition {
    Top,
    Bottom,
}

/// `tabs.show`: strip hidden with one tab (`Auto`) or always shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabsShow {
    Auto,
    Always,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    pub tiled: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

/// `window.hide-chrome`: states in which the chrome is hidden (default fullscreen).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HideChrome {
    pub tiled: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

impl Default for HideChrome {
    fn default() -> Self {
        HideChrome {
            tiled: false,
            maximized: false,
            fullscreen: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FrameSpec {
    pub window: Rect,
    pub tab_count: usize,
    pub style: TabStyle,
    pub position: TabPosition,
    pub show: TabsShow,
    pub state: WindowState,
    pub hide_chrome: HideChrome,
    /// Client-side decorations (off Abyss, or `window.decorations client`).
    pub csd: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameLayout {
    pub tab_strip: Option<Rect>,
    pub content: Rect,
}

fn chrome_hidden(s: &FrameSpec) -> bool {
    (s.state.tiled && s.hide_chrome.tiled)
        || (s.state.maximized && s.hide_chrome.maximized)
        || (s.state.fullscreen && s.hide_chrome.fullscreen)
}

pub fn layout(s: &FrameSpec) -> FrameLayout {
    let strip_shown = s.style != TabStyle::Hidden
        && !chrome_hidden(s)
        && (s.tab_count > 1 || (s.show == TabsShow::Always && s.tab_count > 0));
    if !strip_shown {
        return FrameLayout {
            tab_strip: None,
            content: s.window,
        };
    }
    match s.position {
        TabPosition::Top => {
            let (strip, rest) = s
                .window
                .split_top(metrics::TAB_H + 2.0 * crate::tokens::space::XS);
            FrameLayout {
                tab_strip: Some(strip),
                content: rest,
            }
        }
        TabPosition::Bottom => {
            let (rest, strip) = s
                .window
                .split_bottom(metrics::TAB_H + 2.0 * crate::tokens::space::XS);
            FrameLayout {
                tab_strip: Some(strip),
                content: rest,
            }
        }
    }
}

/// Window-level paint. Nothing on Abyss SSD (the compositor owns the material).
/// CSD lays tint + hairline; the shadow only with blur off (`blur_on == false`).
pub fn draw(spec: &FrameSpec, pal: &Palette, blur_on: bool, out: &mut DrawList) {
    if !spec.csd || chrome_hidden(spec) {
        return;
    }
    if !blur_on {
        out.cmds.push(crate::draw::Cmd::Shadow {
            rect: spec.window,
            radius: radius::WINDOW,
            spread: crate::tokens::space::LG,
            alpha: pal.shadow.a(),
        });
    }
    out.rect(
        spec.window,
        radius::WINDOW,
        pal.bar_tint,
        Some(Rim {
            width: metrics::RIM_W,
            colour: pal.hairline,
        }),
    );
}
