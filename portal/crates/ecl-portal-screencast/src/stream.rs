// SPDX-License-Identifier: AGPL-3.0-only

//! One running share: a thread that pulls frames from a [`Capturer`] source,
//! paces them and pushes them into a sink (the PipeWire producer in
//! production). Dropping a [`RunningStream`] stops the thread and tears the
//! sink (and so the PipeWire node) down.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ecl_portal_proto::Source;

use crate::pipeline::{FrameSink, Pump};
use crate::session::CursorMode;
use crate::source::{CaptureError, CaptureSource, Capturer, Frame, PixelFormat};

/// Frame cap for shares.
pub const MAX_FPS: u32 = 60;
/// How long the first frame may take before the share is abandoned.
const FIRST_FRAME: Duration = Duration::from_secs(5);

/// What a sink needs to know, taken from the first captured frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkConfig {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub max_fps: u32,
}

/// A sink plus the PipeWire node id it published.
pub struct MadeSink {
    pub sink: Box<dyn FrameSink + Send>,
    pub node_id: u32,
}

/// Builds the sink once the frame geometry is known. Called on the stream
/// thread (pw types stay on the thread that made them).
pub trait SinkFactory: Send + Sync {
    fn make(&self, cfg: SinkConfig) -> Result<MadeSink, String>;
}

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("no frame within the timeout")]
    NoFrame,
    #[error("sink: {0}")]
    Sink(String),
    #[error("stream thread failed to start")]
    Thread,
}

impl FrameSink for Box<dyn FrameSink + Send> {
    fn push(&mut self, frame: Frame) -> Result<(), String> {
        (**self).push(frame)
    }
}

struct Boxed(Box<dyn CaptureSource>);

impl CaptureSource for Boxed {
    fn next_frame(&mut self, t: Duration) -> Result<Option<Frame>, CaptureError> {
        self.0.next_frame(t)
    }
}

fn now_ns() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

pub struct RunningStream {
    node_id: u32,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RunningStream {
    pub fn node_id(&self) -> u32 {
        self.node_id
    }

    /// True once the pump ended on its own (compositor revoked, source gone).
    pub fn ended(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    /// Blocking: starts the thread and waits for the node id. Call from
    /// `spawn_blocking`.
    pub fn start(
        capturer: Arc<dyn Capturer>,
        factory: Arc<dyn SinkFactory>,
        source: Source,
        cursor: CursorMode,
    ) -> Result<Self, StreamError> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, StreamError>>();
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ecl-stream".into())
            .spawn(move || {
                if let Err(e) = run(&*capturer, &*factory, &source, cursor, &stop2, &ready_tx) {
                    // Ignored if the caller already has its node id.
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|_| StreamError::Thread)?;
        let node_id = match ready_rx.recv_timeout(FIRST_FRAME + Duration::from_secs(6)) {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                return Err(StreamError::NoFrame);
            }
        };
        Ok(Self {
            node_id,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for RunningStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    capturer: &dyn Capturer,
    factory: &dyn SinkFactory,
    source: &Source,
    cursor: CursorMode,
    stop: &AtomicBool,
    ready: &mpsc::Sender<Result<u32, StreamError>>,
) -> Result<(), StreamError> {
    let mut src = Boxed(capturer.open(source, cursor)?);
    let deadline = Instant::now() + FIRST_FRAME;
    let first = loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(StreamError::NoFrame);
        }
        if let Some(f) = src.next_frame(left.min(Duration::from_millis(250)))? {
            break f;
        }
    };
    let made = factory
        .make(SinkConfig {
            width: first.width,
            height: first.height,
            format: first.format,
            max_fps: MAX_FPS,
        })
        .map_err(StreamError::Sink)?;
    let mut sink = made.sink;
    sink.push(first).map_err(StreamError::Sink)?;
    let _ = ready.send(Ok(made.node_id));
    let mut pump = Pump::new(src, sink, MAX_FPS, now_ns);
    if let Err(e) = pump.run(|| stop.load(Ordering::SeqCst)) {
        tracing::info!("share ended: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::mock::MockCapturer;
    use std::sync::Mutex;

    struct Count(Arc<Mutex<u32>>, Arc<AtomicBool>);
    impl FrameSink for Count {
        fn push(&mut self, _: Frame) -> Result<(), String> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
    }
    impl Drop for Count {
        fn drop(&mut self) {
            self.1.store(true, Ordering::SeqCst);
        }
    }
    struct Fac(Arc<Mutex<u32>>, Arc<AtomicBool>);
    impl SinkFactory for Fac {
        fn make(&self, cfg: SinkConfig) -> Result<MadeSink, String> {
            assert_eq!((cfg.width, cfg.height), (4, 3));
            Ok(MadeSink {
                sink: Box::new(Count(self.0.clone(), self.1.clone())),
                node_id: 42,
            })
        }
    }
    struct FailFac;
    impl SinkFactory for FailFac {
        fn make(&self, _: SinkConfig) -> Result<MadeSink, String> {
            Err("no pipewire".into())
        }
    }

    fn out() -> Source {
        Source::Output {
            name: "DP-1".into(),
            width: 4,
            height: 3,
        }
    }

    #[test]
    fn starts_reports_node_and_drop_tears_down() {
        let pushed = Arc::new(Mutex::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        let s = RunningStream::start(
            Arc::new(MockCapturer { frames: 3 }),
            Arc::new(Fac(pushed.clone(), dropped.clone())),
            out(),
            CursorMode::Hidden,
        )
        .unwrap();
        assert_eq!(s.node_id(), 42);
        drop(s);
        assert!(dropped.load(Ordering::SeqCst), "sink dropped with stream");
        assert!(*pushed.lock().unwrap() >= 1);
    }

    #[test]
    fn sink_failure_is_an_error() {
        let r = RunningStream::start(
            Arc::new(MockCapturer { frames: 3 }),
            Arc::new(FailFac),
            out(),
            CursorMode::Hidden,
        );
        assert!(matches!(r, Err(StreamError::Sink(_))));
    }

    #[test]
    fn capture_refusal_is_an_error() {
        let r = RunningStream::start(
            Arc::new(MockCapturer { frames: 3 }),
            Arc::new(FailFac),
            Source::Window {
                id: 1,
                title: String::new(),
                app_id: String::new(),
            },
            CursorMode::Hidden,
        );
        assert!(matches!(r, Err(StreamError::Capture(_))));
    }
}
