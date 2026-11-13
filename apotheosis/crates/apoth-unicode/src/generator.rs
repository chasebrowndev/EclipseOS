// SPDX-License-Identifier: Apache-2.0
//! Table generator (T-04 §1). Reads the UCD files committed under `crates/apoth-unicode/data/ucd/`
//! and renders `src/tables.rs`. Dev-time only: nothing in the runtime path calls it, there
//! is no build script and no download. `src/bin/gen_tables.rs` writes or checks the file;
//! `tests/tables_current.rs` fails when the committed source is stale.
//!
//! Input files (flat in the UCD directory): `UnicodeData.txt`, `EastAsianWidth.txt`,
//! `emoji-data.txt`, `GraphemeBreakProperty.txt`, `DerivedCoreProperties.txt`; tests also use
//! `GraphemeBreakTest.txt` and `emoji-test.txt`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::props::{
    CLASS_SHIFT, EAST_ASIAN_AMBIGUOUS, EMOJI, EMOJI_PRESENTATION, EXTENDED_PICTOGRAPHIC,
};
use crate::segment::{Class, NUM_CLASSES, NUM_STATES, reference_step};

const CODE_POINTS: usize = 0x11_0000;

/// Read one UCD file from `dir`.
pub fn read(dir: &Path, name: &str) -> Result<String, String> {
    std::fs::read_to_string(dir.join(name))
        .map_err(|e| format!("{}: {e}", dir.join(name).display()))
}

/// Parse `XXXX[..YYYY] ; field ; field # comment` lines into `(lo, hi, fields)`.
pub fn parse_ranges(text: &str) -> Vec<(u32, u32, Vec<String>)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(';').map(str::trim);
        let Some(cps) = fields.next() else { continue };
        let Some((lo, hi)) = parse_cp_range(cps) else {
            continue;
        };
        out.push((lo, hi, fields.map(str::to_owned).collect()));
    }
    out
}

/// Parse `XXXX` or `XXXX..YYYY`.
pub fn parse_cp_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once("..") {
        Some((a, b)) => Some((
            u32::from_str_radix(a.trim(), 16).ok()?,
            u32::from_str_radix(b.trim(), 16).ok()?,
        )),
        None => {
            let v = u32::from_str_radix(s.trim(), 16).ok()?;
            Some((v, v))
        }
    }
}

/// The Unicode version named in the first line of `EastAsianWidth.txt`.
pub fn unicode_version(eaw: &str) -> Result<String, String> {
    let first = eaw.lines().next().unwrap_or("");
    let rest = first
        .split("EastAsianWidth-")
        .nth(1)
        .ok_or("no version in EastAsianWidth.txt")?;
    Ok(rest.trim_end_matches(".txt").trim().to_owned())
}

fn mark(set: &mut [bool], lo: u32, hi: u32) {
    for cp in lo..=hi.min(0x10_FFFF) {
        set[cp as usize] = true;
    }
}

fn gcb_class(name: &str) -> Option<Class> {
    Some(match name {
        "CR" => Class::Cr,
        "LF" => Class::Lf,
        "Control" => Class::Control,
        "Extend" => Class::Extend,
        "ZWJ" => Class::Zwj,
        "Regional_Indicator" => Class::RegionalIndicator,
        "Prepend" => Class::Prepend,
        "SpacingMark" => Class::SpacingMark,
        "L" => Class::L,
        "V" => Class::V,
        "T" => Class::T,
        "LV" => Class::Lv,
        "LVT" => Class::Lvt,
        _ => return None,
    })
}

/// Compute the property word of every code point, and the Unicode version.
#[allow(clippy::needless_range_loop)]
pub fn compute_props(dir: &Path) -> Result<(String, Vec<u16>), String> {
    let eaw_text = read(dir, "EastAsianWidth.txt")?;
    let version = unicode_version(&eaw_text)?;

    let mut ambiguous = vec![false; CODE_POINTS];
    let mut wide = vec![false; CODE_POINTS];
    for (lo, hi, f) in parse_ranges(&eaw_text) {
        match f.first().map(String::as_str) {
            Some("A") => mark(&mut ambiguous, lo, hi),
            Some("W" | "F") => mark(&mut wide, lo, hi),
            _ => {}
        }
    }

    // Zero-width by general category: Mn, Me, Cf, Zl, Zp, Cc.
    let mut zero = vec![false; CODE_POINTS];
    let mut range_start: Option<u32> = None;
    for line in read(dir, "UnicodeData.txt")?.lines() {
        let f: Vec<&str> = line.split(';').collect();
        if f.len() < 3 {
            continue;
        }
        let cp = u32::from_str_radix(f[0], 16).map_err(|e| format!("UnicodeData {}: {e}", f[0]))?;
        let (lo, hi) = if f[1].ends_with(", First>") {
            range_start = Some(cp);
            continue;
        } else if f[1].ends_with(", Last>") {
            (range_start.take().unwrap_or(cp), cp)
        } else {
            (cp, cp)
        };
        if matches!(f[2], "Mn" | "Me" | "Cf" | "Zl" | "Zp" | "Cc") {
            mark(&mut zero, lo, hi);
        }
    }

    let mut emoji = vec![false; CODE_POINTS];
    let mut emoji_pres = vec![false; CODE_POINTS];
    let mut pict = vec![false; CODE_POINTS];
    for (lo, hi, f) in parse_ranges(&read(dir, "emoji-data.txt")?) {
        match f.first().map(String::as_str) {
            Some("Emoji") => mark(&mut emoji, lo, hi),
            Some("Emoji_Presentation") => mark(&mut emoji_pres, lo, hi),
            Some("Extended_Pictographic") => mark(&mut pict, lo, hi),
            _ => {}
        }
    }

    let mut gcb: Vec<Class> = vec![Class::Other; CODE_POINTS];
    for (lo, hi, f) in parse_ranges(&read(dir, "GraphemeBreakProperty.txt")?) {
        if let Some(c) = f.first().and_then(|n| gcb_class(n)) {
            for cp in lo..=hi.min(0x10_FFFF) {
                gcb[cp as usize] = c;
            }
        }
    }

    // 0 none, 1 Consonant, 2 Linker, 3 Extend.
    let mut incb = vec![0u8; CODE_POINTS];
    let mut ignorable = vec![false; CODE_POINTS];
    for (lo, hi, f) in parse_ranges(&read(dir, "DerivedCoreProperties.txt")?) {
        match (f.first().map(String::as_str), f.get(1).map(String::as_str)) {
            (Some("Default_Ignorable_Code_Point"), _) => mark(&mut ignorable, lo, hi),
            (Some("InCB"), Some(v)) => {
                let code = match v {
                    "Consonant" => 1,
                    "Linker" => 2,
                    "Extend" => 3,
                    _ => continue,
                };
                for cp in lo..=hi.min(0x10_FFFF) {
                    incb[cp as usize] = code;
                }
            }
            _ => {}
        }
    }

    let mut out = vec![0u16; CODE_POINTS];
    for cp in 0..CODE_POINTS {
        let mut class = gcb[cp];
        match class {
            Class::Other => {
                if pict[cp] {
                    class = Class::ExtPict;
                } else if incb[cp] == 1 {
                    class = Class::InCbConsonant;
                } else if incb[cp] == 2 {
                    class = Class::InCbLinkerBase;
                }
            }
            Class::Extend => match incb[cp] {
                2 => class = Class::InCbLinker,
                3 => class = Class::InCbExtend,
                _ => {}
            },
            _ => {}
        }
        let width: u16 = if zero[cp] || ignorable[cp] || matches!(gcb[cp], Class::V | Class::T) {
            0
        } else if wide[cp] || emoji_pres[cp] {
            2
        } else {
            1
        };
        let mut w = width | ((class as u16) << CLASS_SHIFT);
        if emoji_pres[cp] {
            w |= EMOJI_PRESENTATION;
        }
        if pict[cp] {
            w |= EXTENDED_PICTOGRAPHIC;
        }
        if ambiguous[cp] {
            w |= EAST_ASIAN_AMBIGUOUS;
        }
        if emoji[cp] {
            w |= EMOJI;
        }
        out[cp] = w;
    }
    Ok((version, out))
}

fn dedupe<'a>(items: impl Iterator<Item = &'a [u16]>) -> (Vec<u16>, Vec<u16>) {
    let mut seen: HashMap<&[u16], u16> = HashMap::new();
    let mut ids = Vec::new();
    let mut flat = Vec::new();
    for item in items {
        let next = seen.len() as u16;
        let id = *seen.entry(item).or_insert_with(|| {
            flat.extend_from_slice(item);
            next
        });
        ids.push(id);
    }
    (ids, flat)
}

fn emit(out: &mut String, name: &str, ty: &str, vals: &[u64]) {
    let _ = writeln!(out, "pub(crate) static {name}: [{ty}; {}] = [", vals.len());
    for chunk in vals.chunks(16) {
        let line: Vec<String> = chunk.iter().map(u64::to_string).collect();
        let _ = writeln!(out, "    {},", line.join(", "));
    }
    out.push_str("];\n");
}

/// Render the full text of `src/tables.rs`.
pub fn generate(dir: &Path) -> Result<String, String> {
    let (version, props) = compute_props(dir)?;
    // Stage 3: 256-scalar pages, identical pages shared (including the all-default page).
    let (page_ids, s3) = dedupe(props.chunks(256));
    // Stage 2: 16 page ids per chunk (one 4096-scalar block), identical chunks shared.
    let (chunk_ids, s2) = dedupe(page_ids.chunks(16));
    let to64 = |v: &[u16]| v.iter().map(|&x| u64::from(x)).collect::<Vec<u64>>();

    let mut out = String::new();
    out.push_str("// SPDX-License-Identifier: Apache-2.0\n");
    out.push_str("// @generated by `cargo run -p apoth-unicode --bin gen-tables` from data/ucd. Do not edit.\n");
    let _ = writeln!(
        out,
        "pub(crate) const UNICODE_VERSION: &str = \"{version}\";"
    );
    emit(&mut out, "S1", "u16", &to64(&chunk_ids));
    emit(&mut out, "S2", "u16", &to64(&s2));
    emit(&mut out, "S3", "u16", &to64(&s3));

    let mut trans = Vec::with_capacity(NUM_STATES * NUM_CLASSES);
    for s in 0..NUM_STATES as u8 {
        for c in Class::ALL {
            let (brk, next) = reference_step(s, c);
            trans.push(u64::from(next) | if brk { 0x80 } else { 0 });
        }
    }
    emit(&mut out, "TRANS", "u8", &trans);
    Ok(out)
}
