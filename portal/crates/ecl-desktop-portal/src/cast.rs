// SPDX-License-Identifier: AGPL-3.0-only

//! The real ScreenCast / Screenshot backend (ADR 0097, D-08 §3/§6).
//!
//! `Start` runs: chooser pane (outputs only) -> `portal.sock` capture confirm
//! (abyss draws it) -> on `Granted`, a capture stream publishing a PipeWire
//! node. The grant lives exactly as long as its `portal.sock` connection, so
//! each running stream owns that connection and closing the stream (Close,
//! peer exit, daemon exit) drops it, which revokes the grant. Nothing is ever
//! written on the connection after the answer.
//!
//! Every failure is a denial (response 2) or a cancel (1); nothing hangs.
//! Everything outside this file's trait seams (pane, portal.sock, wayland
//! capture, PipeWire) is injected so the state machine is tested with fakes.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ecl_portal_core::request::{Reply, Results};
use ecl_portal_core::screencast::{
    BoxFuture, Call, ScreenCastBackend, ScreenCastError, SOURCE_MONITOR,
};
use ecl_portal_core::val::Val;
use ecl_portal_proto::{CaptureConfirm, PaneReply, PaneRequest, Requester, Source};
use ecl_portal_screencast::grant::{self, GrantError};
use ecl_portal_screencast::session::{
    Capabilities, CursorMode, SelectOptions, Session as CastSession, Stream,
};
use ecl_portal_screencast::source::Capturer;
use ecl_portal_screencast::stream::{RunningStream, SinkFactory};
use ecl_portal_screencast::{random_token, remote, screenshot};

use crate::state::lock;

/// Anything whose drop revokes the grant (the `portal.sock` connection).
pub type Hold = Box<dyn Send>;

/// A live grant: `hold` keeps it alive; `revoked` fires when abyss closes the
/// connection (it revokes every grant on session lock). A dropped sender
/// means we released the grant ourselves.
pub struct Held {
    pub id: u64,
    pub hold: Hold,
    pub revoked: tokio::sync::oneshot::Receiver<()>,
}

/// Owns the task that owns the connection; dropping aborts it, which closes
/// the connection and so revokes the grant.
struct WatchGuard(tokio::task::JoinHandle<()>);

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The chooser pane. `None` = the pane failed (treated as a denial).
pub trait Chooser: Send + Sync {
    fn pick(&self, req: PaneRequest) -> BoxFuture<'_, Option<PaneReply>>;
}

/// The `portal.sock` client.
pub trait Granter: Send + Sync {
    fn grant(&self, req: CaptureConfirm) -> BoxFuture<'_, Result<Held, GrantError>>;
}

/// Shareable outputs. Blocking.
pub trait Outputs: Send + Sync {
    fn list(&self) -> Result<Vec<Source>, String>;
}

pub struct PaneChooser;

impl Chooser for PaneChooser {
    fn pick(&self, req: PaneRequest) -> BoxFuture<'_, Option<PaneReply>> {
        Box::pin(async move {
            match crate::pane::ask(&req).await {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::warn!("chooser pane failed: {e}");
                    None
                }
            }
        })
    }
}

pub struct SockGranter;

impl Granter for SockGranter {
    fn grant(&self, req: CaptureConfirm) -> BoxFuture<'_, Result<Held, GrantError>> {
        Box::pin(async move {
            let path = grant::socket_path()
                .ok_or_else(|| GrantError::Unavailable("XDG_RUNTIME_DIR unset".into()))?;
            let g = grant::request_grant(&path, &req, grant::DEFAULT_TIMEOUT).await?;
            let (tx, revoked) = tokio::sync::oneshot::channel();
            let mut conn = g.conn;
            let task = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                // Abyss sends nothing after the answer: EOF or any read
                // result at all means the grant is gone.
                let mut b = [0u8; 1];
                let _ = conn.read(&mut b).await;
                let _ = tx.send(());
            });
            Ok(Held {
                id: g.id,
                hold: Box::new(WatchGuard(task)),
                revoked,
            })
        })
    }
}

pub struct WlrOutputs;

impl Outputs for WlrOutputs {
    fn list(&self) -> Result<Vec<Source>, String> {
        ecl_portal_screencast::wlr::list_outputs().map_err(|e| e.to_string())
    }
}

/// One running share. Field order is drop order: stop capturing first, then
/// release the grant.
struct Active {
    _stream: RunningStream,
    _hold: Hold,
}

struct Live {
    session: CastSession,
    active: Vec<Active>,
}

pub struct CastBackend {
    chooser: Arc<dyn Chooser>,
    granter: Arc<dyn Granter>,
    outputs: Arc<dyn Outputs>,
    capturer: Arc<dyn Capturer>,
    sinks: Arc<dyn SinkFactory>,
    shot_dir: Option<PathBuf>,
    sessions: std::sync::Mutex<HashMap<String, Live>>,
    revoked_tx: tokio::sync::mpsc::UnboundedSender<String>,
    revoked_rx: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<String>>>,
    next_stream: std::sync::atomic::AtomicU64,
}

/// Aborts the in-flight `Start` marker when the `Start` future ends or is
/// dropped (request closed mid-chooser), so the app may call `Start` again.
struct StartGuard<'a> {
    be: &'a CastBackend,
    path: &'a str,
}

impl Drop for StartGuard<'_> {
    fn drop(&mut self) {
        if let Some(l) = lock(&self.be.sessions).get_mut(self.path) {
            l.session.abort_start();
        }
    }
}

fn release(live: Live) {
    // Dropping a stream joins its thread; keep that off the reactor.
    if live.active.is_empty() {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(h) => {
            h.spawn_blocking(move || drop(live));
        }
        Err(_) => drop(live),
    }
}

fn same_output(a: &Source, b: &Source) -> bool {
    matches!((a, b), (Source::Output { name: x, .. }, Source::Output { name: y, .. }) if x == y)
}

impl CastBackend {
    pub fn real() -> Arc<CastBackend> {
        Arc::new(Self::new(
            Arc::new(PaneChooser),
            Arc::new(SockGranter),
            Arc::new(WlrOutputs),
            Arc::new(ecl_portal_screencast::wlr::WlrCapturer),
            Arc::new(ecl_portal_screencast::pw::PwSinkFactory),
            screenshot::pictures_dir().or_else(|| Some(std::env::temp_dir())),
        ))
    }

    pub fn new(
        chooser: Arc<dyn Chooser>,
        granter: Arc<dyn Granter>,
        outputs: Arc<dyn Outputs>,
        capturer: Arc<dyn Capturer>,
        sinks: Arc<dyn SinkFactory>,
        shot_dir: Option<PathBuf>,
    ) -> Self {
        let (revoked_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            revoked_tx,
            revoked_rx: std::sync::Mutex::new(Some(rx)),
            chooser,
            granter,
            outputs,
            capturer,
            sinks,
            shot_dir,
            sessions: Default::default(),
            next_stream: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Session paths whose grant abyss revoked. The daemon closes each one
    /// (emitting `Closed`). Taken once.
    pub fn take_revocations(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<String>> {
        lock(&self.revoked_rx).take()
    }

    async fn list_outputs(&self) -> Vec<Source> {
        let o = self.outputs.clone();
        match tokio::task::spawn_blocking(move || o.list()).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                tracing::warn!("cannot list outputs: {e}");
                Vec::new()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Chooser round trip. The pane's answer is a choice, never an authority:
    /// every pick is matched back to an output we listed ourselves.
    async fn choose(
        &self,
        requester: &Requester,
        outs: &[Source],
        multiple: bool,
    ) -> Result<Vec<Source>, Reply> {
        let req = PaneRequest::PickSource {
            requester: requester.clone(),
            sources: outs.to_vec(),
            allow_multiple: multiple,
            allow_audio: false,
            allow_window: false,
            allow_monitor: true,
            remembered: None,
        };
        let reply = self.chooser.pick(req).await.ok_or_else(Reply::other)?;
        let PaneReply::Sources { chosen, .. } = reply else {
            return Err(match reply {
                PaneReply::Cancelled => Reply::cancelled(),
                _ => Reply::other(),
            });
        };
        let mut picked: Vec<Source> = Vec::new();
        for c in &chosen {
            let Some(real) = outs.iter().find(|o| same_output(o, c)) else {
                return Err(Reply::other());
            };
            if !picked.iter().any(|p| same_output(p, real)) {
                picked.push(real.clone());
            }
        }
        if picked.is_empty() {
            return Err(Reply::cancelled());
        }
        if picked.len() > 1 && !multiple {
            return Err(Reply::other());
        }
        Ok(picked)
    }

    async fn confirm(&self, requester: &Requester, target: &Source) -> Result<Held, Reply> {
        let req = CaptureConfirm {
            app_id_claim: requester.app_id.clone(),
            stream: self
                .next_stream
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            target: target.clone(),
            audio: false,
            persistent: false,
        };
        match self.granter.grant(req).await {
            Ok(g) => Ok(g),
            Err(e) => {
                tracing::info!("capture not granted: {e}");
                Err(Reply::other())
            }
        }
    }

    async fn create_impl(&self, call: Call) -> Reply {
        let mut map = lock(&self.sessions);
        if map.contains_key(&call.session) {
            return Reply::other();
        }
        map.insert(
            call.session,
            Live {
                session: CastSession::new(Capabilities::ABYSS_TODAY),
                active: Vec::new(),
            },
        );
        Reply::ok(Results::new())
    }

    async fn select_impl(&self, call: Call) -> Reply {
        let o = &call.options;
        let sel = SelectOptions {
            types: o.u32("types"),
            multiple: o.bool("multiple"),
            cursor_mode: o.u32("cursor_mode"),
            persist_mode: o.u32("persist_mode"),
            restore_token: o.str("restore_token").map(str::to_owned),
        };
        let mut map = lock(&self.sessions);
        let Some(live) = map.get_mut(&call.session) else {
            return Reply::other();
        };
        match live.session.select_sources(sel) {
            Ok(_) => Reply::ok(Results::new()),
            Err(e) => {
                tracing::info!("SelectSources refused: {e}");
                Reply::other()
            }
        }
    }

    async fn start_impl(&self, call: Call) -> Reply {
        let sel = {
            let mut map = lock(&self.sessions);
            let Some(live) = map.get_mut(&call.session) else {
                return Reply::other();
            };
            match live.session.begin_start() {
                Ok(s) => s.clone(),
                Err(e) => {
                    tracing::info!("Start refused: {e}");
                    return Reply::other();
                }
            }
        };
        let _guard = StartGuard {
            be: self,
            path: &call.session,
        };
        let outs = self.list_outputs().await;
        if outs.is_empty() {
            return Reply::other();
        }
        let picked = match self.choose(&call.requester, &outs, sel.multiple).await {
            Ok(p) => p,
            Err(r) => return r,
        };
        let mut streams = Vec::new();
        let mut active = Vec::new();
        let mut watchers = Vec::new();
        for target in picked {
            let Held {
                id: grant_id,
                hold,
                revoked,
            } = match self.confirm(&call.requester, &target).await {
                Ok(g) => g,
                Err(r) => return r, // earlier `active` entries drop: all-or-nothing
            };
            watchers.push(revoked);
            let (cap, sinks, src) = (self.capturer.clone(), self.sinks.clone(), target.clone());
            let cursor = sel.cursor;
            let started =
                tokio::task::spawn_blocking(move || RunningStream::start(cap, sinks, src, cursor))
                    .await;
            let stream = match started {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::warn!("capture did not start: {e}");
                    return Reply::other();
                }
                Err(_) => return Reply::other(),
            };
            let Source::Output { width, height, .. } = &target else {
                return Reply::other();
            };
            streams.push(Stream {
                node_id: stream.node_id(),
                source: target.clone(),
                grant: grant_id,
                position: None,
                size: (*width, *height),
            });
            active.push(Active {
                _stream: stream,
                _hold: hold,
            });
        }
        let mut map = lock(&self.sessions);
        let Some(live) = map.get_mut(&call.session) else {
            return Reply::other(); // closed while choosing
        };
        // Persistence is not offered (abyss re-asks every time, ADR 0097).
        let started = match live.session.finish_start(streams, String::new) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("finish_start: {e}");
                return Reply::other();
            }
        };
        live.active = active;
        for w in watchers {
            let (tx, path) = (self.revoked_tx.clone(), call.session.clone());
            tokio::spawn(async move {
                if w.await.is_ok() {
                    let _ = tx.send(path);
                }
            });
        }
        let list: Vec<Val> = started
            .streams
            .iter()
            .map(|s| {
                Val::Struct(vec![
                    Val::U32(s.node_id),
                    Val::Dict(vec![
                        (
                            "id".into(),
                            Val::Str(random_token().unwrap_or_else(|_| s.node_id.to_string())),
                        ),
                        ("source_type".into(), Val::U32(s.source_type().bit())),
                        (
                            "size".into(),
                            Val::Struct(vec![Val::I32(s.size.0 as i32), Val::I32(s.size.1 as i32)]),
                        ),
                    ]),
                ])
            })
            .collect();
        Reply::ok(
            Results::new()
                .with("streams", Val::List(list))
                .with("persist_mode", Val::U32(0)),
        )
    }

    async fn screenshot_impl(&self, call: Call) -> Reply {
        let outs = self.list_outputs().await;
        let target = match outs.len() {
            0 => return Reply::other(),
            1 => outs[0].clone(),
            _ => match self.choose(&call.requester, &outs, false).await {
                Ok(mut p) => p.remove(0),
                Err(r) => return r,
            },
        };
        // Same confirm as a share (ADR 0097 has no Screenshot exemption).
        let Held { hold, .. } = match self.confirm(&call.requester, &target).await {
            Ok(g) => g,
            Err(r) => return r,
        };
        let (cap, dir) = (self.capturer.clone(), self.shot_dir.clone());
        let done = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let frame = screenshot::capture_once(
                &*cap,
                &target,
                CursorMode::Hidden,
                Duration::from_secs(3),
            )
            .map_err(|e| e.to_string())?;
            let dir = dir.ok_or("no directory for screenshots")?;
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let path = screenshot::save_png(&frame, &dir, secs).map_err(|e| e.to_string())?;
            Ok(screenshot::file_uri(&path))
        })
        .await;
        drop(hold);
        match done {
            Ok(Ok(uri)) => Reply::ok(Results::new().with_str("uri", uri)),
            Ok(Err(e)) => {
                tracing::warn!("screenshot failed: {e}");
                Reply::other()
            }
            Err(_) => Reply::other(),
        }
    }
}

impl ScreenCastBackend for CastBackend {
    fn available_source_types(&self) -> u32 {
        SOURCE_MONITOR
    }

    fn available_cursor_modes(&self) -> u32 {
        Capabilities::ABYSS_TODAY.cursor_modes
    }

    fn create_session(&self, call: Call) -> BoxFuture<'_, Reply> {
        Box::pin(self.create_impl(call))
    }

    fn select_sources(&self, call: Call) -> BoxFuture<'_, Reply> {
        Box::pin(self.select_impl(call))
    }

    fn start(&self, call: Call) -> BoxFuture<'_, Reply> {
        Box::pin(self.start_impl(call))
    }

    fn open_pipewire_remote(
        &self,
        session: &str,
    ) -> BoxFuture<'_, Result<OwnedFd, ScreenCastError>> {
        let started = lock(&self.sessions)
            .get(session)
            .is_some_and(|l| l.session.open_remote_nodes().is_ok());
        Box::pin(async move {
            if !started {
                return Err(ScreenCastError::NoSession);
            }
            remote::open_remote().map_err(|e| ScreenCastError::Unavailable(e.to_string()))
        })
    }

    fn close_session(&self, session: &str) -> BoxFuture<'_, ()> {
        if let Some(mut live) = lock(&self.sessions).remove(session) {
            live.session.close();
            release(live);
        }
        Box::pin(async {})
    }

    fn screenshot(&self, call: Call) -> BoxFuture<'_, Reply> {
        Box::pin(self.screenshot_impl(call))
    }

    fn pick_color(&self, _: Call) -> BoxFuture<'_, Reply> {
        // Needs a pixel-pick path through abyss; not offered.
        Box::pin(async { Reply::other() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecl_portal_core::identity::host;
    use ecl_portal_core::options::Options;
    use ecl_portal_core::request::Response;
    use ecl_portal_screencast::pipeline::FrameSink;
    use ecl_portal_screencast::source::mock::MockCapturer;
    use ecl_portal_screencast::source::Frame;
    use ecl_portal_screencast::stream::{MadeSink, SinkConfig};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn out(n: &str) -> Source {
        Source::Output {
            name: n.into(),
            width: 4,
            height: 3,
        }
    }

    struct Outs(Vec<Source>);
    impl Outputs for Outs {
        fn list(&self) -> Result<Vec<Source>, String> {
            Ok(self.0.clone())
        }
    }

    struct Pick(Mutex<Option<PaneReply>>);
    impl Chooser for Pick {
        fn pick(&self, _: PaneRequest) -> BoxFuture<'_, Option<PaneReply>> {
            let r = self.0.lock().unwrap().clone();
            Box::pin(async move { r })
        }
    }
    /// Never answers: a request closed mid-chooser.
    struct Hang;
    impl Chooser for Hang {
        fn pick(&self, _: PaneRequest) -> BoxFuture<'_, Option<PaneReply>> {
            Box::pin(std::future::pending())
        }
    }

    type Revokers = Arc<Mutex<Vec<tokio::sync::oneshot::Sender<()>>>>;

    /// Counts live holds so tests can see the grant being released.
    struct Live_(Arc<AtomicUsize>);
    impl Drop for Live_ {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    struct Grants {
        answer: Result<u64, GrantError>,
        revoke: Revokers,
        live: Arc<AtomicUsize>,
        asked: Arc<Mutex<Vec<CaptureConfirm>>>,
    }
    impl Granter for Grants {
        fn grant(&self, req: CaptureConfirm) -> BoxFuture<'_, Result<Held, GrantError>> {
            self.asked.lock().unwrap().push(req);
            let a = self.answer.clone();
            let live = self.live.clone();
            let revoke = self.revoke.clone();
            Box::pin(async move {
                let id = a?;
                live.fetch_add(1, Ordering::SeqCst);
                let (tx, revoked) = tokio::sync::oneshot::channel();
                revoke.lock().unwrap().push(tx);
                Ok(Held {
                    id,
                    hold: Box::new(Live_(live)),
                    revoked,
                })
            })
        }
    }

    struct Sink;
    impl FrameSink for Sink {
        fn push(&mut self, _: Frame) -> Result<(), String> {
            Ok(())
        }
    }
    struct Sinks;
    impl SinkFactory for Sinks {
        fn make(&self, _: SinkConfig) -> Result<MadeSink, String> {
            Ok(MadeSink {
                sink: Box::new(Sink),
                node_id: 99,
            })
        }
    }
    struct NoSinks;
    impl SinkFactory for NoSinks {
        fn make(&self, _: SinkConfig) -> Result<MadeSink, String> {
            Err("pipewire down".into())
        }
    }

    struct H {
        be: CastBackend,
        revoke: Revokers,
        live: Arc<AtomicUsize>,
        asked: Arc<Mutex<Vec<CaptureConfirm>>>,
    }

    fn harness(
        chooser: Arc<dyn Chooser>,
        answer: Result<u64, GrantError>,
        sinks: Arc<dyn SinkFactory>,
        outs: Vec<Source>,
        dir: Option<PathBuf>,
    ) -> H {
        let live = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let revoke: Revokers = Arc::new(Mutex::new(Vec::new()));
        let be = CastBackend::new(
            chooser,
            Arc::new(Grants {
                answer,
                revoke: revoke.clone(),
                live: live.clone(),
                asked: asked.clone(),
            }),
            Arc::new(Outs(outs)),
            Arc::new(MockCapturer { frames: 5 }),
            sinks,
            dir,
        );
        H {
            be,
            revoke,
            live,
            asked,
        }
    }

    fn chosen(n: &str) -> Arc<dyn Chooser> {
        Arc::new(Pick(Mutex::new(Some(PaneReply::Sources {
            chosen: vec![out(n)],
            audio: false,
            remember: false,
        }))))
    }

    fn call(session: &str) -> Call {
        Call {
            requester: host(),
            session: session.into(),
            parent_window: String::new(),
            options: Options::new(),
        }
    }

    async fn ready(h: &H) {
        assert_eq!(
            h.be.create_session(call("/s")).await.response,
            Response::Success
        );
        assert_eq!(
            h.be.select_sources(call("/s")).await.response,
            Response::Success
        );
    }

    #[tokio::test]
    async fn happy_path_publishes_node_and_close_revokes() {
        let h = harness(
            chosen("DP-1"),
            Ok(7),
            Arc::new(Sinks),
            vec![out("DP-1")],
            None,
        );
        ready(&h).await;
        let r = h.be.start(call("/s")).await;
        assert_eq!(r.response, Response::Success);
        let Some(Val::List(streams)) = r.results.get("streams") else {
            panic!("no streams");
        };
        assert_eq!(streams.len(), 1);
        let Val::Struct(f) = &streams[0] else {
            panic!()
        };
        assert_eq!(f[0], Val::U32(99));
        assert_eq!(
            h.live.load(Ordering::SeqCst),
            1,
            "grant held while streaming"
        );
        {
            let asked = h.asked.lock().unwrap();
            assert_eq!(asked.len(), 1);
            assert_eq!(asked[0].target, out("DP-1"));
            assert!(!asked[0].persistent);
        }
        h.be.close_session("/s").await;
        for _ in 0..50 {
            if h.live.load(Ordering::SeqCst) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(h.live.load(Ordering::SeqCst), 0, "close drops the grant");
        // Closed session: nothing left.
        assert!(h.be.open_pipewire_remote("/s").await.is_err());
    }

    #[tokio::test]
    async fn abyss_closing_portal_sock_reports_the_session_and_only_then() {
        let h = harness(
            chosen("DP-1"),
            Ok(7),
            Arc::new(Sinks),
            vec![out("DP-1")],
            None,
        );
        let mut rx = h.be.take_revocations().unwrap();
        assert!(h.be.take_revocations().is_none());
        ready(&h).await;
        assert_eq!(h.be.start(call("/s")).await.response, Response::Success);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err(), "healthy grant reports nothing");
        // Abyss revokes: the daemon is told which session to close.
        h.revoke.lock().unwrap().pop().unwrap().send(()).unwrap();
        let path = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(path, "/s");
        // The daemon then closes it; the grant is released.
        h.be.close_session(&path).await;
        for _ in 0..50 {
            if h.live.load(Ordering::SeqCst) == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(h.live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn closing_ourselves_is_not_a_revocation() {
        let h = harness(
            chosen("DP-1"),
            Ok(7),
            Arc::new(Sinks),
            vec![out("DP-1")],
            None,
        );
        let mut rx = h.be.take_revocations().unwrap();
        ready(&h).await;
        assert_eq!(h.be.start(call("/s")).await.response, Response::Success);
        h.be.close_session("/s").await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn denied_and_timeout_are_response_two_and_hold_nothing() {
        for e in [
            GrantError::Denied,
            GrantError::Timeout,
            GrantError::Unavailable("x".into()),
        ] {
            let h = harness(
                chosen("DP-1"),
                Err(e),
                Arc::new(Sinks),
                vec![out("DP-1")],
                None,
            );
            ready(&h).await;
            assert_eq!(h.be.start(call("/s")).await.response, Response::Other);
            assert_eq!(h.live.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn cancelled_chooser_is_response_one_and_never_asks_abyss() {
        let ch: Arc<dyn Chooser> = Arc::new(Pick(Mutex::new(Some(PaneReply::Cancelled))));
        let h = harness(ch, Ok(1), Arc::new(Sinks), vec![out("DP-1")], None);
        ready(&h).await;
        assert_eq!(h.be.start(call("/s")).await.response, Response::Cancelled);
        assert!(h.asked.lock().unwrap().is_empty());
        // The app may try again.
        assert_eq!(h.be.start(call("/s")).await.response, Response::Cancelled);
    }

    #[tokio::test]
    async fn pane_failure_and_bogus_picks_are_denials() {
        let none: Arc<dyn Chooser> = Arc::new(Pick(Mutex::new(None)));
        let h = harness(none, Ok(1), Arc::new(Sinks), vec![out("DP-1")], None);
        ready(&h).await;
        assert_eq!(h.be.start(call("/s")).await.response, Response::Other);
        // A pane naming an output the daemon never listed gets nothing.
        let h = harness(
            chosen("HDMI-9"),
            Ok(1),
            Arc::new(Sinks),
            vec![out("DP-1")],
            None,
        );
        ready(&h).await;
        assert_eq!(h.be.start(call("/s")).await.response, Response::Other);
        assert!(h.asked.lock().unwrap().is_empty());
        // A window pick cannot be shared.
        let win = Arc::new(Pick(Mutex::new(Some(PaneReply::Sources {
            chosen: vec![Source::Window {
                id: 1,
                title: String::new(),
                app_id: String::new(),
            }],
            audio: false,
            remember: false,
        }))));
        let h = harness(win, Ok(1), Arc::new(Sinks), vec![out("DP-1")], None);
        ready(&h).await;
        assert_eq!(h.be.start(call("/s")).await.response, Response::Other);
        assert!(h.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn no_outputs_is_a_denial() {
        let h = harness(chosen("DP-1"), Ok(1), Arc::new(Sinks), vec![], None);
        ready(&h).await;
        assert_eq!(h.be.start(call("/s")).await.response, Response::Other);
    }

    #[tokio::test]
    async fn capture_failure_releases_the_grant() {
        let h = harness(
            chosen("DP-1"),
            Ok(7),
            Arc::new(NoSinks),
            vec![out("DP-1")],
            None,
        );
        ready(&h).await;
        assert_eq!(h.be.start(call("/s")).await.response, Response::Other);
        assert_eq!(h.live.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn dropping_a_pending_start_allows_a_retry() {
        let h = harness(
            Arc::new(Hang),
            Ok(1),
            Arc::new(Sinks),
            vec![out("DP-1")],
            None,
        );
        ready(&h).await;
        let t = tokio::time::timeout(Duration::from_millis(100), h.be.start(call("/s"))).await;
        assert!(t.is_err(), "chooser never answered");
        // Not stuck in StartInFlight: a second Start reaches the chooser again.
        let t = tokio::time::timeout(Duration::from_millis(100), h.be.start(call("/s"))).await;
        assert!(t.is_err());
    }

    #[tokio::test]
    async fn out_of_order_calls_are_denied() {
        let h = harness(
            chosen("DP-1"),
            Ok(7),
            Arc::new(Sinks),
            vec![out("DP-1")],
            None,
        );
        // Start before CreateSession / SelectSources.
        assert_eq!(h.be.start(call("/s")).await.response, Response::Other);
        assert_eq!(
            h.be.create_session(call("/s")).await.response,
            Response::Success
        );
        assert_eq!(h.be.start(call("/s")).await.response, Response::Other);
        // Double CreateSession on one path.
        assert_eq!(
            h.be.create_session(call("/s")).await.response,
            Response::Other
        );
        assert!(h.asked.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn screenshot_confirms_then_writes_a_png() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(
            chosen("DP-1"),
            Ok(3),
            Arc::new(Sinks),
            vec![out("DP-1")],
            Some(d.path().to_owned()),
        );
        let r = h.be.screenshot(call("")).await;
        assert_eq!(r.response, Response::Success);
        let uri = r
            .results
            .get("uri")
            .and_then(Val::as_str)
            .unwrap()
            .to_owned();
        assert!(uri.starts_with("file://") && uri.ends_with(".png"), "{uri}");
        assert_eq!(
            h.asked.lock().unwrap().len(),
            1,
            "screenshot is confirmed too"
        );
        assert_eq!(
            h.live.load(Ordering::SeqCst),
            0,
            "grant released after the frame"
        );
    }

    #[tokio::test]
    async fn screenshot_denied_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let h = harness(
            chosen("DP-1"),
            Err(GrantError::Denied),
            Arc::new(Sinks),
            vec![out("DP-1")],
            Some(d.path().to_owned()),
        );
        assert_eq!(h.be.screenshot(call("")).await.response, Response::Other);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }
}
