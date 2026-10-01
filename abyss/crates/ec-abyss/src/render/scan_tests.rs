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
