// SPDX-License-Identifier: AGPL-3.0-only
//! The media placement protocol (T-16 §2) against `Term`: requests out, events in,
//! replies, draws, offscreen pause, sessions and the per-pane cap.

use std::sync::Arc;
use std::time::Instant;

use apoth_term::apoth_image::Image;
use apoth_term::{MediaCtl, MediaEvent, MediaRequest, MediaSource, Term, TermConfig};

fn term() -> Term {
    Term::new(TermConfig {
        rows: 10,
        cols: 40,
        ..TermConfig::default()
    })
}

fn replies(t: &mut Term) -> String {
    String::from_utf8(t.replies().take_all()).unwrap()
}

fn open_key(t: &mut Term) -> (u64, (u32, u32), MediaSource) {
    let r = t.take_media_requests();
    match r.as_slice() {
        [
            MediaRequest::Open {
                key,
                box_px,
                source,
                ..
            },
        ] => (*key, *box_px, source.clone()),
        other => panic!("{other:?}"),
    }
}

fn frame(w: u32, h: u32) -> MediaEvent {
    MediaEvent::Frame(Arc::new(Image {
        width: w,
        height: h,
        rgba: vec![200; (w * h * 4) as usize],
    }))
}

#[test]
fn open_ready_frame_and_end() {
    let now = Instant::now();
    let mut t = term();
    let w = t.feed(b"\x1b_Ma=o,s=f,t=99,i=5,c=20,r=4\x1b\\", now);
    assert!(w.media);
    let (key, box_px, src) = open_key(&mut t);
    assert_eq!(src, MediaSource::Token(99));
    // 20x4 cells at the default 8x16 cell.
    assert_eq!(box_px, (160, 64));
    // The cursor sits below the box.
    assert_eq!((t.cursor().y, t.cursor().x), (4, 0));
    let ready = MediaEvent::Ready {
        width: 160,
        height: 90,
        duration_ms: 2000,
    };
    t.media_event(key, ready, now);
    assert_eq!(replies(&mut t), "\x1b_Mi=5;ready,w=160,h=90,d=2000\x1b\\");
    let w = t.media_event(key, frame(160, 90), now);
    assert!(w.dirty);
    let mut d = Vec::new();
    t.image_draws(t.primary().history_rows(), t.rows(), now, &mut d);
    assert_eq!(d.len(), 1);
    // A 16:9 frame in a 160x64 box: full height, centred.
    assert_eq!(d[0].dst, [23, 0, 113, 64]);
    let first = d[0].key;
    t.media_event(key, frame(160, 90), now);
    t.image_draws(t.primary().history_rows(), t.rows(), now, &mut d);
    assert_eq!(
        d[0].replaces,
        Some(first),
        "same-size frames reuse the texture"
    );
    t.feed(b"\x1b_Ma=q,i=5\x1b\\", now);
    assert_eq!(
        replies(&mut t),
        "\x1b_Mi=5;state=playing,pos=0,dur=2000\x1b\\"
    );
    t.media_event(key, MediaEvent::Ended, now);
    assert_eq!(replies(&mut t), "\x1b_Mi=5;ended\x1b\\");
    t.feed(b"\x1b_Ma=x,i=5\x1b\\", now);
    assert_eq!(t.take_media_requests(), vec![MediaRequest::Close { key }]);
    assert!(!t.has_media());
}

#[test]
fn controls_and_errors() {
    let now = Instant::now();
    let mut t = term();
    t.feed(
        b"\x1b_Ma=o,s=f,t=1,i=1,c=4,r=2,pause,loop=1,volume=50\x1b\\",
        now,
    );
    let r = t.take_media_requests();
    assert!(matches!(
        r[0],
        MediaRequest::Open {
            play: false,
            looping: true,
            volume: 0.5,
            ..
        }
    ));
    let key = match r[0] {
        MediaRequest::Open { key, .. } => key,
        _ => unreachable!(),
    };
    t.feed(b"\x1b_Ma=c,i=1,play,seek=1500,mute=1\x1b\\", now);
    assert_eq!(
        t.take_media_requests(),
        vec![
            MediaRequest::Control {
                key,
                ctl: MediaCtl::Play
            },
            MediaRequest::Control {
                key,
                ctl: MediaCtl::Seek(1500)
            },
            MediaRequest::Control {
                key,
                ctl: MediaCtl::Mute(true)
            },
        ]
    );
    t.feed(b"\x1b_Ma=c,i=9,play\x1b\\", now);
    assert_eq!(replies(&mut t), "\x1b_Mi=9;error=no-such-placement\x1b\\");
    t.feed(b"\x1b_Ma=c,i=1,rate=2\x1b\\", now);
    assert_eq!(replies(&mut t), "\x1b_Mi=1;error=unsupported\x1b\\");
    t.feed(b"\x1b_Ma=o,s=f,i=2,bogus=1\x1b\\", now);
    assert_eq!(replies(&mut t), "\x1b_Mi=2;error=bad-request\x1b\\");
    // q=1: no success replies; errors still come.
    t.media_event(key, MediaEvent::Error("helper-protocol"), now);
    assert_eq!(replies(&mut t), "\x1b_Mi=1;error=helper-protocol\x1b\\");
    assert!(!t.has_media());
}

#[test]
fn per_pane_cap_and_path_and_data_sources() {
    let now = Instant::now();
    let mut t = term();
    t.feed(b"\x1b_Ma=o,s=f,t=1,i=1,c=2,r=1\x1b\\", now);
    t.feed(b"\x1b_Ma=o,s=f,t=2,i=2,c=2,r=1\x1b\\", now);
    t.feed(b"\x1b_Ma=o,s=f,t=3,i=3,c=2,r=1\x1b\\", now);
    assert_eq!(t.take_media_requests().len(), 2);
    assert_eq!(replies(&mut t), "\x1b_Mi=3;error=quota\x1b\\");
    t.feed(b"\x1b_Ma=x,i=1\x1b\\\x1b_Ma=x,i=2\x1b\\", now);
    t.take_media_requests();
    // `s=p`: the path is base64 in the payload.
    t.feed(b"\x1b_Ma=o,s=p,i=4,c=2,r=1;L3RtcC9hLndlYm0=\x1b\\", now);
    let (_, _, src) = open_key(&mut t);
    assert_eq!(src, MediaSource::Path(b"/tmp/a.webm".to_vec()));
    // `s=d` in two chunks.
    t.feed(
        b"\x1b_Ma=o,s=d,i=5,c=2,r=1,m=1;aGVs\x1b\\\x1b_Mm=0;bG8=\x1b\\",
        now,
    );
    let (_, _, src) = open_key(&mut t);
    assert_eq!(src, MediaSource::Data(b"hello".to_vec()));
}

#[test]
fn path_source_is_refused_when_file_media_is_denied() {
    let now = Instant::now();
    let mut t = Term::new(TermConfig {
        deny: apoth_term::deny::GRAPHICS_FILE,
        ..TermConfig::default()
    });
    t.feed(b"\x1b_Ma=o,s=p,i=4;L3RtcC9h\x1b\\", now);
    assert!(t.take_media_requests().is_empty());
    assert_eq!(replies(&mut t), "\x1b_Mi=4;error=path-refused\x1b\\");
}

#[test]
fn scrolling_off_the_screen_pauses_and_history_loss_closes() {
    let now = Instant::now();
    let mut c = TermConfig {
        rows: 4,
        cols: 10,
        ..TermConfig::default()
    };
    c.scrollback.lines = Some(10);
    let mut t = Term::new(c);
    t.feed(b"\x1b_Ma=o,s=f,t=1,i=1,c=4,r=2\x1b\\", now);
    let (key, ..) = open_key(&mut t);
    t.media_event(
        key,
        MediaEvent::Ready {
            width: 4,
            height: 2,
            duration_ms: 0,
        },
        now,
    );
    t.feed(b"\r\n\r\n\r\n\r\n", now);
    assert_eq!(
        t.take_media_requests(),
        vec![MediaRequest::Control {
            key,
            ctl: MediaCtl::Pause
        }]
    );
    t.feed(&b"\n".repeat(20_000), now);
    assert_eq!(t.take_media_requests(), vec![MediaRequest::Close { key }]);
    assert!(!t.has_media());
}

#[test]
fn a_session_takes_its_media_with_it_and_ris_closes_everything() {
    let now = Instant::now();
    let mut t = term();
    t.feed(
        b"\x1b_Ma=b,i=7\x1b\\\x1b_Ma=o,s=f,t=1,i=1,c=2,r=1\x1b\\",
        now,
    );
    let r = t.take_media_requests();
    let key = match r[0] {
        MediaRequest::Open {
            key, session: 7, ..
        } => key,
        ref o => panic!("{o:?}"),
    };
    // A clean end still removes video.
    t.feed(b"\x1b_Ma=e,i=7\x1b\\", now);
    assert_eq!(t.take_media_requests(), vec![MediaRequest::Close { key }]);
    t.feed(b"\x1b_Ma=o,s=f,t=1,i=2,c=2,r=1\x1b\\", now);
    let (key, ..) = open_key(&mut t);
    t.feed(b"\x1bc", now);
    assert_eq!(t.take_media_requests(), vec![MediaRequest::Close { key }]);
    assert!(!t.has_media());
}

#[test]
fn denied_graphics_ignore_media_silently() {
    let now = Instant::now();
    let mut t = Term::new(TermConfig {
        deny: apoth_term::deny::GRAPHICS_DIRECT,
        ..TermConfig::default()
    });
    t.feed(b"\x1b_Ma=o,s=f,t=1,i=1\x1b\\", now);
    assert!(t.take_media_requests().is_empty());
    assert!(replies(&mut t).is_empty());
}
