// SPDX-License-Identifier: AGPL-3.0-only
//! Per-display pictures on the Wallpaper page (`wallpaper.output`).
//!
//! The compositor keeps one global picture (`wallpaper.path`, `.mode`,
//! `.color`) and a list of per-connector overrides. The list comes back from
//! `get_config` under `collections."wallpaper.output"`; every write is a
//! `set_config_collection` call on one connector. An upsert replaces the whole
//! override block, so each write carries the block's other fields as they are,
//! and a `null` field inherits the global value. A block with nothing left in
//! it is removed, which is what "Use the same as all displays" does.
//!
//! Every change applies live, as the Display pane's do, and is undoable: the
//! toast takes back the block as it was. A picture's size is read from its
//! file header, so the page can say when a picture is much smaller than the
//! display it is set to cover. Nothing here decodes an image.

mod view;

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use iced::Task;
use serde_json::{json, Value};

use crate::app::{App, Message, Undoable};
use crate::conn::Problem;
use crate::output::Output;

pub use view::{card, hero, status};

/// The collection node, as `get_config` and `set_config_collection` name it.
pub const COLLECTION: &str = "wallpaper.output";
/// The global keys an override inherits.
pub const PATH: &str = "wallpaper.path";
pub const MODE: &str = "wallpaper.mode";
pub const COLOR: &str = "wallpaper.color";
/// What the compositor does with a global key nobody has set.
pub const DEFAULT_MODE: &str = "fill";
/// A picture needing more than this to cover its display is called out. Above
/// about 2x the enlargement is visible as softness, so a note is worth a line.
pub const WARN_SCALE: f64 = 2.0;

/// One connector's override block. `None` is a field that inherits.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    pub name: String,
    pub path: Option<String>,
    pub mode: Option<String>,
    pub color: Option<String>,
}

impl Entry {
    /// A block with no field set is no override at all.
    fn is_empty(&self) -> bool {
        self.path.is_none() && self.mode.is_none() && self.color.is_none()
    }
}

/// A display in the page's list: a connected output, or an unplugged one
/// that still has an override (so the override can be changed or removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub connected: bool,
}

/// One edit to a connector's block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Path(Option<String>),
    Mode(Option<String>),
    Color(Option<String>),
    /// "Use the same as all displays": the whole block goes.
    Clear,
}

/// What one write sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Upsert(Entry),
    Remove(String),
}

/// What undoing one change writes back: the block as it was, or none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restore {
    pub name: String,
    pub before: Option<Entry>,
}

/// The page's state. Read on every reload and on every config change, so it
/// is never older than the compositor's answer.
#[derive(Debug, Clone, Default)]
pub struct Pane {
    /// Every override the compositor reports, in file order.
    pub entries: Vec<Entry>,
    /// The displays the page lists: connected first, then unplugged overrides.
    pub listed: Vec<Listed>,
    /// Header-read size of each picture a listed display uses, by path. A
    /// picture that cannot be read is absent.
    pub sizes: HashMap<String, (u32, u32)>,
    /// The display the page is about; `None` is "All displays".
    pub selected: Option<String>,
    /// The display whose colour picker is open.
    pub picker: Option<String>,
    /// A colour held in the picker and not yet written. It is written on
    /// release, so a drag is one write and not one per pixel.
    pub drag: Option<(String, [u8; 4])>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// Pick a display; `None` is "All displays".
    Select(Option<String>),
    /// Open the file chooser for this display's picture.
    Browse(String),
    /// The chooser answered: a path, a cancel, or why it could not open.
    Browsed(String, Result<Option<std::path::PathBuf>, String>),
    /// Set this display's sizing mode.
    Mode(String, String),
    /// Open or close this display's colour picker.
    Picker(String),
    /// The picker moved to this colour.
    Drag(String, [u8; 4]),
    /// The picker was let go: write the colour held.
    Release(String),
    /// Use the same as all displays.
    Clear(String),
}

impl Pane {
    /// The entry for a connector, if it has an override.
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// The display the card is about, when there is one to show.
    pub fn card_for(&self) -> Option<&str> {
        let name = self.selected.as_deref()?;
        self.listed.iter().any(|l| l.name == name).then_some(name)
    }
}

/// Read the `wallpaper.output` collection out of a `get_config` reply's
/// `collections`. The connector field is `name` in the contract; the
/// compositor's read spells it `output`, so both are taken. An entry without
/// one is skipped, not an error.
pub fn parse(collections: &Value) -> Vec<Entry> {
    collections
        .get(COLLECTION)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(entry_of).collect())
        .unwrap_or_default()
}

fn entry_of(v: &Value) -> Option<Entry> {
    let name = v
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| v.get("output").and_then(Value::as_str))?;
    let field = |k: &str| {
        v.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Some(Entry {
        name: name.to_owned(),
        path: field("path"),
        mode: field("mode"),
        color: field("color"),
    })
}

/// The display list: every connected connector, then each unplugged one that
/// has an override. A connector is listed once.
pub fn listed(connected: &[&str], entries: &[Entry]) -> Vec<Listed> {
    let mut out: Vec<Listed> = Vec::new();
    for name in connected {
        if !out.iter().any(|l| l.name == *name) {
            out.push(Listed {
                name: (*name).to_owned(),
                connected: true,
            });
        }
    }
    for e in entries {
        if !out.iter().any(|l| l.name == e.name) {
            out.push(Listed {
                name: e.name.clone(),
                connected: false,
            });
        }
    }
    out
}

/// The block a change leaves a connector with, as one write: `None` when the
/// change is a no-op (nothing is sent), otherwise an upsert of the whole
/// block, or a removal when nothing is left in it.
pub fn plan(current: Option<&Entry>, name: &str, change: Change) -> Option<Plan> {
    let mut next = current.cloned().unwrap_or_else(|| Entry {
        name: name.to_owned(),
        ..Entry::default()
    });
    match change {
        Change::Path(v) => next.path = v,
        Change::Mode(v) => next.mode = v,
        Change::Color(v) => next.color = v,
        Change::Clear => {
            next.path = None;
            next.mode = None;
            next.color = None;
        }
    }
    match current {
        None if next.is_empty() => None,
        None => Some(Plan::Upsert(next)),
        Some(c) if *c == next => None,
        Some(_) if next.is_empty() => Some(Plan::Remove(name.to_owned())),
        Some(_) => Some(Plan::Upsert(next)),
    }
}

/// The `set_config_collection` params for a plan. Every field of an upsert is
/// sent, `null` included: an absent key would be read as "keep", and the
/// block is replaced whole.
pub fn params(plan: &Plan) -> Value {
    match plan {
        Plan::Upsert(e) => json!({
            "collection": COLLECTION,
            "op": "upsert",
            "name": e.name,
            "entry": { "path": e.path, "mode": e.mode, "color": e.color },
        }),
        Plan::Remove(name) => json!({
            "collection": COLLECTION,
            "op": "remove",
            "name": name,
        }),
    }
}

/// The write that takes an undo back.
pub fn restore_plan(r: &Restore) -> Plan {
    match &r.before {
        Some(e) => Plan::Upsert(e.clone()),
        None => Plan::Remove(r.name.clone()),
    }
}

/// The picture's pixel size from its file header, for PNG and JPEG only. A
/// file that is missing, unreadable or another format is `None`, and the
/// page shows nothing for it.
pub fn image_size(path: &Path) -> Option<(u32, u32)> {
    let mut file = File::open(path).ok()?;
    header_size(&mut file)
}

/// [`image_size`] over any seekable reader.
pub fn header_size<R: Read + Seek>(r: &mut R) -> Option<(u32, u32)> {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    r.seek(SeekFrom::Start(0)).ok()?;
    let mut sig = [0u8; 8];
    r.read_exact(&mut sig).ok()?;
    if sig == PNG {
        // length(4), "IHDR"(4), width(4), height(4), all big-endian.
        let mut ihdr = [0u8; 16];
        r.read_exact(&mut ihdr).ok()?;
        if &ihdr[4..8] != b"IHDR" {
            return None;
        }
        let w = u32::from_be_bytes(ihdr[8..12].try_into().ok()?);
        let h = u32::from_be_bytes(ihdr[12..16].try_into().ok()?);
        return dims(w, h);
    }
    if sig[..2] == [0xFF, 0xD8] {
        return jpeg_size(r);
    }
    None
}

/// Walk the JPEG segments to the first frame header (SOF0..SOF15, but not the
/// DHT, JPG and DAC markers that share the range). Each step consumes at least
/// two bytes, so a short or corrupt file ends in a read error, not a loop.
fn jpeg_size<R: Read + Seek>(r: &mut R) -> Option<(u32, u32)> {
    r.seek(SeekFrom::Start(2)).ok()?;
    let mut b = [0u8; 1];
    loop {
        r.read_exact(&mut b).ok()?;
        if b[0] != 0xFF {
            return None;
        }
        // Fill bytes may pad a marker.
        loop {
            r.read_exact(&mut b).ok()?;
            if b[0] != 0xFF {
                break;
            }
        }
        let m = b[0];
        match m {
            // Standalone markers carry no length.
            0x01 | 0xD0..=0xD7 => continue,
            // End of image or start of scan before any frame header.
            0xD9 | 0xDA => return None,
            _ => {}
        }
        let mut len = [0u8; 2];
        r.read_exact(&mut len).ok()?;
        let len = i64::from(u16::from_be_bytes(len));
        if len < 2 {
            return None;
        }
        let frame = (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC);
        if frame {
            // precision(1), height(2), width(2).
            let mut sof = [0u8; 5];
            r.read_exact(&mut sof).ok()?;
            let h = u32::from(u16::from_be_bytes([sof[1], sof[2]]));
            let w = u32::from(u16::from_be_bytes([sof[3], sof[4]]));
            return dims(w, h);
        }
        r.seek(SeekFrom::Current(len - 2)).ok()?;
    }
}

fn dims(w: u32, h: u32) -> Option<(u32, u32)> {
    (w > 0 && h > 0).then_some((w, h))
}

/// How far a picture is enlarged to cover a display: the larger of the two
/// ratios, so the picture fills both ways. A picture bigger than the display
/// is below 1.
pub fn cover_scale(picture: (u32, u32), display: (i64, i64)) -> f64 {
    let (pw, ph) = (f64::from(picture.0), f64::from(picture.1));
    let (dw, dh) = (display.0 as f64, display.1 as f64);
    (dw / pw).max(dh / ph)
}

/// The enlargement to call out, or `None` when it is at or under
/// [`WARN_SCALE`].
pub fn scale_warning(picture: (u32, u32), display: (i64, i64)) -> Option<f64> {
    let s = cover_scale(picture, display);
    (s > WARN_SCALE).then_some(s)
}

/// The picture a display shows: its own, else the global one. `None` when
/// the global path is unset (the colour alone).
pub fn effective_path(app: &App, name: &str) -> Option<String> {
    let own = app.wall.entry(name).and_then(|e| e.path.clone());
    own.or_else(|| {
        app.key(PATH)
            .and_then(|k| k.value.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    })
}

/// Re-read the overrides and the picture sizes. Runs on every reload and
/// config change, never from a `view`: a header read is a few bytes, but it
/// is still a file.
pub(crate) fn read(app: &mut App) {
    app.wall.entries = parse(&app.conn.collections);
    let connected: Vec<&str> = app.outputs.iter().map(|o| o.name.as_str()).collect();
    let listed = listed(&connected, &app.wall.entries);
    app.wall.listed = listed;
    if app
        .wall
        .selected
        .as_ref()
        .is_some_and(|s| !app.wall.listed.iter().any(|l| &l.name == s))
    {
        app.wall.selected = None;
    }
    if app
        .wall
        .picker
        .as_ref()
        .is_some_and(|p| app.wall.selected.as_ref() != Some(p))
    {
        app.wall.picker = None;
    }
    let mut sizes = HashMap::new();
    for l in &app.wall.listed {
        if let Some(path) = effective_path(app, &l.name) {
            if let Some(size) = image_size(Path::new(&path)) {
                sizes.insert(path, size);
            }
        }
    }
    app.wall.sizes = sizes;
}

/// The output a listed connector is, if it is connected.
pub fn output_of<'a>(app: &'a App, name: &str) -> Option<&'a Output> {
    app.outputs.iter().find(|o| o.name == name)
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Select(sel) => {
            app.wall.selected = sel;
            app.wall.picker = None;
            app.wall.drag = None;
        }
        Msg::Browse(name) => {
            return Task::perform(crate::portal::pick_image(), move |r| {
                Message::Wall(Msg::Browsed(name.clone(), r))
            });
        }
        Msg::Browsed(name, result) => match result {
            Ok(Some(p)) => write(app, &name, Change::Path(Some(p.to_string_lossy().into_owned()))),
            Ok(None) => {}
            Err(e) => app.banner = Some(Problem::Other(e)),
        },
        Msg::Mode(name, mode) => write(app, &name, Change::Mode(Some(mode))),
        Msg::Picker(name) => {
            app.wall.picker = if app.wall.picker.as_ref() == Some(&name) {
                None
            } else {
                Some(name)
            };
        }
        Msg::Drag(name, rgba) => app.wall.drag = Some((name, rgba)),
        Msg::Release(name) => {
            if let Some((held, rgba)) = app.wall.drag.clone() {
                if held == name {
                    app.wall.drag = None;
                    let hex = crate::schema::hex(rgba, false);
                    write(app, &name, Change::Color(Some(hex)));
                }
            }
        }
        Msg::Clear(name) => write(app, &name, Change::Clear),
    }
    Task::none()
}

/// One change, live: send it, toast it with its undo, and re-read. A refusal
/// is the banner, with the compositor's own words (`entry.mode: …`).
fn write(app: &mut App, name: &str, change: Change) {
    let current = app.wall.entry(name).cloned();
    let note = describe(name, &change);
    let Some(p) = plan(current.as_ref(), name, change) else {
        return;
    };
    match send(app, &p) {
        Ok(()) => {
            app.banner = None;
            app.undo.push(
                note,
                Undoable::Wall(Restore {
                    name: name.to_owned(),
                    before: current,
                }),
            );
            app.reload();
        }
        Err(problem) => app.banner = Some(problem),
    }
}

/// Debug builds only: `SETTINGS_PREVIEW_WALL=<connector>` opens the page on
/// that display's card, so the card can be screenshotted without a pointer.
/// A name the page does not list shows the "All displays" view.
#[cfg(debug_assertions)]
pub fn preview_env(app: &mut App) {
    if let Ok(name) = std::env::var("SETTINGS_PREVIEW_WALL") {
        app.wall.selected = Some(name);
    }
}

/// Take an override change back. Returns the note the toast says afterwards.
pub fn undo(app: &mut App, note: &str, r: Restore) -> Result<String, Problem> {
    send(app, &restore_plan(&r))?;
    app.banner = None;
    app.reload();
    Ok(format!("Undone: {note}"))
}

fn send(app: &mut App, plan: &Plan) -> Result<(), Problem> {
    let reply = app.conn.call("set_config_collection", params(plan))?;
    if reply.get("applied").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let why = reply
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("The compositor did not write it.");
    Err(Problem::Other(why.to_owned()))
}

/// What the toast says a change did.
fn describe(name: &str, change: &Change) -> String {
    match change {
        Change::Path(Some(p)) => {
            let file = Path::new(p)
                .file_name()
                .map_or_else(|| p.clone(), |f| f.to_string_lossy().into_owned());
            format!("{name}: picture {file}")
        }
        Change::Path(None) => format!("{name}: no picture"),
        Change::Mode(Some(m)) => format!("{name}: {m}"),
        Change::Mode(None) => format!("{name}: sizing from all displays"),
        Change::Color(Some(c)) => format!("{name}: colour {c}"),
        Change::Color(None) => format!("{name}: colour from all displays"),
        Change::Clear => format!("{name} uses all displays again"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn entry(name: &str, path: Option<&str>, mode: Option<&str>, color: Option<&str>) -> Entry {
        Entry {
            name: name.into(),
            path: path.map(Into::into),
            mode: mode.map(Into::into),
            color: color.map(Into::into),
        }
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(b"IHDR");
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[8, 6, 0, 0, 0]);
        b.extend_from_slice(&[0, 0, 0, 0]);
        b
    }

    /// A JPEG with an APP0 and a DQT ahead of the frame header, and a fill
    /// byte before the frame marker.
    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8];
        b.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        b.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x05, 0x00, 0x01, 0x02]);
        b.extend_from_slice(&[0xFF, 0xFF, 0xC0, 0x00, 0x11, 0x08]);
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        b
    }

    #[test]
    fn a_picture_is_sized_from_its_png_header() {
        let got = header_size(&mut Cursor::new(png(736, 414)));
        assert_eq!(got, Some((736, 414)));
    }

    #[test]
    fn a_picture_is_sized_from_its_jpeg_frame_header() {
        let got = header_size(&mut Cursor::new(jpeg(1080, 1920)));
        assert_eq!(got, Some((1080, 1920)));
    }

    #[test]
    fn a_truncated_or_foreign_file_has_no_size() {
        let whole = png(736, 414);
        assert_eq!(header_size(&mut Cursor::new(&whole[..20])), None);
        assert_eq!(header_size(&mut Cursor::new(b"GIF89a....".to_vec())), None);
        assert_eq!(header_size(&mut Cursor::new(Vec::<u8>::new())), None);
        let mut no_ihdr = png(1, 1);
        no_ihdr[12..16].copy_from_slice(b"IDAT");
        assert_eq!(header_size(&mut Cursor::new(no_ihdr)), None);
        let jpg = jpeg(0, 0);
        assert_eq!(header_size(&mut Cursor::new(jpg)), None);
    }

    #[test]
    fn a_missing_file_shows_nothing() {
        assert_eq!(image_size(Path::new("/nonexistent/ec-wallpaper.png")), None);
    }

    #[test]
    fn a_small_picture_on_a_portrait_display_is_called_out() {
        // The case the page exists for: 736x414 on a 1080x1920 portrait output.
        let s = scale_warning((736, 414), (1080, 1920)).expect("warned");
        assert!((s - 1920.0 / 414.0).abs() < 1e-9, "{s}");
        assert!(s > WARN_SCALE);
    }

    #[test]
    fn the_warning_threshold_is_about_two_times() {
        // Exactly 2x is not above it; a hair over is.
        assert_eq!(scale_warning((960, 540), (1920, 1080)), None);
        assert!(scale_warning((959, 539), (1920, 1080)).is_some());
        // A picture that covers its display, or is bigger, is never called out.
        assert_eq!(scale_warning((1920, 1080), (1080, 1920)), None);
        assert_eq!(scale_warning((4000, 4000), (1920, 1080)), None);
    }

    #[test]
    fn a_change_sends_the_whole_block_so_the_other_fields_survive() {
        let cur = entry("DP-1", Some("/p/a.png"), None, Some("#112233"));
        let p = plan(Some(&cur), "DP-1", Change::Mode(Some("fit".into()))).unwrap();
        assert_eq!(
            params(&p),
            json!({
                "collection": "wallpaper.output",
                "op": "upsert",
                "name": "DP-1",
                "entry": { "path": "/p/a.png", "mode": "fit", "color": "#112233" },
            })
        );
    }

    #[test]
    fn an_inherited_field_is_sent_as_null() {
        let p = plan(None, "HDMI-A-2", Change::Path(Some("/p/b.png".into()))).unwrap();
        let v = params(&p);
        assert_eq!(
            v["entry"],
            json!({ "path": "/p/b.png", "mode": null, "color": null })
        );
        assert_eq!(v["name"], "HDMI-A-2");
    }

    #[test]
    fn a_colour_write_sends_only_the_colour_field_changed() {
        let cur = entry("DP-1", None, Some("center"), None);
        let p = plan(Some(&cur), "DP-1", Change::Color(Some("#0b0906".into()))).unwrap();
        assert_eq!(
            params(&p)["entry"],
            json!({ "path": null, "mode": "center", "color": "#0b0906" })
        );
    }

    #[test]
    fn use_the_same_as_all_displays_removes_the_block() {
        let cur = entry("DP-1", Some("/p/a.png"), Some("fit"), None);
        let p = plan(Some(&cur), "DP-1", Change::Clear).unwrap();
        assert_eq!(
            params(&p),
            json!({ "collection": "wallpaper.output", "op": "remove", "name": "DP-1" })
        );
    }

    #[test]
    fn clearing_the_last_field_of_a_block_removes_it() {
        let cur = entry("DP-1", None, Some("fit"), None);
        assert_eq!(
            plan(Some(&cur), "DP-1", Change::Mode(None)),
            Some(Plan::Remove("DP-1".into()))
        );
    }

    #[test]
    fn a_no_op_change_sends_nothing() {
        let cur = entry("DP-1", None, Some("fit"), None);
        assert_eq!(plan(Some(&cur), "DP-1", Change::Mode(Some("fit".into()))), None);
        assert_eq!(plan(None, "DP-1", Change::Clear), None);
        assert_eq!(plan(None, "DP-1", Change::Path(None)), None);
    }

    #[test]
    fn undo_puts_the_block_back_or_takes_the_new_one_away() {
        let before = entry("DP-1", Some("/p/a.png"), None, None);
        let r = Restore {
            name: "DP-1".into(),
            before: Some(before.clone()),
        };
        assert_eq!(restore_plan(&r), Plan::Upsert(before));
        let none = Restore {
            name: "DP-1".into(),
            before: None,
        };
        assert_eq!(restore_plan(&none), Plan::Remove("DP-1".into()));
    }

    #[test]
    fn the_collection_is_read_by_either_spelling_of_the_connector() {
        let reply = json!({
            "wallpaper.output": [
                { "name": "DP-1", "path": "/a.png", "mode": null, "color": "#112233aa" },
                { "output": "HDMI-A-2", "path": null, "mode": "fit", "color": null },
                { "path": "/orphan.png" },
            ]
        });
        let got = parse(&reply);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], entry("DP-1", Some("/a.png"), None, Some("#112233aa")));
        assert_eq!(got[1], entry("HDMI-A-2", None, Some("fit"), None));
        assert!(parse(&json!({})).is_empty());
    }

    #[test]
    fn the_list_is_connected_displays_then_unplugged_overrides() {
        let entries = vec![
            entry("HDMI-A-2", Some("/b.png"), None, None),
            entry("DP-1", Some("/a.png"), None, None),
            entry("DP-9", None, Some("fit"), None),
        ];
        let got = listed(&["DP-1", "eDP-1"], &entries);
        let names: Vec<(&str, bool)> = got.iter().map(|l| (l.name.as_str(), l.connected)).collect();
        assert_eq!(
            names,
            vec![
                ("DP-1", true),
                ("eDP-1", true),
                ("HDMI-A-2", false),
                ("DP-9", false),
            ]
        );
    }

    #[test]
    fn a_display_with_no_override_and_no_output_is_not_listed() {
        assert!(listed(&[], &[]).is_empty());
        let got = listed(&["DP-1"], &[]);
        assert_eq!(
            got,
            vec![Listed {
                name: "DP-1".into(),
                connected: true
            }]
        );
    }

    #[test]
    fn a_connected_display_with_an_override_is_listed_once() {
        let entries = vec![entry("DP-1", Some("/a.png"), None, None)];
        let got = listed(&["DP-1"], &entries);
        assert_eq!(
            got,
            vec![Listed {
                name: "DP-1".into(),
                connected: true
            }]
        );
    }

    #[test]
    fn the_card_only_follows_a_listed_display() {
        let mut p = Pane {
            listed: listed(&["DP-1"], &[entry("HDMI-A-2", Some("/b.png"), None, None)]),
            selected: Some("HDMI-A-2".into()),
            ..Pane::default()
        };
        assert_eq!(p.card_for(), Some("HDMI-A-2"));
        p.selected = Some("GONE-1".into());
        assert_eq!(p.card_for(), None);
        p.selected = None;
        assert_eq!(p.card_for(), None);
    }
}
