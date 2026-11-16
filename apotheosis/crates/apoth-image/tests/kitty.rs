// SPDX-License-Identifier: AGPL-3.0-only
mod common;
use apoth_image::kitty::*;
use apoth_image::{ErrCode, Limits};
use common::*;

fn parse(s: &str) -> Header {
    parse_apc(s.as_bytes()).unwrap().0
}

#[test]
fn golden_transmit_place() {
    let h = parse("Ga=T,f=24,s=2,v=1,i=7,p=3,c=4,r=2,x=1,y=2,w=3,h=4,X=5,Y=6,z=-5,C=1,q=2");
    assert_eq!(
        h.ids,
        Ids {
            id: 7,
            number: 0,
            placement: 3
        }
    );
    assert_eq!(h.quiet, 2);
    let Command::TransmitPlace(t, p) = h.command else {
        panic!()
    };
    assert_eq!((t.format, t.width, t.height), (Format::Rgb24, 2, 1));
    assert_eq!(t.medium, Medium::Direct);
    assert_eq!(
        (p.cols, p.rows, p.src_x, p.src_y, p.src_w, p.src_h),
        (4, 2, 1, 2, 3, 4)
    );
    assert_eq!((p.x_off, p.y_off, p.z, p.no_cursor_move), (5, 6, -5, true));
}

#[test]
fn defaults_are_transmit_rgba_direct() {
    let Command::Transmit(t) = parse("Gi=1,s=1,v=1").command else {
        panic!()
    };
    assert_eq!(
        (t.format, t.medium, t.compressed),
        (Format::Rgba32, Medium::Direct, false)
    );
}

#[test]
fn unicode_placeholder_and_below_background_z() {
    let Command::Place(p) = parse("Ga=p,i=9,U=1,c=10,r=5,z=-1073741825").command else {
        panic!()
    };
    assert!(p.unicode);
    assert!(p.z < Z_BELOW_BACKGROUND);
}

#[test]
fn relative_and_animation_parse_only() {
    let Command::Place(p) = parse("Ga=p,i=2,P=1,Q=4,H=-3,V=2").command else {
        panic!()
    };
    assert_eq!(
        p.relative,
        Some(Relative {
            parent_id: 1,
            parent_placement: 4,
            h: -3,
            v: 2
        })
    );
    let Command::Frame(f) = parse("Ga=f,i=1,s=1,v=1,c=2,r=3,X=1,z=100").command else {
        panic!()
    };
    assert_eq!(
        (f.base_frame, f.edit_frame, f.gap_ms, f.composition),
        (2, 3, Some(100), 1)
    );
    let Command::Animate(a) = parse("Ga=a,i=1,s=3,v=2,r=1,z=50,c=4").command else {
        panic!()
    };
    assert_eq!(
        (a.state, a.loops, a.edit_frame, a.gap_ms, a.current_frame),
        (3, 2, 1, 50, 4)
    );
    let Command::Compose(c) = parse("Ga=c,i=1,r=1,c=2,x=1,y=2,w=3,h=4,X=5,Y=6,C=1").command else {
        panic!()
    };
    assert_eq!(
        (c.src_frame, c.dst_frame, c.w, c.dst_y, c.src_x, c.replace),
        (1, 2, 3, 2, 5, true)
    );
}

#[test]
fn delete_selectors_cover_the_set() {
    use DeleteTarget::*;
    let cases: &[(&str, DeleteTarget, bool)] = &[
        ("Ga=d", Visible, false),
        ("Ga=d,d=A", Visible, true),
        (
            "Ga=d,d=i,i=5,p=2",
            Id {
                id: 5,
                placement: 2,
            },
            false,
        ),
        (
            "Ga=d,d=I,i=5",
            Id {
                id: 5,
                placement: 0,
            },
            true,
        ),
        (
            "Ga=d,d=n,I=8",
            Number {
                number: 8,
                placement: 0,
            },
            false,
        ),
        ("Ga=d,d=c", Cursor, false),
        ("Ga=d,d=f,i=3", Frames { id: 3, number: 0 }, false),
        ("Ga=d,d=p,x=3,y=4", Cell { x: 3, y: 4 }, false),
        ("Ga=d,d=Q,x=3,y=4,z=-2", CellZ { x: 3, y: 4, z: -2 }, true),
        ("Ga=d,d=r,x=10,y=20", IdRange { lo: 10, hi: 20 }, false),
        ("Ga=d,d=x,x=7", Column(7), false),
        ("Ga=d,d=Y,y=7", Row(7), true),
        ("Ga=d,d=z,z=3", Z(3), false),
    ];
    for (s, t, free) in cases {
        let Command::Delete(d) = parse(s).command else {
            panic!("{s}")
        };
        assert_eq!((d.target, d.free), (*t, *free), "{s}");
    }
}

#[test]
fn hostile_keys() {
    for s in [
        "Gi=99999999999",
        "Gi=1,I=2",
        "Ga=d,d=r,x=9,y=1",
        "Gf=7",
        "Gt=x",
        "Go=q",
        "Gq=3",
        "Gi=-1",
        "Ga=T,C=2",
        "Ga=Z",
        "Gi",
        "Gm=2",
    ] {
        let e = parse_apc(s.as_bytes()).unwrap_err();
        assert_eq!(e.code, ErrCode::Einval, "{s}");
    }
    assert!(parse_apc(b"X").is_err());
    // unknown keys are ignored
    assert!(parse_apc(b"Gi=1,k=whatever,s=1,v=1").is_ok());
}

#[test]
fn replies() {
    let ids = Ids {
        id: 3,
        number: 0,
        placement: 0,
    };
    assert_eq!(format_reply(ids, Ok(()), 0).unwrap(), b"\x1b_Gi=3;OK\x1b\\");
    assert!(format_reply(ids, Ok(()), 1).is_none());
    assert!(format_reply(ids, Err(ErrCode::Enoent), 1).is_some());
    assert!(format_reply(ids, Err(ErrCode::Enoent), 2).is_none());
    assert!(format_reply(Ids::default(), Ok(()), 0).is_none());
    let n = Ids {
        id: 4,
        number: 9,
        placement: 2,
    };
    let r = format_reply(n, Err(ErrCode::Enospc), 0).unwrap();
    assert_eq!(r, b"\x1b_Gi=4,I=9,p=2;ENOSPC:image quota exhausted\x1b\\");
}

#[test]
fn reply_injection_never_echoes_input() {
    // Hostile bytes in every field; the reply stays inside the closed alphabet.
    let evil = "Gi=1,a=\x1b,t=\x1b\\\x1b]52;c;AAAA\x07,x=\r\n;\x1b\\\x1b_G";
    let e = parse_apc(evil.as_bytes());
    let ids = e.map_or_else(|e| e.ids, |(h, _)| h.ids);
    let r = format_reply(ids, Err(ErrCode::Einval), 0).unwrap();
    assert_eq!(r.iter().filter(|&&b| b == 0x1b).count(), 2);
    assert!(r.iter().all(|&b| b == 0x1b || (0x20..0x7f).contains(&b)));
    for code in [
        ErrCode::Einval,
        ErrCode::Enoent,
        ErrCode::EnoentFile,
        ErrCode::Enospc,
        ErrCode::Efbig,
        ErrCode::Ebadpng,
        ErrCode::Enodata,
        ErrCode::Etoodeep,
        ErrCode::Ecycle,
        ErrCode::Enoparent,
    ] {
        assert!(
            code.reply_text()
                .bytes()
                .all(|b| b.is_ascii_graphic() || b == b' ')
        );
    }
}

#[test]
fn reply_queue_rate_limit_and_overflow() {
    let lim = Limits {
        reply_burst: 3,
        reply_per_sec: 10,
        reply_queue_bytes: 40,
        ..Limits::default()
    };
    let mut q = ReplyQueue::new(&lim);
    assert!(q.push(b"0123456789", 0));
    assert!(q.push(b"0123456789", 0));
    assert!(q.push(b"0123456789", 0));
    assert!(!q.push(b"0123456789", 0)); // burst exhausted
    assert_eq!(q.dropped(), 1);
    assert!(q.push(b"0123456789", 1000)); // refilled, queue now 40 bytes
    assert!(!q.push(b"0", 2000)); // queue full
    assert_eq!(q.dropped(), 2);
    assert_eq!(q.drain().len(), 40);
    assert!(q.is_empty());
}

#[test]
fn chunked_transfer_assembles() {
    let mut a = Assembler::new(Limits::default());
    let pixels: Vec<u8> = (0..16).collect(); // 2x2 RGBA
    let full = b64(&pixels);
    let (c1, c2) = full.split_at(12);
    assert!(matches!(
        a.push(&apc("a=t,f=32,s=2,v=2,i=5,m=1", c1), 0),
        Event::Pending
    ));
    // only the first chunk carries keys
    let Event::Transfer(t) = a.push(&apc("m=0", c2), 1) else {
        panic!()
    };
    assert_eq!(t.payload, pixels);
    assert_eq!(t.header.ids.id, 5);
    let Command::Transmit(spec) = t.header.command else {
        panic!()
    };
    let img = decode_transfer(&spec, t.payload, &Limits::default()).unwrap();
    assert_eq!((img.width, img.height, img.rgba.len()), (2, 2, 16));
    assert_eq!(a.pending_len(), 0);
}

#[test]
fn interleaved_chunks_by_id() {
    let mut a = Assembler::new(Limits::default());
    let x = b64(&[1u8; 4]);
    let y = b64(&[2u8; 4]);
    assert!(matches!(
        a.push(&apc("i=1,s=1,v=1,m=1", ""), 0),
        Event::Pending
    ));
    assert!(matches!(
        a.push(&apc("i=2,s=1,v=1,m=1", ""), 0),
        Event::Pending
    ));
    let Event::Transfer(t) = a.push(&apc("i=1,m=0", &x), 1) else {
        panic!()
    };
    assert_eq!(t.payload, [1; 4]);
    let Event::Transfer(t) = a.push(&apc("i=2,m=0", &y), 1) else {
        panic!()
    };
    assert_eq!(t.payload, [2; 4]);
}

#[test]
fn unterminated_single_chunk_without_m_completes() {
    let mut a = Assembler::new(Limits::default());
    let Event::Transfer(t) = a.push(&apc("a=T,f=24,s=1,v=1,i=1", &b64(&[1, 2, 3])), 0) else {
        panic!()
    };
    assert_eq!(t.payload, [1, 2, 3]);
}

#[test]
fn more_is_ignored_on_a_shared_memory_transmission() {
    // mpv's `--vo=kitty --vo-kitty-use-shm=yes` sends `m=1` on every shm frame and never a
    // final chunk; chunking only applies to direct data, so each one is a whole transfer.
    let mut a = Assembler::new(Limits::default());
    for _ in 0..2 {
        let Event::Transfer(t) = a.push(
            &apc("a=T,t=s,f=24,s=1,v=1,q=2,m=1", &b64(b"mpv-kitty-1")),
            0,
        ) else {
            panic!("an shm transmission with m=1 is complete")
        };
        assert_eq!(t.payload, b"mpv-kitty-1");
    }
}

#[test]
fn bad_chunks() {
    let lim = Limits {
        max_chunk_b64: 8,
        max_transfer_b64: 16,
        ..Limits::default()
    };
    let mut a = Assembler::new(lim);
    // non-final chunk not a multiple of 4
    let Event::Error(f) = a.push(&apc("i=1,s=1,v=1,m=1", "AAAAA"), 0) else {
        panic!()
    };
    assert_eq!(f.code, ErrCode::Einval);
    // padding in a non-final chunk
    let Event::Error(f) = a.push(&apc("i=1,s=1,v=1,m=1", "AA=="), 0) else {
        panic!()
    };
    assert_eq!(f.code, ErrCode::Einval);
    // bad alphabet
    let Event::Error(f) = a.push(&apc("i=1,s=1,v=1", "AA!A"), 0) else {
        panic!()
    };
    assert_eq!(f.code, ErrCode::Enodata);
    // oversize chunk
    let Event::Error(f) = a.push(&apc("i=1,s=1,v=1", "AAAAAAAAAAAA"), 0) else {
        panic!()
    };
    assert_eq!(f.code, ErrCode::Efbig);
    // total cap independent of the chunk size
    assert!(matches!(
        a.push(&apc("i=2,s=1,v=1,m=1", "AAAAAAAA"), 0),
        Event::Pending
    ));
    assert!(matches!(
        a.push(&apc("i=2,m=1", "AAAAAAAA"), 0),
        Event::Pending
    ));
    let Event::Error(f) = a.push(&apc("i=2,m=1", "AAAA"), 0) else {
        panic!()
    };
    assert_eq!(f.code, ErrCode::Efbig);
    assert_eq!(f.ids.id, 2);
    assert_eq!(a.pending_len(), 0);
}

#[test]
fn stale_transfers_expire() {
    let mut a = Assembler::new(Limits::default());
    assert!(matches!(
        a.push(&apc("i=1,s=1,v=1,m=1", "AAAA"), 0),
        Event::Pending
    ));
    assert_eq!(a.pending_len(), 1);
    // the continuation after 10 s+ finds nothing pending and is a stray command
    let ev = a.push(&apc("i=1,m=0", "AAAA"), 10_001);
    assert_eq!(a.stale_dropped(), 1);
    assert!(!matches!(ev, Event::Pending));
}

#[test]
fn pending_count_is_capped() {
    let lim = Limits {
        max_pending_transfers: 2,
        ..Limits::default()
    };
    let mut a = Assembler::new(lim);
    for i in 1..=5 {
        let _ = a.push(&apc(&format!("i={i},s=1,v=1,m=1"), "AAAA"), 0);
    }
    assert_eq!(a.pending_len(), 2);
}

#[test]
fn raw_formats() {
    let lim = Limits::default();
    let rgb = TransmitSpec {
        format: Format::Rgb24,
        medium: Medium::Direct,
        compressed: false,
        width: 2,
        height: 1,
        size: 0,
        offset: 0,
    };
    let img = decode_transfer(&rgb, vec![1, 2, 3, 4, 5, 6], &lim).unwrap();
    assert_eq!(img.rgba, [1, 2, 3, 255, 4, 5, 6, 255]);
    assert_eq!(
        decode_transfer(&rgb, vec![1, 2, 3], &lim).unwrap_err(),
        ErrCode::Enodata
    );
    let zero = TransmitSpec {
        width: 0,
        ..rgb.clone()
    };
    assert_eq!(
        decode_transfer(&zero, vec![], &lim).unwrap_err(),
        ErrCode::Einval
    );
    let wide = TransmitSpec {
        width: 10_001,
        ..rgb.clone()
    };
    assert_eq!(
        decode_transfer(&wide, vec![], &lim).unwrap_err(),
        ErrCode::Einval
    );
    // decoded cap checked before any allocation: no data supplied, still Efbig
    let big = TransmitSpec {
        width: 10_000,
        height: 10_000,
        ..rgb.clone()
    };
    let small = Limits {
        max_decoded_bytes: 1000,
        ..Limits::default()
    };
    assert_eq!(
        decode_transfer(&big, vec![], &small).unwrap_err(),
        ErrCode::Efbig
    );
}

#[test]
fn zlib_and_bombs() {
    let lim = Limits::default();
    let raw = vec![7u8; 4 * 4 * 4];
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let spec = TransmitSpec {
        format: Format::Rgba32,
        medium: Medium::Direct,
        compressed: true,
        width: 4,
        height: 4,
        size: 0,
        offset: 0,
    };
    let img = decode_transfer(&spec, z, &lim).unwrap();
    assert_eq!(img.rgba, raw);
    // a bomb inflating past the declared size is stopped at the limit
    let big = vec![0u8; 50_000_000];
    let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&big, 1);
    assert!(bomb.len() < 1_000_000);
    assert_eq!(
        decode_transfer(&spec, bomb, &lim).unwrap_err(),
        ErrCode::Efbig
    );
    assert_eq!(
        decode_transfer(&spec, b"not zlib".to_vec(), &lim).unwrap_err(),
        ErrCode::Enodata
    );
}

#[test]
fn png_decode_and_hostile() {
    let lim = Limits::default();
    let px = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 9, 9, 9, 255];
    let png = png_rgba(2, 2, &px);
    let spec = TransmitSpec {
        format: Format::Png,
        medium: Medium::Direct,
        compressed: false,
        width: 0,
        height: 0,
        size: 0,
        offset: 0,
    };
    let img = decode_transfer(&spec, png.clone(), &lim).unwrap();
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(img.rgba, px);
    // compressed PNG file
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&png, 6);
    let zs = TransmitSpec {
        compressed: true,
        ..spec.clone()
    };
    assert_eq!(decode_transfer(&zs, z, &lim).unwrap().rgba, px);
    // truncated and garbage
    assert_eq!(
        decode_png(&png[..png.len() / 2], &lim).unwrap_err(),
        ErrCode::Ebadpng
    );
    assert_eq!(decode_png(b"garbage", &lim).unwrap_err(), ErrCode::Ebadpng);
    // oversize rejected from the header, before the pixel buffer
    let tiny = Limits {
        max_decoded_bytes: 8,
        ..Limits::default()
    };
    assert_eq!(decode_png(&png, &tiny).unwrap_err(), ErrCode::Efbig);
    let axis = Limits {
        max_axis: 1,
        ..Limits::default()
    };
    assert_eq!(decode_png(&png, &axis).unwrap_err(), ErrCode::Einval);
}

#[test]
fn capability_probe_keys() {
    assert!(supported_keys(b'T').contains("U"));
    assert_eq!(supported_keys(b'?'), "");
    assert_eq!(parse("Ga=q,i=1,s=1,v=1,e=T").probe, Some(b'T'));
    assert!(matches!(
        parse("Ga=q,i=1,s=1,v=1").command,
        Command::Query(_)
    ));
}

#[test]
fn pending_transfers_are_charged_and_capped() {
    use apoth_image::store::ProcessBudget;
    let lim = Limits {
        quota: 100,
        quota_total: 1 << 30,
        ..Limits::default()
    };
    let budget = ProcessBudget::new();
    let mut a = Assembler::with_budget(lim, budget.clone());
    let chunk = b64(&[7u8; 60]);
    assert!(matches!(
        a.push(&apc("a=t,f=24,s=100,v=100,i=1,m=1", &chunk), 0),
        Event::Pending
    ));
    assert_eq!(budget.used(), 60);
    // a second transfer would push the pane over its quota
    assert!(matches!(
        a.push(&apc("a=t,f=24,s=100,v=100,i=2,m=1", &chunk), 1),
        Event::Error(_)
    ));
    assert_eq!(budget.used(), 60);
    a.expire(1_000_000);
    assert_eq!(a.pending_len(), 0);
    assert_eq!(budget.used(), 0);
}
