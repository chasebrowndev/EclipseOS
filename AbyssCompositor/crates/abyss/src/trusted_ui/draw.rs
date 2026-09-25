// SPDX-License-Identifier: AGPL-3.0-only
//! Drawing the destructive-action prompt (COMP-10 §3.10). **TCB.**
//!
//! Built from the same pieces as every other compositor-drawn card: the
//! bundled bitmap font and `text::rasterize`, so nothing here depends on a
//! client's fonts, theme or pixels. Prepended by the backend as its own pass,
//! which keeps it out of `capture.rs` by construction (ADR 0040).

use smithay::{
    backend::renderer::{
        element::{
            solid::{SolidColorBuffer, SolidColorRenderElement},
            texture::{TextureBuffer, TextureRenderElement},
            Kind,
        },
        gles::{GlesRenderer, GlesTexture},
    },
    output::Output,
    utils::{Logical, Point, Scale, Size},
};

use super::{Focus, Request, TrustedUi};
use crate::render::{
    annotation::upload,
    text::{self, Card},
    AbyssRenderElement,
};

/// Characters per line inside the card.
const COLS: usize = 60;
const DIM: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const DIM_ALPHA: f32 = 0.7;

/// The uploaded card, per buffer scale, valid for one prompt and one focus.
#[derive(Default)]
pub struct ArtCache {
    art: Vec<(usize, Card, TextureBuffer<GlesTexture>)>,
}

/// `123456789` as `"123.5 MB"`, decimal units, as disk vendors print them.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// What the prompt says. Pure, so the tests read the words a human would.
///
/// The requester's text is only ever the model (already sanitised at parse),
/// the by-id name (charset-checked) and a number; everything else is ours.
pub fn card(req: &Request, focus: Focus) -> Card {
    let mut body = vec![
        "The system is asking to ERASE this disk. Everything on it will be lost. This cannot be undone."
            .to_string(),
        String::new(),
        format!("Disk  {}", req.model),
        format!("Size  {}", human_size(req.size_bytes)),
        format!("ID    {}", req.disk),
        String::new(),
        // DA-03: on the live medium no personal phrase can exist yet, so the
        // prompt says so rather than pretending to be authenticated by one.
        "No personal phrase is set (anti-spoofing unconfigured). This prompt is drawn by the \
         compositor and appears only when the system asks to erase a disk."
            .to_string(),
        String::new(),
    ];
    let button = |label: &str, on: bool| {
        if on {
            format!("[ {label} ]")
        } else {
            format!("  {label}  ")
        }
    };
    body.push(button("Deny (default)", focus == Focus::Deny));
    body.push(button("Allow: erase this disk", focus == Focus::Allow));
    body.push(String::new());
    body.push("Esc denies. Tab switches. Enter selects.".to_string());
    // Built directly rather than through `Card::new`, whose line cap suits
    // annotations; every string above is ASCII by construction.
    let body = body.iter().flat_map(|p| wrapped(p)).collect();
    Card {
        chip: "TRUSTED".to_string(),
        title: vec!["Erase this disk".to_string()],
        body,
    }
}

fn wrapped(p: &str) -> Vec<String> {
    if p.is_empty() {
        vec![String::new()]
    } else {
        text::wrap(p, COLS)
    }
}

/// The prompt's elements for one output, front-to-back: the card, then the
/// dim behind it. Empty when no prompt is up.
pub fn elements(renderer: &mut GlesRenderer, ui: &mut TrustedUi, output: &Output) -> Vec<AbyssRenderElement> {
    let Some(p) = ui.prompt.as_mut() else {
        return Vec::new();
    };
    let Some(mode) = output.current_mode() else {
        return Vec::new();
    };
    let scale = Scale::from(output.current_scale().fractional_scale());
    let logical: Size<i32, Logical> = mode.size.to_f64().to_logical(scale).to_i32_round();
    let dev = (scale.x.round() as usize).max(1);

    let card = card(&p.request, p.focus);
    p.art.art.retain(|(d, c, _)| *d == dev && *c == card);
    if p.art.art.is_empty() {
        let Some(buf) = upload(renderer, &text::rasterize(&card, dev), dev) else {
            // Cannot draw the prompt: show only the dim. The seat stays
            // grabbed and the timeout still denies, which fails closed.
            return vec![dim(logical, scale)];
        };
        p.art.art.push((dev, card.clone(), buf));
    }
    let (w, h) = card.size();
    let loc: Point<i32, Logical> = (
        ((logical.w - w as i32) / 2).max(0),
        ((logical.h - h as i32) / 2).max(0),
    )
        .into();
    let mut out = Vec::with_capacity(2);
    if let Some((_, _, buf)) = p.art.art.first() {
        p.drawn = true;
        out.push(AbyssRenderElement::Texture(
            TextureRenderElement::from_texture_buffer(
                loc.to_f64().to_physical(scale),
                buf,
                None,
                None,
                None,
                Kind::Unspecified,
            ),
        ));
    }
    out.push(dim(logical, scale));
    out
}

fn dim(size: Size<i32, Logical>, scale: Scale<f64>) -> AbyssRenderElement {
    let buffer = SolidColorBuffer::new(size, DIM);
    AbyssRenderElement::Solid(SolidColorRenderElement::from_buffer(
        &buffer,
        (0, 0),
        scale,
        DIM_ALPHA,
        Kind::Unspecified,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> Request {
        Request {
            disk: "nvme-Samsung_980".into(),
            model: "Samsung SSD 980".into(),
            size_bytes: 500_107_862_016,
        }
    }

    fn text_of(c: &Card) -> String {
        c.body.join("\n")
    }

    #[test]
    fn the_card_names_the_disk_and_says_it_is_irreversible() {
        let t = text_of(&card(&req(), Focus::Deny));
        assert!(t.contains("Samsung SSD 980"));
        assert!(t.contains("500.1 GB"));
        assert!(t.contains("nvme-Samsung_980"));
        assert!(t.contains("cannot be undone"));
        assert!(t.contains("anti-spoofing unconfigured"));
    }

    #[test]
    fn focus_is_visible_and_starts_on_deny() {
        let d = text_of(&card(&req(), Focus::Deny));
        assert!(d.contains("[ Deny (default) ]") && !d.contains("[ Allow"));
        let a = text_of(&card(&req(), Focus::Allow));
        assert!(a.contains("[ Allow: erase this disk ]") && !a.contains("[ Deny"));
    }

    #[test]
    fn nothing_on_the_card_is_outside_the_font() {
        let c = card(&req(), Focus::Allow);
        for l in c.title.iter().chain(&c.body) {
            assert!(l.bytes().all(|b| (0x20..0x7f).contains(&b)), "{l:?}");
        }
    }

    #[test]
    fn size_units() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_000), "1.0 kB");
        assert_eq!(human_size(2_000_000_000_000), "2.0 TB");
    }
}
