// SPDX-License-Identifier: AGPL-3.0-only
//! The landing page: this computer at a glance, the appearance choice, the
//! switches reached for most, and doors into the busiest pages.
//!
//! Accent ledger: the one live yellow is the power reading (the battery
//! charge, or the uptime on a machine with none). The scheme card that is
//! chosen, and a quick switch that is on, are a brighter fill and a crisp rim.
//!
//! Everything shown is read without privilege: the host name, `os-release`,
//! `/proc/uptime` and the battery's `capacity` under `/sys/class/power_supply`
//! are world-readable files, and the display and link readings are the ones
//! the Display and Network pages already hold. They are read once at start,
//! on reload and when the page is opened, never from a `view`.

use iced::widget::{button, column, container, responsive, row, text, Column, Row, Space};
use iced::{Alignment, Element, Length, Theme};

use ec_ui::theme;
use ec_ui::tokens::{art, color, font, radius, size, space};
use ec_ui::widget::art::{tile, Glyph, Hue};
use ec_ui::widget::{chevron_turn, focus_ring, glass_panel as panel, hairline, micro_label};

use crate::app::{locate, App, Message};
use crate::focus::Focus;
use crate::pane::Page;
use crate::schema::Control;

/// What the machine says about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sys {
    pub host: String,
    pub os: String,
    pub kernel: String,
    pub uptime: Option<u64>,
    /// Charge percent and whether it is charging, for the first battery.
    pub battery: Option<(u8, bool)>,
}

impl Sys {
    pub fn read() -> Sys {
        let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
        Sys {
            host: read("/proc/sys/kernel/hostname").trim().to_owned(),
            os: pretty_name(&read("/etc/os-release")),
            kernel: read("/proc/sys/kernel/osrelease").trim().to_owned(),
            uptime: parse_uptime(&read("/proc/uptime")),
            battery: battery(),
        }
    }
}

/// `PRETTY_NAME` of an `os-release` file, unquoted.
pub fn pretty_name(os_release: &str) -> String {
    os_release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').trim_matches('\'').to_owned())
        .unwrap_or_default()
}

/// Whole seconds of `/proc/uptime`'s first field.
pub fn parse_uptime(s: &str) -> Option<u64> {
    let secs: f64 = s.split_whitespace().next()?.parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then_some(secs as u64)
}

/// `3 d 4 h`, `4 h 12 m`, `12 m`: the two largest units that are not zero.
pub fn uptime_text(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h) {
        (0, 0) => format!("{m} m"),
        (0, h) => format!("{h} h {m} m"),
        (d, h) => format!("{d} d {h} h"),
    }
}

fn battery() -> Option<(u8, bool)> {
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    for entry in dir.flatten() {
        let p = entry.path();
        let read = |f: &str| std::fs::read_to_string(p.join(f)).unwrap_or_default();
        if read("type").trim() != "Battery" {
            continue;
        }
        let pct: u8 = read("capacity").trim().parse().ok()?;
        return Some((pct.min(100), read("status").trim() == "Charging"));
    }
    None
}

/// The page's blocks, under its header.
pub fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let mut out = vec![device(app), appearance(app), quick(app)];
    out.extend(recent(app));
    out.push(doors(app));
    out
}

/// How many columns `n` equal cells take in `width`: as many as fit at `min_w`,
/// then spread evenly over the rows that needs, so five cells are 3 + 2 and
/// not 4 + 1. A continuous function of the width, with no threshold of its own.
pub fn columns(width: f32, min_w: f32, gap: f32, n: usize) -> usize {
    if n == 0 || !width.is_finite() {
        return 1;
    }
    let fit = (((width + gap) / (min_w + gap)).floor().max(1.0) as usize).min(n);
    let rows = n.div_ceil(fit);
    n.div_ceil(rows)
}

/// `n` cells built by `make`, in rows of [`columns`] equal columns.
fn grid<'a>(
    n: usize,
    min_w: f32,
    gap: f32,
    make: impl Fn(usize) -> Element<'a, Message, Theme> + 'a,
) -> Element<'a, Message, Theme> {
    responsive(move |size| {
        let cols = columns(size.width, min_w, gap, n);
        let mut out = Column::new().spacing(gap).width(Length::Fill);
        let mut i = 0;
        while i < n {
            let mut r = Row::new().spacing(gap).width(Length::Fill);
            for c in 0..cols {
                r = r.push(if i + c < n {
                    make(i + c)
                } else {
                    Space::new().width(Length::Fill).into()
                });
            }
            out = out.push(r);
            i += cols;
        }
        out.into()
    })
    .height(Length::Shrink)
    .into()
}

fn text_line<'a>(
    s: impl Into<String>,
    f: iced::Font,
    px: f32,
    style: fn(&Theme) -> iced::widget::text::Style,
) -> Element<'a, Message, Theme> {
    text(s.into()).font(f).size(px).style(style).into()
}

/// One cell of the status grid: a subject, its reading and a line under it.
fn cell<'a>(subject: &str, reading: String, detail: String, live: bool) -> Element<'a, Message, Theme> {
    column![
        micro_label(subject),
        text_line(
            reading,
            font::UI_MEDIUM,
            size::PROMPT,
            if live {
                theme::text_accent
            } else {
                theme::text_primary
            },
        ),
        text_line(detail, font::UI, size::BODY_SMALL, theme::text_tertiary),
    ]
    .spacing(space::LINE_GAP * 2.0)
    .width(Length::Fill)
    .into()
}

/// The device card: the host, what it runs, and a 2x2 of readings.
fn device(app: &App) -> Element<'_, Message, Theme> {
    let sys = &app.sys;
    let host = if sys.host.is_empty() {
        "This computer"
    } else {
        sys.host.as_str()
    };
    let mut under = vec![sys.os.clone()];
    if !sys.kernel.is_empty() {
        under.push(format!("kernel {}", sys.kernel));
    }
    under.retain(|s| !s.is_empty());
    let head = row![
        tile(Glyph::Monitor, Hue::Home, art::HERO_TILE, false),
        column![
            text_line(host, font::UI_SEMIBOLD, art::HOST_SIZE, theme::text_primary),
            text_line(
                if under.is_empty() {
                    "Eclipse".to_owned()
                } else {
                    under.join(" \u{b7} ")
                },
                font::UI,
                size::BODY_SMALL,
                theme::text_secondary,
            ),
        ]
        .spacing(space::LINE_GAP),
    ]
    .spacing(space::CARD)
    .align_y(Alignment::Center);

    let on: Vec<_> = app.outputs.iter().filter(|o| o.enabled).collect();
    let (display, display_detail) = match on.first() {
        Some(o) => (
            match on.len() {
                1 => o.mode_display(),
                n => format!("{n} displays"),
            },
            format!("{} \u{b7} scale {}", o.name, o.scale),
        ),
        None => ("No display".to_owned(), "not reported".to_owned()),
    };
    let (net, net_detail) = app.net.chip();
    let (power, power_detail, live_power) = match (sys.battery, sys.uptime) {
        (Some((pct, charging)), _) => (
            format!("{pct} %"),
            if charging { "charging" } else { "on battery" }.to_owned(),
            true,
        ),
        (None, _) => ("No battery".to_owned(), "mains power".to_owned(), false),
    };
    let up = sys.uptime.map_or("\u{2014}".to_owned(), uptime_text);

    let cells = [
        ("Display", display, display_detail, false),
        ("Network", net, net_detail, false),
        // The one live yellow: the battery, or the uptime without one.
        ("Power", power, power_detail, live_power),
        (
            "Uptime",
            up,
            "since boot".to_owned(),
            !live_power && sys.uptime.is_some(),
        ),
    ];
    panel(
        app.glass_radius,
        app.hairline_cards,
        column![
            head,
            hairline(),
            grid(4, art::CELL_MIN_W, space::BLOCK, move |i| {
                let (subject, reading, detail, live) = &cells[i];
                cell(subject, reading.clone(), detail.clone(), *live)
            }),
        ]
        .spacing(space::BLOCK),
    )
}

/// The appearance choice, as the scheme cards, and the way on to the rest of
/// the colour settings. The scheme cards live here and only here; the Blur &
/// glass page keeps the settings rows (colour scheme, themes, accent).
fn appearance(app: &App) -> Element<'_, Message, Theme> {
    let scheme = app
        .key(crate::schema::COLOR_SCHEME)
        .and_then(|k| k.value.as_str())
        .unwrap_or("dark");
    let more = focus_ring(
        ec_ui::widget::pill("Colour, themes and accent", false, Message::Open(Page::Blur)),
        app.focus == Some(Focus::More),
        radius::PILL,
        Length::Shrink,
    );
    panel(
        app.glass_radius,
        app.hairline_cards,
        column![
            row![micro_label("Appearance"), Space::new().width(Length::Fill), more]
                .align_y(Alignment::Center),
            crate::previews::scheme_cards(app),
            text_line(
                crate::schema::scheme_blurb(scheme),
                font::UI,
                size::BODY_SMALL,
                theme::text_tertiary
            ),
        ]
        .spacing(space::ROW_Y),
    )
}

/// The switches reached for most: key, glyph. Only a key the compositor
/// reports as a switch is drawn, so a tile can never be a dead control.
const QUICK: &[(&str, &str, Glyph)] = &[
    ("decoration.shadow.enabled", "Window shadows", Glyph::Shadow),
    ("animations.enabled", "Animations", Glyph::Motion),
    ("input.touchpad.natural-scroll", "Natural scroll", Glyph::Touchpad),
    ("input.touchpad.tap-to-click", "Tap to click", Glyph::Touchpad),
    ("bar.fold-when-idle", "Fold bar when idle", Glyph::Bar),
    ("ui.show-key-hints", "Key hints", Glyph::Keycap),
];

/// The quick switches this compositor reports, as `(key, label, glyph)`.
pub fn quick_keys(app: &App) -> Vec<(&'static str, &'static str, Glyph)> {
    QUICK
        .iter()
        .filter(|(path, _, _)| {
            app.key(path)
                .is_some_and(|k| k.control == Control::Toggle && !k.locked())
        })
        .copied()
        .collect()
}

fn quick(app: &App) -> Element<'_, Message, Theme> {
    let keys = quick_keys(app);
    if keys.is_empty() {
        return Space::new().into();
    }
    let n = keys.len();
    let tiles = grid(n, art::QUICK_W, space::PILL_GAP, move |i| {
        let (path, label, glyph) = keys[i];
        let hue = crate::pane::page_of(path).map_or(Hue::Home, |p| p.section().hue());
        let on = app.key(path).is_some_and(|k| k.as_bool());
        let face = row![
            tile(glyph, hue, art::TILE, on),
            column![
                text_line(label, font::UI_MEDIUM, size::BODY_SMALL, theme::text_primary),
                text_line(
                    if on { "On" } else { "Off" },
                    font::UI,
                    size::MICRO,
                    theme::text_tertiary
                ),
            ]
            .spacing(space::LINE_GAP),
        ]
        .spacing(space::ROW_Y)
        .align_y(Alignment::Center);
        focus_ring(
            button(face)
                .padding([space::ROW_Y, space::CARD])
                .width(Length::Fill)
                .on_press(Message::Toggled(path.to_owned(), !on))
                .style(move |t: &Theme, status| {
                    let mut s = theme::inset_cell(t, status);
                    s.border.radius = radius::INSET.into();
                    if on {
                        s.background = Some(iced::Background::Color(color::highlight_soft()));
                        s.border.color = color::highlight_rim();
                        s.text_color = color::text();
                    }
                    s
                }),
            app.focus == Some(Focus::Quick(i)),
            radius::INSET,
            Length::Fill,
        )
    });
    panel(
        app.glass_radius,
        app.hairline_cards,
        column![micro_label("Quick settings"), tiles].spacing(space::ROW_Y),
    )
}

/// Settings opened from search lately, when the history is on and not empty.
fn recent(app: &App) -> Option<Element<'_, Message, Theme>> {
    let items = app.search.recent(4);
    if items.is_empty() {
        return None;
    }
    let pills: Vec<Element<'_, Message, Theme>> = items
        .into_iter()
        .filter_map(|(label, path)| {
            locate(&path).map(|at| ec_ui::widget::pill(&label, false, Message::Reveal(at)))
        })
        .collect();
    let wrap = Row::with_children(pills)
        .spacing(space::PILL_GAP)
        .wrap()
        .vertical_spacing(space::PILL_GAP);
    Some(panel(
        app.glass_radius,
        app.hairline_cards,
        column![micro_label("Recently opened"), wrap].spacing(space::ROW_Y),
    ))
}

/// The pages people come for, as a list: a tile, a name, a line, a chevron.
pub const DOORS: &[Page] = &[
    Page::Display,
    Page::Network,
    Page::Wallpaper,
    Page::Keyboard,
    Page::Blur,
    Page::BarAppearance,
    Page::Session,
    Page::Privacy,
];

fn doors(app: &App) -> Element<'_, Message, Theme> {
    let mut list = Column::new().spacing(space::LINE_GAP).width(Length::Fill);
    list = list.push(container(micro_label("Jump to")).padding([0.0, space::CARD / 2.0]));
    for (n, page) in DOORS.iter().enumerate() {
        let s = page.section();
        let face = row![
            tile(page.glyph(), s.hue(), art::TILE, false),
            column![
                text_line(page.title(), font::UI_MEDIUM, size::BODY, theme::text_primary),
                text_line(s.blurb(), font::UI, size::BODY_SMALL, theme::text_tertiary),
            ]
            .spacing(space::LINE_GAP),
            Space::new().width(Length::Fill),
            chevron_turn(0.0, color::text_tertiary()),
        ]
        .spacing(space::ROW_Y)
        .align_y(Alignment::Center);
        list = list.push(focus_ring(
            button(face)
                .padding([space::NAV_Y, space::CARD / 2.0])
                .width(Length::Fill)
                .on_press(Message::Open(*page))
                .style(|_t: &Theme, status| {
                    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                    button::Style {
                        background: hovered.then(|| iced::Background::Color(color::lift_soft())),
                        text_color: color::text(),
                        border: iced::border::rounded(radius::INSET),
                        ..button::Style::default()
                    }
                }),
            app.focus == Some(Focus::Door(n)),
            radius::INSET,
            Length::Fill,
        ));
    }
    panel(app.glass_radius, app.hairline_cards, list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_is_unquoted() {
        assert_eq!(
            pretty_name("NAME=\"Eclipse\"\nPRETTY_NAME=\"Eclipse OS 0.2\"\nID=eclipse\n"),
            "Eclipse OS 0.2"
        );
        assert_eq!(pretty_name("NAME=x\n"), "");
    }

    #[test]
    fn uptime_reads_the_first_field_and_refuses_junk() {
        assert_eq!(parse_uptime("12345.67 9999.00\n"), Some(12345));
        assert_eq!(parse_uptime(""), None);
        assert_eq!(parse_uptime("nan 1"), None);
        assert_eq!(parse_uptime("-4 1"), None);
    }

    #[test]
    fn uptime_names_its_two_largest_units() {
        assert_eq!(uptime_text(0), "0 m");
        assert_eq!(uptime_text(59 * 60), "59 m");
        assert_eq!(uptime_text(4 * 3600 + 12 * 60), "4 h 12 m");
        assert_eq!(uptime_text(3 * 86_400 + 4 * 3600 + 59), "3 d 4 h");
    }

    #[test]
    fn columns_are_continuous_and_balanced() {
        // Five cells never read 4 + 1.
        assert_eq!(columns(848.0, 156.0, 6.0, 5), 5);
        assert_eq!(columns(700.0, 156.0, 6.0, 5), 3);
        assert_eq!(columns(300.0, 156.0, 6.0, 5), 1);
        assert_eq!(columns(10.0, 156.0, 6.0, 5), 1);
        assert_eq!(columns(f32::NAN, 156.0, 6.0, 5), 1);
        assert_eq!(columns(900.0, 200.0, 18.0, 4), 4);
        assert_eq!(columns(500.0, 200.0, 18.0, 4), 2);
        // Non-increasing as the width shrinks, one step at a time never to zero.
        let mut last = usize::MAX;
        for w in (0..1200).rev().step_by(7) {
            let c = columns(w as f32, 160.0, 6.0, 6);
            assert!((1..=6).contains(&c) && c <= last, "{w}: {c} after {last}");
            last = c;
        }
    }

    #[test]
    fn every_door_is_a_page_with_a_section() {
        for p in DOORS {
            assert!(Page::all().any(|q| q == *p));
        }
    }
}
