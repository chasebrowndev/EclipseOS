// SPDX-License-Identifier: AGPL-3.0-only
//! Presentation policy (T-07 §3; RENDERING-59, -61, -62, -63, -68; PERFORMANCE-76).
//!
//! The decisions are pure functions over capability lists so they test without a GPU. The
//! executor maps wgpu's `PresentMode`, `CompositeAlphaMode`, `DeviceType` and
//! `SurfaceError` onto these enums; wgpu 30 was not readable when this was written, so that
//! mapping is not here.

/// Present modes the policy distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentKind {
    /// Mailbox.
    Mailbox,
    /// Fifo.
    Fifo,
    /// Any other mode (never requested; Immediate in particular).
    Other,
}

/// Mailbox if offered, else Fifo. `Immediate` is never requested.
#[must_use]
pub fn choose_present_mode(offered: &[PresentKind]) -> PresentKind {
    if offered.contains(&PresentKind::Mailbox) {
        PresentKind::Mailbox
    } else {
        PresentKind::Fifo
    }
}

/// `desired_maximum_frame_latency`.
pub const FRAME_LATENCY: u32 = 1;

/// Composite alpha modes the policy distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaKind {
    /// Opaque.
    Opaque,
    /// Premultiplied.
    PreMultiplied,
    /// Anything else (post-multiplied, inherit).
    Other,
}

/// The alpha choice and whether `apoth doctor` must report a fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlphaChoice {
    /// Chosen mode.
    pub mode: AlphaKind,
    /// True when premultiplied is unavailable and the surface is opaque.
    pub opaque_fallback: bool,
}

/// PreMultiplied when offered, else opaque (reported).
#[must_use]
pub fn choose_alpha(offered: &[AlphaKind]) -> AlphaChoice {
    if offered.contains(&AlphaKind::PreMultiplied) {
        AlphaChoice {
            mode: AlphaKind::PreMultiplied,
            opaque_fallback: false,
        }
    } else {
        AlphaChoice {
            mode: AlphaKind::Opaque,
            opaque_fallback: true,
        }
    }
}

/// Adapter classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterClass {
    /// Discrete GPU.
    Discrete,
    /// Integrated GPU.
    Integrated,
    /// Virtual GPU.
    Virtual,
    /// Software rasteriser (lavapipe, llvmpipe).
    Cpu,
    /// Unknown.
    Other,
}

/// What the selector knows about one adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterCandidate {
    /// Class.
    pub class: AdapterClass,
    /// The surface offers an alpha-capable (PreMultiplied) mode on this adapter.
    pub alpha_capable: bool,
    /// The compositor renders on this adapter.
    pub compositor: bool,
}

/// Picks an adapter index: the compositor's adapter if usable; otherwise hardware with an
/// alpha-capable surface; software only when no hardware adapter offers alpha (or none
/// exists). Returns `None` for an empty list.
#[must_use]
pub fn choose_adapter(c: &[AdapterCandidate]) -> Option<usize> {
    let hardware = |a: &AdapterCandidate| a.class != AdapterClass::Cpu;
    let any_hw_alpha = c.iter().any(|a| hardware(a) && a.alpha_capable);
    let score = |a: &AdapterCandidate| -> i32 {
        let mut s = 0;
        if hardware(a) {
            s += 100;
        } else if any_hw_alpha {
            s -= 1000; // never software while hardware offers alpha
        }
        if a.alpha_capable {
            s += 50;
        }
        if a.compositor {
            s += 30;
        }
        s += match a.class {
            AdapterClass::Discrete => 3,
            AdapterClass::Integrated => 2,
            AdapterClass::Virtual => 1,
            _ => 0,
        };
        s
    };
    c.iter()
        .enumerate()
        .max_by_key(|(i, a)| (score(a), std::cmp::Reverse(*i)))
        .map(|(i, _)| i)
}

/// Surface acquire/present failures the policy distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceFault {
    /// Outdated.
    Outdated,
    /// Suboptimal.
    Suboptimal,
    /// Lost.
    Lost,
    /// Occluded.
    Occluded,
    /// Timeout (not named by T-07; treated like Occluded: skip the frame).
    Timeout,
    /// The device was lost.
    DeviceLost,
    /// Out of memory or an internal error.
    Fatal,
}

/// What to do about a fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultAction {
    /// Reconfigure the surface and draw again.
    Reconfigure,
    /// Recreate the surface.
    RecreateSurface,
    /// Rebuild device, pipelines and atlases from CPU state.
    RebuildDevice,
    /// Skip this frame (the next damage or frame callback retries).
    SkipFrame,
    /// Unrecoverable.
    Abort,
}

/// Maps a fault to its action.
#[must_use]
pub fn fault_action(f: SurfaceFault) -> FaultAction {
    match f {
        SurfaceFault::Outdated | SurfaceFault::Suboptimal => FaultAction::Reconfigure,
        SurfaceFault::Lost => FaultAction::RecreateSurface,
        SurfaceFault::DeviceLost => FaultAction::RebuildDevice,
        SurfaceFault::Occluded | SurfaceFault::Timeout => FaultAction::SkipFrame,
        SurfaceFault::Fatal => FaultAction::Abort,
    }
}

/// The clear colour: transparent black, premultiplied. With translucent windows all surface
/// colour is premultiplied (T-07 §3); the blend state is `One, OneMinusSrcAlpha`.
pub const CLEAR_PREMULTIPLIED: [f32; 4] = [0.0, 0.0, 0.0, 0.0];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_mode_prefers_mailbox_never_immediate() {
        assert_eq!(
            choose_present_mode(&[PresentKind::Fifo, PresentKind::Mailbox]),
            PresentKind::Mailbox
        );
        assert_eq!(
            choose_present_mode(&[PresentKind::Other, PresentKind::Fifo]),
            PresentKind::Fifo
        );
        assert_eq!(
            choose_present_mode(&[PresentKind::Other]),
            PresentKind::Fifo
        );
        assert_eq!(choose_present_mode(&[]), PresentKind::Fifo);
    }

    #[test]
    fn alpha_falls_back_to_opaque_and_reports() {
        let c = choose_alpha(&[AlphaKind::Opaque, AlphaKind::PreMultiplied]);
        assert_eq!(
            (c.mode, c.opaque_fallback),
            (AlphaKind::PreMultiplied, false)
        );
        let c = choose_alpha(&[AlphaKind::Opaque, AlphaKind::Other]);
        assert_eq!((c.mode, c.opaque_fallback), (AlphaKind::Opaque, true));
    }

    fn cand(class: AdapterClass, alpha: bool, comp: bool) -> AdapterCandidate {
        AdapterCandidate {
            class,
            alpha_capable: alpha,
            compositor: comp,
        }
    }

    #[test]
    fn adapter_prefers_compositor_and_never_software_over_alpha_hardware() {
        let c = [
            cand(AdapterClass::Cpu, true, false),
            cand(AdapterClass::Integrated, true, false),
        ];
        assert_eq!(choose_adapter(&c), Some(1));
        let c = [
            cand(AdapterClass::Discrete, true, false),
            cand(AdapterClass::Integrated, true, true),
        ];
        assert_eq!(choose_adapter(&c), Some(1), "compositor's adapter wins");
        let c = [
            cand(AdapterClass::Cpu, true, true),
            cand(AdapterClass::Discrete, true, false),
        ];
        assert_eq!(
            choose_adapter(&c),
            Some(1),
            "software never wins while hardware offers alpha"
        );
    }

    #[test]
    fn adapter_software_only_when_nothing_else() {
        assert_eq!(
            choose_adapter(&[cand(AdapterClass::Cpu, true, false)]),
            Some(0)
        );
        let c = [
            cand(AdapterClass::Cpu, true, false),
            cand(AdapterClass::Discrete, false, false),
        ];
        assert_eq!(
            choose_adapter(&c),
            Some(1),
            "hardware without alpha still beats software"
        );
        assert_eq!(choose_adapter(&[]), None);
    }

    #[test]
    fn faults_map_to_actions() {
        assert_eq!(
            fault_action(SurfaceFault::Outdated),
            FaultAction::Reconfigure
        );
        assert_eq!(
            fault_action(SurfaceFault::Suboptimal),
            FaultAction::Reconfigure
        );
        assert_eq!(
            fault_action(SurfaceFault::Lost),
            FaultAction::RecreateSurface
        );
        assert_eq!(
            fault_action(SurfaceFault::DeviceLost),
            FaultAction::RebuildDevice
        );
        assert_eq!(fault_action(SurfaceFault::Occluded), FaultAction::SkipFrame);
        assert_eq!(fault_action(SurfaceFault::Timeout), FaultAction::SkipFrame);
    }
}
