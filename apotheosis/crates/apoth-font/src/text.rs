// SPDX-License-Identifier: AGPL-3.0-only
//! Small text-classification helpers the font layer needs without a Unicode dependency:
//! presentation (emoji/text), script, and a conservative cluster segmenter used by the
//! diagnostics (`apoth fonts resolve`). The grid supplies its own clusters in the terminal
//! path; these only have to agree on the sequences T-06 §4 names (ZWJ, keycap, VS, flags,
//! skin tones, tag sequences).

/// Requested presentation of a cluster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    /// Colour glyph wanted (VS16, keycap, ZWJ sequence, flag, default-emoji character).
    Emoji,
    /// Monochrome glyph wanted (VS15).
    Text,
    /// Nothing asked for either way.
    Neutral,
}

/// True for code points that never need their own glyph when deciding coverage.
pub fn is_default_ignorable(cp: u32) -> bool {
    matches!(cp,
        0x00AD | 0x034F | 0x061C | 0x180B..=0x180F | 0x200B..=0x200F | 0x202A..=0x202E
        | 0x2060..=0x206F | 0xFE00..=0xFE0F | 0xFEFF | 0xE0100..=0xE01EF)
}

/// Emoji_Presentation = Yes (the default-emoji code points), from the emoji data tables.
const EMOJI_DEFAULT: &[(u32, u32)] = &[
    (0x231A, 0x231B),
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x2693, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x1F004, 0x1F004),
    (0x1F0CF, 0x1F0CF),
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F1E6, 0x1F1FF),
    (0x1F201, 0x1F201),
    (0x1F21A, 0x1F21A),
    (0x1F22F, 0x1F22F),
    (0x1F232, 0x1F236),
    (0x1F238, 0x1F23A),
    (0x1F250, 0x1F251),
    (0x1F300, 0x1F320),
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D7),
    (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA70, 0x1FA7C),
    (0x1FA80, 0x1FA89),
    (0x1FA8F, 0x1FAC6),
    (0x1FACE, 0x1FADC),
    (0x1FADF, 0x1FAE9),
    (0x1FAF0, 0x1FAF8),
];

/// True if `cp` is presented as emoji by default.
pub fn emoji_default_presentation(cp: u32) -> bool {
    let i = EMOJI_DEFAULT.partition_point(|&(_, b)| b < cp);
    EMOJI_DEFAULT.get(i).is_some_and(|&(a, _)| a <= cp)
}

/// Decides the presentation of a cluster from its code points.
pub fn presentation(cps: &[u32]) -> Presentation {
    let Some(&base) = cps.first() else {
        return Presentation::Neutral;
    };
    if cps.contains(&0xFE0F) || cps.contains(&0x20E3) {
        return Presentation::Emoji;
    }
    if cps.contains(&0xFE0E) {
        return Presentation::Text;
    }
    if cps.contains(&0x200D) && cps.iter().any(|&c| emoji_default_presentation(c)) {
        return Presentation::Emoji;
    }
    if emoji_default_presentation(base) {
        return Presentation::Emoji;
    }
    Presentation::Neutral
}

/// ISO 15924 tag of the script of `cp`. `Zyyy` is common (extends any run), `Zinh` inherited.
pub fn script_of(cp: u32) -> [u8; 4] {
    const T: &[(u32, u32, &[u8; 4])] = &[
        (0x0300, 0x036F, b"Zinh"),
        (0x0370, 0x03FF, b"Grek"),
        (0x0400, 0x052F, b"Cyrl"),
        (0x0530, 0x058F, b"Armn"),
        (0x0590, 0x05FF, b"Hebr"),
        (0x0600, 0x06FF, b"Arab"),
        (0x0750, 0x077F, b"Arab"),
        (0x08A0, 0x08FF, b"Arab"),
        (0x0900, 0x097F, b"Deva"),
        (0x0980, 0x09FF, b"Beng"),
        (0x0A00, 0x0A7F, b"Guru"),
        (0x0A80, 0x0AFF, b"Gujr"),
        (0x0B00, 0x0B7F, b"Orya"),
        (0x0B80, 0x0BFF, b"Taml"),
        (0x0C00, 0x0C7F, b"Telu"),
        (0x0C80, 0x0CFF, b"Knda"),
        (0x0D00, 0x0D7F, b"Mlym"),
        (0x0D80, 0x0DFF, b"Sinh"),
        (0x0E00, 0x0E7F, b"Thai"),
        (0x0E80, 0x0EFF, b"Laoo"),
        (0x0F00, 0x0FFF, b"Tibt"),
        (0x1000, 0x109F, b"Mymr"),
        (0x10A0, 0x10FF, b"Geor"),
        (0x1100, 0x11FF, b"Hang"),
        (0x1200, 0x139F, b"Ethi"),
        (0x1780, 0x17FF, b"Khmr"),
        (0x1800, 0x18AF, b"Mong"),
        (0x1C80, 0x1C8F, b"Cyrl"),
        (0x1E00, 0x1EFF, b"Latn"),
        (0x1F00, 0x1FFF, b"Grek"),
        (0x2800, 0x28FF, b"Brai"),
        (0x2C60, 0x2C7F, b"Latn"),
        (0x2DE0, 0x2DFF, b"Cyrl"),
        (0x2E80, 0x2FDF, b"Hani"),
        (0x3040, 0x309F, b"Hira"),
        (0x30A0, 0x30FF, b"Kana"),
        (0x3100, 0x312F, b"Bopo"),
        (0x3130, 0x318F, b"Hang"),
        (0x3400, 0x4DBF, b"Hani"),
        (0x4E00, 0x9FFF, b"Hani"),
        (0xA640, 0xA69F, b"Cyrl"),
        (0xA720, 0xA7FF, b"Latn"),
        (0xAC00, 0xD7AF, b"Hang"),
        (0xF900, 0xFAFF, b"Hani"),
        (0xFB50, 0xFDFF, b"Arab"),
        (0xFE70, 0xFEFF, b"Arab"),
        (0xFF21, 0xFF3A, b"Latn"),
        (0xFF41, 0xFF5A, b"Latn"),
        (0x20000, 0x3FFFF, b"Hani"),
    ];
    if (0x41..=0x5A).contains(&cp)
        || (0x61..=0x7A).contains(&cp)
        || (0xC0..=0x24F).contains(&cp) && cp != 0xD7 && cp != 0xF7
    {
        return *b"Latn";
    }
    let i = T.partition_point(|&(_, b, _)| b < cp);
    match T.get(i) {
        Some(&(a, _, t)) if a <= cp => *t,
        _ => *b"Zyyy",
    }
}

/// True for scripts that extend whatever run they appear in.
pub fn script_is_weak(tag: [u8; 4]) -> bool {
    &tag == b"Zyyy" || &tag == b"Zinh"
}

/// A representative block of each script, used to rank system fallback candidates by how much
/// of the script they cover.
pub fn script_probe(tag: [u8; 4]) -> (u32, u32) {
    match &tag {
        b"Latn" => (0x41, 0x24F),
        b"Grek" => (0x370, 0x3FF),
        b"Cyrl" => (0x400, 0x4FF),
        b"Hebr" => (0x5D0, 0x5EA),
        b"Arab" => (0x600, 0x6FF),
        b"Deva" => (0x900, 0x97F),
        b"Thai" => (0xE01, 0xE5B),
        b"Hang" => (0xAC00, 0xD7A3),
        b"Hani" => (0x4E00, 0x9FFF),
        b"Hira" => (0x3041, 0x309F),
        b"Kana" => (0x30A0, 0x30FF),
        b"Brai" => (0x2800, 0x28FF),
        _ => (0, 0),
    }
}

/// Approximation of Extended_Pictographic: enough for ZWJ sequences.
fn is_pictographic(cp: u32) -> bool {
    emoji_default_presentation(cp)
        || matches!(cp, 0xA9 | 0xAE | 0x203C | 0x2049 | 0x2122 | 0x2139
            | 0x2190..=0x21FF | 0x2300..=0x23FF | 0x2460..=0x24FF | 0x25A0..=0x27BF
            | 0x2900..=0x297F | 0x2B00..=0x2BFF | 0x3030 | 0x303D | 0x3297 | 0x3299
            | 0x1F000..=0x1FAFF)
}

fn is_extender(cp: u32) -> bool {
    matches!(cp,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF | 0x3099..=0x309A | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F
        | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0xE0100..=0xE01EF | 0x200C)
}

/// Splits `s` into clusters: a base plus combining marks / variation selectors / skin tones /
/// tags / keycap, ZWJ joins, and regional-indicator pairs. Not full UAX #29; the grid owns
/// real segmentation.
pub fn segment_clusters(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut chars = s.char_indices().peekable();
    let mut ri_count = 0u32;
    let mut after_zwj = false;
    while let Some((i, c)) = chars.next() {
        let cp = c as u32;
        let joins = i != start
            && (is_extender(cp) || cp == 0x200D || (after_zwj && is_pictographic(cp)) || {
                (0x1F1E6..=0x1F1FF).contains(&cp) && ri_count % 2 == 1
            });
        if i != start && !joins {
            out.push(&s[start..i]);
            start = i;
            ri_count = 0;
        }
        if (0x1F1E6..=0x1F1FF).contains(&cp) {
            ri_count += 1;
        } else if !is_extender(cp) && cp != 0x200D {
            ri_count = 0;
        }
        after_zwj = cp == 0x200D;
        if chars.peek().is_none() {
            out.push(&s[start..]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_rules() {
        assert_eq!(presentation(&[0x41]), Presentation::Neutral);
        assert_eq!(presentation(&[0x1F600]), Presentation::Emoji);
        assert_eq!(presentation(&[0x2764, 0xFE0F]), Presentation::Emoji);
        assert_eq!(presentation(&[0x2764, 0xFE0E]), Presentation::Text);
        assert_eq!(presentation(&[0x31, 0xFE0F, 0x20E3]), Presentation::Emoji);
        assert_eq!(
            presentation(&[0x1F468, 0x200D, 0x1F469]),
            Presentation::Emoji
        );
        assert_eq!(presentation(&[0xA9]), Presentation::Neutral);
    }

    #[test]
    fn emoji_table_is_sorted() {
        assert!(EMOJI_DEFAULT.windows(2).all(|w| w[0].1 < w[1].0));
    }

    #[test]
    fn scripts() {
        assert_eq!(&script_of('a' as u32), b"Latn");
        assert_eq!(&script_of('я' as u32), b"Cyrl");
        assert_eq!(&script_of('漢' as u32), b"Hani");
        assert_eq!(&script_of(' ' as u32), b"Zyyy");
        assert_eq!(&script_of(0x301), b"Zinh");
        assert!(script_is_weak(script_of('1' as u32)));
    }

    #[test]
    fn clusters() {
        assert_eq!(segment_clusters("ab"), vec!["a", "b"]);
        assert_eq!(segment_clusters("e\u{301}x"), vec!["e\u{301}", "x"]);
        assert_eq!(
            segment_clusters("1\u{FE0F}\u{20E3}!"),
            vec!["1\u{FE0F}\u{20E3}", "!"]
        );
        let fam = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(segment_clusters(fam), vec![fam]);
        let flags = "\u{1F1FA}\u{1F1F8}\u{1F1EB}\u{1F1F7}";
        assert_eq!(
            segment_clusters(flags),
            vec!["\u{1F1FA}\u{1F1F8}", "\u{1F1EB}\u{1F1F7}"]
        );
        assert_eq!(
            segment_clusters("\u{1F44D}\u{1F3FD}"),
            vec!["\u{1F44D}\u{1F3FD}"]
        );
        assert!(segment_clusters("").is_empty());
        assert_eq!(segment_clusters("a\u{200D}b"), vec!["a\u{200D}", "b"]);
    }
}
