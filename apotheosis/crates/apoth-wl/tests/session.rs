// SPDX-License-Identifier: AGPL-3.0-only
//! Session-level test (T-09 §8): `apoth-wl` against a real compositor, `ec-abyss --backend
//! headless` in a private runtime dir (never a visible window, never the live session).
//!
//! The parent test starts the compositor and captures its pid, then re-runs this test binary
//! as a child with `WAYLAND_DISPLAY` pointing at it (so no process-global env is mutated).
//! The child connects, maps a window, waits for the first configure and checks scale and the
//! capability table, then drives a resize storm. The compositor is killed by pid.
//!
//! Two entry points share the child body:
//! - `against_spawned_headless_abyss` starts `ec-abyss` itself (`$APOTH_EC_ABYSS` or `$PATH`);
//! - `against_given_display` connects to `$APOTH_TEST_WAYLAND_DISPLAY` (with
//!   `$APOTH_TEST_XDG_RUNTIME_DIR`, default `$XDG_RUNTIME_DIR`) when it is set. Never point it
//!   at a live session.
//!
//! Without a compositor they skip unless `APOTH_REQUIRE_SESSION=1` (set it in the gate job so
//! the tests fail instead of passing vacuously).

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use apoth_wl::mime::Want;
use apoth_wl::{
    BindState, InputKind, SelectionEvent, SelectionKind, TransferStatus, WindowSpec, WlConfig,
    WlEvent,
};

const CHILD_ENV: &str = "APOTH_WL_SESSION_CHILD";
const SPAWNED: &str = "against_spawned_headless_abyss";
const GIVEN: &str = "against_given_display";
const MARK: &str = "SESSION-OK";

fn find_in_path(name: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("APOTH_EC_ABYSS") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| PathBuf::from(d).join(name))
        .find(|p| p.is_file())
}

struct Compositor {
    child: Child,
    dir: PathBuf,
}

impl Drop for Compositor {
    fn drop(&mut self) {
        // By the pid we spawned; never by name.
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start_compositor(bin: &PathBuf) -> Compositor {
    // Unix socket paths are short; keep the runtime dir short too.
    let dir = std::env::temp_dir().join(format!("apw{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let child = Command::new(bin)
        .args(["--backend", "headless", "--size", "800x600"])
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ec-abyss");
    let comp = Compositor { child, dir };
    let sock = comp.dir.join("wayland-1");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sock.exists() {
        assert!(
            Instant::now() < deadline,
            "compositor socket never appeared"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    comp
}

fn require_or_skip(why: &str) {
    assert!(
        std::env::var_os("APOTH_REQUIRE_SESSION").is_none(),
        "APOTH_REQUIRE_SESSION is set but {why}"
    );
    eprintln!("skipped: {why}");
}

/// Re-run this binary as the child for `test` with the given display.
fn run_child(test: &str, runtime_dir: &std::path::Path, display: &str) {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD_ENV, "1")
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("WAYLAND_DISPLAY", display)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && text.contains(MARK),
        "child failed:\n{text}"
    );
    println!("{text}");
}

#[test]
fn against_spawned_headless_abyss() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child();
        return;
    }
    let Some(bin) = find_in_path("ec-abyss") else {
        require_or_skip("ec-abyss was not found");
        return;
    };
    let comp = start_compositor(&bin);
    run_child(SPAWNED, &comp.dir, "wayland-1");
}

#[test]
fn against_given_display() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child();
        return;
    }
    // Opt-in by design: the spawned-compositor test is the gate; this one points at a
    // compositor the caller already started (hidden, headless; never a live session).
    let Some(display) = std::env::var_os("APOTH_TEST_WAYLAND_DISPLAY") else {
        eprintln!("skipped: APOTH_TEST_WAYLAND_DISPLAY is not set");
        return;
    };
    let dir = std::env::var_os("APOTH_TEST_XDG_RUNTIME_DIR")
        .or_else(|| std::env::var_os("XDG_RUNTIME_DIR"))
        .map(PathBuf::from)
        .expect("a runtime dir for the test display");
    run_child(GIVEN, &dir, &display.to_string_lossy());
}

struct Harness {
    event_loop: calloop::EventLoop<'static, Vec<WlEvent>>,
    events: Vec<WlEvent>,
    seen: Vec<WlEvent>,
}

impl Harness {
    /// Dispatch for up to `ms`, returning when `done` accepts an event (events are kept in
    /// `seen` for later inspection).
    fn pump_until(&mut self, ms: u64, mut done: impl FnMut(&WlEvent) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < deadline {
            self.event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut self.events)
                .unwrap();
            let mut hit = false;
            for ev in self.events.drain(..) {
                if let WlEvent::Fatal(f) = &ev {
                    panic!("fatal: {f:?}");
                }
                hit |= done(&ev);
                self.seen.push(ev);
            }
            if hit {
                return true;
            }
        }
        false
    }
}

fn child() {
    let (source, handle) = apoth_wl::connect(WlConfig::default()).expect("connect");
    let event_loop: calloop::EventLoop<'static, Vec<WlEvent>> =
        calloop::EventLoop::try_new().unwrap();
    event_loop
        .handle()
        .insert_source(source, |ev, _, evs| evs.push(ev))
        .unwrap();
    let mut h = Harness {
        event_loop,
        events: Vec::new(),
        seen: Vec::new(),
    };

    // Capability detection: fatal globals bound; every optional global we know how to use is
    // bound when advertised, never left at "present".
    let info = handle.wayland_info(None);
    assert!(info.contains("xdg_wm_base") && info.contains("wl_compositor"));
    let caps = handle.capabilities();
    for fatal in ["wl_compositor", "wl_subcompositor", "wl_shm", "xdg_wm_base"] {
        assert!(caps.is_bound(fatal), "{fatal} not bound");
    }
    for (spec, state) in caps.rows() {
        if spec.requirement == apoth_wl::caps::Requirement::Optional {
            assert!(
                !matches!(state, BindState::Present { .. }),
                "{} advertised but not bound",
                spec.interface
            );
        }
    }
    println!("capabilities:\n{info}");
    let seats = handle.seats();
    assert!(!seats.is_empty());
    let seat = seats[0];

    let win = handle.create_window(WindowSpec {
        title: "apoth-wl session test".into(),
        app_id: Some("org.eclipseos.Apotheosis".into()),
        ..WindowSpec::default()
    });
    handle.flush();
    assert!(
        h.pump_until(
            10_000,
            |e| matches!(e, WlEvent::Configure(c) if c.window == win)
        ),
        "no configure within 10 s"
    );
    let scale = handle.scale(win).expect("scale");
    assert!(scale.as_f64() >= 1.0);
    let (_logical, buffer) = handle.surface_size(win).expect("surface size");
    assert!(buffer.0 > 0 && buffer.1 > 0);

    // Optional protocols are harmless to call whatever the compositor offers.
    let _ = handle.set_content_type(win, apoth_wl::ContentType::None);
    let _ = handle.set_idle_inhibit(win, true);
    let _ = handle.set_idle_inhibit(win, false);
    let _ = handle.set_toplevel_tag(win, "apoth-test", "session test");
    let _ = handle.set_icon_name(win, Some("utilities-terminal"));
    let _ = handle.ring_bell(Some(win));
    if caps.is_bound("xdg_activation_v1") {
        assert!(handle.request_activation_token(win));
        assert!(
            h.pump_until(3_000, |e| matches!(e, WlEvent::ActivationToken { .. })),
            "no activation token"
        );
    }

    // Map the window (a CPU buffer: there is no GPU surface in this test), then wait for
    // keyboard focus to get an input serial.
    handle
        .attach_solid_buffer(win, buffer, 0x0020_2020)
        .expect("attach");
    handle.flush();
    let is_enter = |e: &WlEvent| {
        matches!(
            e,
            WlEvent::Input(i) if matches!(i.kind, InputKind::KeyboardEnter { .. })
        )
    };
    // The compositor may have focused the window already (enter before our first pump).
    let focused = h.seen.iter().any(is_enter) || h.pump_until(5_000, is_enter);
    println!("keyboard focus after map: {focused}");
    for ev in &h.seen {
        if let WlEvent::Configure(c) = ev {
            println!(
                "configure: size={:?} activated={}",
                c.size, c.states.activated
            );
        }
    }

    // Clipboard round trip: own the selection, see it offered back, read it through a pipe.
    if focused {
        let payload = b"apoth-wl clipboard round trip \xe2\x82\xac".to_vec();
        handle
            .set_selection(seat, SelectionKind::Clipboard, payload.clone())
            .expect("set_selection");
        handle.flush();
        let _ = h.pump_until(2_000, |e| {
            matches!(e, WlEvent::Selection(SelectionEvent::Offered { .. }))
        });
        assert!(handle.owns_selection(seat, SelectionKind::Clipboard));
        let mimes = handle.selection_mimes(seat, SelectionKind::Clipboard);
        println!("selection mimes: {mimes:?}");
        let t = handle
            .receive_selection(seat, SelectionKind::Clipboard, Want::Text)
            .expect("receive_selection");
        let got: std::rc::Rc<std::cell::RefCell<Option<apoth_wl::TransferDone>>> =
            Default::default();
        let sink = got.clone();
        let lh = h.event_loop.handle();
        t.insert(&lh, move |d, _| *sink.borrow_mut() = Some(d))
            .map_err(|_| ())
            .expect("insert transfer");
        // The send half arrives as a `WlEvent::Send(Transfer)`; insert it as the app would.
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.borrow().is_none() && Instant::now() < deadline {
            h.event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut h.events)
                .unwrap();
            for ev in h.events.drain(..) {
                match ev {
                    WlEvent::Send(tr) => {
                        tr.insert(&lh, |_, _| {})
                            .map_err(|_| ())
                            .expect("insert send");
                    }
                    WlEvent::Fatal(f) => panic!("fatal: {f:?}"),
                    _ => {}
                }
            }
        }
        let done = got
            .borrow_mut()
            .take()
            .expect("clipboard transfer finished");
        assert_eq!(done.status, TransferStatus::Complete);
        assert_eq!(done.data.0, payload);
        println!("clipboard round trip ok ({} bytes)", done.data.0.len());
    } else {
        println!("clipboard: skipped, compositor gave no keyboard focus");
    }

    // Resize storm: 1000 geometry changes and title updates, then dispatches.
    for i in 0..1000u32 {
        handle.set_window_size(win, (400 + i % 300, 300 + i % 200));
        if i % 100 == 0 {
            handle.set_title(win, &format!("storm {i}"));
        }
    }
    handle.flush();
    h.pump_until(300, |_| false);
    assert!(handle.surface_size(win).is_some());
    handle.destroy_window(win);
    handle.flush();
    h.pump_until(100, |_| false);
    println!("{MARK}");
}
