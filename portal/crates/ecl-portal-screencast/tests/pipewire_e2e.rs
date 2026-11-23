// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end checks that need a live session. Both are `#[ignore]`d.
//!
//! Run (inside a session with PipeWire; the second also needs abyss with the
//! portal admission and a granted capture, i.e. `ecl-desktop-portal` at
//! `/usr/bin/ecl-desktop-portal`, so it is normally exercised through the
//! portal itself, see below):
//!
//! ```text
//! cargo test -p ecl-portal-screencast --test pipewire_e2e -- --ignored --nocapture
//! pw-dump | grep -A3 "Eclipse Screen Share"     # while the first test sleeps
//! ```
#![cfg(all(feature = "pipewire", feature = "wayland"))]

use std::sync::Arc;
use std::time::Duration;

use ecl_portal_proto::Source;
use ecl_portal_screencast::pw::PwSinkFactory;
use ecl_portal_screencast::session::CursorMode;
use ecl_portal_screencast::source::mock::{solid, MockCapturer};
use ecl_portal_screencast::source::{CaptureError, CaptureSource, Capturer, Frame, PixelFormat};
use ecl_portal_screencast::stream::RunningStream;

/// Needs only a running PipeWire daemon: mock frames in, real node out.
#[test]
#[ignore = "needs a running PipeWire session"]
fn publishes_a_pipewire_node_from_mock_frames() {
    let s = RunningStream::start(
        Arc::new(MockCapturer { frames: 1000 }),
        Arc::new(PwSinkFactory),
        Source::Output {
            name: "mock".into(),
            width: 640,
            height: 360,
        },
        CursorMode::Hidden,
    )
    .expect("stream starts");
    assert_ne!(s.node_id(), 0);
    // `ECL_E2E_HOLD_SECS` keeps the node up long enough to attach a consumer
    // (`gst-launch-1.0 pipewiresrc target-object=eclipse-screen-share ...`).
    let hold = std::env::var("ECL_E2E_HOLD_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    std::thread::sleep(Duration::from_secs(hold));
    assert!(!s.ended());
}

/// Needs the abyss side: the Wayland compositor must admit this process for
/// screencopy under a live grant, so a bare test binary is refused. Run the
/// real flow instead: start `ecl-desktop-portal`, then from any client
/// `xdg-desktop-portal` ScreenCast `Start` (e.g. OBS, or a browser's
/// getDisplayMedia) and approve abyss's confirm.
#[test]
#[ignore = "needs abyss to admit the portal under a live grant (main-thread work)"]
fn real_abyss_capture_reaches_pipewire() {
    use ecl_portal_screencast::wlr::WlrCapturer;
    let outs = ecl_portal_screencast::wlr::list_outputs().expect("wayland session");
    let first = outs.into_iter().next().expect("one output");
    let s = RunningStream::start(
        Arc::new(WlrCapturer),
        Arc::new(PwSinkFactory),
        first,
        CursorMode::Hidden,
    )
    .expect("abyss admits the capture");
    assert_ne!(s.node_id(), 0);
}

/// A source that keeps producing one frame per ~16 ms for as long as it is
/// asked, like a compositor with a screen that keeps changing. Pixels are
/// Xrgb8888 `[1, 2, 3, 0]` (B, G, R, unused): a consumer that negotiates RGBA
/// must see A = 255 (the unused byte is 0 in the source).
struct TickSource {
    w: u32,
    h: u32,
}

impl CaptureSource for TickSource {
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<Frame>, CaptureError> {
        std::thread::sleep(timeout.min(Duration::from_millis(16)));
        Ok(Some(solid(
            self.w,
            self.h,
            PixelFormat::Xrgb8888,
            [1, 2, 3, 0],
        )))
    }
}

struct TickCapturer;

impl Capturer for TickCapturer {
    fn open(
        &self,
        source: &Source,
        _c: CursorMode,
    ) -> Result<Box<dyn CaptureSource>, CaptureError> {
        match source {
            Source::Output { width, height, .. } => Ok(Box::new(TickSource {
                w: *width,
                h: *height,
            })),
            Source::Window { .. } => Err(CaptureError::Unsupported("window")),
        }
    }
}

/// Like the first test, but frames never stop, so a consumer attached at any
/// time must keep receiving them. Hold time as above.
#[test]
#[ignore = "needs a running PipeWire session"]
fn keeps_delivering_frames_to_a_consumer() {
    let s = RunningStream::start(
        Arc::new(TickCapturer),
        Arc::new(PwSinkFactory),
        Source::Output {
            name: "tick".into(),
            width: 640,
            height: 360,
        },
        CursorMode::Hidden,
    )
    .expect("stream starts");
    assert_ne!(s.node_id(), 0);
    let hold = std::env::var("ECL_E2E_HOLD_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    std::thread::sleep(Duration::from_secs(hold));
    assert!(!s.ended());
}
