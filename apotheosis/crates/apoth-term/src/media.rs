// SPDX-License-Identifier: AGPL-3.0-only
//! Media placements (T-16 §2): the `ESC _ M <keys> ; <payload> ESC \` protocol, the
//! placements' tracked pins, and their frames in the draw list. Playback itself is the
//! caller's: `feed` turns commands into [`MediaRequest`]s ([`crate::Term::take_media_requests`])
//! and the caller answers with [`MediaEvent`]s ([`crate::Term::media_event`]), so `feed`
//! stays deterministic and never touches a decoder.
//!
//! Keys: `a` (`o` open, `c` control, `q` query, `x` close; `b`/`e` are T-16 §1.1
//! sessions), `i` the client's id (1..=u32), `s` source (`f` fd token `t=`, `p` path,
//! `d` data; payload base64, `m=1` for more chunks), `c`,`r` cells, `z`, `q` quiet (1: no
//! success replies, 2: none). Controls (`a=c`, and on `a=o`): `play`, `pause`, `seek=<ms>`,
//! `loop=0|1`, `volume=0..100`, `mute=0|1`; `rate` other than 1 is `error=unsupported`.
//! Replies: `ESC _ M i=<id>;<msg> ESC \` with `ready,w=,h=,d=`, `ended`, `error=<code>`,
//! `audio-unavailable`, and for `a=q` `state=<s>,pos=<ms>,dur=<ms>`.
//!
//! Spec gaps decided here (reported): a placement is fitted inside its `c`x`r` cell box,
//! centred; without `c`/`r` it spans the screen width and up to the rows below the cursor
//! (at most the screen); the cursor moves to the first column of the row below the box;
//! "rows leave the screen" means no row of the box is in the active area; a session's
//! media goes when the session ends either way (video is never a still image); `s=d`
//! data is bounded by the kitty transfer cap.

use std::sync::Arc;

use apoth_grid::{PinId, PinKind};
use apoth_image::Image;
use apoth_image::anim::frame_serial;
use apoth_image::store::Screen as Part;

use crate::config::deny;
use crate::graphics::ImageDraw;
use crate::machine::Machine;

/// Media settings (`media.*`).
#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// `media.enabled`.
    pub enabled: bool,
    /// Concurrent placements per pane (T-16 §5: 2).
    pub per_pane: usize,
    /// `media.offscreen = stop` (default `pause`).
    pub offscreen_stop: bool,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            per_pane: 2,
            offscreen_stop: false,
        }
    }
}

/// Where a placement's bytes come from.
#[derive(Clone, PartialEq, Eq)]
pub enum MediaSource {
    /// `s=f`: an fd `apoth play` passed over the control socket, by token.
    Token(u64),
    /// `s=p`: a path (the caller applies the path policy and the T-15 §1 open rules).
    Path(Vec<u8>),
    /// `s=d`: the whole file, sent in-band.
    Data(Vec<u8>),
}

impl std::fmt::Debug for MediaSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the path or the bytes.
        match self {
            MediaSource::Token(_) => f.write_str("Token"),
            MediaSource::Path(_) => f.write_str("Path"),
            MediaSource::Data(d) => write!(f, "Data({} bytes)", d.len()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MediaCtl {
    Play,
    Pause,
    Seek(u64),
    Loop(bool),
    /// 0..=1.
    Volume(f32),
    Mute(bool),
}

/// What the caller is asked to do. `key` names the placement (unique per term).
#[derive(Debug, Clone, PartialEq)]
pub enum MediaRequest {
    Open {
        key: u64,
        source: MediaSource,
        /// The box the video is fitted into, in pixels at the current cell size.
        box_px: (u32, u32),
        play: bool,
        looping: bool,
        volume: f32,
        muted: bool,
        /// The T-16 §1.1 session the placement belongs to (0: none).
        session: u64,
    },
    Control {
        key: u64,
        ctl: MediaCtl,
    },
    Close {
        key: u64,
    },
}

/// The caller's answers.
#[derive(Debug, Clone)]
pub enum MediaEvent {
    Ready {
        width: u32,
        height: u32,
        duration_ms: u64,
    },
    Frame(Arc<Image>),
    /// Whole seconds played.
    Position(u64),
    AudioUnavailable,
    Ended,
    /// The placement stopped for good with this code (and is removed).
    Error(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Opening,
    Playing,
    Paused,
    Ended,
}

#[derive(Debug)]
struct Place {
    key: u64,
    id: u32,
    part: Part,
    pin: PinId,
    cols: u32,
    rows: u32,
    z: i32,
    session: u64,
    quiet: u8,
    state: State,
    /// The client wants it playing (offscreen pause resumes to this).
    want_play: bool,
    offscreen: bool,
    frame: Option<(u64, Arc<Image>)>,
    /// The frame before, when the same size: its texture may be overwritten.
    prev: Option<u64>,
    duration_ms: u64,
    pos_s: u64,
}

/// A half-received `s=d` open.
#[derive(Debug)]
struct Chunks {
    id: u32,
    b64: Vec<u8>,
    keys: Vec<u8>,
}

#[derive(Debug, Default)]
pub(crate) struct MediaState {
    places: Vec<Place>,
    requests: Vec<MediaRequest>,
    chunks: Option<Chunks>,
    next_key: u64,
}

impl MediaState {
    pub(crate) fn has_requests(&self) -> bool {
        !self.requests.is_empty()
    }

    pub(crate) fn take_requests(&mut self) -> Vec<MediaRequest> {
        std::mem::take(&mut self.requests)
    }

    pub(crate) fn has_places(&self) -> bool {
        !self.places.is_empty()
    }

    /// RIS: every placement of the old state is closed; keys stay unique.
    pub(crate) fn carry_from(&mut self, old: &MediaState) {
        self.next_key = old.next_key;
        self.requests = old.requests.clone();
        self.requests.extend(
            old.places
                .iter()
                .map(|p| MediaRequest::Close { key: p.key }),
        );
    }
}

/// Parsed control keys.
#[derive(Debug, Default)]
struct Keys {
    verb: u8,
    id: u32,
    source: u8,
    token: u64,
    cols: u32,
    rows: u32,
    z: i32,
    more: bool,
    quiet: u8,
    ctl: Vec<MediaCtl>,
    bad: bool,
    unsupported: bool,
}

fn num<T: std::str::FromStr>(v: &[u8]) -> Option<T> {
    if v.is_empty() || v.len() > 20 {
        return None;
    }
    std::str::from_utf8(v).ok()?.parse().ok()
}

fn parse_keys(keys: &[u8]) -> Keys {
    let mut k = Keys::default();
    for kv in keys.split(|&b| b == b',') {
        let (name, val) = match kv.iter().position(|&b| b == b'=') {
            Some(i) => (&kv[..i], Some(&kv[i + 1..])),
            None => (kv, None),
        };
        let ok = match (name, val) {
            (b"a", Some([v])) => {
                k.verb = *v;
                true
            }
            (b"i", Some(v)) => num(v).filter(|&i: &u32| i > 0).map(|i| k.id = i).is_some(),
            (b"s", Some([v @ (b'f' | b'p' | b'd')])) => {
                k.source = *v;
                true
            }
            (b"t", Some(v)) => num(v).map(|t| k.token = t).is_some(),
            (b"c", Some(v)) => num(v)
                .filter(|&c: &u32| c <= 1000)
                .map(|c| k.cols = c)
                .is_some(),
            (b"r", Some(v)) => num(v)
                .filter(|&r: &u32| r <= 1000)
                .map(|r| k.rows = r)
                .is_some(),
            (b"z", Some(v)) => num(v).map(|z| k.z = z).is_some(),
            (b"m", Some(v)) => num(v).map(|m: u8| k.more = m == 1).is_some(),
            (b"q", Some(v)) => num(v).map(|q: u8| k.quiet = q.min(2)).is_some(),
            (b"play", None) => {
                k.ctl.push(MediaCtl::Play);
                true
            }
            (b"pause", None) => {
                k.ctl.push(MediaCtl::Pause);
                true
            }
            (b"seek", Some(v)) => num(v).map(|ms| k.ctl.push(MediaCtl::Seek(ms))).is_some(),
            (b"loop", Some(v)) => num(v)
                .map(|l: u8| k.ctl.push(MediaCtl::Loop(l == 1)))
                .is_some(),
            (b"mute", Some(v)) => num(v)
                .map(|m: u8| k.ctl.push(MediaCtl::Mute(m == 1)))
                .is_some(),
            (b"volume", Some(v)) => num(v)
                .filter(|&p: &u32| p <= 100)
                .map(|p| k.ctl.push(MediaCtl::Volume(p as f32 / 100.0)))
                .is_some(),
            (b"rate", Some(v)) => {
                if v != b"1" && v != b"1.0" {
                    k.unsupported = true;
                }
                true
            }
            (b"p", Some(_)) => true, // session pid (T-16 §1.1), read by the session parser
            ([], None) => true,
            _ => false,
        };
        if !ok {
            k.bad = true;
        }
    }
    k
}

impl Machine {
    /// `ESC _ M …`: a session marker or a media command.
    pub(crate) fn media_apc(&mut self, buf: &[u8]) {
        let Some(body) = buf.strip_prefix(b"M") else {
            return;
        };
        let (keys, payload) = match body.iter().position(|&b| b == b';') {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => (body, &[][..]),
        };
        // A continuation chunk carries only `m=` (and `q=`).
        if let Some(c) = self.gfx.media.chunks.take() {
            let k = parse_keys(keys);
            if k.verb == 0 {
                self.media_chunk(c, &k, payload);
                return;
            }
            // Anything else abandons the transfer.
        }
        let k = parse_keys(keys);
        match k.verb {
            b'b' | b'e' => self.gfx_session(buf),
            b'o' | b'c' | b'q' | b'x' => self.media_command(k, keys, payload),
            _ => {}
        }
    }

    fn media_reply(&mut self, id: u32, quiet: u8, ok: bool, msg: &str) {
        if quiet >= 2 || (ok && quiet >= 1) || id == 0 {
            return;
        }
        self.reply_str(format!("\x1b_Mi={id};{msg}\x1b\\"));
    }

    fn media_command(&mut self, k: Keys, keys: &[u8], payload: &[u8]) {
        if k.id == 0 {
            return;
        }
        let quiet = k.quiet;
        if self.cfg.deny & deny::GRAPHICS_DIRECT != 0 {
            return; // denied: no effect, no reply (as for kitty graphics)
        }
        if !self.cfg.graphics.media.enabled {
            self.media_reply(k.id, quiet, false, "error=disabled");
            return;
        }
        if k.bad {
            self.media_reply(k.id, quiet, false, "error=bad-request");
            return;
        }
        if k.unsupported {
            self.media_reply(k.id, quiet, false, "error=unsupported");
            return;
        }
        let part = crate::graphics::part_of(self.cur);
        let find = |m: &Machine| {
            m.gfx
                .media
                .places
                .iter()
                .position(|p| p.id == k.id && p.part == part)
        };
        match k.verb {
            b'o' => {
                if k.source == b'd' && k.more {
                    if payload.len() > self.cfg.graphics.limits.max_transfer_b64 {
                        self.media_reply(k.id, quiet, false, "error=too-large");
                        return;
                    }
                    self.gfx.media.chunks = Some(Chunks {
                        id: k.id,
                        b64: payload.to_vec(),
                        keys: keys.to_vec(),
                    });
                    return;
                }
                let src = match self.media_source(&k, payload) {
                    Ok(s) => s,
                    Err(code) => {
                        self.media_reply(k.id, quiet, false, code);
                        return;
                    }
                };
                self.media_open(k, src);
            }
            b'c' => {
                let Some(i) = find(self) else {
                    self.media_reply(k.id, quiet, false, "error=no-such-placement");
                    return;
                };
                for c in k.ctl {
                    let p = &mut self.gfx.media.places[i];
                    match c {
                        MediaCtl::Play => {
                            p.want_play = true;
                            if p.state != State::Opening {
                                p.state = State::Playing;
                            }
                        }
                        MediaCtl::Pause => {
                            p.want_play = false;
                            if p.state == State::Playing {
                                p.state = State::Paused;
                            }
                        }
                        MediaCtl::Seek(_) if p.state == State::Ended => {
                            p.state = if p.want_play {
                                State::Playing
                            } else {
                                State::Paused
                            };
                        }
                        _ => {}
                    }
                    let key = p.key;
                    // Offscreen: remember the wish, do not resume playback.
                    let held = p.offscreen && c == MediaCtl::Play;
                    if !held {
                        self.gfx
                            .media
                            .requests
                            .push(MediaRequest::Control { key, ctl: c });
                    }
                }
            }
            b'q' => {
                let Some(i) = find(self) else {
                    self.media_reply(k.id, quiet, false, "error=no-such-placement");
                    return;
                };
                let p = &self.gfx.media.places[i];
                let st = match p.state {
                    State::Opening => "opening",
                    State::Playing if p.offscreen => "paused",
                    State::Playing => "playing",
                    State::Paused => "paused",
                    State::Ended => "ended",
                };
                let msg = format!(
                    "state={st},pos={},dur={}",
                    p.pos_s.saturating_mul(1000),
                    p.duration_ms
                );
                // A query answers unless q=2.
                let id = p.id;
                self.media_reply(id, if quiet >= 2 { 2 } else { 0 }, false, &msg);
            }
            b'x' => {
                if let Some(i) = find(self) {
                    self.media_remove(i);
                }
            }
            _ => {}
        }
    }

    fn media_chunk(&mut self, mut c: Chunks, k: &Keys, payload: &[u8]) {
        if c.b64.len() + payload.len() > self.cfg.graphics.limits.max_transfer_b64 {
            let id = c.id;
            self.media_reply(id, 0, false, "error=too-large");
            return;
        }
        c.b64.extend_from_slice(payload);
        if k.more {
            self.gfx.media.chunks = Some(c);
            return;
        }
        let mut keys = parse_keys(&c.keys);
        keys.more = false;
        let id = keys.id;
        let quiet = keys.quiet;
        match self.media_source(&keys, &c.b64) {
            Ok(src) => self.media_open(keys, src),
            Err(code) => self.media_reply(id, quiet, false, code),
        }
    }

    fn media_source(&self, k: &Keys, payload: &[u8]) -> Result<MediaSource, &'static str> {
        let b64 = |p: &[u8]| crate::osc::b64_decode(p).ok_or("error=bad-request");
        match k.source {
            b'f' | 0 if k.token != 0 => Ok(MediaSource::Token(k.token)),
            b'p' => {
                if self.cfg.deny & deny::GRAPHICS_FILE != 0 {
                    return Err("error=path-refused");
                }
                let p = b64(payload)?;
                if p.is_empty() || p.len() > 4096 {
                    return Err("error=bad-request");
                }
                Ok(MediaSource::Path(p))
            }
            b'd' => {
                let d = b64(payload)?;
                if d.is_empty() {
                    return Err("error=bad-request");
                }
                Ok(MediaSource::Data(d))
            }
            _ => Err("error=bad-request"),
        }
    }

    fn media_open(&mut self, k: Keys, source: MediaSource) {
        let part = crate::graphics::part_of(self.cur);
        // The same id again replaces the old placement.
        if let Some(i) = self
            .gfx
            .media
            .places
            .iter()
            .position(|p| p.id == k.id && p.part == part)
        {
            self.media_remove(i);
        }
        if self.gfx.media.places.len() >= self.cfg.graphics.media.per_pane {
            self.media_reply(k.id, k.quiet, false, "error=quota");
            return;
        }
        let cols = if k.cols == 0 {
            self.cols as u32
        } else {
            k.cols.min(self.cols as u32)
        };
        let rows = if k.rows == 0 {
            (self.rows - self.y).max(1) as u32
        } else {
            k.rows.min(self.rows as u32)
        };
        let gi = crate::graphics::grid_of(part);
        let Some(pin) = self.panes[gi]
            .grid
            .track_active(self.y, self.x, PinKind::ImagePlacement)
        else {
            self.media_reply(k.id, k.quiet, false, "error=bad-request");
            return;
        };
        let (cw, ch) = self.cfg.cell_px;
        let box_px = (
            cols.saturating_mul(cw.max(1)),
            rows.saturating_mul(ch.max(1)),
        );
        self.gfx.media.next_key += 1;
        let key = self.gfx.media.next_key;
        let mut play = true;
        let (mut looping, mut volume, mut muted) = (false, 1.0, false);
        for c in &k.ctl {
            match *c {
                MediaCtl::Pause => play = false,
                MediaCtl::Play => play = true,
                MediaCtl::Loop(l) => looping = l,
                MediaCtl::Volume(v) => volume = v,
                MediaCtl::Mute(m) => muted = m,
                MediaCtl::Seek(_) => {}
            }
        }
        let session = self.gfx.current_session();
        self.gfx.media.places.push(Place {
            key,
            id: k.id,
            part,
            pin,
            cols,
            rows,
            z: k.z,
            session,
            quiet: k.quiet,
            state: State::Opening,
            want_play: play,
            offscreen: false,
            frame: None,
            prev: None,
            duration_ms: 0,
            pos_s: 0,
        });
        self.gfx.media.requests.push(MediaRequest::Open {
            key,
            source,
            box_px,
            play,
            looping,
            volume,
            muted,
            session,
        });
        // The cursor goes to the first column below the box.
        for _ in 0..rows {
            self.index();
        }
        self.cursor_moved();
        self.x = 0;
        self.wrap = false;
    }

    fn media_remove(&mut self, i: usize) {
        let p = self.gfx.media.places.remove(i);
        let gi = crate::graphics::grid_of(p.part);
        self.panes[gi].grid.untrack_pin(p.pin);
        self.panes[gi].grid.mark_full();
        self.gfx
            .media
            .requests
            .push(MediaRequest::Close { key: p.key });
    }

    /// A session ended (cleanly or not): its media goes.
    pub(crate) fn media_session_end(&mut self, id: u64) {
        while let Some(i) = self.gfx.media.places.iter().position(|p| p.session == id) {
            self.media_remove(i);
        }
    }

    /// After each feed: placements whose pins died are freed; placements with no row in
    /// the active area pause (or stop), and resume when a row is back.
    pub(crate) fn media_collect(&mut self, dead: &[PinId]) {
        let mut i = 0;
        while i < self.gfx.media.places.len() {
            let p = &self.gfx.media.places[i];
            if dead.contains(&p.pin) {
                let p = self.gfx.media.places.remove(i);
                self.gfx
                    .media
                    .requests
                    .push(MediaRequest::Close { key: p.key });
                continue;
            }
            let g = &self.panes[crate::graphics::grid_of(p.part)].grid;
            let off = match g.pin_pos(p.pin) {
                Some((abs, _)) => abs + p.rows as usize <= g.history_rows(),
                None => true,
            };
            if off != p.offscreen {
                if off && self.cfg.graphics.media.offscreen_stop {
                    self.media_remove(i);
                    continue;
                }
                let p = &mut self.gfx.media.places[i];
                p.offscreen = off;
                if p.want_play && p.state == State::Playing {
                    let ctl = if off { MediaCtl::Pause } else { MediaCtl::Play };
                    self.gfx
                        .media
                        .requests
                        .push(MediaRequest::Control { key: p.key, ctl });
                }
            }
            i += 1;
        }
    }

    /// The caller's answer for placement `key`.
    pub(crate) fn media_event(&mut self, key: u64, ev: MediaEvent) {
        let Some(i) = self.gfx.media.places.iter().position(|p| p.key == key) else {
            return;
        };
        let (id, quiet) = (self.gfx.media.places[i].id, self.gfx.media.places[i].quiet);
        match ev {
            MediaEvent::Ready {
                width,
                height,
                duration_ms,
            } => {
                let p = &mut self.gfx.media.places[i];
                p.duration_ms = duration_ms;
                p.state = if p.want_play {
                    State::Playing
                } else {
                    State::Paused
                };
                self.media_reply(
                    id,
                    quiet,
                    true,
                    &format!("ready,w={width},h={height},d={duration_ms}"),
                );
            }
            MediaEvent::Frame(img) => {
                let p = &mut self.gfx.media.places[i];
                p.prev = p
                    .frame
                    .as_ref()
                    .filter(|(_, o)| (o.width, o.height) == (img.width, img.height))
                    .map(|(k, _)| *k);
                p.frame = Some((frame_serial(), img));
                let gi = crate::graphics::grid_of(p.part);
                self.panes[gi].grid.mark_full();
            }
            MediaEvent::Position(s) => self.gfx.media.places[i].pos_s = s,
            MediaEvent::AudioUnavailable => self.media_reply(id, quiet, false, "audio-unavailable"),
            MediaEvent::Ended => {
                self.gfx.media.places[i].state = State::Ended;
                self.media_reply(id, quiet, true, "ended");
            }
            MediaEvent::Error(code) => {
                let p = self.gfx.media.places.remove(i);
                let gi = crate::graphics::grid_of(p.part);
                self.panes[gi].grid.untrack_pin(p.pin);
                self.panes[gi].grid.mark_full();
                self.media_reply(id, quiet, false, &format!("error={code}"));
            }
        }
    }

    /// Media quads for the current screen (see `gfx_draws`), appended to `out`.
    pub(crate) fn media_draws(&self, top_abs: usize, rows: usize, out: &mut Vec<ImageDraw>) {
        let part = crate::graphics::part_of(self.cur);
        let (cw, ch) = self.cfg.cell_px;
        let (cw, ch) = (i64::from(cw.max(1)), i64::from(ch.max(1)));
        for p in self.gfx.media.places.iter().filter(|p| p.part == part) {
            let Some((key, img)) = &p.frame else {
                continue;
            };
            let g = &self.panes[crate::graphics::grid_of(part)].grid;
            let Some((abs, col)) = g.pin_pos(p.pin) else {
                continue;
            };
            let vrow = abs as i64 - top_abs as i64;
            if vrow >= rows as i64 || vrow + i64::from(p.rows) <= 0 {
                continue;
            }
            let (bw, bh) = (i64::from(p.cols) * cw, i64::from(p.rows) * ch);
            let (iw, ih) = (i64::from(img.width.max(1)), i64::from(img.height.max(1)));
            // Fit inside the box, centred.
            let (w, h) = if iw * bh <= ih * bw {
                (iw * bh / ih, bh)
            } else {
                (bw, ih * bw / iw)
            };
            let x = col as i64 * cw + (bw - w) / 2;
            let y = vrow * ch + (bh - h) / 2;
            let c = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
            out.push(ImageDraw {
                key: *key,
                image: Arc::clone(img),
                dst: [c(x), c(y), c(w.max(1)), c(h.max(1))],
                src: [0, 0, img.width, img.height],
                z: p.z,
                replaces: p.prev,
                dirty: None,
            });
        }
    }
}
