// SPDX-License-Identifier: AGPL-3.0-only
mod common;
use apoth_image::Limits;
use apoth_image::iterm::*;
use common::*;

fn body(args: &str, data: &str) -> Vec<u8> {
    format!("File={args}:{data}").into_bytes()
}

#[test]
fn parses_all_keys() {
    let name = b64(b"cat.png");
    let b = body(
        &format!("name={name};size=12;width=10;height=50%;preserveAspectRatio=0;inline=1"),
        "QUJD",
    );
    let (f, data) = parse_file(&b).unwrap();
    assert_eq!(f.name.as_deref(), Some("cat.png"));
    assert_eq!(
        (f.size, f.width, f.height, f.preserve_aspect),
        (Some(12), Dim::Cells(10), Dim::Percent(50), false)
    );
    assert_eq!(data, b"QUJD");
    let (f, _) = parse_file(&body("inline=1;width=20px;height=auto", "")).unwrap();
    assert_eq!(
        (f.width, f.height, f.preserve_aspect),
        (Dim::Px(20), Dim::Auto, true)
    );
}

#[test]
fn refuses_and_rejects() {
    assert_eq!(
        parse_file(&body("inline=0", "AA")).unwrap_err(),
        ITermError::InlineRefused
    );
    assert_eq!(
        parse_file(&body("size=1", "AA")).unwrap_err(),
        ITermError::InlineRefused
    );
    assert_eq!(parse_file(b"Fil=x:").unwrap_err(), ITermError::NotFile);
    assert_eq!(
        parse_file(b"File=inline=1").unwrap_err(),
        ITermError::BadArgs
    );
    assert_eq!(
        parse_file(&body("inline=1;width=abc", "")).unwrap_err(),
        ITermError::BadArgs
    );
    assert_eq!(
        parse_file(&body("inline=1;width=-3", "")).unwrap_err(),
        ITermError::BadArgs
    );
    assert_eq!(
        parse_file(&body("inline=1;size=x", "")).unwrap_err(),
        ITermError::BadArgs
    );
    // a control-laden name is sanitised, never an error
    let n = b64(b"a\x1b[2Jb");
    let (f, _) = parse_file(&body(&format!("inline=1;name={n}"), "")).unwrap();
    assert_eq!(f.name.as_deref(), Some("a[2Jb"));
}

#[test]
fn decodes_png_and_checks_size() {
    let px = [1, 2, 3, 255, 4, 5, 6, 255];
    let png = png_rgba(2, 1, &px);
    let b = body(&format!("inline=1;size={}", png.len()), &b64(&png));
    let (f, data) = parse_file(&b).unwrap();
    let img = decode_file(&f, data, &Limits::default()).unwrap();
    assert_eq!(img.rgba, px);
    let (mut f2, _) = parse_file(&b).unwrap();
    f2.size = Some(3);
    assert_eq!(
        decode_file(&f2, data, &Limits::default()).unwrap_err(),
        ITermError::BadData
    );
}

#[test]
fn hostile_payloads() {
    let (f, _) = parse_file(&body("inline=1", "")).unwrap();
    let lim = Limits::default();
    assert_eq!(
        decode_file(&f, b"!!!!", &lim).unwrap_err(),
        ITermError::BadData
    );
    assert_eq!(
        decode_file(&f, b64(b"GIF89a....").as_bytes(), &lim).unwrap_err(),
        ITermError::Unsupported
    );
    let small = Limits {
        max_decoded_bytes: 16,
        ..Limits::default()
    };
    assert_eq!(
        decode_file(&f, &[b'A'; 4000], &small).unwrap_err(),
        ITermError::TooLarge
    );
    let huge = ITermFile {
        size: Some(u64::MAX),
        ..f.clone()
    };
    assert_eq!(
        decode_file(&huge, b"", &lim).unwrap_err(),
        ITermError::TooLarge
    );
}
