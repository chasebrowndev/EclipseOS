// SPDX-License-Identifier: AGPL-3.0-only
//! Players over the real sandboxed helper and a fake sink (T-16 §6): frames arrive paced
//! by the audio clock, the end is reported, seeks and loops restart, and a hostile helper
//! is killed with `helper-protocol`.

use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use apoth_media::player::{Control, Event, Opts, Player, SinkKind};
use apoth_media::sink::FakeSink;

fn fixture(name: &str) -> OwnedFd {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    OwnedFd::from(File::open(p).unwrap())
}

fn opts(sink: SinkKind) -> Opts {
    Opts {
        box_w: 160,
        box_h: 90,
        sink,
        helper: Some(PathBuf::from(env!("CARGO_BIN_EXE_apoth-media-helper"))),
        ..Opts::default()
    }
}

fn spawn(name: &str, o: Opts) -> (Player, mpsc::Receiver<(Instant, Event)>) {
    let (tx, rx) = mpsc::channel();
    let p = Player::spawn(
        fixture(name),
        o,
        Box::new(move |e| {
            let _ = tx.send((Instant::now(), e));
        }),
    );
    (p, rx)
}

/// Collect events until `Ended` or an error (or the deadline).
fn until_end(rx: &mpsc::Receiver<(Instant, Event)>, secs: u64) -> Vec<(Instant, Event)> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut v = Vec::new();
    while let Ok(e) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        let stop = matches!(e.1, Event::Ended | Event::Error(_));
        v.push(e);
        if stop {
            break;
        }
    }
    v
}

fn frames(v: &[(Instant, Event)]) -> usize {
    v.iter().filter(|e| matches!(e.1, Event::Frame(_))).count()
}

#[test]
fn plays_to_the_end_in_real_time_on_the_audio_clock() {
    let sink = FakeSink::realtime(48_000 * 2 * 2);
    let (_p, rx) = spawn("av1-opus-160x90.webm", opts(SinkKind::Fake(sink.clone())));
    let t0 = Instant::now();
    let ev = until_end(&rx, 15);
    assert!(
        matches!(
            ev.first().map(|e| &e.1),
            Some(Event::Ready {
                width: 160,
                height: 90,
                audio: true,
                ..
            })
        ),
        "{:?}",
        ev.first().map(|e| &e.1)
    );
    assert!(
        matches!(ev.last().map(|e| &e.1), Some(Event::Ended)),
        "{:?}",
        ev.last().map(|e| &e.1)
    );
    // 2 s of 30 fps: most frames shown, in about 2 s of wall time.
    let n = frames(&ev);
    assert!(n >= 45, "{n} frames shown");
    let took = ev.last().unwrap().0 - t0;
    assert!(
        took >= Duration::from_millis(1700) && took < Duration::from_secs(5),
        "{took:?}"
    );
    assert!(sink.state.lock().unwrap().written > 300_000);
}

#[test]
fn without_a_sink_video_runs_on_the_frame_clock() {
    let (_p, rx) = spawn("av1-opus-160x90.webm", opts(SinkKind::Off));
    let ev = until_end(&rx, 15);
    assert!(matches!(ev[0].1, Event::Ready { audio: false, .. }));
    assert!(
        matches!(ev.last().unwrap().1, Event::Ended),
        "{:?}",
        ev.last()
    );
    assert!(frames(&ev) >= 45);
}

#[test]
fn seek_and_loop_restart_playback() {
    let mut o = opts(SinkKind::Fake(FakeSink::realtime(48_000 * 4)));
    o.looping = true;
    let (p, rx) = spawn("av1-opus-160x90.webm", o);
    // Looping: still playing after more than one pass.
    let deadline = Instant::now() + Duration::from_millis(3500);
    let mut n = 0;
    while let Ok((_, e)) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        assert!(!matches!(e, Event::Ended | Event::Error(_)), "{e:?}");
        n += usize::from(matches!(e, Event::Frame(_)));
    }
    assert!(n > 60, "{n} frames over a loop");
    p.control(Control::Loop(false));
    p.control(Control::Seek(1500));
    let ev = until_end(&rx, 10);
    assert!(
        matches!(ev.last().unwrap().1, Event::Ended),
        "{:?}",
        ev.last()
    );
    // Half a second left after the seek.
    assert!(frames(&ev) < 40, "{}", frames(&ev));
}

#[test]
fn audio_only_plays_through_the_sink() {
    let sink = FakeSink::realtime(48_000 * 2);
    let (_p, rx) = spawn("sine-1s.flac", opts(SinkKind::Fake(sink.clone())));
    let ev = until_end(&rx, 10);
    assert!(matches!(
        ev[0].1,
        Event::Ready {
            width: 0,
            audio: true,
            ..
        }
    ));
    assert!(
        matches!(ev.last().unwrap().1, Event::Ended),
        "{:?}",
        ev.last()
    );
    assert!(sink.state.lock().unwrap().written >= 90_000);
}

#[test]
fn pause_holds_and_play_resumes() {
    let mut o = opts(SinkKind::Fake(FakeSink::realtime(48_000 * 4)));
    o.play = false;
    let (p, rx) = spawn("av1-opus-160x90.webm", o);
    std::thread::sleep(Duration::from_millis(800));
    let held: Vec<_> = rx.try_iter().collect();
    // The first frame shows; nothing advances.
    assert!(frames(&held) <= 1, "{}", frames(&held));
    p.control(Control::Play);
    let ev = until_end(&rx, 10);
    assert!(
        matches!(ev.last().unwrap().1, Event::Ended),
        "{:?}",
        ev.last()
    );
    assert!(frames(&ev) >= 45);
}

#[test]
fn forged_frame_metadata_kills_the_helper() {
    let mut o = opts(SinkKind::Off);
    o.test_args = vec!["--test-forge=frame".into()];
    let (p, rx) = spawn("av1-opus-160x90.webm", o);
    let ev = until_end(&rx, 10);
    assert!(
        matches!(ev.last().unwrap().1, Event::Error("helper-protocol")),
        "{:?}",
        ev.last()
    );
    assert_eq!(frames(&ev), 0);
    gone(p.helper_pid());
}

#[test]
fn pcm_over_the_granted_rate_kills_the_helper() {
    let mut o = opts(SinkKind::Fake(FakeSink::realtime(48_000 * 4)));
    o.test_args = vec!["--test-forge=pcm".into()];
    let (p, rx) = spawn("av1-opus-160x90.webm", o);
    let ev = until_end(&rx, 10);
    assert!(
        matches!(ev.last().unwrap().1, Event::Error("helper-protocol")),
        "{:?}",
        ev.last()
    );
    gone(p.helper_pid());
}

#[test]
fn a_helper_without_its_sandbox_never_plays() {
    let mut o = opts(SinkKind::Off);
    o.test_args = vec!["--test-partial-landlock".into()];
    let (_p, rx) = spawn("av1-opus-160x90.webm", o);
    let ev = until_end(&rx, 10);
    assert_eq!(ev.len(), 1);
    assert!(
        matches!(ev[0].1, Event::Error("sandbox-unavailable")),
        "{:?}",
        ev[0].1
    );
}

/// The helper process is gone (killed and reaped) shortly after.
fn gone(pid: u32) {
    assert_ne!(pid, 0);
    let deadline = Instant::now() + Duration::from_secs(3);
    let path = format!("/proc/{pid}/stat");
    while Instant::now() < deadline {
        match std::fs::read_to_string(&path) {
            Err(_) => return,
            // A zombie still awaiting reaping counts as alive for this test.
            Ok(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    panic!("helper {pid} still exists");
}

/// A mid-clip frame is a real picture (not flat), in RGBA at the fitted size. With
/// `APOTH_MEDIA_FRAME_PNG=<path>` it is also written out for a visual check.
#[test]
fn frames_are_pictures() {
    let (_p, rx) = spawn("av1-opus-160x90.webm", opts(SinkKind::Off));
    let v = until_end(&rx, 20);
    let imgs: Vec<_> = v
        .iter()
        .filter_map(|e| match &e.1 {
            Event::Frame(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    let f = &imgs[imgs.len() / 2];
    assert_eq!((f.width, f.height), (160, 90));
    assert_eq!(f.rgba.len(), 160 * 90 * 4);
    let mut lum: Vec<u32> = f
        .rgba
        .chunks(4)
        .map(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]))
        .collect();
    assert!(f.rgba.chunks(4).all(|p| p[3] == 255), "opaque");
    lum.sort_unstable();
    assert!(
        lum[lum.len() * 9 / 10] > lum[lum.len() / 10] + 30,
        "not flat"
    );
    if let Some(path) = std::env::var_os("APOTH_MEDIA_FRAME_PNG") {
        let file = File::create(path).unwrap();
        let mut enc = png::Encoder::new(file, f.width, f.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&f.rgba)
            .unwrap();
    }
}

/// Three looping players at once, no sound server (the `glass_shots` load): none of their
/// helpers dies. rav1d's debug-build overlap tracker used to abort a frame-threaded helper
/// within a few loops (`helper-crashed`); dev builds now decode rav1d like release.
#[test]
fn three_loopers_stay_up() {
    let ps: Vec<_> = (0..3)
        .map(|_| {
            let mut o = opts(SinkKind::Off);
            o.looping = true;
            spawn("av1-opus-160x90.webm", o)
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut n = [0usize; 3];
    while Instant::now() < deadline {
        for (i, (_, rx)) in ps.iter().enumerate() {
            for (_, e) in rx.try_iter() {
                assert!(
                    !matches!(e, Event::Ended | Event::Error(_)),
                    "player {i}: {e:?}"
                );
                n[i] += usize::from(matches!(e, Event::Frame(_)));
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(n.iter().all(|&f| f > 120), "{n:?} frames");
}
