// SPDX-License-Identifier: AGPL-3.0-only
//! Source-scanning checks that span `capture.rs` and the backends. They moved
//! here from the `select`, `annotation` and `drop` modules when those went to
//! `ec-abyss-render`: `capture.rs` and `backend/*.rs` stay in this crate.

/// ADR 0040: annotations are capture-invisible *by construction* --
/// `capture::capture_elements` builds its own pass list and never consults
/// the annotation module. There is no GL context in a unit test to compare
/// two rendered lists, so the assertion is made where the property actually
/// lives: in the source of the capture pass. The pick marker (ADR 0054) is
/// built by `draw` in the annotation module, so it is covered by the same
/// assertion.
#[test]
fn the_capture_pass_cannot_see_annotations() {
    let src = include_str!("capture.rs");
    assert!(
        !src.contains("annotation"),
        "capture.rs referenced the annotation pass; capture exclusion is \
         supposed to hold because it never looks"
    );
}

/// COMP-18 §1: the per-frame element vector is front-to-back, so trusted
/// UI must be spliced ahead of annotations in every backend. Each backend
/// expresses that differently -- drm appends, winit and headless splice at
/// zero -- so the check is per file and on the relative order of the two
/// calls, which is the thing that must not be swapped.
#[test]
fn the_trusted_indicator_stays_above_annotations_in_every_backend() {
    for (src, path, indicator_first) in [
        (include_str!("../backend/drm.rs"), "drm.rs", true),
        (include_str!("../backend/winit.rs"), "winit.rs", false),
        (include_str!("../backend/headless.rs"), "headless.rs", false),
    ] {
        let ann = src
            .find("annotation::annotation_elements")
            .unwrap_or_else(|| panic!("{path} does not draw annotations"));
        let ind = src
            .find("capture::indicator")
            .unwrap_or_else(|| panic!("{path} does not draw the indicator"));
        assert_eq!(
            ind < ann,
            indicator_first,
            "{path} puts the annotation pass on the wrong side of trusted UI"
        );
    }
}

/// COMP-02 §9: animation is render-only and never in capture. Ghosts and
/// snapshots are drawn by `collect_elements` alone; `capture.rs` builds its own
/// pass from `space`, so a closing window's leftover frame can never reach a
/// screenshot or a screencast. Asserted where the property lives, in the source.
#[test]
fn the_capture_pass_cannot_see_animation_or_ghosts() {
    let src = include_str!("capture.rs").to_ascii_lowercase();
    for word in ["anim", "ghost", "snapshot"] {
        assert!(
            !src.contains(word),
            "capture.rs references `{word}`; capture must never read the animation store"
        );
    }
}

/// CAP-01: captures draw the screen's effects through `still.rs`, which must
/// hold the same line `capture.rs` does. It never reads the on-screen store's
/// in-flight state or ghosts (so nothing mid-transition reaches a capture),
/// and it never names the on-screen store (so a captured backdrop can only
/// sample the redacted list it is spliced into).
#[test]
fn the_capture_effects_cannot_see_animation_or_the_screen_store() {
    let src = include_str!("../../../ec-abyss-render/src/still.rs").to_ascii_lowercase();
    for word in ["anim", "ghost", "snapshot", "annotation", "select", "drop::"] {
        assert!(
            !src.contains(word),
            "still.rs references `{word}`; the capture effects must not read it"
        );
    }
    let capture = include_str!("capture.rs");
    assert!(
        !capture.contains("state.borders") && !capture.contains("collect_elements"),
        "capture.rs reaches the on-screen render store; its effects must come from `still`"
    );
}

#[test]
fn the_capture_pass_cannot_see_the_selector() {
    let src = include_str!("capture.rs");
    assert!(
        !src.contains("selector_elements") && !src.contains("region_select"),
        "capture.rs names the region selector; a selector in a capture would be read back as screen content"
    );
}

#[test]
fn the_selector_sits_between_annotations_and_the_trusted_indicator_in_every_backend() {
    for (src, path, indicator_first) in [
        (include_str!("../backend/drm.rs"), "drm.rs", true),
        (include_str!("../backend/winit.rs"), "winit.rs", false),
        (include_str!("../backend/headless.rs"), "headless.rs", false),
    ] {
        let ann = src
            .find("annotation::annotation_elements")
            .unwrap_or_else(|| panic!("{path} does not draw annotations"));
        let sel = src
            .find("select::selector_elements")
            .unwrap_or_else(|| panic!("{path} does not draw the region selector"));
        let ind = src
            .find("capture::indicator")
            .unwrap_or_else(|| panic!("{path} does not draw the indicator"));
        // drm appends top-first; winit and headless splice each pass in at
        // index 0, which reverses source order.
        assert_eq!(
            ind < sel,
            indicator_first,
            "{path}: selector is on the wrong side of trusted UI"
        );
        assert_eq!(
            sel < ann,
            indicator_first,
            "{path}: selector is on the wrong side of the annotation pass"
        );
    }
}

#[test]
fn the_capture_pass_cannot_see_the_guides() {
    let src = include_str!("capture.rs");
    assert!(
        !src.contains("drop_elements") && !src.contains("tile_drag"),
        "capture.rs names the drop guides; they are compositor chrome, not screen content"
    );
}

#[test]
fn every_backend_draws_the_guides() {
    for (src, path) in [
        (include_str!("../backend/drm.rs"), "drm.rs"),
        (include_str!("../backend/winit.rs"), "winit.rs"),
        (include_str!("../backend/headless.rs"), "headless.rs"),
    ] {
        assert!(
            src.contains("drop::drop_elements"),
            "{path} does not draw the drop guides"
        );
    }
}

#[test]
fn the_capture_pass_cannot_see_the_window_menu() {
    // The window menu is compositor chrome (COMP-05 §7.1, ADR 0090): an overlay
    // pass spliced in by the backends, never part of what a capture records.
    let src = include_str!("capture.rs");
    for name in ["menu_elements", "winmenu", "MenuView", "menu_cache"] {
        assert!(
            !src.contains(name),
            "capture.rs names `{name}`; the window menu is never in a capture pass list"
        );
    }
}

#[test]
fn every_backend_draws_the_window_menu() {
    for (src, path) in [
        (include_str!("../backend/drm.rs"), "drm.rs"),
        (include_str!("../backend/winit.rs"), "winit.rs"),
        (include_str!("../backend/headless.rs"), "headless.rs"),
    ] {
        assert!(
            src.contains("menu::menu_elements"),
            "{path} does not draw the window menu"
        );
    }
}

#[test]
fn the_capture_pass_cannot_see_the_tile_preview() {
    // The Quick Tile edge preview (`mode "de"`, COMP-05 §3.1) is a drag overlay
    // like the Radiant guides: spliced in by the backends, never recorded.
    let src = include_str!("capture.rs");
    for name in ["preview_elements", "tile_preview", "quicktile"] {
        assert!(
            !src.contains(name),
            "capture.rs names `{name}`; the tile preview is never in a capture pass list"
        );
    }
    let still = include_str!("../../../ec-abyss-render/src/still.rs");
    assert!(
        !still.contains("preview_elements"),
        "still.rs draws the tile preview into a capture's effects"
    );
}

#[test]
fn every_backend_draws_the_tile_preview() {
    for (src, path) in [
        (include_str!("../backend/drm.rs"), "drm.rs"),
        (include_str!("../backend/winit.rs"), "winit.rs"),
        (include_str!("../backend/headless.rs"), "headless.rs"),
    ] {
        assert!(
            src.contains("drop::preview_elements"),
            "{path} does not draw the tile preview"
        );
    }
}

#[test]
fn the_capture_pass_cannot_see_the_overlays() {
    // The window switcher, the overview and hover previews (COMP-05 §7.1,
    // ADR 0090) are compositor overlays: spliced in by the backends, never
    // recorded. `capture.rs` may *classify* a window (the thumbnails read it)
    // but must not draw or read any overlay.
    let src = include_str!("capture.rs");
    for name in [
        "overlay_elements",
        "overview::",
        "OverlayView",
        "OverlayCache",
        "overlay_cache",
        "ThumbStore",
        "state.thumbs",
        "state.switcher",
        "state.overview",
        "state.preview",
        "state.corners",
    ] {
        assert!(
            !src.contains(name),
            "capture.rs names `{name}`; the overlays are never in a capture pass list"
        );
    }
    for (name, still) in [
        ("still.rs", include_str!("../../../ec-abyss-render/src/still.rs")),
        (
            "effects.rs",
            include_str!("../../../ec-abyss-render/src/effects.rs"),
        ),
    ] {
        assert!(
            !still.contains("overlay_elements") && !still.contains("overview::"),
            "{name} draws an overlay into a capture's effects"
        );
    }
}

#[test]
fn every_backend_draws_the_overlays_before_trusted_ui() {
    // Same front-to-back rule as the other untrusted passes: trusted UI is
    // spliced ahead of the overlay in every backend (drm appends top-first,
    // winit and headless splice each pass in at index 0).
    for (src, path, indicator_first) in [
        (include_str!("../backend/drm.rs"), "drm.rs", true),
        (include_str!("../backend/winit.rs"), "winit.rs", false),
        (include_str!("../backend/headless.rs"), "headless.rs", false),
    ] {
        let overlay = src
            .find("overview::overlay_elements")
            .or_else(|| src.find("overview::draw"))
            .unwrap_or_else(|| panic!("{path} does not draw the overlays"));
        let ind = src
            .find("capture::indicator")
            .unwrap_or_else(|| panic!("{path} does not draw the indicator"));
        assert_eq!(
            ind < overlay,
            indicator_first,
            "{path} puts the overlay pass on the wrong side of trusted UI"
        );
        let trusted = src
            .find("trusted_ui::elements")
            .unwrap_or_else(|| panic!("{path} does not draw trusted UI"));
        assert_eq!(
            trusted < overlay,
            indicator_first,
            "{path}: overlay and trusted UI are in the wrong order"
        );
    }
}

#[test]
fn only_a_cleared_window_reaches_a_thumbnail() {
    // The one door to the GPU is `thumbs::refresh`, and it draws a window only
    // for a face `shown` (which asks `classify`) gave pixels. The renderer-side
    // store is state-free, so this is asserted in the source: `shown` calls
    // `classify`, and `refresh` skips every face but `Pixels`.
    let thumbs = include_str!("../overview/thumbs.rs");
    assert!(thumbs.contains("classify(state, window, geo)"));
    let refresh = &thumbs[thumbs.find("pub fn refresh").expect("refresh")..];
    let first_render = refresh.find("store.render(").expect("render call");
    let gate = refresh.find("split(wanted)").expect("the pixels gate");
    assert!(
        gate < first_render,
        "the pixels gate must come before the render call"
    );
    let split = &thumbs[thumbs.find("pub fn split").expect("split")..];
    assert!(
        split[..split.find("\n}\n").unwrap()].contains("Face::Pixels"),
        "split gates on the pixels face"
    );
    // And nothing else in the overlay code renders a thumbnail.
    for (name, src) in [
        ("mod.rs", include_str!("../overview/mod.rs")),
        ("switcher.rs", include_str!("../overview/switcher.rs")),
        ("preview.rs", include_str!("../overview/preview.rs")),
        ("corners.rs", include_str!("../overview/corners.rs")),
    ] {
        assert!(
            !src.contains("store.render(") && !src.contains("ThumbStore"),
            "overview/{name} renders a thumbnail outside the classified path"
        );
    }
}
