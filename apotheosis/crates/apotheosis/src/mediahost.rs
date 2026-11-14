// SPDX-License-Identifier: AGPL-3.0-only
//! The native media protocol's host side (T-16 §2-§5, ADR 0118). A term turns media APCs
//! into [`MediaRequest`]s; the thread that owns the term (a pane's io thread, a media
//! window's feeder) hands them to its [`MediaHost`], which opens the source, applies the
//! audio policy and runs one [`Player`] (and so one sandboxed helper) per placement. Player
//! events come back through a channel and a poke, and the owner feeds them to the term.
//!
//! Sources: `s=f,t=<token>` claims a descriptor `apoth play` attached over the control
//! socket (`media.attach`); `s=p` opens a path only beneath a `media.allow-paths` directory,
//! without following symlinks; `s=d` copies the transferred bytes into a memfd. The helper
//! never opens anything itself. Content is never logged: keys, codes and counts only.

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, Instant};

use apoth_config::Config;
use apoth_media::player::{Control, Event, Opts, Player, SinkKind};
use apoth_term::{MediaCtl, MediaEvent, MediaRequest, MediaSource};

/// Players in the whole process (T-16 §5: per-pane 2 is the term's, this is the total).
const MAX_PLAYERS: usize = 4;
/// Largest source frame decoded.
const MAX_W: u32 = 3840;
const MAX_H: u32 = 2160;
/// Unclaimed `media.attach` descriptors held at once, and how long each waits.
const MAX_TOKENS: usize = 16;
const TOKEN_TTL: Duration = Duration::from_secs(10);

/// `media.audio-output`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioOut {
    Auto,
    Off,
}

/// `media.autoplay-audio`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Autoplay {
    Focused,
    Never,
    Always,
}

/// The `media.*` and `graphics.sandbox-decode` keys the host reads (live).
#[derive(Clone, Debug)]
pub(crate) struct MediaSettings {
    pub(crate) allow_paths: Vec<std::path::PathBuf>,
    pub(crate) av1_software: bool,
    pub(crate) audio: AudioOut,
    pub(crate) autoplay: Autoplay,
    pub(crate) sandbox_decode: bool,
}

impl Default for MediaSettings {
    fn default() -> Self {
        MediaSettings {
            allow_paths: Vec::new(),
            av1_software: true,
            audio: AudioOut::Auto,
            autoplay: Autoplay::Focused,
            sandbox_decode: true,
        }
    }
}

impl MediaSettings {
    pub(crate) fn from_config(c: &Config) -> MediaSettings {
        MediaSettings {
            allow_paths: c
                .list("media.allow-paths")
                .iter()
                .map(std::path::PathBuf::from)
                .filter(|p| p.is_absolute())
                .collect(),
            av1_software: c.bool("media.av1-software"),
            // `pulse` and `auto` both mean the PulseAudio-protocol server (pipewire-pulse).
            audio: if c.str("media.audio-output") == Some("off") {
                AudioOut::Off
            } else {
                AudioOut::Auto
            },
            autoplay: match c.str("media.autoplay-audio") {
                Some("never") => Autoplay::Never,
                Some("always") => Autoplay::Always,
                _ => Autoplay::Focused,
            },
            sandbox_decode: c.bool("graphics.sandbox-decode"),
        }
    }
}

static SETTINGS: RwLock<Option<MediaSettings>> = RwLock::new(None);

/// Install the settings in force (every load and reload).
pub(crate) fn configure(c: &Config) {
    *SETTINGS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(MediaSettings::from_config(c));
}

pub(crate) fn settings() -> MediaSettings {
    SETTINGS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .unwrap_or_default()
}

/// Descriptors attached with `media.attach`, by token: single use, expiring.
static TOKENS: Mutex<Vec<(u64, OwnedFd, Instant)>> = Mutex::new(Vec::new());

fn random_token() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    static N: AtomicUsize = AtomicUsize::new(0);
    loop {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(N.fetch_add(1, Ordering::Relaxed));
        h.write_u128(Instant::now().elapsed().as_nanos());
        let t = h.finish();
        if t != 0 {
            return t;
        }
    }
}

/// Hold `fd` for a media APC; `None` when too many are waiting.
pub(crate) fn attach(fd: OwnedFd) -> Option<u64> {
    let mut t = TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    t.retain(|(_, _, at)| now.duration_since(*at) < TOKEN_TTL);
    if t.len() >= MAX_TOKENS {
        return None;
    }
    let tok = random_token();
    t.push((tok, fd, now));
    Some(tok)
}

fn claim(token: u64) -> Option<OwnedFd> {
    let mut t = TOKENS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    t.retain(|(_, _, at)| now.duration_since(*at) < TOKEN_TTL);
    let i = t.iter().position(|(k, _, _)| *k == token)?;
    Some(t.swap_remove(i).1)
}

/// `s=p`: beneath an allowed directory, no symlink anywhere, a regular file.
fn open_path(path: &[u8], allow: &[std::path::PathBuf]) -> Result<OwnedFd, &'static str> {
    use rustix::fs::{FileType, Mode, OFlags, ResolveFlags};
    use std::os::unix::ffi::OsStrExt;
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(path));
    if !path.is_absolute() {
        return Err("path-refused");
    }
    for dir in allow {
        let Ok(rel) = path.strip_prefix(dir) else {
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let Ok(d) = rustix::fs::open(
            dir,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ) else {
            continue;
        };
        let fd = rustix::fs::openat2(
            &d,
            rel,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|_| "path-refused")?;
        let st = rustix::fs::fstat(&fd).map_err(|_| "path-refused")?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
            return Err("path-refused");
        }
        return Ok(fd);
    }
    Err("path-refused")
}

/// `s=d`: the bytes in a sealed-size memfd at offset 0.
fn memfd(data: &[u8]) -> Result<OwnedFd, &'static str> {
    use rustix::fs::MemfdFlags;
    let fd =
        rustix::fs::memfd_create("apoth-media", MemfdFlags::CLOEXEC).map_err(|_| "resources")?;
    let mut off = 0;
    while off < data.len() {
        match rustix::io::write(&fd, &data[off..]) {
            Ok(n) if n > 0 => off += n,
            Err(rustix::io::Errno::INTR) => {}
            _ => return Err("resources"),
        }
    }
    rustix::fs::seek(&fd, rustix::fs::SeekFrom::Start(0)).map_err(|_| "resources")?;
    Ok(fd)
}

/// Wakes the thread that owns the host.
pub(crate) type Poke = Arc<dyn Fn() + Send + Sync>;

/// Whether audio may be heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    /// Unmuted, or the program may unmute.
    Open,
    /// `apoth play` in an unfocused pane: opens when the pane takes focus.
    UntilFocus,
    /// No gesture: muted, and the program cannot unmute (T-16 §5).
    Closed,
}

static PLAYERS: AtomicUsize = AtomicUsize::new(0);

struct Slot {
    player: Option<Player>,
    gate: Gate,
    /// The program asked for mute (kept when a gate opens).
    want_muted: bool,
}

impl Drop for Slot {
    fn drop(&mut self) {
        PLAYERS.fetch_sub(1, Ordering::AcqRel);
        if let Some(p) = self.player.take() {
            // Joining the player kills and reaps its helper: never on the owner's thread.
            if std::thread::Builder::new()
                .name("apoth-media-stop".into())
                .spawn(move || drop(p))
                .is_err()
            {
                tracing::warn!("cannot start a media stop thread");
            }
        }
    }
}

/// One term's players.
pub(crate) struct MediaHost {
    slots: HashMap<u64, Slot>,
    tx: mpsc::Sender<(u64, MediaEvent)>,
    rx: mpsc::Receiver<(u64, MediaEvent)>,
    poke: Poke,
    /// A poke is outstanding (cleared by `drain`), so a burst of events pokes once.
    poked: Arc<AtomicBool>,
    /// Test seam: the helper and sink.
    test: Option<(Opts, SinkKind)>,
}

impl MediaHost {
    pub(crate) fn new(poke: Poke) -> MediaHost {
        let (tx, rx) = mpsc::channel();
        MediaHost {
            slots: HashMap::new(),
            tx,
            rx,
            poke,
            poked: Arc::new(AtomicBool::new(false)),
            test: None,
        }
    }

    #[cfg(test)]
    fn with_test(mut self, opts: Opts) -> MediaHost {
        let sink = opts.sink.clone();
        self.test = Some((opts, sink));
        self
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn send(&self, key: u64, ev: MediaEvent) {
        send(&self.tx, &self.poked, &self.poke, key, ev);
    }

    /// Act on the term's requests. Outcomes (including immediate errors) arrive through
    /// [`MediaHost::drain`].
    pub(crate) fn requests(&mut self, reqs: Vec<MediaRequest>, focused: bool) {
        for r in reqs {
            match r {
                MediaRequest::Open {
                    key,
                    source,
                    box_px,
                    play,
                    looping,
                    volume,
                    muted,
                    session: _,
                } => {
                    if let Err(code) =
                        self.open(key, source, box_px, play, looping, volume, muted, focused)
                    {
                        self.send(key, MediaEvent::Error(code));
                    }
                }
                MediaRequest::Control { key, ctl } => {
                    let Some(s) = self.slots.get_mut(&key) else {
                        continue;
                    };
                    let c = match ctl {
                        MediaCtl::Play => Control::Play,
                        MediaCtl::Pause => Control::Pause,
                        MediaCtl::Seek(ms) => Control::Seek(ms),
                        MediaCtl::Loop(l) => Control::Loop(l),
                        MediaCtl::Volume(v) => Control::Volume(v),
                        MediaCtl::Mute(m) => {
                            s.want_muted = m;
                            if !m && s.gate != Gate::Open {
                                continue;
                            }
                            Control::Mute(m)
                        }
                    };
                    if let Some(p) = &s.player {
                        p.control(c);
                    }
                }
                MediaRequest::Close { key } => {
                    self.slots.remove(&key);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn open(
        &mut self,
        key: u64,
        source: MediaSource,
        box_px: (u32, u32),
        play: bool,
        looping: bool,
        volume: f32,
        muted: bool,
        focused: bool,
    ) -> Result<(), &'static str> {
        let set = settings();
        if !set.av1_software {
            // Software AV1 is the only video decoder until hardware decode lands (M36).
            return Err("unsupported");
        }
        let explicit = matches!(source, MediaSource::Token(_));
        let fd = match source {
            MediaSource::Token(t) => claim(t).ok_or("bad-token")?,
            MediaSource::Path(p) => open_path(&p, &set.allow_paths)?,
            MediaSource::Data(d) => memfd(&d)?,
        };
        if PLAYERS.fetch_add(1, Ordering::AcqRel) >= MAX_PLAYERS {
            PLAYERS.fetch_sub(1, Ordering::AcqRel);
            return Err("quota");
        }
        // T-16 §5: audio starts only for an explicit `apoth play` in the focused pane (or
        // `autoplay-audio always`); otherwise it plays muted until a gesture.
        let (sink, gate) = match (set.audio, set.autoplay, explicit, focused) {
            (AudioOut::Off, ..) => (SinkKind::Off, Gate::Closed),
            (_, Autoplay::Always, ..) | (_, _, true, true) => (SinkKind::Pulse, Gate::Open),
            (_, _, true, false) => (SinkKind::Pulse, Gate::UntilFocus),
            (_, Autoplay::Never, false, _) => (SinkKind::Off, Gate::Closed),
            (_, Autoplay::Focused, false, _) => (SinkKind::Pulse, Gate::Closed),
        };
        let mut opts = Opts {
            box_w: box_px.0.max(1),
            box_h: box_px.1.max(1),
            max_w: MAX_W,
            max_h: MAX_H,
            sink,
            volume: volume.clamp(0.0, 1.0),
            muted: muted || gate != Gate::Open,
            looping,
            play,
            helper: None,
            test_args: Vec::new(),
        };
        if let Some((t, sink)) = &self.test {
            opts.helper.clone_from(&t.helper);
            opts.test_args.clone_from(&t.test_args);
            if !matches!(opts.sink, SinkKind::Off) {
                opts.sink = sink.clone();
            }
        }
        let (tx, poked, poke) = (
            self.tx.clone(),
            Arc::clone(&self.poked),
            Arc::clone(&self.poke),
        );
        let player = Player::spawn(
            fd,
            opts,
            Box::new(move |ev| {
                let ev = match ev {
                    Event::Ready {
                        width,
                        height,
                        duration_ms,
                        audio: _,
                        video_w,
                        video_h,
                        dar,
                    } => MediaEvent::Ready {
                        width,
                        height,
                        duration_ms,
                        video_w,
                        video_h,
                        dar,
                    },
                    Event::AudioUnavailable => MediaEvent::AudioUnavailable,
                    Event::Frame(f) => MediaEvent::Frame(f),
                    Event::Position(s) => MediaEvent::Position(s),
                    Event::Ended => MediaEvent::Ended,
                    Event::Error(c) => MediaEvent::Error(c),
                };
                send(&tx, &poked, &poke, key, ev);
            }),
        );
        self.slots.insert(
            key,
            Slot {
                player: Some(player),
                gate,
                want_muted: muted,
            },
        );
        Ok(())
    }

    /// Events since the last drain, oldest first; of a placement's frames only the newest.
    pub(crate) fn drain(&mut self, focused: bool) -> Vec<(u64, MediaEvent)> {
        self.poked.store(false, Ordering::Release);
        if focused {
            for s in self.slots.values_mut() {
                if s.gate == Gate::UntilFocus {
                    s.gate = Gate::Open;
                    if !s.want_muted
                        && let Some(p) = &s.player
                    {
                        p.control(Control::Mute(false));
                    }
                }
            }
        }
        let mut out: Vec<(u64, MediaEvent)> = self.rx.try_iter().collect();
        let mut seen = std::collections::HashSet::new();
        for i in (0..out.len()).rev() {
            if matches!(out[i].1, MediaEvent::Frame(_)) && !seen.insert(out[i].0) {
                out.remove(i);
            }
        }
        for (k, e) in &out {
            if matches!(e, MediaEvent::Error(_)) {
                self.slots.remove(k);
            }
        }
        out
    }
}

fn send(
    tx: &mpsc::Sender<(u64, MediaEvent)>,
    poked: &AtomicBool,
    poke: &Poke,
    key: u64,
    ev: MediaEvent,
) {
    if tx.send((key, ev)).is_ok() && !poked.swap(true, Ordering::AcqRel) {
        poke();
    }
}

/// A poke that writes `byte` to a wake pipe.
pub(crate) fn pipe_poke(fd: Arc<OwnedFd>, byte: u8) -> Poke {
    Arc::new(move || {
        let _ = rustix::io::write(fd.as_fd(), &[byte]);
    })
}

/// The image decoder for a pane's decode jobs (`graphics.sandbox-decode`).
pub(crate) fn image_decoder() -> Box<dyn apoth_term::apoth_image::decoder::PixelDecoder + Send> {
    if settings().sandbox_decode {
        Box::new(apoth_media::image::Sandboxed::new())
    } else {
        Box::new(apoth_term::apoth_image::decoder::InProcess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_single_use() {
        let (a, _b) = rustix::pipe::pipe().unwrap();
        let t = attach(a).unwrap();
        assert!(claim(t).is_some());
        assert!(claim(t).is_none());
        assert!(claim(t ^ 1).is_none());
    }

    #[test]
    fn paths_stay_beneath_allowed_dirs_without_symlinks() {
        let dir = std::env::temp_dir().join(format!("apoth-mh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ok")).unwrap();
        std::fs::write(dir.join("ok/a.webm"), b"x").unwrap();
        std::fs::write(dir.join("outside"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("outside"), dir.join("ok/link")).unwrap();
        let allow = vec![dir.join("ok")];
        let p = |s: &str| dir.join(s).into_os_string().into_encoded_bytes();
        assert!(open_path(&p("ok/a.webm"), &allow).is_ok());
        assert_eq!(open_path(&p("ok/link"), &allow).err(), Some("path-refused"));
        assert_eq!(open_path(&p("outside"), &allow).err(), Some("path-refused"));
        assert_eq!(
            open_path(&p("ok/../outside"), &allow).err(),
            Some("path-refused")
        );
        assert_eq!(open_path(&p("ok"), &allow).err(), Some("path-refused"));
        assert_eq!(open_path(b"rel/a.webm", &allow).err(), Some("path-refused"));
        assert_eq!(open_path(&p("ok/a.webm"), &[]).err(), Some("path-refused"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn data_lands_in_a_memfd_at_offset_zero() {
        let fd = memfd(b"hello").unwrap();
        let mut b = [0u8; 8];
        assert_eq!(rustix::io::read(&fd, &mut b).unwrap(), 5);
        assert_eq!(&b[..5], b"hello");
    }

    #[test]
    fn a_bad_token_is_an_error_and_frames_coalesce() {
        let pokes = Arc::new(AtomicUsize::new(0));
        let p2 = Arc::clone(&pokes);
        let mut h = MediaHost::new(Arc::new(move || {
            p2.fetch_add(1, Ordering::Relaxed);
        }))
        .with_test(Opts::default());
        h.requests(
            vec![MediaRequest::Open {
                key: 7,
                source: MediaSource::Token(12345),
                box_px: (64, 64),
                play: true,
                looping: false,
                volume: 1.0,
                muted: false,
                session: 0,
            }],
            true,
        );
        let img = Arc::new(apoth_term::apoth_image::Image {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
        });
        h.send(3, MediaEvent::Frame(Arc::clone(&img)));
        h.send(3, MediaEvent::Position(1));
        h.send(3, MediaEvent::Frame(img));
        assert_eq!(pokes.load(Ordering::Relaxed), 1);
        let ev = h.drain(false);
        assert!(matches!(ev[0], (7, MediaEvent::Error("bad-token"))));
        assert!(matches!(ev[1], (3, MediaEvent::Position(1))));
        assert!(matches!(ev[2], (3, MediaEvent::Frame(_))));
        assert_eq!(ev.len(), 3);
        assert!(h.is_empty());
    }
}
