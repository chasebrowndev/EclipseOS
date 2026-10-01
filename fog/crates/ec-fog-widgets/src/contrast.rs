// SPDX-License-Identifier: AGPL-3.0-only

//! WCAG contrast for text over glass (FOG §Visual design, "Contrast floor").
//!
//! Glass lets the desktop through, so the colour behind a label depends on
//! whatever is under the window. The floor is checked against the worst
//! backdrop: a tint is only as translucent as the brightest wallpaper allows.
//!
//! The model follows the pixels. Inside the window iced blends in linear
//! light: a label of alpha `ta` over a tint of alpha `a` leaves a
//! premultiplied pixel. The compositor then lays that pixel, scaled by the
//! window opacity, over the (blurred) backdrop, in a blend space it chooses:
//! both are modelled and a floor is only met when it holds in the one asked.

/// An sRGB-encoded colour, channels 0 to 1.
pub type Rgb = [f32; 3];

/// WCAG 2 AA for body text.
pub const AA: f32 = 4.5;

pub const WHITE: Rgb = [1.0, 1.0, 1.0];
pub const BLACK: Rgb = [0.0, 0.0, 0.0];

/// Backdrops a desktop can put behind a window: the extremes and the
/// saturated primaries and secondaries. White is the worst case for light
/// text, black for dark.
pub const WORST: [Rgb; 8] = [
    WHITE,
    BLACK,
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [1.0, 1.0, 0.0],
    [0.0, 1.0, 1.0],
    [1.0, 0.0, 1.0],
];

/// Where a blend happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    /// On sRGB-encoded values (most compositors).
    Srgb,
    /// On linear light (iced, with gamma correction).
    Linear,
}

pub fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn to_srgb(l: f32) -> f32 {
    if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// WCAG relative luminance.
pub fn luminance(c: Rgb) -> f32 {
    0.2126 * to_linear(c[0]) + 0.7152 * to_linear(c[1]) + 0.0722 * to_linear(c[2])
}

/// WCAG contrast ratio, 1 to 21.
pub fn ratio(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `top` at `alpha` over opaque `under`.
pub fn over(top: Rgb, alpha: f32, under: Rgb, space: Space) -> Rgb {
    let a = alpha.clamp(0.0, 1.0);
    std::array::from_fn(|i| match space {
        Space::Srgb => top[i] * a + under[i] * (1.0 - a),
        Space::Linear => to_srgb(to_linear(top[i]) * a + to_linear(under[i]) * (1.0 - a)),
    })
}

/// A translucent window surface: a tint of `alpha` inside a window the
/// compositor shows at `opacity`, blending in `space`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    pub tint: Rgb,
    pub alpha: f32,
    pub opacity: f32,
    pub space: Space,
}

impl Surface {
    /// What the eye sees where there is no text.
    pub fn ground(&self, backdrop: Rgb) -> Rgb {
        over(self.tint, self.alpha * self.opacity, backdrop, self.space)
    }

    /// What the eye sees under a label of colour `text` at alpha `ta`.
    pub fn ink(&self, text: Rgb, ta: f32, backdrop: Rgb) -> Rgb {
        // In-window, linear, premultiplied: text over tint.
        let a = ta + self.alpha * (1.0 - ta);
        if a <= 0.0 {
            return backdrop;
        }
        let colour: Rgb = std::array::from_fn(|i| {
            let p = to_linear(text[i]) * ta + to_linear(self.tint[i]) * self.alpha * (1.0 - ta);
            to_srgb(p / a)
        });
        over(colour, a * self.opacity, backdrop, self.space)
    }

    pub fn contrast(&self, text: Rgb, ta: f32, backdrop: Rgb) -> f32 {
        ratio(self.ink(text, ta, backdrop), self.ground(backdrop))
    }

    /// The lowest contrast any of `texts` gets over any of `backdrops`.
    pub fn worst(&self, texts: &[(Rgb, f32)], backdrops: &[Rgb]) -> f32 {
        texts
            .iter()
            .flat_map(|&(t, ta)| backdrops.iter().map(move |&b| self.contrast(t, ta, b)))
            .fold(f32::INFINITY, f32::min)
    }
}

/// The steps [`min_alpha`] searches: 1/256, one 8-bit alpha step.
const STEPS: u32 = 256;

/// The least tint alpha, at least `from`, at which every text in `texts`
/// meets `floor` over every backdrop in `backdrops`; `None` when even an
/// opaque tint cannot.
pub fn min_alpha(
    surface: Surface,
    from: f32,
    texts: &[(Rgb, f32)],
    backdrops: &[Rgb],
    floor: f32,
) -> Option<f32> {
    let start = (from.clamp(0.0, 1.0) * STEPS as f32).ceil() as u32;
    (start..=STEPS)
        .map(|k| k as f32 / STEPS as f32)
        .find(|&alpha| Surface { alpha, ..surface }.worst(texts, backdrops) >= floor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(v: u32) -> Rgb {
        [
            ((v >> 16) & 0xff) as f32 / 255.0,
            ((v >> 8) & 0xff) as f32 / 255.0,
            (v & 0xff) as f32 / 255.0,
        ]
    }

    #[test]
    fn wcag_reference_ratios() {
        assert!((ratio(WHITE, BLACK) - 21.0).abs() < 1e-3);
        assert!((ratio(WHITE, WHITE) - 1.0).abs() < 1e-6);
        // #767676 is the classic lightest grey passing AA on white; #777 fails.
        assert!(ratio(hex(0x767676), WHITE) >= AA);
        assert!(ratio(hex(0x777777), WHITE) < AA);
        assert!((luminance(hex(0x808080)) - 0.2159).abs() < 1e-3);
    }

    #[test]
    fn blend_spaces_differ_the_right_way() {
        let s = over(WHITE, 0.5, BLACK, Space::Srgb);
        let l = over(WHITE, 0.5, BLACK, Space::Linear);
        assert!((s[0] - 0.5).abs() < 1e-6);
        // Half the light is brighter than half the code value.
        assert!((l[0] - 0.7354).abs() < 1e-3, "{l:?}");
        for c in [0.0, 0.2, 0.5, 1.0] {
            assert!((to_srgb(to_linear(c)) - c).abs() < 1e-5);
        }
    }

    #[test]
    fn an_opaque_surface_ignores_the_backdrop() {
        let s = Surface {
            tint: hex(0x0b0906),
            alpha: 1.0,
            opacity: 1.0,
            space: Space::Srgb,
        };
        let c = s.contrast(WHITE, 1.0, WHITE);
        assert!((c - s.contrast(WHITE, 1.0, BLACK)).abs() < 1e-4);
        assert!(c > 18.0, "{c}");
    }

    #[test]
    fn min_alpha_is_the_tightest_passing_step() {
        let base = hex(0x0b0906);
        let texts = [(WHITE, 1.0), (WHITE, 0.64)];
        for space in [Space::Srgb, Space::Linear] {
            let s = Surface {
                tint: base,
                alpha: 0.0,
                opacity: 1.0,
                space,
            };
            let a = min_alpha(s, 0.0, &texts, &WORST, AA).unwrap();
            assert!(Surface { alpha: a, ..s }.worst(&texts, &WORST) >= AA);
            let below = a - 1.0 / STEPS as f32;
            assert!(Surface { alpha: below, ..s }.worst(&texts, &WORST) < AA);
            // White behind the glass is what sets it.
            assert!(Surface { alpha: below, ..s }.worst(&texts, &[WHITE]) < AA);
            assert!(a > 0.3 && a < 0.95, "{space:?}: {a}");
        }
    }

    #[test]
    fn linear_compositing_needs_more_tint_than_srgb() {
        let s = Surface {
            tint: hex(0x0b0906),
            alpha: 0.0,
            opacity: 1.0,
            space: Space::Linear,
        };
        let t = [(WHITE, 1.0)];
        let lin = min_alpha(s, 0.0, &t, &[WHITE], AA).unwrap();
        let srgb = min_alpha(
            Surface {
                space: Space::Srgb,
                ..s
            },
            0.0,
            &t,
            &[WHITE],
            AA,
        )
        .unwrap();
        // In linear light a dark tint must cover far more of a white wall.
        assert!(lin > srgb, "linear {lin} srgb {srgb}");
    }

    #[test]
    fn window_opacity_raises_the_floor_and_can_make_it_unreachable() {
        let base = hex(0x0b0906);
        let s = Surface {
            tint: base,
            alpha: 0.0,
            opacity: 1.0,
            space: Space::Srgb,
        };
        let t = [(WHITE, 1.0)];
        let full = min_alpha(s, 0.0, &t, &[WHITE], AA).unwrap();
        let dim = min_alpha(Surface { opacity: 0.87, ..s }, 0.0, &t, &[WHITE], AA).unwrap();
        assert!(dim > full, "{dim} <= {full}");
        // A requested floor below the minimum is raised to it; above, kept.
        assert_eq!(min_alpha(s, 0.99, &t, &[WHITE], AA), Some(0.9921875));
        // Grey text on a mostly transparent window can never pass.
        let ghost = Surface { opacity: 0.2, ..s };
        assert_eq!(
            min_alpha(ghost, 0.0, &[(hex(0x808080), 1.0)], &WORST, AA),
            None
        );
    }
}
