// SPDX-License-Identifier: AGPL-3.0-only
//! Settings > Desktop > Lock screen: where the clock, the account line, the
//! password field and the Glances sit on the lock (ADR 0126).
//!
//! The same editor as the login screen's (`layout_view`, `layout_edit`), on the
//! lock's tile set and drawn as the lock draws it (`lock_mock`). The file is the
//! user's own, `$XDG_CONFIG_HOME/eclipse/lock-layout`, so there is no root helper
//! and no Apply: a finished gesture is written at once, the way every other
//! setting applies. The write is atomic (a temp file, then a rename), the file
//! is private to the user, and a failed write puts the editor back to what the
//! file holds, with the reason on the page. The lock reads it at start, so a
//! change shows the next time the screen locks.
//!
//! "Reset layout" removes the file: nothing stored is the built-in layout.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use ec_greeter::{Clock, Settings};
use ec_layout::{validate_with, Layout, LOCK_KINDS, LOCK_REQUIRED, MAX_LINES_BYTES};
use iced::widget::{button, column, container, row, text, Space};
use iced::{Alignment, Element, Length, Theme};

use ec_ui::theme;
use ec_ui::tokens::{color, font, space};
use ec_ui::widget::{glass_panel as panel, micro_label, pick_pill, pill_group};

use super::layout_edit::{normalized_for, Screen};
use super::{layout_view, mono_tertiary, sample, sample_label_w, small};
use crate::app::{App, Message};

/// The page's state: what is on screen, what the file holds, and the last
/// thing that went wrong.
#[derive(Debug, Clone)]
pub struct LockScreen {
    /// The editor's layout.
    pub layout: Layout,
    /// The login screen's clock preset, which the lock follows.
    pub clock: Option<Clock>,
    pub sample_batteries: sample::Batteries,
    pub sample_link: sample::Link,
    /// The last write's failure, in a sentence; cleared by the next success.
    pub error: Option<String>,
    /// The page's own words for what just happened.
    pub saved: bool,
}

impl Default for LockScreen {
    fn default() -> Self {
        LockScreen {
            layout: Layout::lock_default(),
            clock: None,
            sample_batteries: sample::Batteries::default(),
            sample_link: sample::Link::default(),
            error: None,
            saved: false,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// The editor finished a gesture: the whole new layout.
    SetLayout(Layout),
    /// "Reset layout": the built-in arrangement; the file is removed.
    Reset,
    SampleBatteries(sample::Batteries),
    SampleLink(sample::Link),
}

fn msg(m: Msg) -> Message {
    Message::LockScreen(m)
}

// ---------------------------------------------------------------- the file

/// `$XDG_CONFIG_HOME/eclipse/lock-layout`, or `~/.config/eclipse/lock-layout`;
/// only absolute roots count, as the lock reads it (`ec_lock::face::config_path`).
pub fn config_path(xdg: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let root = match (xdg, home) {
        (Some(x), _) if x.starts_with('/') => PathBuf::from(x),
        (_, Some(h)) if h.starts_with('/') => Path::new(h).join(".config"),
        _ => return None,
    };
    Some(root.join("eclipse").join("lock-layout"))
}

fn path() -> Option<PathBuf> {
    config_path(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// The layout a file's text holds, as the lock reads it, and as the editor can
/// show it: a layout the lock would refuse is the built-in one, and tiles the
/// lock does not draw (a Users or Power tile an older file named) are dropped,
/// so the next write is a file the editor itself would have made.
pub fn from_text(text: &str) -> Layout {
    match Layout::from_lines(text) {
        Ok(mut l)
            if validate_with(&l, &LOCK_REQUIRED).is_ok()
                && l.tiles.iter().all(|t| !t.kind.needs_session()) =>
        {
            l.tiles.retain(|t| LOCK_KINDS.contains(&t.kind));
            normalized_for(Screen::Lock, l)
        }
        _ => Layout::lock_default(),
    }
}

/// The file's layout, or the built-in one. Never errors: a file the lock would
/// not use is not one the page should pretend it does.
fn read(path: &Path) -> Layout {
    let ok = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() <= MAX_LINES_BYTES as u64);
    if !ok {
        return Layout::lock_default();
    }
    std::fs::read_to_string(path).map_or_else(|_| Layout::lock_default(), |t| from_text(&t))
}

/// Store `l` at `path`: the built-in layout is nothing stored (the file goes),
/// anything else is the lines, written to a private temp file and renamed over.
pub fn write(path: &Path, l: &Layout) -> std::io::Result<()> {
    if *l == Layout::lock_default() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no directory"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".lock-layout.{}.tmp", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let done = f
        .write_all(l.to_lines().as_bytes())
        .and_then(|()| f.sync_all())
        .and_then(|()| std::fs::rename(&tmp, path));
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}

/// Read what is on this machine: the lock's layout file and the login screen's
/// clock preset, which the lock follows.
pub fn load(app: &mut App) {
    let clock = std::fs::read_to_string(Path::new(ec_greeter::STATE_DIR).join(ec_greeter::SETTINGS_FILE))
        .ok()
        .and_then(|t| Settings::parse(&t).ok())
        .and_then(|s| s.clock);
    let p = &mut app.lock;
    p.layout = path().map_or_else(Layout::lock_default, |f| read(&f));
    p.clock = clock;
    p.error = None;
    p.saved = false;
}

// ------------------------------------------------------------------ update

pub fn update(app: &mut App, m: Msg) -> iced::Task<Message> {
    let p = &mut app.lock;
    match m {
        Msg::SetLayout(l) => {
            if Screen::Lock.accepts(&l) && l != p.layout {
                apply(p, l);
            }
        }
        Msg::Reset => apply(p, Layout::lock_default()),
        Msg::SampleBatteries(b) => p.sample_batteries = b,
        Msg::SampleLink(l) => p.sample_link = l,
    }
    iced::Task::none()
}

/// Show `l` and store it; if the store fails, go back to what the file holds.
fn apply(p: &mut LockScreen, l: Layout) {
    let Some(file) = path() else {
        p.error = Some("There is no home folder to keep the layout in.".into());
        return;
    };
    match write(&file, &l) {
        Ok(()) => {
            p.layout = l;
            p.error = None;
            p.saved = true;
        }
        Err(e) => {
            // The detail goes to the log; the page says one thing.
            eprintln!("ec-settings: lock layout: {e}");
            p.layout = read(&file);
            p.error = Some("Could not save the layout. It is back to what is stored.".into());
            p.saved = false;
        }
    }
}

pub fn status(app: &App) -> (String, String) {
    if app.lock.layout == Layout::lock_default() {
        ("built-in layout".into(), "nothing customised".into())
    } else {
        (
            "customised".into(),
            format!("{} tiles", app.lock.layout.tiles.len()),
        )
    }
}

// -------------------------------------------------------------------- view

/// What the editor draws with: the desktop's own palette, as the lock does,
/// over the lock's opaque ground.
fn skin(p: &LockScreen) -> layout_view::Skin {
    let pal = color::palette();
    layout_view::Skin {
        base: color::to_iced(pal.base),
        ink: color::to_iced(pal.text),
        pal,
        dense: false,
        clock: sample::clock(p.clock),
        batteries: p.sample_batteries.data(),
        link: p.sample_link.quality(),
        greeting: String::new(),
        backdrop: None,
    }
}

/// The lock editor on its own, for `examples/builder_preview.rs` (`LOCK=1`):
/// the lock's tiles in the builder, to set beside `ec-lock`'s own preview.
/// Not used by the app.
pub fn preview_editor<'a, M: Clone + 'a>(
    layout: &'a Layout,
    clock: Option<Clock>,
    batteries: &str,
    link: &str,
    on_change: impl Fn(Layout) -> M + 'a,
) -> Element<'a, M, Theme> {
    let p = LockScreen {
        clock,
        sample_batteries: match batteries {
            "none" => sample::Batteries::None,
            "several" => sample::Batteries::Several,
            _ => sample::Batteries::One,
        },
        sample_link: match link {
            "none" => sample::Link::None,
            "weak" => sample::Link::Weak,
            _ => sample::Link::Strong,
        },
        ..LockScreen::default()
    };
    layout_view::editor(layout, skin(&p), on_change)
        .screen(Screen::Lock)
        .into()
}

pub fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let p = &app.lock;
    let edited = p.layout != Layout::lock_default();
    let reset = button(
        small("Reset layout")
            .font(font::UI_MEDIUM)
            .wrapping(text::Wrapping::None),
    )
    .padding([space::PILL_Y, space::PILL_X])
    .on_press_maybe(edited.then(|| msg(Msg::Reset)))
    .style(theme::link);
    let state = if p.saved {
        format!("{} tiles \u{b7} saved", p.layout.tiles.len())
    } else {
        format!("{} tiles", p.layout.tiles.len())
    };
    let mut col = column![
        row![
            micro_label("lock screen"),
            container(mono_tertiary(state)).width(Length::Fill).align_x(Alignment::End),
            reset,
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center),
        container(
            layout_view::editor(&p.layout, skin(p), |l| msg(Msg::SetLayout(l))).screen(Screen::Lock)
        )
        .width(Length::Fill)
        .align_x(Alignment::Center),
        samples(p),
        small(
            "Drag a tile to move it; pull any edge or corner to resize it. Tap or drag a tile below to add it. \
             Changes are saved as you make them and show the next time the screen locks."
        )
        .style(theme::text_tertiary),
    ]
    .spacing(space::ROW_Y);
    if let Some(e) = &p.error {
        col = col.push(small(e.clone()).style(theme::text_danger));
    }
    vec![panel(app.glass_radius, app.hairline_cards, col)]
}

/// What the preview's Glances pretend the machine has. Display only.
fn samples(p: &LockScreen) -> Element<'_, Message, Theme> {
    let batteries = sample::Batteries::ALL
        .into_iter()
        .map(|b| {
            pick_pill(
                b.label(),
                p.sample_batteries == b,
                Some(msg(Msg::SampleBatteries(b))),
            )
        })
        .collect();
    let link = sample::Link::ALL
        .into_iter()
        .map(|l| pick_pill(l.label(), p.sample_link == l, Some(msg(Msg::SampleLink(l)))))
        .collect();
    let line = |name: &'static str, pills| {
        row![
            container(small(name).style(theme::text_tertiary)).width(Length::Fixed(sample_label_w())),
            container(pill_group(pills)).width(Length::Fill),
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center)
    };
    column![
        line("Preview battery", batteries),
        line("Preview Wi-Fi", link),
        Space::new().height(space::ZERO)
    ]
    .spacing(space::PILL_GAP)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_layout::{Kind, Rect};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ec-settings-lock-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn custom() -> Layout {
        let mut l = Layout::lock_default();
        l.set(Kind::Battery, Rect::new(18, 1, 2, 2));
        l.set(Kind::Wifi, Rect::new(18, 4, 2, 1));
        l
    }

    #[test]
    fn a_written_layout_reads_back_and_the_lock_would_accept_it() {
        let d = dir("roundtrip");
        let f = d.join("eclipse").join("lock-layout");
        write(&f, &custom()).unwrap();
        assert_eq!(read(&f), custom());
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.len() <= MAX_LINES_BYTES);
        // Exactly the rule the lock applies to the text.
        let l = Layout::from_lines(&text).unwrap();
        assert_eq!(validate_with(&l, &LOCK_REQUIRED), Ok(()));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(f.parent().unwrap()).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_built_in_layout_is_nothing_stored() {
        let d = dir("builtin");
        let f = d.join("lock-layout");
        write(&f, &custom()).unwrap();
        assert!(f.exists());
        write(&f, &Layout::lock_default()).unwrap();
        assert!(!f.exists());
        // Removing what is not there is fine.
        write(&f, &Layout::lock_default()).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_file_the_lock_would_refuse_is_the_built_in_layout() {
        for bad in [
            "",
            "garbage",
            "tile.clock = \"0 0 4 2\"\n",
            "tile.password = \"0 0 6 2\"\ntile.clock = \"0 0 4 2\"\n",
        ] {
            assert_eq!(from_text(bad), Layout::lock_default(), "{bad:?}");
        }
    }

    #[test]
    fn tiles_the_lock_does_not_draw_are_dropped_on_read() {
        let old = "tile.users = \"0 0 4 3\"\ntile.password = \"8 9 8 2\"\ntile.power = \"0 12 3 1\"\ntile.battery = \"18 1 2 2\"\n";
        let l = from_text(old);
        assert!(l.get(Kind::Users).is_none() && l.get(Kind::Power).is_none());
        assert_eq!(l.get(Kind::Password), Some(Rect::new(8, 9, 8, 2)));
        assert!(l.get(Kind::Battery).is_some());
        assert!(Screen::Lock.accepts(&l));
    }

    #[test]
    fn a_symlink_or_a_huge_file_is_not_read() {
        let d = dir("refuse");
        let real = d.join("real");
        std::fs::write(&real, custom().to_lines()).unwrap();
        let link = d.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(read(&link), Layout::lock_default());
        let big = d.join("big");
        std::fs::write(&big, "x".repeat(MAX_LINES_BYTES + 1)).unwrap();
        assert_eq!(read(&big), Layout::lock_default());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_path_wants_absolute_roots() {
        assert_eq!(
            config_path(Some("/c"), Some("/h")),
            Some(PathBuf::from("/c/eclipse/lock-layout"))
        );
        assert_eq!(
            config_path(None, Some("/h")),
            Some(PathBuf::from("/h/.config/eclipse/lock-layout"))
        );
        assert_eq!(config_path(Some("rel"), None), None);
    }

    #[test]
    fn the_editor_only_accepts_what_the_lock_draws() {
        let mut l = Layout::lock_default();
        assert!(Screen::Lock.accepts(&l));
        l.set(Kind::Power, Rect::new(0, 12, 3, 1));
        assert!(!Screen::Lock.accepts(&l), "the lock draws no Power tile");
        let mut no_field = Layout::lock_default();
        no_field.tiles.retain(|t| t.kind != Kind::Password);
        assert!(!Screen::Lock.accepts(&no_field));
    }
}
