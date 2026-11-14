// SPDX-License-Identifier: AGPL-3.0-only
//! Graphics R2 (T-15 §1, §3, §6; T-16 §1.1): animation frames and playback, frame
//! composition, Unicode placeholders, relative placements, iTerm2 inline images and
//! `apoth icat` sessions.

use std::time::{Duration, Instant};

use apoth_term::{ImageDraw, Term, TermConfig};
use base64::Engine as _;

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn rgba(w: u32, h: u32, px: [u8; 4]) -> String {
    b64(&px.repeat((w * h) as usize))
}

fn term() -> Term {
    Term::new(TermConfig {
        rows: 6,
        cols: 20,
        cell_px: (8, 16),
        ..TermConfig::default()
    })
}

fn draws(t: &Term, now: Instant) -> (Vec<ImageDraw>, Option<Instant>) {
    let mut d = Vec::new();
    let due = t.image_draws(t.primary().history_rows(), t.rows(), now, &mut d);
    (d, due)
}

fn replies(t: &mut Term) -> String {
    String::from_utf8_lossy(&t.replies().take_all()).into_owned()
}

/// A 2-frame animation of a 4x2 image: red root, green second frame.
fn animated(t: &mut Term, now: Instant) {
    let seq = format!(
        "\x1b_Ga=T,f=32,s=4,v=2,i=5,C=1,q=1;{}\x1b\\\
         \x1b_Ga=f,f=32,s=4,v=2,i=5,z=100,q=1;{}\x1b\\",
        rgba(4, 2, [255, 0, 0, 255]),
        rgba(4, 2, [0, 255, 0, 255]),
    );
    t.feed(seq.as_bytes(), now);
}

#[test]
fn frames_load_and_play_on_the_clock_with_a_due_time() {
    let mut t = term();
    let t0 = Instant::now();
    animated(&mut t, t0);
    assert_eq!(
        t.image_store().frame_count(apoth_image_screen(), 5),
        Some(2)
    );
    // Stopped on frame 1 until a=a starts it: no due time.
    let (d, due) = draws(&t, t0);
    assert_eq!(d.len(), 1);
    assert_eq!(&d[0].image.rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(due, None);
    // Frame 1 gap 0 (root) is gapless when looping: set it to 50 ms and loop.
    t.feed(
        b"\x1b_Ga=a,i=5,r=1,z=50,q=1\x1b\\\x1b_Ga=a,i=5,s=3,v=1,q=1\x1b\\",
        t0,
    );
    let (d, due) = draws(&t, t0 + Duration::from_millis(10));
    assert_eq!(&d[0].image.rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(due, Some(t0 + Duration::from_millis(50)));
    let (d, due) = draws(&t, t0 + Duration::from_millis(60));
    assert_eq!(&d[0].image.rgba[..4], &[0, 255, 0, 255]);
    assert_eq!(due, Some(t0 + Duration::from_millis(150)));
    assert!(
        d[0].replaces.is_some(),
        "frames are same-size texture updates"
    );
    // Back to frame 1 after 50 + 100 ms.
    let (d, _) = draws(&t, t0 + Duration::from_millis(160));
    assert_eq!(&d[0].image.rgba[..4], &[255, 0, 0, 255]);
    // Stop: no more wakeups.
    t.feed(
        b"\x1b_Ga=a,i=5,s=1,q=1\x1b\\",
        t0 + Duration::from_millis(170),
    );
    assert_eq!(draws(&t, t0 + Duration::from_millis(400)).1, None);
}

fn apoth_image_screen() -> apoth_term::apoth_image::store::Screen {
    apoth_term::apoth_image::store::Screen::Main
}

#[test]
fn a_frame_lands_at_x_y_on_a_y_coloured_canvas_and_records_its_change_rect() {
    let mut t = term();
    let now = Instant::now();
    t.feed(
        format!(
            "\x1b_Ga=t,f=32,s=4,v=2,i=1,q=1;{}\x1b\\",
            rgba(4, 2, [9, 9, 9, 255])
        )
        .as_bytes(),
        now,
    );
    // 1x1 white pixel at (2, 1) on an opaque blue canvas.
    t.feed(
        format!(
            "\x1b_Ga=f,f=32,s=1,v=1,i=1,x=2,y=1,Y=255,q=1;{}\x1b\\",
            rgba(1, 1, [255, 255, 255, 255])
        )
        .as_bytes(),
        now,
    );
    let st = t.image_store();
    let f = st.frame_image(apoth_image_screen(), 1, 1).unwrap();
    assert_eq!(
        &f.rgba[..4],
        &[0, 0, 0, 255],
        "Y=0x000000ff is opaque black"
    );
    let at = (4 + 2) * 4;
    assert_eq!(&f.rgba[at..at + 4], &[255, 255, 255, 255]);
    // Out of bounds: EINVAL.
    t.feed(
        format!(
            "\x1b_Ga=f,f=32,s=1,v=1,i=1,x=4;{}\x1b\\",
            rgba(1, 1, [1, 1, 1, 1])
        )
        .as_bytes(),
        now,
    );
    assert!(replies(&mut t).contains("EINVAL"));
    // Unknown image: ENOENT.
    t.feed(
        format!(
            "\x1b_Ga=f,f=32,s=1,v=1,i=77;{}\x1b\\",
            rgba(1, 1, [1, 1, 1, 1])
        )
        .as_bytes(),
        now,
    );
    assert!(replies(&mut t).contains("ENOENT"));
}

#[test]
fn compose_copies_between_frames_and_rejects_overlap() {
    let mut t = term();
    let now = Instant::now();
    animated(&mut t, now);
    // Copy a 1x1 of frame 2 (green) to frame 1 at (3, 1), replacing.
    t.feed(b"\x1b_Ga=c,i=5,r=2,c=1,w=1,h=1,x=3,y=1,C=1\x1b\\", now);
    assert!(replies(&mut t).contains("OK"));
    let f = t
        .image_store()
        .frame_image(apoth_image_screen(), 5, 0)
        .unwrap();
    let at = (4 + 3) * 4;
    assert_eq!(&f.rgba[at..at + 4], &[0, 255, 0, 255]);
    assert_eq!(&f.rgba[..4], &[255, 0, 0, 255]);
    // Same frame, overlapping rects.
    t.feed(b"\x1b_Ga=c,i=5,r=1,c=1,w=2,h=1,x=1,X=0\x1b\\", now);
    assert!(replies(&mut t).contains("EINVAL"));
    // d=f drops the frames after the root.
    t.feed(b"\x1b_Ga=d,d=f,i=5\x1b\\", now);
    assert_eq!(
        t.image_store().frame_count(apoth_image_screen(), 5),
        Some(1)
    );
    assert_eq!(t.placements().len(), 1, "placements stay");
}

/// Placeholder cell text: U+10EEEE + row + column diacritics.
fn ph(row: usize, col: usize) -> String {
    use apoth_term::apoth_image::placeholder::DIACRITICS;
    let mut s = String::from('\u{10EEEE}');
    s.push(char::from_u32(DIACRITICS[row]).unwrap());
    s.push(char::from_u32(DIACRITICS[col]).unwrap());
    s
}

#[test]
fn placeholder_cells_draw_one_quad_per_run() {
    let mut t = term();
    let now = Instant::now();
    // 16x32 image, virtual placement 2 cols x 2 rows, id 42 (fg = 42 as 24-bit).
    t.feed(
        format!(
            "\x1b_Ga=T,U=1,f=32,s=16,v=32,i=42,p=3,c=2,r=2,q=2;{}\x1b\\",
            rgba(16, 32, [1, 2, 3, 255])
        )
        .as_bytes(),
        now,
    );
    assert_eq!(t.cursor().x, 0, "U=1 never moves the cursor");
    assert!(
        t.placements().is_empty(),
        "virtual placements have no cell box"
    );
    let (d, _) = draws(&t, now);
    assert!(d.is_empty(), "nothing until placeholders are printed");
    let row0 = format!("{}{}", ph(0, 0), ph(0, 1));
    let row1 = format!("{}{}", ph(1, 0), ph(1, 1));
    let s = format!("\x1b[2;3H\x1b[38;2;0;0;42m{row0}\x1b[3;3H{row1}\x1b[m");
    t.feed(s.as_bytes(), now);
    let (d, _) = draws(&t, now);
    assert_eq!(d.len(), 1, "a 2x2 run merges into one quad: {d:?}");
    // The 16x32 image fits a 2x2 box of 8x16 cells (16x32): exactly the box.
    assert_eq!(d[0].dst, [16, 16, 16, 32]);
    assert_eq!(d[0].src, [0, 0, 16, 32]);
    // Inferred marks: a bare placeholder continues the column.
    let s = format!("\x1b[6;1H\x1b[38;2;0;0;42m{}\u{10EEEE}\x1b[m", ph(0, 0));
    t.feed(s.as_bytes(), now);
    let (d, _) = draws(&t, now);
    assert_eq!(d.len(), 2);
    // d=a never removes a virtual placement; d=i does.
    t.feed(b"\x1b_Ga=d,d=a,q=2\x1b\\", now);
    assert_eq!(draws(&t, now).0.len(), 2);
    t.feed(b"\x1b_Ga=d,d=i,i=42,q=2\x1b\\", now);
    assert!(draws(&t, now).0.is_empty());
}

#[test]
fn placeholder_quads_stay_on_the_pixel_grid_at_every_scale() {
    for cell in [(8u32, 16u32), (10, 20), (12, 24), (16, 32)] {
        let mut t = Term::new(TermConfig {
            rows: 6,
            cols: 20,
            cell_px: cell,
            ..TermConfig::default()
        });
        let now = Instant::now();
        t.feed(
            format!(
                "\x1b_Ga=T,U=1,f=32,s=3,v=5,i=7,c=3,r=2,q=2;{}\x1b\\",
                rgba(3, 5, [9, 9, 9, 255])
            )
            .as_bytes(),
            now,
        );
        let s = format!(
            "\x1b[1;1H\x1b[38;5;7m{}{}{}\x1b[2;1H{}{}{}\x1b[m",
            ph(0, 0),
            ph(0, 1),
            ph(0, 2),
            ph(1, 0),
            ph(1, 1),
            ph(1, 2)
        );
        t.feed(s.as_bytes(), now);
        let (d, _) = draws(&t, now);
        assert_eq!(d.len(), 1, "cell {cell:?}");
        let [x, y, w, h] = d[0].dst;
        let (bw, bh) = (3 * cell.0 as i32, 2 * cell.1 as i32);
        // Aspect kept (3:5 in a 3x2-cell box), centred, inside the box.
        assert!(
            x >= 0 && y >= 0 && x + w <= bw && y + h <= bh,
            "{cell:?} {:?}",
            d[0].dst
        );
        assert_eq!(h, bh, "height limits: {cell:?}");
        let want_w = (bh * 3 / 5).max(1);
        assert!((w - want_w).abs() <= 1, "{cell:?} w {w} want {want_w}");
    }
}

#[test]
fn relative_placements_follow_the_parent_and_cascade() {
    let mut t = term();
    let now = Instant::now();
    t.feed(
        format!(
            "\x1b_Ga=T,f=32,s=8,v=16,i=1,p=1,C=1,q=2;{}\x1b\\",
            rgba(8, 16, [1, 1, 1, 255])
        )
        .as_bytes(),
        now,
    );
    t.feed(
        format!(
            "\x1b_Ga=t,f=32,s=8,v=16,i=2,q=2;{}\x1b\\",
            rgba(8, 16, [2, 2, 2, 255])
        )
        .as_bytes(),
        now,
    );
    let x0 = t.cursor().x;
    // Missing parent.
    t.feed(b"\x1b_Ga=p,i=2,p=1,P=9,Q=1\x1b\\", now);
    assert!(replies(&mut t).contains("ENOPARENT"));
    // Child two cells right, one down of the parent; the cursor does not move.
    t.feed(b"\x1b_Ga=p,i=2,p=1,P=1,Q=1,H=2,V=1\x1b\\", now);
    assert!(replies(&mut t).contains("OK"));
    assert_eq!(t.cursor().x, x0);
    let (d, _) = draws(&t, now);
    assert_eq!(d.len(), 2);
    let child = d.iter().find(|q| q.image.rgba[0] == 2).unwrap();
    assert_eq!((child.dst[0], child.dst[1]), (16, 16));
    // A cycle: make parent 1 a child of its own child.
    t.feed(b"\x1b_Ga=p,i=1,p=1,P=2,Q=1\x1b\\", now);
    assert!(replies(&mut t).contains("ECYCLE"));
    // Deleting the parent deletes the child.
    t.feed(b"\x1b_Ga=d,d=i,i=1,q=2\x1b\\", now);
    assert!(draws(&t, now).0.is_empty());
}

#[test]
fn relative_chains_deeper_than_eight_are_refused() {
    let mut t = term();
    let now = Instant::now();
    t.feed(
        format!(
            "\x1b_Ga=T,f=32,s=8,v=16,i=1,p=1,C=1,q=2;{}\x1b\\",
            rgba(8, 16, [1, 1, 1, 255])
        )
        .as_bytes(),
        now,
    );
    for p in 2..=9u32 {
        t.feed(
            format!("\x1b_Ga=p,i=1,p={p},P=1,Q={},H=1,q=1\x1b\\", p - 1).as_bytes(),
            now,
        );
    }
    assert!(replies(&mut t).is_empty(), "depth 8 is allowed");
    t.feed(b"\x1b_Ga=p,i=1,p=10,P=1,Q=9,H=1\x1b\\", now);
    assert!(replies(&mut t).contains("ETOODEEP"));
}

fn png(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().unwrap();
        wr.write_image_data(&px.repeat((w * h) as usize)).unwrap();
    }
    out
}

#[test]
fn iterm2_inline_images_place_at_the_cursor() {
    let mut t = term();
    let now = Instant::now();
    let data = png(16, 32, [5, 6, 7, 255]);
    let s = format!(
        "\x1b]1337;File=inline=1;size={}:{}\x07after",
        data.len(),
        b64(&data)
    );
    t.feed(s.as_bytes(), now);
    let p = t.placements();
    assert_eq!(p.len(), 1);
    assert_eq!((p[0].cols, p[0].rows), (2, 2));
    let (d, _) = draws(&t, now);
    assert_eq!(d[0].dst, [0, 0, 16, 32]);
    // inline=0 is refused.
    let s = format!("\x1b]1337;File=size={}:{}\x07", data.len(), b64(&data));
    t.feed(s.as_bytes(), now);
    assert_eq!(t.placements().len(), 1);
    // User variables still work beside File=.
    t.feed(b"\x1b]1337;SetUserVar=k=dg==\x07", now);
    assert_eq!(t.user_vars().get("k").map(String::as_str), Some("v"));
}

#[test]
fn iterm2_off_ignores_files() {
    let mut t = Term::new(TermConfig {
        graphics: apoth_term::GraphicsConfig {
            iterm2: false,
            ..Default::default()
        },
        ..TermConfig::default()
    });
    let data = png(2, 2, [1, 1, 1, 255]);
    let s = format!("\x1b]1337;File=inline=1:{}\x07", b64(&data));
    t.feed(s.as_bytes(), Instant::now());
    assert!(t.placements().is_empty());
}

#[test]
fn a_clean_session_end_keeps_stills_and_drops_animations() {
    let mut t = term();
    let now = Instant::now();
    t.feed(b"\x1b_Ma=b,i=77\x1b\\", now);
    assert!(t.has_image_sessions());
    assert!(!t.bind_image_sessions(4242));
    t.feed(
        format!(
            "\x1b_Ga=T,f=32,s=8,v=16,i=1,q=2;{}\x1b\\",
            rgba(8, 16, [1, 1, 1, 255])
        )
        .as_bytes(),
        now,
    );
    animated(&mut t, now);
    assert_eq!(t.placements().len(), 2);
    t.feed(b"\x1b_Ma=e,i=77\x1b\\", now);
    assert!(!t.has_image_sessions());
    let p = t.placements();
    assert_eq!(p.len(), 1, "the still image stays");
    assert_eq!(p[0].image, 1);
}

#[test]
fn a_session_whose_process_left_the_foreground_loses_every_placement() {
    let mut t = term();
    let now = Instant::now();
    t.feed(b"\x1b_Ma=b,i=9\x1b\\", now);
    t.bind_image_sessions(4242);
    t.feed(
        format!(
            "\x1b_Ga=T,f=32,s=8,v=16,i=1,q=2;{}\x1b\\",
            rgba(8, 16, [1, 1, 1, 255])
        )
        .as_bytes(),
        now,
    );
    // Still in the foreground: nothing happens.
    assert!(!t.bind_image_sessions(4242));
    assert_eq!(t.placements().len(), 1);
    // The shell took the terminal back (the process was killed).
    assert!(t.bind_image_sessions(100));
    assert!(t.placements().is_empty());
    assert!(!t.has_image_sessions());
}
