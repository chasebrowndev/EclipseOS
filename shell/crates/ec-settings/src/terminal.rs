// SPDX-License-Identifier: AGPL-3.0-only
//! The Terminal pages (Apotheosis T-10 §5, milestone M26): every scalar key of
//! `apoth_config::schema::TABLE`, on pages the paired `META` table names.
//!
//! Not the control socket and not `abyss.kdl`. This section links `apoth-config`
//! and edits `~/.config/eclipse/apotheosis.kdl` itself through the lossless
//! editor (`apoth_config::edit::apply_write`): comments, order and unknown
//! nodes survive, the write is temp + fsync + rename, and a value whose winning
//! layer is `/etc` or a fragment is overridden at the top of the user file,
//! never edited in place. The terminal reloads live, so its own windows are the
//! preview (T-10 §5, R1). ADR 0107 Option 2 is why the controls come from the
//! table and not from a hand-written list; ADR 0113 is why there is no Abyss
//! round trip.
//!
//! Collection editors (binds, profiles, rules, layouts, grants, range maps)
//! are M27. The `Map`-typed keys are listed in [`COLLECTION_KEYS`] and shown
//! read-only here; `tests/coverage.rs` fails on any other key without a control.
//!
//! Shape: header (status chip) -> hero (a status grid, or on Colors the palette
//! the terminal will draw) -> a filter bar -> one glass panel of grouped rows.
//! Grid, bar, panel: no two adjacent blocks share a silhouette.
//!
//! Accent ledger: one live yellow per page, the "changed" cell of the status
//! grid when this page holds a value you have set. On Colors the palette is
//! the hero and nothing is yellow. Everything else is white at 1.0 / 0.64 / 0.40.

use std::collections::HashMap;
use std::path::PathBuf;

use apoth_config::edit::apply_write;
use apoth_config::fs::RealFs;
use apoth_config::load::{default_input, load};
use apoth_config::meta::{Meta, META};
use apoth_config::model::{Config, Layer};
use apoth_config::overrides::parse_override_value;
use apoth_config::schema::TABLE;
use apoth_config::types::{Dv, Key, Reload, Ty, Value};
use apoth_config::value::kdl_str;
use iced::widget::{column, container, pick_list, row, text, text_input, Column, Space};
use iced::{Element, Length, Task, Theme};

use ec_ui::theme;
use ec_ui::tokens::{color, font, radius, size, space};
use ec_ui::widget::{
    badge, color_picker, edge_note, glass_panel as panel, hairline, list_row, micro_label, pill, pill_group,
    row_caption, status_cell, status_grid, swatch_button, value as mono, NumericSlider, Toggle,
};

use crate::app::{verdict_row, App, Message};
use crate::pane::Page;

/// Keys whose value is a map: a collection, edited on the M27 pages. Shown
/// read-only here so the setting is never hidden. `tests/coverage.rs` checks
/// that this is exactly the set of `Map`-typed rows.
pub const COLLECTION_KEYS: &[&str] = &[
    "font.map",
    "font.cell-width-override",
    "shell.env",
    "security.sequence",
    "socket.grants",
    crate::term_open::HANDLERS,
];

/// The widget a key is edited with, chosen from its type alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Control {
    Toggle,
    /// A bounded number short enough to drag.
    Slider {
        min: f64,
        max: f64,
        integral: bool,
    },
    /// Few variants: pills.
    Pills(&'static [&'static str]),
    /// Many variants: a dropdown.
    Dropdown(&'static [&'static str]),
    Colour,
    /// Sixteen colours: the palette strip and a field.
    Palette,
    /// Everything else: a validated field in KDL argument syntax.
    Text,
}

/// Above this many steps a number is typed, not dragged.
const DRAG_STEPS: f64 = 1000.0;
/// Above this many variants an enum is a dropdown.
const MAX_PILLS: usize = 4;

/// The one type-to-widget mapping of this section; `None` is a collection.
/// Exhaustive on purpose: a new `Ty` variant stops the build here.
pub fn control_for(ty: &Ty) -> Option<Control> {
    Some(match *ty {
        Ty::Bool => Control::Toggle,
        Ty::Int { min, max } if ((max - min) as f64) <= DRAG_STEPS => Control::Slider {
            min: min as f64,
            max: max as f64,
            integral: true,
        },
        Ty::Float { min, max } if max - min <= DRAG_STEPS => Control::Slider {
            min,
            max,
            integral: false,
        },
        Ty::Enum(v) if v.len() <= MAX_PILLS => Control::Pills(v),
        Ty::Enum(v) => Control::Dropdown(v),
        Ty::Color => Control::Colour,
        Ty::Palette16 => Control::Palette,
        Ty::Map { .. } => return None,
        Ty::Int { .. }
        | Ty::Float { .. }
        | Ty::Str
        | Ty::StrList
        | Ty::Font
        | Ty::FontFeatures
        | Ty::KeyChord
        | Ty::Duration { .. }
        | Ty::Path
        | Ty::Action
        | Ty::Regex
        | Ty::Template
        | Ty::Bytes { .. }
        | Ty::Argv
        | Ty::CodepointRanges
        | Ty::Match
        | Ty::StringSet
        | Ty::ThemeRef
        | Ty::Adjust => Control::Text,
    })
}

/// The Terminal page a `META` page string (`appearance/fonts`) belongs to.
/// `None` fails `tests/coverage.rs`: a key the table adds on a page this
/// section has no home for would otherwise be unreachable.
pub fn page_for(meta_page: &str) -> Option<Page> {
    Some(match meta_page {
        "appearance/fonts" | "text/unicode" => Page::TermFont,
        "appearance/colors" | "appearance/cursor" => Page::TermColors,
        "appearance/window" | "appearance/interface" | "appearance/tabs-panes" | "behaviour/windows" => {
            Page::TermWindow
        }
        "behaviour/scrollback" | "behaviour/selection" => Page::TermScrollback,
        "behaviour/mouse-keyboard" | "behaviour/notifications" => Page::TermInput,
        "behaviour/shell" | "behaviour/sessions" | "automation/macros" | "automation/layouts"
        | "automation/menus" | "advanced/files" => Page::TermShell,
        "keys/binds" => Page::TermBinds,
        "automation/profiles" => Page::TermProfiles,
        "automation/rules" => Page::TermRules,
        "security/clipboard" | "security/links" | "security/policy" | "security/socket" => Page::TermSecurity,
        "media/graphics" | "media/playback" | "advanced/rendering" | "accessibility" => Page::TermMedia,
        _ => return None,
    })
}

/// The Terminal page of a table key.
pub fn page_of_key(path: &str) -> Option<Page> {
    let i = apoth_config::schema::index_of(path)?;
    page_for(META[i].page)
}

/// The heading a key sits under on its page: its `META` group, with the two
/// font sub-blocks that would otherwise be one long list pulled out.
fn group_of(m: &Meta) -> &'static str {
    if m.path.starts_with("font.adjust-") {
        "Cell adjustments"
    } else if m.path.starts_with("font.builtin-glyphs.") {
        "Built-in glyphs"
    } else {
        m.group
    }
}

/// The rows of a page, in schema order.
fn page_rows(page: Page) -> impl Iterator<Item = (&'static Key, &'static Meta)> {
    TABLE
        .iter()
        .zip(META.iter())
        .filter(move |(_, m)| page_for(m.page) == Some(page))
}

/// What happened, from a control.
#[derive(Debug, Clone)]
pub enum Msg {
    Toggled(&'static str, bool),
    Chose(&'static str, String),
    Slid(&'static str, f64),
    Released(&'static str),
    Typed(&'static str, String),
    Commit(&'static str),
    Reset(&'static str),
    Picker(&'static str),
    Picked(&'static str, [u8; 4]),
    Filter(String),
    ChangedOnly,
    Reload,
    Lists(crate::term_lists::Msg),
    Open(crate::term_open::Msg),
}

/// The pages' state: the loaded config and what is being typed.
#[derive(Debug, Default)]
pub struct Terminal {
    pub(crate) cfg: Option<Config>,
    pub(crate) user_main: PathBuf,
    /// Errors from the last load, first message kept for the banner.
    errors: usize,
    first_error: Option<String>,
    /// A write that failed, in the editor's own words.
    write_error: Option<String>,
    /// The collection editors (M27): binds, profiles, rules.
    pub(crate) lists: crate::term_lists::Lists,
    /// The Open-with rules editor (`open.handlers`).
    pub(crate) open: crate::term_open::Open,
    drafts: HashMap<&'static str, String>,
    invalid: HashMap<&'static str, String>,
    /// A held slider's position.
    live: HashMap<&'static str, f64>,
    picker: Option<&'static str>,
    filter: String,
    changed_only: bool,
    /// The active Abyss theme's seeds; `None` until fetched (dark seeds then).
    seeds: Option<ec_theme::Seeds>,
}

impl Terminal {
    /// Read the layered config. A missing file is not an error: every key is
    /// at its default and the first write creates it.
    pub fn reload(&mut self) {
        self.read();
        self.drafts.clear();
        self.invalid.clear();
        self.live.clear();
    }

    /// Clear the filter, so a revealed row is on its page.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.changed_only = false;
    }

    /// Re-read the layers, leaving what is being typed alone.
    pub(crate) fn read(&mut self) {
        let fs = RealFs;
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let input = default_input(&fs, &home, xdg.as_deref());
        let result = load(&fs, &input, None);
        self.errors = result.error_count();
        self.first_error = result
            .diagnostics
            .iter()
            .find(|d| d.is_error())
            .map(|d| d.render());
        self.user_main = input.user.unwrap_or_default();
        self.cfg = Some(result.config);
        self.lists.refresh(&self.user_main);
        self.open.refresh(&self.user_main);
    }

    pub(crate) fn cfg(&self) -> Config {
        self.cfg.clone().unwrap_or_default()
    }

    /// Write (or, for `None`, unset) one key and re-read the layers.
    fn write(&mut self, path: &'static str, value: Option<Value>) {
        let cfg = self.cfg();
        match apply_write(&RealFs, &cfg, &self.user_main, &[], path, value.as_ref(), None) {
            Ok(_) => self.write_error = None,
            Err(e) => self.write_error = Some(format!("{path}: {e}")),
        }
        // Only the written key's draft is spent; the others are still being typed.
        self.drafts.remove(path);
        self.invalid.remove(path);
        self.live.remove(path);
        self.read();
    }

    /// Keys on `page` whose value is set in a file the user owns.
    fn changed_on(&self, page: Page) -> usize {
        let Some(cfg) = &self.cfg else { return 0 };
        page_rows(page).filter(|(k, _)| is_set(cfg, k.path)).count()
    }

    fn changed_total(&self) -> usize {
        let Some(cfg) = &self.cfg else { return 0 };
        TABLE.iter().filter(|k| is_set(cfg, k.path)).count()
    }

    fn filtering(&self) -> bool {
        self.changed_only || !self.filter.trim().is_empty()
    }
}

/// Is the key's winning value from somewhere other than the built-in default?
fn is_set(cfg: &Config, path: &str) -> bool {
    cfg.origin(path).is_some_and(|o| o.layer != Layer::Default)
}

/// Does the key match the filter box (label, path, keywords, doc)?
fn matches(filter: &str, key: &Key, m: &Meta) -> bool {
    let f = filter.trim().to_lowercase();
    f.is_empty()
        || f.split_whitespace().all(|w| {
            m.label.to_lowercase().contains(w)
                || key.path.contains(w)
                || key.doc.to_lowercase().contains(w)
                || m.keywords.iter().any(|k| k.to_lowercase().contains(w))
        })
}

/// A value as the text a field shows: KDL argument syntax, so what is typed
/// back is exactly what `parse_override_value` reads.
pub fn text_of(key: &Key, v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => match key.ty {
            Ty::Duration { .. } => ec_config_schema::format_duration((*i).max(0) as u64),
            Ty::Bytes { .. } => ec_config_schema::format_bytes((*i).max(0) as u64),
            _ => i.to_string(),
        },
        Value::Float(f) => trim_float(*f),
        Value::Str(s) => s.clone(),
        Value::Color(c) => apoth_config::lang::format_color(*c),
        Value::List(l) => l.iter().map(|s| kdl_str(s)).collect::<Vec<_>>().join(" "),
        Value::Map(m) => format!("{} entries", m.len()),
    }
}

fn trim_float(f: f64) -> String {
    let s = format!("{f:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// What an empty field stands for.
fn placeholder(key: &Key) -> String {
    match key.default {
        Dv::Computed { .. } => "Automatic".to_owned(),
        Dv::Null | Dv::Empty => "Not set".to_owned(),
        _ => {
            let d = text_of(key, &key.default_value());
            if d.is_empty() {
                "Not set".to_owned()
            } else {
                d
            }
        }
    }
}

/// Parse a typed draft; empty means "back to the default".
fn parse_draft(key: &Key, text: &str) -> Result<Option<Value>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let attempt = |t: &str| parse_override_value(key, t);
    if key.ty.is_arg_list() {
        // Bare `#rrggbb`, `150ms` and friends are not KDL: quote each word
        // that is not already a string.
        return attempt(&quote_words(text))
            .map(Some)
            .map_err(|why| clean_reason(key, &why));
    }
    // The loader's own reason beats "not valid KDL": ask again as one string.
    attempt(text)
        .or_else(|_| attempt(&kdl_str(text)))
        .map(Some)
        .map_err(|why| clean_reason(key, &why))
}

/// A typed value checked and parsed as the settings pages do; `None` when empty.
pub fn check_value(key: &Key, text: &str) -> Result<Option<Value>, String> {
    parse_draft(key, text)
}

/// Split on whitespace outside double quotes and quote every bare word.
fn quote_words(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut had = false;
    for ch in text.chars() {
        match ch {
            '"' => {
                in_q = !in_q;
                had = true;
                cur.push(ch);
            }
            c if c.is_whitespace() && !in_q => {
                if had {
                    out.push(std::mem::take(&mut cur));
                    had = false;
                }
            }
            c => {
                had = true;
                cur.push(c);
            }
        }
    }
    if had {
        out.push(cur);
    }
    out.iter()
        .map(|w| {
            if w.starts_with('"') && w.ends_with('"') && w.len() >= 2 {
                w.clone()
            } else {
                kdl_str(w)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn clean_reason(key: &Key, why: &str) -> String {
    if matches!(key.ty, Ty::Color) {
        return "expected a colour such as #rrggbb".to_owned();
    }
    if why != "not valid KDL" {
        return why.to_owned();
    }
    match key.ty {
        Ty::Color => "expected a colour such as #rrggbb".to_owned(),
        _ => "not a value this setting takes".to_owned(),
    }
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    let t = &mut app.terminal;
    match msg {
        Msg::Toggled(p, on) => t.write(p, Some(Value::Bool(on))),
        Msg::Chose(p, v) => t.write(p, Some(Value::Str(v))),
        Msg::Slid(p, v) => {
            t.live.insert(p, v);
            t.drafts.remove(p);
            t.invalid.remove(p);
        }
        Msg::Released(p) => {
            if let (Some(v), Some(key)) = (t.live.remove(p), apoth_config::schema::key(p)) {
                let value = match key.ty {
                    Ty::Int { .. } => Value::Int(v.round() as i64),
                    _ => Value::Float((v * 1000.0).round() / 1000.0),
                };
                t.write(p, Some(value));
            }
        }
        Msg::Typed(p, text) => {
            if let Some(key) = apoth_config::schema::key(p) {
                match parse_draft(key, &text) {
                    Ok(_) => {
                        t.invalid.remove(p);
                    }
                    Err(why) => {
                        t.invalid.insert(p, why);
                    }
                }
            }
            t.drafts.insert(p, text);
        }
        Msg::Commit(p) => {
            let Some(key) = apoth_config::schema::key(p) else {
                return Task::none();
            };
            let Some(draft) = t.drafts.get(p).cloned() else {
                return Task::none();
            };
            match parse_draft(key, &draft) {
                Ok(v) => t.write(p, v),
                Err(why) => {
                    t.invalid.insert(p, why);
                }
            }
        }
        Msg::Reset(p) => t.write(p, None),
        Msg::Picker(p) => t.picker = if t.picker == Some(p) { None } else { Some(p) },
        Msg::Picked(p, c) => {
            let hex = apoth_config::lang::format_color(c);
            t.invalid.remove(p);
            t.drafts.insert(p, hex);
        }
        Msg::Filter(f) => t.filter = f,
        Msg::ChangedOnly => t.changed_only = !t.changed_only,
        Msg::Reload => t.reload(),
        Msg::Lists(m) => return crate::term_lists::update(app, m),
        Msg::Open(m) => return crate::term_open::update(app, m),
    }
    Task::none()
}

/// Re-read the active theme's seeds, which the terminal's unset colours follow.
pub fn sync_theme(app: &mut App) {
    if let Some(info) = app
        .conn
        .call("get_theme", serde_json::json!({}))
        .ok()
        .and_then(|v| ec_ui::ipc::parse_theme(&v))
    {
        app.terminal.seeds = Some(info.seeds);
    }
}

fn msg(m: Msg) -> Message {
    Message::Terminal(m)
}

/// The header chip: how much of the file this page holds.
pub fn status(app: &App, page: Page) -> (String, String) {
    if matches!(page, Page::TermBinds | Page::TermProfiles | Page::TermRules) {
        return crate::term_lists::status(app, page);
    }
    let t = &app.terminal;
    let n = t.changed_on(page);
    let state = match n {
        0 => "all defaults".to_owned(),
        1 => "1 value set".to_owned(),
        n => format!("{n} values set"),
    };
    let measure = if t.errors > 0 {
        format!("{} problems in the file", t.errors)
    } else {
        "apotheosis.kdl".to_owned()
    };
    (state, measure)
}

pub fn blocks(app: &App, page: Page) -> Vec<Element<'_, Message, Theme>> {
    if matches!(page, Page::TermBinds | Page::TermProfiles | Page::TermRules) {
        return crate::term_lists::blocks(app, page);
    }
    let t = &app.terminal;
    let mut out: Vec<Element<'_, Message, Theme>> = Vec::new();
    if t.errors > 0 {
        out.push(edge_note(
            &format!("{} problems in apotheosis.kdl", t.errors),
            t.first_error
                .as_deref()
                .unwrap_or("The last good values stay live."),
            color::danger(),
        ));
    }
    if let Some(e) = &t.write_error {
        out.push(edge_note("Could not write apotheosis.kdl", e, color::danger()));
    }
    out.push(hero(app, page));
    out.push(filter_bar(t));
    out.push(rows_panel(app, page));
    out
}

// ---- hero ---------------------------------------------------------------

fn hero(app: &App, page: Page) -> Element<'_, Message, Theme> {
    if page == Page::TermColors {
        return palette_hero(app);
    }
    let t = &app.terminal;
    let total = page_rows(page).count();
    let set = t.changed_on(page);
    let rows: Vec<_> = page_rows(page).collect();
    let slow = rows.iter().filter(|(k, _)| k.reload != Reload::Live).count();
    let file = if t.user_main.as_os_str().is_empty() {
        "no file".to_owned()
    } else {
        t.user_main
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
    };
    let exists = t.user_main.exists();
    let dir = t
        .user_main
        .parent()
        .map(|p| tilde(&p.to_string_lossy()))
        .unwrap_or_default();
    let cells = vec![
        status_cell(
            &file,
            if exists { "In use" } else { "Not created yet" },
            &dir,
            false,
        ),
        // The count of keys you have set; the slider knob below is the page's yellow.
        status_cell(
            "set by you",
            &format!("{set} of {total}"),
            "the rest follow the defaults",
            false,
        ),
        status_cell(
            "takes effect",
            if slow == 0 { "Live" } else { "Mostly live" },
            &if slow == 0 {
                "the running terminal re-reads it".to_owned()
            } else {
                format!("{slow} need a new pane, window or restart")
            },
            false,
        ),
    ];
    status_grid(cells, 3)
}

fn tilde(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_owned(),
    }
}

/// Colors: the palette the terminal draws, over the background it draws it on.
/// A colour unset in the file follows the active theme (`ec_theme::terminal_scheme`,
/// the same derivation the terminal runs); a set `colors.*` key overrides it.
fn palette_hero(app: &App) -> Element<'_, Message, Theme> {
    let t = &app.terminal;
    let cfg = t.cfg();
    let rgba = |c: [u8; 4]| iced::Color::from_rgba8(c[0], c[1], c[2], f32::from(c[3]) / 255.0);
    let seeds = t.seeds.unwrap_or(ec_theme::DARK_SEEDS);
    let scheme = ec_theme::terminal_scheme(&seeds);
    let bg = cfg
        .color("colors.background")
        .map(rgba)
        .unwrap_or_else(|| color::to_iced(scheme.background));
    let fg = cfg
        .color("colors.foreground")
        .map(rgba)
        .unwrap_or_else(|| color::to_iced(scheme.foreground));
    let set: Vec<[u8; 4]> = cfg
        .list("colors.palette")
        .iter()
        .filter_map(|h| apoth_config::lang::parse_color(h))
        .collect();
    let from_file = set.len() == 16;
    let ansi: Vec<iced::Color> = if from_file {
        set.iter().map(|c| rgba(*c)).collect()
    } else {
        scheme.ansi.iter().map(|c| color::to_iced(*c)).collect()
    };
    let mut strip = iced::widget::Row::new().spacing(space::PILL_GAP);
    for c in ansi.iter().copied() {
        strip = strip.push(
            container(Space::new())
                .width(Length::FillPortion(1))
                .height(Length::Fixed(space::CHIP_H))
                .style(move |_t: &Theme| container::Style {
                    background: Some(iced::Background::Color(c)),
                    border: iced::Border {
                        color: color::border_strong(),
                        width: space::HAIRLINE,
                        radius: radius::INSET.into(),
                    },
                    ..container::Style::default()
                }),
        );
    }
    let word = |t: &'static str, c: iced::Color| text(t).font(font::DATA).size(size::MONO).color(c);
    let listing = row![
        word("user@host ~ % ls", fg),
        word("Documents", ansi[12]),
        word("build.sh", ansi[10]),
        word("core.dump", ansi[9]),
        word("notes.txt", fg),
    ]
    .spacing(space::CONTROL_GAP);
    let sample = container(
        column![
            listing,
            text(if from_file {
                "palette from apotheosis.kdl"
            } else {
                "follows the active theme"
            })
            .font(font::DATA)
            .size(size::MONO)
            .color(fg.scale_alpha(0.6)),
        ]
        .spacing(space::ROW_Y),
    )
    .padding(space::CARD)
    .width(Length::Fill)
    .style(move |_t: &Theme| container::Style {
        background: Some(iced::Background::Color(bg)),
        border: iced::Border {
            color: color::border_strong(),
            width: space::HAIRLINE,
            radius: radius::INSET.into(),
        },
        ..container::Style::default()
    });
    panel(
        app.glass_radius,
        app.hairline_cards,
        column![micro_label("What the terminal draws"), sample, strip].spacing(space::ROW_Y),
    )
}

// ---- filter -------------------------------------------------------------

fn filter_bar(t: &Terminal) -> Element<'_, Message, Theme> {
    let field = text_input("Search every terminal setting", &t.filter)
        .on_input(|s| msg(Msg::Filter(s)))
        .font(font::UI)
        .size(size::BODY)
        .style(theme::eclipse_input);
    row![
        container(field).width(Length::Fixed(space::FIELD_W * 1.6)),
        pill("Changed only", t.changed_only, msg(Msg::ChangedOnly)),
        Space::new().width(Length::Fill),
        container(mono(&format!("{} set in the file", t.changed_total())))
            .width(Length::Fixed(space::FIELD_W))
            .align_x(iced::Alignment::End),
    ]
    .spacing(space::CONTROL_GAP)
    .align_y(iced::Alignment::Center)
    .into()
}

// ---- rows ---------------------------------------------------------------

fn rows_panel(app: &App, page: Page) -> Element<'_, Message, Theme> {
    let t = &app.terminal;
    let cfg = t.cfg();
    // (heading, rows) in first-seen order.
    let mut groups: Vec<(String, Vec<Element<'_, Message, Theme>>)> = Vec::new();
    let filtering = t.filtering();
    let mut shown = 0usize;
    let pages: Vec<Page> = if filtering {
        Page::terminal_pages().to_vec()
    } else {
        vec![page]
    };
    for p in pages {
        for (key, m) in page_rows(p) {
            if filtering && (!matches(&t.filter, key, m) || (t.changed_only && !is_set(&cfg, key.path))) {
                continue;
            }
            let group = if filtering {
                p.title().to_owned()
            } else {
                group_of(m).to_owned()
            };
            shown += 1;
            let el = key_row(t, &cfg, key, m);
            match groups.iter().position(|(g, _)| *g == group) {
                Some(i) => groups[i].1.push(el),
                None => groups.push((group, vec![el])),
            }
        }
    }
    if shown == 0 {
        return edge_note(
            "Nothing matches",
            if t.changed_only {
                "No value in your file matches. Everything else follows the defaults."
            } else {
                "No terminal setting matches that search."
            },
            color::neutral(),
        );
    }
    let labelled = groups.len() > 1;
    let mut col = Column::new().spacing(space::ROW_Y);
    for (i, (group, rows)) in groups.into_iter().enumerate() {
        if i > 0 {
            col = col.push(hairline());
        }
        if labelled {
            col = col.push(micro_label(&group));
        }
        for r in rows {
            col = col.push(r);
        }
    }
    panel(app.glass_radius, app.hairline_cards, col)
}

/// One key: its label, its control, and what it knows about itself — where the
/// value comes from, whether it needs more than a reload, and the way back.
fn key_row<'a>(
    t: &'a Terminal,
    cfg: &Config,
    key: &'static Key,
    m: &'static Meta,
) -> Element<'a, Message, Theme> {
    let path = key.path;
    if path == crate::term_open::HANDLERS {
        return crate::term_open::view(t, cfg, m.label);
    }
    let Some(control) = control_for(&key.ty) else {
        let n = cfg.get(path).and_then(Value::as_map).map_or(0, <[_]>::len);
        return list_row(
            m.label,
            row![
                mono(&match n {
                    0 => "none".to_owned(),
                    1 => "1 entry".to_owned(),
                    n => format!("{n} entries"),
                }),
                badge("editor coming soon"),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(iced::Alignment::Center),
        );
    };
    let value = cfg.get(path).cloned().unwrap_or(Value::Null);
    // At a computed default: no value of its own, whatever the environment says.
    let auto = key.default.is_computed() && value.is_null();
    let widget: Element<'a, Message, Theme> = match control {
        Control::Toggle => Toggle::new(value.as_bool().unwrap_or(false), move |on| {
            msg(Msg::Toggled(path, on))
        })
        .into(),
        Control::Pills(values) => {
            let cur = value.as_str().unwrap_or_default().to_owned();
            let mut pills: Vec<Element<'a, Message, Theme>> = Vec::new();
            // An automatic default is a state of its own, and the way back to it.
            if auto {
                pills.push(pill("Automatic", value.is_null(), msg(Msg::Reset(path))));
            }
            pills.extend(
                values
                    .iter()
                    .map(|v| pill(v, *v == cur, msg(Msg::Chose(path, (*v).to_owned())))),
            );
            pill_group(pills)
        }
        Control::Dropdown(values) => pick_list(
            values.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>(),
            value.as_str().map(str::to_owned),
            move |v| msg(Msg::Chose(path, v)),
        )
        .placeholder(if auto { "Automatic" } else { "Not set" })
        .font(font::UI)
        .text_size(size::BODY)
        .style(dropdown_style)
        .menu_style(menu_style)
        .into(),
        Control::Slider { min, max, integral } => {
            let now = t
                .live
                .get(path)
                .copied()
                .or_else(|| value.as_float())
                .unwrap_or(min)
                .clamp(min, max);
            let shown = t.drafts.get(path).cloned().unwrap_or_else(|| {
                if auto && !t.live.contains_key(path) {
                    "auto".to_owned()
                } else if integral {
                    format!("{}", now.round() as i64)
                } else {
                    trim_float(now)
                }
            });
            let step = if integral {
                1.0
            } else if max - min <= 10.0 {
                0.05
            } else {
                0.5
            };
            let slider = NumericSlider::new(
                min..=max,
                now,
                shown,
                move |v| msg(Msg::Slid(path, v)),
                move |s| msg(Msg::Typed(path, s)),
            )
            .step(step)
            .on_release(msg(Msg::Released(path)))
            .on_commit(msg(Msg::Commit(path)))
            .invalid(t.invalid.contains_key(path));
            if key.default.is_computed() {
                row![pill("Automatic", auto, msg(Msg::Reset(path))), slider]
                    .spacing(space::CONTROL_GAP)
                    .align_y(iced::Alignment::Center)
                    .into()
            } else {
                slider.into()
            }
        }
        Control::Text | Control::Palette => text_field(t, key, &value, None),
        Control::Colour => {
            let c = t
                .drafts
                .get(path)
                .and_then(|d| apoth_config::lang::parse_color(d))
                .or_else(|| value.as_color());
            // Always the swatch slot, so the field's place in the tree never
            // changes while typing and keeps keyboard focus.
            text_field(t, key, &value, Some(c))
        }
    };
    let mut trail = iced::widget::Row::new()
        .spacing(space::CONTROL_GAP)
        .align_y(iced::Alignment::Center);
    if let Some(o) = cfg.origin(path).filter(|o| o.layer != Layer::Default) {
        let label = if o.when {
            format!("{} \u{b7} when", o.layer.name())
        } else {
            o.layer.name().to_owned()
        };
        trail = trail.push(badge(&label));
    } else if key.default.is_computed() {
        trail = trail.push(badge("automatic"));
    }
    if key.reload != Reload::Live {
        trail = trail.push(badge(key.reload.name()));
    }
    // The Reset slot is always there, so nothing shifts when a value is set.
    trail = trail.push(if cfg.origin(path).is_some_and(|o| o.layer == Layer::User) {
        pill("Reset", false, msg(Msg::Reset(path)))
    } else {
        container(
            text("Reset")
                .font(font::UI_MEDIUM)
                .size(size::BODY_SMALL)
                .color(iced::Color::TRANSPARENT),
        )
        .padding([space::PILL_Y, space::PILL_X])
        .into()
    });
    let trail = container(trail)
        .width(Length::Fixed(space::FIELD_W))
        .align_x(iced::Alignment::End);
    let reason = t.invalid.get(path).map_or("", String::as_str);
    let mut line = column![list_row(
        m.label,
        row![widget, trail]
            .spacing(space::CONTROL_GAP)
            .align_y(iced::Alignment::Center)
    )];
    if let Control::Colour = control {
        if t.picker == Some(path) {
            let c = t
                .drafts
                .get(path)
                .and_then(|d| apoth_config::lang::parse_color(d))
                .or_else(|| value.as_color())
                .unwrap_or([0, 0, 0, 255]);
            line = line.push(
                container(color_picker(
                    c,
                    true,
                    move |c| msg(Msg::Picked(path, c)),
                    msg(Msg::Commit(path)),
                ))
                .padding([0.0, space::CARD]),
            );
        }
    }
    let mut line = line.push(refusal_line(reason));
    if auto {
        if let Dv::Computed { doc, .. } = key.default {
            line = line.push(row_caption(&format!("Automatic: {doc}")));
        }
    }
    if matches!(control, Control::Palette) {
        line = line.push(row_caption(
            "Sixteen colours, in ANSI order, as #rrggbb separated by spaces.",
        ));
    }
    container(line)
        .id(crate::app::row_id(path))
        .width(Length::Fill)
        .into()
}

/// A dropdown field: the text input's glass fill and rim, brighter on hover
/// and while open. No accent fill.
pub(crate) fn dropdown_style(_t: &Theme, status: pick_list::Status) -> pick_list::Style {
    let (fill, rim) = match status {
        pick_list::Status::Active => (color::cell(), color::cell_rim()),
        pick_list::Status::Hovered => (color::cell_hover(), color::cell_rim_hover()),
        pick_list::Status::Opened { .. } => (color::cell_press(), color::border_strong()),
    };
    pick_list::Style {
        text_color: color::text(),
        placeholder_color: color::text_tertiary(),
        handle_color: color::text_secondary(),
        background: iced::Background::Color(fill),
        border: iced::Border {
            color: rim,
            width: space::HAIRLINE,
            radius: radius::INSET.into(),
        },
    }
}

/// The popup: the menu material, rows brighten with a crisp rim, never gold.
pub(crate) fn menu_style(_t: &Theme) -> iced::overlay::menu::Style {
    iced::overlay::menu::Style {
        background: iced::Background::Color(color::menu_ground()),
        border: iced::Border {
            color: color::border_strong(),
            width: space::HAIRLINE,
            radius: radius::INSET.into(),
        },
        text_color: color::text_secondary(),
        selected_text_color: color::text(),
        selected_background: iced::Background::Color(color::cell_hover()),
        shadow: iced::Shadow::default(),
    }
}

fn refusal_line<'a>(reason: &str) -> Element<'a, Message, Theme> {
    let line = container(
        text(reason.to_owned())
            .font(font::UI)
            .size(size::BODY_SMALL)
            .style(theme::text_danger),
    );
    if reason.is_empty() {
        line.max_height(0.0).into()
    } else {
        line.padding(
            iced::Padding::ZERO
                .left(space::CARD)
                .right(space::CARD)
                .bottom(space::ROW_Y),
        )
        .into()
    }
}

/// A validated field, optionally with its colour's swatch in front.
fn text_field<'a>(
    t: &'a Terminal,
    key: &'static Key,
    value: &Value,
    swatch: Option<Option<[u8; 4]>>,
) -> Element<'a, Message, Theme> {
    let path = key.path;
    let shown = t.drafts.get(path).cloned().unwrap_or_else(|| text_of(key, value));
    let invalid = t.invalid.contains_key(path);
    let mut input = text_input(&placeholder(key), &shown)
        .on_input(move |s| msg(Msg::Typed(path, s)))
        .font(font::UI)
        .size(size::BODY)
        .style(if invalid {
            theme::eclipse_input_invalid
        } else {
            theme::eclipse_input
        });
    if !invalid {
        input = input.on_submit(msg(Msg::Commit(path)));
    }
    let field = container(verdict_row(input, invalid))
        .width(Length::Fill)
        .max_width(space::FIELD_W + space::CONTROL_GAP + space::VERDICT_W);
    match swatch {
        Some(c) => row![
            swatch_button(
                c.map_or_else(color::cell, |c| {
                    iced::Color::from_rgba8(c[0], c[1], c[2], f32::from(c[3]) / 255.0)
                }),
                t.picker == Some(path),
                msg(Msg::Picker(path)),
            ),
            field
        ]
        .spacing(space::CONTROL_GAP)
        .align_y(iced::Alignment::Center)
        .into(),
        None => field.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_control_or_is_a_collection() {
        for (k, m) in TABLE.iter().zip(META.iter()) {
            assert_eq!(k.path, m.path);
            let collection = COLLECTION_KEYS.contains(&k.path);
            assert_eq!(control_for(&k.ty).is_none(), collection, "{}", k.path);
            assert!(
                page_for(m.page).is_some(),
                "{} is on no terminal page ({})",
                k.path,
                m.page
            );
        }
        for c in COLLECTION_KEYS {
            assert!(apoth_config::schema::key(c).is_some(), "{c} is not a key");
        }
    }

    #[test]
    fn typed_text_round_trips_through_the_loader_syntax() {
        let key = |p| apoth_config::schema::key(p).unwrap();
        for p in [
            "cursor.blink-interval",
            "scrollback.bytes",
            "font.family",
            "bell.command",
            "mouse.selection-override",
            "colors.background",
            "font.features",
            "window.title",
            "theme",
        ] {
            let k = key(p);
            let v = match k.default_value() {
                Value::Null => continue,
                v => v,
            };
            let text = text_of(k, &v);
            if text.is_empty() {
                continue;
            }
            assert_eq!(parse_draft(k, &text), Ok(Some(v)), "{p}: {text}");
        }
    }

    #[test]
    fn a_bad_draft_is_refused_and_an_empty_one_resets() {
        let k = apoth_config::schema::key("cursor.blink-interval").unwrap();
        assert!(parse_draft(k, "banana").is_err());
        assert_eq!(parse_draft(k, "  "), Ok(None));
        assert_eq!(parse_draft(k, "250ms"), Ok(Some(Value::Int(250))));
    }
}

#[cfg(test)]
mod draft_tests {
    use super::*;

    #[test]
    fn bare_hex_lists_and_colours_parse_with_clear_reasons() {
        let pal = apoth_config::schema::key("colors.palette").unwrap();
        let sixteen = (0..16)
            .map(|i| format!("#{i:02x}{i:02x}{i:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(matches!(parse_draft(pal, &sixteen), Ok(Some(Value::List(l))) if l.len() == 16));
        let cur = apoth_config::schema::key("colors.cursor").unwrap();
        assert!(parse_draft(cur, "#ff8800").unwrap().is_some());
        assert_eq!(
            parse_draft(cur, "#zz12").unwrap_err(),
            "expected a colour such as #rrggbb"
        );
    }
}
