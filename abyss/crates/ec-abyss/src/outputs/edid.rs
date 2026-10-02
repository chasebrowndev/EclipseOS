// SPDX-License-Identifier: AGPL-3.0-only
//! Minimal EDID 1.x base-block parsing (COMP-03 §2).
//!
//! Smithay 0.7.0 ships no EDID helper (`smithay-drm-extras` is not a
//! dependency), so we read the connector's `EDID` property blob and pull
//! make/model/serial out of the 128-byte base block ourselves. Only the fields
//! that make up an output *identity* are parsed; nothing here is used for
//! timing or colorimetry.

/// Manufacturer, model and serial as advertised by the monitor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EdidInfo {
    pub make: String,
    pub model: String,
    pub serial: String,
}

/// Parse the base block. Returns `None` if the header or checksum is wrong.
pub fn parse(blob: &[u8]) -> Option<EdidInfo> {
    if blob.len() < 128 {
        return None;
    }
    let base = &blob[..128];
    if base[..8] != [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00] {
        return None;
    }
    if base.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0 {
        return None;
    }

    let make = manufacturer(base[8], base[9]);
    let product = u16::from_le_bytes([base[10], base[11]]);
    let serial_num = u32::from_le_bytes([base[12], base[13], base[14], base[15]]);

    // Descriptors: four 18-byte blocks. A display descriptor starts with
    // 00 00 00 <tag>; 0xFC is the monitor name, 0xFF the serial string.
    let mut name = None;
    let mut serial_str = None;
    for i in 0..4 {
        let d = &base[54 + i * 18..54 + (i + 1) * 18];
        if d[0] != 0 || d[1] != 0 || d[2] != 0 {
            continue; // a detailed timing descriptor
        }
        match d[3] {
            0xFC => name = descriptor_text(&d[5..18]),
            0xFF => serial_str = descriptor_text(&d[5..18]),
            _ => {}
        }
    }

    Some(EdidInfo {
        make,
        model: name.unwrap_or_else(|| format!("{product:04X}")),
        // Without a serial-string descriptor the numeric serial is often a
        // constant the vendor never bothered to vary (0x01010101 is common), so
        // the product code goes in too — two different models of the same make
        // must not share an identity.
        serial: serial_str.unwrap_or_else(|| format!("{product:04X}-{serial_num:08X}")),
    })
}

/// Three 5-bit letters packed big-endian, `A` == 1.
fn manufacturer(hi: u8, lo: u8) -> String {
    let v = u16::from_be_bytes([hi, lo]);
    let letter = |shift: u16| -> char {
        let c = ((v >> shift) & 0x1F) as u8;
        if (1..=26).contains(&c) {
            (b'A' + c - 1) as char
        } else {
            '?'
        }
    };
    [letter(10), letter(5), letter(0)].iter().collect()
}

/// Descriptor strings are 0x0A-terminated and space-padded, latin-1.
fn descriptor_text(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|b| *b == 0x0A).unwrap_or(bytes.len());
    let s: String = bytes[..end]
        .iter()
        .map(|b| {
            if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '?'
            }
        })
        .collect();
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        // "DEL" = D(4) E(5) L(12) -> 0b00100_00101_01100
        let id: u16 = (4 << 10) | (5 << 5) | 12;
        e[8..10].copy_from_slice(&id.to_be_bytes());
        e[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        e[12..16].copy_from_slice(&0xDEADBEEFu32.to_le_bytes());
        // monitor name descriptor
        let d = 54;
        e[d + 3] = 0xFC;
        e[d + 5..d + 5 + 6].copy_from_slice(b"U2720\n");
        // serial descriptor
        let d = 72;
        e[d + 3] = 0xFF;
        e[d + 5..d + 5 + 5].copy_from_slice(b"ABC1\n");
        let sum = e[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        e[127] = (0u8).wrapping_sub(sum);
        e
    }

    #[test]
    fn parses_make_model_serial() {
        let info = parse(&fixture()).expect("valid edid");
        assert_eq!(info.make, "DEL");
        assert_eq!(info.model, "U2720");
        assert_eq!(info.serial, "ABC1");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(&[0u8; 128]).is_none());
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn falls_back_to_numeric_fields() {
        let mut e = fixture();
        e[54 + 3] = 0x10; // not a name descriptor
        e[72 + 3] = 0x10; // not a serial descriptor
        let sum = e[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        e[127] = (0u8).wrapping_sub(sum);
        let info = parse(&e).expect("valid edid");
        assert_eq!(info.model, "1234");
        assert_eq!(info.serial, "1234-DEADBEEF");
    }
}
