// SPDX-License-Identifier: AGPL-3.0-only
//! Graphics wiring beyond the fixture corpus (T-15 §1, §4, §5): the decode barrier, file
//! media and its consent, chunk expiry, quotas, history and reflow.

use std::time::{Duration, Instant};

use apoth_image::kitty::Outcome;
use apoth_term::{Term, TermConfig, deny};
use base64::Engine as _;

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn rgb(w: u32, h: u32) -> String {
    rgbf(w, h, 0x80)
}

/// Distinct fills give distinct images (identical payloads share one blob).
fn rgbf(w: u32, h: u32, fill: u8) -> String {
    b64(&vec![fill; (w * h * 3) as usize])
}

fn term(f: impl FnOnce(&mut TermConfig)) -> Term {
    let mut c = TermConfig {
        rows: 4,
        cols: 10,
        ..TermConfig::default()
    };
    f(&mut c);
    Term::new(c)
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().unwrap();
        wr.write_image_data(&vec![7u8; (w * h * 3) as usize])
            .unwrap();
    }
    out
}

#[test]
fn async_decode_holds_the_stream_until_the_worker_answers() {
    let mut t = term(|c| c.graphics.inline_decode = false);
    let now = Instant::now();
    let seq = format!("\x1b_Ga=T,f=24,s=2,v=1,i=7;{}\x1b\\after", rgb(2, 1));
    let w = t.feed(seq.as_bytes(), now);
    assert!(w.jobs && t.blocked());
    assert_eq!(t.row_text(0).trim_end(), "", "text after the APC waits");
    // More output arrives while blocked: held, in order.
    t.feed(b"!", now);
    let jobs = t.take_image_jobs();
    assert_eq!(jobs.len(), 1);
    let done = jobs.into_iter().next().unwrap().run();
    assert!(matches!(done.outcome, Outcome::Image(_)));
    t.image_done(done, now);
    assert!(!t.blocked());
    assert_eq!(t.replies().take_all(), b"\x1b_Gi=7;OK\x1b\\");
    // The image moved the cursor (down 1, right 1) before the text ran.
    assert_eq!(t.row_text(1).trim_end(), " after!");
    assert_eq!(t.placements().len(), 1);
}

#[test]
fn a_stale_answer_does_not_unblock() {
    let mut t = term(|c| c.graphics.inline_decode = false);
    let now = Instant::now();
    t.feed(
        format!("\x1b_Ga=t,f=24,s=2,v=1,i=7;{}\x1b\\x", rgb(2, 1)).as_bytes(),
        now,
    );
    let mut job = t.take_image_jobs().pop().unwrap();
    job.token += 1000;
    t.image_done(job.run(), now);
    assert!(t.blocked());
}

#[test]
fn file_media_reads_a_temp_file_and_consumes_it() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "tty-graphics-protocol-apoth-{}.png",
        std::process::id()
    ));
    std::fs::write(&path, png(3, 2)).unwrap();
    let mut t = term(|_| {});
    let seq = format!(
        "\x1b_Ga=T,t=t,f=100,i=3,C=1;{}\x1b\\",
        b64(path.to_str().unwrap().as_bytes())
    );
    t.feed(seq.as_bytes(), Instant::now());
    assert_eq!(t.replies().take_all(), b"\x1b_Gi=3;OK\x1b\\");
    assert!(!path.exists(), "temp files are consumed by the read");
}

#[test]
fn file_media_denied_has_no_effect_and_no_reply() {
    let mut t = term(|c| c.deny = deny::GRAPHICS_FILE);
    t.feed(
        format!("\x1b_Ga=t,t=f,f=100,i=3;{}\x1b\\", b64(b"/etc/hostname")).as_bytes(),
        Instant::now(),
    );
    assert!(t.replies().take_all().is_empty());
    assert!(t.events_ref().is_empty());
}

#[test]
fn file_media_ask_raises_an_event_and_waits() {
    let mut t = term(|c| c.deny = deny::GRAPHICS_FILE_ASK);
    let now = Instant::now();
    let seq = format!(
        "\x1b_Ga=t,t=f,f=100,i=3;{}\x1b\\text",
        b64(b"/nonexistent/apoth.png")
    );
    t.feed(seq.as_bytes(), now);
    let token = t.events().graphics_file.take().expect("ask event");
    assert!(t.blocked());
    assert_eq!(t.row_text(0).trim_end(), "");
    t.graphics_file_decision(token, true, now);
    assert_eq!(
        t.replies().take_all(),
        b"\x1b_Gi=3;ENOENT:file not readable\x1b\\"
    );
    assert_eq!(t.row_text(0).trim_end(), "text");

    // Denied by the user: no effect, no reply, the stream resumes.
    let mut t = term(|c| c.deny = deny::GRAPHICS_FILE_ASK);
    t.feed(seq.as_bytes(), now);
    let token = t.events().graphics_file.take().unwrap();
    t.graphics_file_decision(token, false, now);
    assert!(t.replies().take_all().is_empty());
    assert_eq!(t.row_text(0).trim_end(), "text");
}

#[test]
fn graphics_direct_denied_skips_both_protocols() {
    let mut t = term(|c| c.deny = deny::GRAPHICS_DIRECT);
    let now = Instant::now();
    t.feed(
        format!("\x1b_Ga=T,f=24,s=2,v=1,i=7;{}\x1b\\", rgb(2, 1)).as_bytes(),
        now,
    );
    t.feed(b"\x1bP0;1;0q#1;2;100;0;0#1~~\x1b\\\x1b[?1;1S", now);
    assert!(t.replies().take_all().is_empty());
    assert!(!t.has_images());
}

#[test]
fn half_received_chunks_expire_on_the_timer() {
    let mut t = term(|_| {});
    let t0 = Instant::now();
    t.feed(b"\x1b_Ga=T,f=24,s=2,v=1,i=7,m=1;/wAA\x1b\\", t0);
    let dl = t.next_deadline().expect("chunk deadline");
    assert!(dl > t0 + Duration::from_secs(9));
    t.tick(dl + Duration::from_millis(1));
    assert!(t.next_deadline().is_none());
    // The continuation now starts a fresh (bad) transfer instead of completing the old.
    t.feed(b"\x1b_Gm=0;/wAA\x1b\\", dl + Duration::from_millis(2));
    assert!(!t.has_images());
}

#[test]
fn quota_evicts_unreferenced_images_first() {
    let mut t = term(|c| c.graphics.limits.quota = 3 * 4 * 100);
    let now = Instant::now();
    // Two unplaced 10x10 images fill most of the quota; a third evicts the oldest.
    for id in 1..=3 {
        t.feed(
            format!("\x1b_Ga=t,f=24,s=10,v=10,i={id};{}\x1b\\", rgbf(10, 10, id)).as_bytes(),
            now,
        );
    }
    let st = t.image_store();
    assert_eq!(st.image_count(), 3);
    t.feed(
        format!("\x1b_Ga=t,f=24,s=10,v=10,i=4;{}\x1b\\", rgbf(10, 10, 4)).as_bytes(),
        now,
    );
    let st = t.image_store();
    assert_eq!(st.image_count(), 3);
    assert!(st.get(apoth_image::store::Screen::Main, 1).is_none());
    assert!(st.used() <= 1200);
}

#[test]
fn on_screen_images_are_never_evicted_enospc_instead() {
    let mut t = term(|c| c.graphics.limits.quota = 400);
    let now = Instant::now();
    t.feed(
        format!("\x1b_Ga=T,f=24,s=10,v=10,i=1,C=1,q=1;{}\x1b\\", rgb(10, 10)).as_bytes(),
        now,
    );
    t.feed(
        format!("\x1b_Ga=t,f=24,s=10,v=10,i=2;{}\x1b\\", rgbf(10, 10, 2)).as_bytes(),
        now,
    );
    assert_eq!(
        t.replies().take_all(),
        b"\x1b_Gi=2;ENOSPC:image quota exhausted\x1b\\"
    );
    assert_eq!(t.placements().len(), 1);
}

#[test]
fn placements_leave_with_their_history_rows() {
    let mut t = term(|c| c.scrollback.lines = Some(10));
    let now = Instant::now();
    t.feed(
        format!("\x1b_Ga=T,f=24,s=2,v=1,i=1,C=1,q=1;{}\x1b\\", rgb(2, 1)).as_bytes(),
        now,
    );
    t.feed(&b"\n".repeat(20_000), now);
    assert!(t.placements().is_empty(), "evicted with its row");
}

#[test]
fn ed3_keeps_on_screen_placements_and_drops_history_ones() {
    let mut t = term(|_| {});
    let now = Instant::now();
    t.feed(
        format!("\x1b_Ga=T,f=24,s=2,v=1,i=1,C=1,q=1;{}\x1b\\", rgb(2, 1)).as_bytes(),
        now,
    );
    t.feed(b"\n\n\n\n\n", now); // into history
    t.feed(
        format!(
            "\x1b[2;3H\x1b_Ga=T,f=24,s=2,v=1,i=2,C=1,q=1;{}\x1b\\",
            rgb(2, 1)
        )
        .as_bytes(),
        now,
    );
    assert_eq!(t.placements().len(), 2);
    t.feed(b"\x1b[3J", now);
    let p = t.placements();
    assert_eq!(p.len(), 1);
    assert_eq!((p[0].image, p[0].row, p[0].col), (2, 1, 2));
}

#[test]
fn resize_reflow_keeps_the_anchor_cell() {
    let mut t = term(|_| {});
    let now = Instant::now();
    t.feed(
        format!("abc\x1b_Ga=T,f=24,s=2,v=1,i=1,C=1,q=1;{}\x1b\\", rgb(2, 1)).as_bytes(),
        now,
    );
    t.resize(6, 20);
    let p = t.placements();
    assert_eq!((p.len(), p[0].row, p[0].col), (1, 0, 3));
}

#[test]
fn draws_scale_with_the_cell_size_and_snap_to_pixels() {
    let mut t = term(|c| c.cell_px = (8, 16));
    let now = Instant::now();
    t.feed(
        format!(
            "\x1b[2;3H\x1b_Ga=T,f=24,s=8,v=16,i=1,C=1,X=3,q=1;{}\x1b\\",
            rgb(8, 16)
        )
        .as_bytes(),
        now,
    );
    for (cell, want) in [
        ((8, 16), [19, 16, 8, 16]),
        ((10, 20), [24, 20, 10, 20]),
        ((12, 24), [29, 24, 12, 24]),
        ((16, 32), [38, 32, 16, 32]),
    ] {
        t.set_cell_px(cell.0, cell.1);
        let mut d = Vec::new();
        t.image_draws(t.primary().history_rows(), t.rows(), &mut d);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].dst, want, "cell {cell:?}");
    }
}

#[test]
fn scrolled_back_viewports_see_history_images() {
    let mut t = term(|_| {});
    let now = Instant::now();
    t.feed(
        format!("\x1b_Ga=T,f=24,s=2,v=1,i=1,C=1,q=1;{}\x1b\\", rgb(2, 1)).as_bytes(),
        now,
    );
    t.feed(b"\n\n\n\n\n", now);
    let mut d = Vec::new();
    let hist = t.primary().history_rows();
    t.image_draws(hist, t.rows(), &mut d);
    assert!(d.is_empty(), "off the top of the live view");
    t.image_draws(0, t.rows(), &mut d);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].dst[1], 0);
}
