// SPDX-License-Identifier: AGPL-3.0-only

//! fog-ui: the iced frontend for Fog (FOG §UI and navigation).
//!
//! `fog-ui [PATH]` — opens PATH, or the current directory. All listing I/O
//! and sorting happens in `fogd`; this process only draws what it sends.

mod app;
mod bench;
mod clip;
mod conn;
mod edit;
mod motion;
mod ops;
mod palette;
mod parts;
mod state;
mod theme;
mod view;

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};

use iced::{window, Size};

/// Wayland app-id, so Radiant priorities and zones can target Fog.
const APP_ID: &str = "os.eclipse.fog";

/// `arg` made absolute against `cwd` and normalized lexically (`.` dropped,
/// `..` pops), like a shell's `cd`. No filesystem access.
fn resolve(cwd: &Path, arg: Option<OsString>) -> Vec<u8> {
    let joined = match arg {
        Some(a) => cwd.join(from_uri(a)),
        None => cwd.to_path_buf(),
    };
    let mut out = PathBuf::from("/");
    for c in joined.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out.as_os_str().as_bytes().to_vec()
}

/// A `file://` URI (as `%U` in the desktop entry passes it) decoded to its
/// path; anything else is returned unchanged.
fn from_uri(arg: OsString) -> OsString {
    let b = arg.as_bytes();
    let Some(rest) = b.strip_prefix(b"file://") else {
        return arg;
    };
    let rest = rest.strip_prefix(b"localhost").unwrap_or(rest);
    let mut out = Vec::with_capacity(rest.len());
    let mut i = 0;
    while i < rest.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        match (
            rest[i],
            rest.get(i + 1).copied().and_then(hex),
            rest.get(i + 2).copied().and_then(hex),
        ) {
            (b'%', Some(h), Some(l)) => {
                out.push((h * 16 + l) as u8);
                i += 3;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    OsString::from_vec(out)
}

fn main() -> iced::Result {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let path = resolve(&cwd, std::env::args_os().nth(1));

    // Read before iced starts: the UI thread never does filesystem I/O.
    let config = match fog_config::path().map(|p| fog_config::load(&p)) {
        Some(Ok(c)) => c,
        Some(Err(e)) => {
            eprintln!("fog-ui: {e}; using defaults");
            fog_config::defaults()
        }
        None => fog_config::defaults(),
    };
    // The modified column's zone and `~`, likewise read up front.
    let tz = jiff::tz::TimeZone::system();
    let home = std::env::var_os("HOME")
        .map(OsStringExt::into_vec)
        .unwrap_or_else(|| b"/".to_vec());
    // The desktop's rounding, blur, opacity and motion, and the contrast
    // floor over them (FOG §Visual design).
    theme::init(
        fog_config::theme::load(&fog_config::theme::paths()),
        config.appearance,
    );

    let app = iced::application(
        move || app::App::new(path.clone(), config.clone(), tz.clone(), home.clone()),
        app::update,
        view::view,
    );
    theme::font::BYTES
        .iter()
        .fold(app, |app, face| app.font(*face))
        .title("Fog")
        .subscription(app::subscription)
        .theme(|_: &app::App| theme::iced_theme())
        .style(|_: &app::App, _| iced::theme::Style {
            // The window's own ground is the tint the view paints; the
            // compositor's blur shows through it.
            background_color: iced::Color::TRANSPARENT,
            text_color: theme::color::TEXT,
        })
        .default_font(theme::DEFAULT_FONT)
        .window(window::Settings {
            size: Size::new(theme::size::WINDOW_W, theme::size::WINDOW_H),
            transparent: true,
            platform_specific: window::settings::PlatformSpecific {
                application_id: APP_ID.to_owned(),
                ..Default::default()
            },
            ..window::Settings::default()
        })
        .run()
}

#[cfg(test)]
mod tests {
    use super::resolve;
    use std::path::Path;

    #[test]
    fn resolve_is_lexical_and_absolute() {
        let cwd = Path::new("/home/u");
        assert_eq!(resolve(cwd, None), b"/home/u");
        assert_eq!(resolve(cwd, Some("src/./x/..".into())), b"/home/u/src");
        assert_eq!(resolve(cwd, Some("/etc/".into())), b"/etc");
        assert_eq!(resolve(cwd, Some("../../..".into())), b"/");
    }

    #[test]
    fn resolve_decodes_file_uris() {
        let cwd = Path::new("/home/u");
        assert_eq!(resolve(cwd, Some("file:///tmp/a%20b".into())), b"/tmp/a b");
        assert_eq!(resolve(cwd, Some("file://localhost/etc".into())), b"/etc");
        assert_eq!(resolve(cwd, Some("file:///x/%zz".into())), b"/x/%zz");
    }
}
