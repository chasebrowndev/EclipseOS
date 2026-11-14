// SPDX-License-Identifier: AGPL-3.0-only
use apoth_image::Limits;
use apoth_image::sixel::*;

const BG: [u8; 4] = [10, 20, 30, 255];

fn run_with(p2: u16, data: &[u8], lim: &Limits) -> Result<SixelOutput, SixelError> {
    // aspect 1:1 unless the data says otherwise
    let mut d = SixelDecoder::new(SixelParams::from_dcs(&[9, p2], BG, None), lim);
    d.feed(data)?;
    d.finish()
}

fn run(data: &[u8]) -> SixelOutput {
    run_with(1, data, &Limits::default()).unwrap()
}

fn px(o: &SixelOutput, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * o.image.width + x) * 4) as usize;
    o.image.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn rgb_register_and_bits() {
    // red register, one column with bits 0 and 2 set ('E' = 0x45-0x3f = 6 = 0b110 -> bits1,2)
    let o = run(b"#1;2;100;0;0#1E");
    assert_eq!((o.image.width, o.image.height), (1, 3));
    assert_eq!(px(&o, 0, 0), [0, 0, 0, 0]); // bit 0 clear, transparent (P2=1)
    assert_eq!(px(&o, 0, 1), [255, 0, 0, 255]);
    assert_eq!(px(&o, 0, 2), [255, 0, 0, 255]);
}

#[test]
fn repeat_cr_newline() {
    // 3 wide red full band, CR, overdraw 1 px green, newline, 2 px blue next band
    let o = run(b"#1;2;100;0;0#2;2;0;100;0#3;2;0;0;100#1!3~$#2~-#3!2~");
    assert_eq!((o.image.width, o.image.height), (3, 12));
    assert_eq!(px(&o, 0, 0), [0, 255, 0, 255]);
    assert_eq!(px(&o, 1, 0), [255, 0, 0, 255]);
    assert_eq!(px(&o, 2, 5), [255, 0, 0, 255]);
    assert_eq!(px(&o, 1, 6), [0, 0, 255, 255]);
    assert_eq!(px(&o, 2, 6), [0, 0, 0, 0]);
}

#[test]
fn hls_register() {
    // DEC hue 120 = red at L50 S100
    let o = run(b"#0;1;120;50;100~");
    assert_eq!(px(&o, 0, 0), [255, 0, 0, 255]);
    let o = run(b"#0;1;0;50;100~"); // hue 0 = blue
    assert_eq!(px(&o, 0, 0), [0, 0, 255, 255]);
    let o = run(b"#0;1;240;50;100~"); // hue 240 = green
    assert_eq!(px(&o, 0, 0), [0, 255, 0, 255]);
    let o = run(b"#0;1;0;50;0~"); // zero saturation = grey
    assert_eq!(px(&o, 0, 0), [128, 128, 128, 255]);
}

#[test]
fn default_palette_and_clamping() {
    let o = run(b"#2~"); // VT340 register 2 = 80,13,13 %
    assert_eq!(px(&o, 0, 0), [204, 33, 33, 255]);
    // out-of-range percent clamps; out-of-range register index clamps to the last register
    let o = run(b"#9999;2;500;0;0#9999~");
    assert_eq!(px(&o, 0, 0), [255, 0, 0, 255]);
    let o = run(b"#1;2;100;0;0 \r\n#1\n~");
    assert_eq!(px(&o, 0, 0), [255, 0, 0, 255]);
}

#[test]
fn raster_attributes_aspect_and_trim() {
    // Pan:Pad = 2:1 doubles rows; only 1 sixel row written -> 1 px * 2
    let o = run_with(1, b"\"2;1;1;6#1;2;100;100;100#1@", &Limits::default()).unwrap();
    assert_eq!((o.image.width, o.image.height), (1, 2));
    // default (P1=0) is 2:1 as well
    let mut d = SixelDecoder::new(SixelParams::from_dcs(&[], BG, None), &Limits::default());
    d.feed(b"#1;2;100;100;100#1@").unwrap();
    assert_eq!(d.finish().unwrap().image.height, 2);
    // trailing blank rows trimmed: a second band with nothing set contributes nothing
    let o = run(b"#1;2;100;100;100#1~-?");
    assert_eq!(o.image.height, 6);
}

#[test]
fn background_fill_for_p2_0_and_2() {
    for p2 in [0, 2] {
        let o = run_with(p2, b"#1;2;100;0;0#1A?~", &Limits::default()).unwrap();
        // column 1 was skipped by '?': unwritten pixels show the pane background
        assert_eq!(px(&o, 1, 0), BG);
        assert_eq!(px(&o, 0, 0), BG);
        assert_eq!(px(&o, 0, 1), [255, 0, 0, 255]);
    }
}

#[test]
fn streaming_split_anywhere_is_identical() {
    let data: &[u8] = b"\"1;1;4;6#1;2;100;0;0#2;1;120;50;100#1!2~#2!2~$#1@@@@-#2~~~~";
    let whole = run(data);
    for cut in 0..data.len() {
        let mut d = SixelDecoder::new(SixelParams::from_dcs(&[9, 1], BG, None), &Limits::default());
        d.feed(&data[..cut]).unwrap();
        d.feed(&data[cut..]).unwrap();
        assert_eq!(d.finish().unwrap().image, whole.image, "cut {cut}");
    }
}

#[test]
fn registers_carry_when_1070_reset() {
    let a = run(b"#7;2;100;0;0~");
    let mut d = SixelDecoder::new(
        SixelParams::from_dcs(&[9, 1], BG, Some(a.registers.clone())),
        &Limits::default(),
    );
    d.feed(b"#7~").unwrap();
    assert_eq!(px(&d.finish().unwrap(), 0, 0), [255, 0, 0, 255]);
}

#[test]
fn hostile_limits() {
    let lim = Limits {
        sixel_max_bytes: 100,
        ..Limits::default()
    };
    let mut d = SixelDecoder::new(SixelParams::from_dcs(&[], BG, None), &lim);
    assert_eq!(d.feed(&[b'~'; 101]), Err(SixelError::TooManyBytes));
    // tiny stream, huge raster attributes
    let r = run_with(1, b"\"1;1;100000;100000#1~", &Limits::default());
    assert_eq!(r.unwrap_err(), SixelError::TooLarge);
    let r = run_with(1, b"\"1;1;9000;9000#1~", &Limits::default());
    assert_eq!(r.unwrap_err(), SixelError::TooLarge); // 81 Mpx > 64 Mpx cap
    // huge repeat
    assert_eq!(
        run_with(1, b"#1!999999999~", &Limits::default()).unwrap_err(),
        SixelError::TooLarge
    );
    // many bands
    let mut d = SixelDecoder::new(SixelParams::from_dcs(&[9, 1], BG, None), &Limits::default());
    let mut err = None;
    for _ in 0..3000 {
        if let Err(e) = d.feed(b"~-") {
            err = Some(e);
            break;
        }
    }
    assert_eq!(err, Some(SixelError::TooLarge));
    // pixel cap independent of byte cap
    let lim = Limits {
        sixel_max_pixels: 100,
        ..Limits::default()
    };
    assert_eq!(
        run_with(1, b"!20~!20?", &lim).unwrap_err(),
        SixelError::TooLarge
    );
    // empty streams
    assert_eq!(
        run_with(1, b"", &Limits::default()).unwrap_err(),
        SixelError::Empty
    );
    assert_eq!(
        run_with(1, b"#1;2;1;2;3$-$-", &Limits::default()).unwrap_err(),
        SixelError::Empty
    );
    // garbage never panics
    let junk: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let _ = run_with(1, &junk, &Limits::default());
}

// -- independent encoder round trip --------------------------------------------------

/// Minimal sixel encoder (test harness): palette of percent colours, one pass per colour.
fn encode(w: usize, h: usize, pal: &[[u32; 3]], idx: &[usize]) -> Vec<u8> {
    let mut s = format!("\"1;1;{w};{h}");
    for (i, c) in pal.iter().enumerate() {
        s += &format!("#{i};2;{};{};{}", c[0], c[1], c[2]);
    }
    for band in 0..h.div_ceil(6) {
        for ci in 0..pal.len() {
            let mut row = String::new();
            let mut any = false;
            for x in 0..w {
                let mut bits = 0u8;
                for b in 0..6 {
                    let y = band * 6 + b;
                    if y < h && idx[y * w + x] == ci {
                        bits |= 1 << b;
                    }
                }
                any |= bits != 0;
                row.push((bits + 0x3f) as char);
            }
            if any {
                s += &format!("#{ci}{row}$");
            }
        }
        s += "-";
    }
    s.into_bytes()
}

#[test]
fn round_trip_generated_images() {
    let pal = [
        [100, 0, 0],
        [0, 100, 0],
        [0, 0, 100],
        [100, 100, 0],
        [20, 40, 60],
    ];
    let mut seed = 0x1234_5678u32;
    for &(w, h) in &[
        (1usize, 1usize),
        (7, 6),
        (13, 7),
        (40, 25),
        (3, 19),
        (64, 64),
    ] {
        let idx: Vec<usize> = (0..w * h)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 16) as usize % pal.len()
            })
            .collect();
        let o = run(&encode(w, h, &pal, &idx));
        assert_eq!(
            (o.image.width as usize, o.image.height as usize),
            (w, h),
            "{w}x{h}"
        );
        for y in 0..h {
            for x in 0..w {
                let c = pal[idx[y * w + x]];
                let want = [c[0], c[1], c[2]].map(|v| ((v * 255 + 50) / 100) as u8);
                let got = px(&o, x as u32, y as u32);
                assert_eq!(got, [want[0], want[1], want[2], 255], "{w}x{h} at {x},{y}");
            }
        }
    }
}

#[test]
fn cell_row_slices() {
    let o = run(b"#1;2;1;1;1#1~~-~~-~~");
    assert_eq!(o.image.height, 18);
    let s = o.image.cell_row_slices(8);
    assert_eq!(s, vec![0..8, 8..16, 16..18]);
}
