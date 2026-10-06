// SPDX-License-Identifier: AGPL-3.0-only
//! `input` block: keyboard, touchpad and per-device overrides (COMP-13 §1.2,
//! COMP-04).

/// `input { ... }` (COMP-13 §1.2, COMP-04). Keyboard settings are pushed to the
/// seat on reload; pointer settings are applied per libinput device as it
/// appears, and on reload to every device already open.
#[derive(Debug, Clone)]
pub struct Input {
    pub kb_layout: String,
    pub kb_variant: String,
    pub kb_options: Option<String>,
    pub repeat_rate: i32,
    pub repeat_delay: i32,
    /// `flat` | `adaptive`.
    pub accel_profile: String,
    /// libinput's normalised speed, -1.0..=1.0; 0 is the device default.
    pub accel_speed: f64,
    /// Scroll method for pointers that are not touchpads: `default` (leave
    /// the device's own, which is how a trackpoint keeps scrolling), `none`
    /// or `on-button-down`.
    pub scroll_method: String,
    pub touchpad: Touchpad,
    /// `device "<libinput name>" { .. }` children in file order, one per
    /// name: a later block for the same name overrides an earlier one key by
    /// key. KDL-only.
    pub devices: Vec<InputDevice>,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            kb_layout: "us".into(),
            kb_variant: String::new(),
            kb_options: None,
            repeat_rate: 40,
            repeat_delay: 300,
            accel_profile: "adaptive".into(),
            accel_speed: 0.0,
            scroll_method: "default".into(),
            touchpad: Touchpad::default(),
            devices: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Touchpad {
    pub natural_scroll: bool,
    pub tap_to_click: bool,
    /// Disable-while-typing.
    pub dwt: bool,
    /// `clickfinger` | `button-areas`.
    pub click_method: String,
    /// Tap, then hold the second tap down, to drag.
    pub tap_and_drag: bool,
    /// `two-finger` | `edge` | `none`.
    pub scroll_method: String,
}

/// One `input { device "<name>" { .. } }` block: any subset of the global
/// pointer keys, plus `calibration`, which only a device block may carry.
/// `None` inherits the global key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputDevice {
    /// The libinput device name, matched exactly.
    pub name: String,
    pub accel_profile: Option<String>,
    pub accel_speed: Option<f64>,
    pub scroll_method: Option<String>,
    pub touchpad: TouchpadOverride,
    /// libinput's 2x3 calibration matrix, row-major.
    pub calibration: Option<[f32; 6]>,
}

/// The `touchpad { }` half of a device block; `None` inherits.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TouchpadOverride {
    pub natural_scroll: Option<bool>,
    pub tap_to_click: Option<bool>,
    pub tap_and_drag: Option<bool>,
    pub dwt: Option<bool>,
    pub click_method: Option<String>,
    pub scroll_method: Option<String>,
}

/// One validated pointer key of `input` or of a `device` block.
pub(crate) enum PointerKey {
    AccelProfile(String),
    AccelSpeed(f64),
    ScrollMethod(String),
    Calibration([f32; 6]),
}

/// One validated key of a `touchpad` block.
pub(crate) enum TouchpadKey {
    NaturalScroll(bool),
    TapToClick(bool),
    TapAndDrag(bool),
    Dwt(bool),
    ClickMethod(String),
    ScrollMethod(String),
}

impl Default for Touchpad {
    fn default() -> Self {
        Self {
            natural_scroll: false,
            tap_to_click: false,
            dwt: false,
            click_method: "clickfinger".into(),
            tap_and_drag: true,
            scroll_method: "two-finger".into(),
        }
    }
}
