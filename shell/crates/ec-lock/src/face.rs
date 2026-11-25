// SPDX-License-Identifier: AGPL-3.0-only
//! What the lock screen draws besides the password field: the user's tile
//! layout, the Glances' numbers and the entrance motion (ADR 0126).
//!
//! **None of this can touch the unlock gate.** `Face` is read by `view.rs`
//! and written by two messages that only set drawing state (`Frame`,
//! `Glance`); neither converts to an unlock. Nothing here can fail loudly:
//! a layout file that is missing, a symlink, too big, unparsable or invalid
//! is the built-in layout, and the built-in layout has the password field.
//!
//! The layout is read once, at start, from
//! `$XDG_CONFIG_HOME/eclipse/lock-layout` (default `~/.config/eclipse/`):
//! the lines `tile.<kind> = "x y w h"` the login screen's file uses.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ec_glance::{batteries, bounded, wifi, Battery, Wifi};
use ec_layout::{validate_with, Kind, Layout, LOCK_REQUIRED, MAX_LINES_BYTES};
use iced::futures::channel::mpsc::Sender;
use iced::Subscription;

use crate::app::Message;
use crate::tokens::{glance as limits, motion};

pub const POWER_SUPPLY: &str = "/sys/class/power_supply";
pub const NET: &str = "/sys/class/net";
pub const WIRELESS: &str = "/proc/net/wireless";

/// One poll's answer. Batteries are sorted internal first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub batteries: Vec<Battery>,
    pub wifi: Option<Wifi>,
}

/// Read both sources once. Blocking: call it under `bounded`.
pub fn read() -> Snapshot {
    let mut b = batteries(Path::new(POWER_SUPPLY));
    b.sort_by_key(|b| !b.internal);
    Snapshot {
        batteries: b,
        wifi: wifi(Path::new(NET), WIRELESS),
    }
}

/// The lock's own layout when nothing is configured: the clock, one line for
/// the account, the field. No Users tile (the lock has one account) and no
/// Power tile (a locked session is not the place to turn the machine off).
/// It always has the Password tile. A lock file needs only that one (the
/// settings editor writes no Users or Power tile, and the lock would not draw them).
pub fn builtin() -> Layout {
    Layout::lock_default()
}

/// `$XDG_CONFIG_HOME/eclipse/lock-layout`, or `~/.config/eclipse/lock-layout`.
/// Only absolute roots count.
pub fn config_path(xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let root = match (xdg, home) {
        (Some(x), _) if x.starts_with('/') => PathBuf::from(x),
        (_, Some(h)) if h.starts_with('/') => Path::new(h).join(".config"),
        _ => return None,
    };
    Some(root.join("eclipse").join("lock-layout"))
}

/// Up to `cap` bytes of a regular file as text. `O_NOFOLLOW` (a symlink is
/// refused), `O_NONBLOCK` (a FIFO cannot hold the lock up) and a size cap
/// that does not trust the file's length. `None` for any failure.
pub fn read_capped(path: &Path, cap: usize) -> Option<String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > cap {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// A layout from file text: it must parse and validate whole, and name no
/// tile that needs the session. Anything else is the built-in layout.
pub fn layout_from_text(text: &str) -> Layout {
    match Layout::from_lines(text) {
        Ok(l)
            if validate_with(&l, &LOCK_REQUIRED).is_ok()
                && l.tiles.iter().all(|t| !t.kind.needs_session()) =>
        {
            l
        }
        _ => builtin(),
    }
}

/// The user's layout, or the built-in one. Never panics, never errors.
pub fn load_layout() -> Layout {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    config_path(xdg.as_deref(), home.as_deref())
        .and_then(|p| read_capped(&p, MAX_LINES_BYTES))
        .map_or_else(builtin, |t| layout_from_text(&t))
}

/// `0 -> 1` as a damped spring settles: the step response, in closed form, so
/// the view needs a time and nothing else. Overshoots about a tenth once.
pub fn enter(t: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    let w0 = motion::STIFFNESS.sqrt();
    let zeta = motion::DAMPING / (2.0 * w0);
    let (decay, wd) = (zeta * w0, w0 * (1.0 - zeta * zeta).max(0.0).sqrt());
    if wd <= 0.0 {
        return 1.0;
    }
    1.0 - (-decay * t).exp() * ((wd * t).cos() + decay / wd * (wd * t).sin())
}

/// The drawing state besides the password.
#[derive(Debug, Clone)]
pub struct Face {
    layout: Layout,
    glance: Snapshot,
    /// When the face was made: the entrance counts from here, on the wall
    /// clock, not on frames. A compositor that sends a lock surface no frame
    /// callbacks (abyss sends none today) would otherwise hold every tile but
    /// the password at "not yet" for good.
    born: Instant,
    /// The latest frame (frames now only prompt a redraw).
    now: Instant,
}

impl Face {
    pub fn new(layout: Layout) -> Self {
        let now = Instant::now();
        Face {
            layout,
            glance: Snapshot::default(),
            born: now,
            now,
        }
    }

    /// Read the user's layout and start.
    pub fn load() -> Self {
        Self::new(load_layout())
    }

    /// Already settled, for the preview.
    pub fn settled(layout: Layout, glance: Snapshot) -> Self {
        let now = Instant::now();
        Face {
            layout,
            glance,
            born: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
            now,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn glance(&self) -> &Snapshot {
        &self.glance
    }

    pub fn set_glance(&mut self, snap: Option<Snapshot>) {
        // `None` is a read that timed out: keep what is on screen.
        if let Some(s) = snap {
            self.glance = s;
        }
    }

    pub fn frame(&mut self, now: Instant) {
        self.now = now;
    }

    fn total(&self) -> Duration {
        Duration::from_millis(motion::STAGGER_MS * self.layout.tiles.len() as u64 + motion::SETTLE_MS)
    }

    /// Whether the entrance is still running: the view asks for frames only
    /// while this is true.
    pub fn animating(&self) -> bool {
        Instant::now().saturating_duration_since(self.born) < self.total()
    }

    /// Whether the glance feed is worth polling: a Glance tile is placed.
    pub fn wants_glance(&self) -> bool {
        self.layout
            .tiles
            .iter()
            .any(|t| matches!(t.kind, Kind::Battery | Kind::Wifi))
    }

    /// How far tile number `index` has appeared: 0 before, 1 once in place.
    /// The password pane never animates: it is there to type into from the
    /// first frame, and a stalled frame clock must never hide it.
    pub fn appeared(&self, index: usize, kind: Kind) -> f32 {
        if kind == Kind::Password {
            return 1.0;
        }
        let t = Instant::now().saturating_duration_since(self.born).as_secs_f32()
            - (motion::STAGGER_MS * index as u64) as f32 / 1000.0;
        if t * 1000.0 >= motion::SETTLE_MS as f32 {
            1.0
        } else {
            enter(t)
        }
    }
}

/// Frames while the entrance runs, the Glance feed while one is placed.
pub fn subscription(face: &Face) -> Subscription<Message> {
    let mut subs = Vec::new();
    if face.animating() {
        subs.push(iced::window::frames().map(Message::Frame));
    }
    if face.wants_glance() {
        subs.push(Subscription::run(poll));
    }
    Subscription::batch(subs)
}

/// A snapshot now, then one every few seconds, read on a thread of its own
/// under a time limit. `None` means the read timed out.
fn poll() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(1, |mut out: Sender<Message>| async move {
        std::thread::spawn(move || loop {
            let snap = bounded(limits::LIMIT, read);
            if let Err(e) = out.try_send(Message::Glance(snap)) {
                if e.is_disconnected() {
                    break;
                }
            }
            std::thread::sleep(limits::EVERY);
        });
        std::future::pending::<()>().await
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_layout::Rect;
    use std::fs;

    #[test]
    fn the_builtin_layout_has_the_field_and_fits() {
        let l = builtin();
        assert!(l.get(Kind::Password).is_some());
        assert!(l.tiles.iter().all(|t| t.rect.in_grid()));
        // A lock layout needs only the password: no Users tile.
        assert_eq!(validate_with(&l, &LOCK_REQUIRED), Ok(()));
    }

    /// What Settings' lock editor writes (`Layout::to_lines`, no Users, no Power)
    /// is exactly what the lock reads back.
    #[test]
    fn what_the_editor_writes_the_lock_reads_back() {
        let mut l = builtin();
        l.set(Kind::Battery, Rect::new(18, 1, 2, 2));
        l.set(Kind::Wifi, Rect::new(18, 4, 2, 1));
        assert_eq!(layout_from_text(&l.to_lines()), l);
    }

    #[test]
    fn a_good_file_is_used_and_a_bad_one_is_the_builtin() {
        let good = "tile.users = \"0 0 4 3\"\ntile.password = \"8 9 8 2\"\ntile.battery = \"18 1 4 2\"\n";
        let l = layout_from_text(good);
        assert_eq!(l.get(Kind::Password), Some(Rect::new(8, 9, 8, 2)));
        assert!(l.get(Kind::Battery).is_some());
        for bad in [
            "",
            "garbage",
            "tile.clock = \"0 0 4 2\"\n",
            "tile.users = \"0 0 4 3\"\ntile.password = \"0 0 6 2\"\n",
            "tile.users = \"0 0 4 3\"\ntile.password = \"8 9 8 2\"\ntile.now_playing = \"0 5 6 2\"\n",
        ] {
            assert_eq!(layout_from_text(bad), builtin(), "{bad:?}");
        }
    }

    #[test]
    fn config_path_wants_absolute_roots() {
        let want = Some(PathBuf::from("/c/eclipse/lock-layout"));
        assert_eq!(config_path(Some("/c"), Some("/h")), want);
        assert_eq!(
            config_path(None, Some("/h")),
            Some(PathBuf::from("/h/.config/eclipse/lock-layout"))
        );
        assert_eq!(config_path(Some("rel"), None), None);
        assert_eq!(config_path(None, None), None);
    }

    #[test]
    fn reading_is_capped_and_refuses_links_and_non_files() {
        let d = std::env::temp_dir().join(format!("ec-lock-face-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let ok = d.join("ok");
        fs::write(&ok, "hello").unwrap();
        assert_eq!(read_capped(&ok, 16).as_deref(), Some("hello"));
        assert_eq!(read_capped(&ok, 4), None, "over the cap");
        let link = d.join("link");
        std::os::unix::fs::symlink(&ok, &link).unwrap();
        assert_eq!(read_capped(&link, 16), None, "symlink");
        assert_eq!(read_capped(&d, 16), None, "directory");
        assert_eq!(read_capped(&d.join("absent"), 16), None);
        let bin = d.join("bin");
        fs::write(&bin, [0xff, 0xfe]).unwrap();
        assert_eq!(read_capped(&bin, 16), None, "not utf-8");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn the_spring_starts_at_zero_overshoots_a_little_and_settles() {
        assert_eq!(enter(-1.0), 0.0);
        assert_eq!(enter(0.0), 0.0);
        let peak = (0..200).map(|i| enter(i as f32 / 100.0)).fold(0.0, f32::max);
        assert!(peak > 1.0 && peak < 1.2, "{peak}");
        assert!((enter(3.0) - 1.0).abs() < 0.001);
    }

    #[test]
    fn the_password_pane_is_never_hidden_and_the_rest_count_from_birth() {
        let mut f = Face::new(builtin());
        assert_eq!(f.appeared(2, Kind::Password), 1.0);
        assert!(f.appeared(0, Kind::Clock) < 1.0);
        assert!(f.animating());
        // No frame ever arrives: the wall clock alone settles the entrance.
        let long_ago = Instant::now().checked_sub(Duration::from_secs(5)).unwrap();
        f.born = long_ago;
        assert_eq!(f.appeared(0, Kind::Clock), 1.0);
        assert!(!f.animating());
    }
}
