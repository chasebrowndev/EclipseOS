// SPDX-License-Identifier: AGPL-3.0-only
//! OSC and DCS string handling with the per-protocol caps of T-03 §5.
//!
//! OSC payload as delivered by `apoth-vt` is the text after the first `;` of
//! `OSC <cmd> ; <payload>`.

use apoth_grid::Semantic;
use apoth_vt::{StrEnd, StrHead, StrKind, StrRoute};

use crate::color;
use crate::config::deny;
use crate::events::{ClipboardSet, CommandEvent, Mark, Progress, UserVar};
use crate::machine::{Machine, StrMode};
use crate::sgr;
use crate::shell;

const CAP_SMALL: u32 = 4096;
const CAP_133: u32 = 8192;
const CAP_NOTIFY: u32 = 8192;
const CAP_PROGRESS: u32 = 64;
const CAP_DECRQSS: u32 = 64;
/// Hyperlink URI and id limits (T-02 §1.4, T-18 §5).
const MAX_URI: usize = 2083;
const MAX_ID: usize = 250;

enum Nine {
    Progress,
    Silent,
    Text,
}

fn classify_nine(sel: &[u8]) -> Nine {
    if sel.is_empty() || !sel.iter().all(u8::is_ascii_digit) {
        return Nine::Text;
    }
    let mut v: u64 = 0;
    for &d in sel {
        v = (v * 10 + u64::from(d - b'0')).min(1 << 20);
    }
    match v {
        4 => Nine::Progress,
        1..=12 => Nine::Silent,
        _ => Nine::Text,
    }
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Strip C0, DEL, C1 and bidi controls (T-18 §6).
pub(crate) fn sanitize(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|&c| !c.is_control() && !is_bidi(c))
        .collect()
}

/// Standard-alphabet base64; trailing `=` allowed, anything else invalid.
pub(crate) fn b64_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut s = s;
    while let [rest @ .., b'='] = s {
        s = rest;
    }
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 2);
    let (mut acc, mut n) = (0u32, 0);
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        n += 1;
        if n == 4 {
            out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
            acc = 0;
            n = 0;
        }
    }
    match n {
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
        _ => {}
    }
    Some(out)
}

impl Machine {
    pub(crate) fn str_start(&mut self, kind: StrKind, head: &StrHead) -> StrRoute {
        self.sbuf.clear();
        self.strm = StrMode::None;
        self.gfx.sixel_abort();
        match kind {
            StrKind::Osc => {
                let Some(cmd) = head.osc_command() else {
                    return StrRoute::SKIP;
                };
                let (mode, cap) = match cmd {
                    0..=2 => (StrMode::Title(cmd), CAP_SMALL),
                    4 => (StrMode::Palette, CAP_SMALL),
                    10..=19 => (StrMode::Dyn(cmd), CAP_SMALL),
                    104 => (StrMode::ResetPalette, CAP_SMALL),
                    110..=119 => (StrMode::ResetDyn(cmd), CAP_SMALL),
                    7 => (StrMode::Cwd, CAP_SMALL),
                    8 => (StrMode::Link, CAP_SMALL),
                    52 => (
                        StrMode::Clip,
                        u32::try_from(self.cfg.osc52_max).unwrap_or(u32::MAX),
                    ),
                    133 | 633 => (StrMode::Mark, CAP_133),
                    9 => match classify_nine(head.osc_selector()) {
                        Nine::Progress => (StrMode::Progress, CAP_PROGRESS),
                        Nine::Silent => return StrRoute::SKIP,
                        Nine::Text => (StrMode::Notify, CAP_NOTIFY),
                    },
                    777 => (StrMode::Notify777, CAP_NOTIFY),
                    99 => (StrMode::Notify99, CAP_NOTIFY),
                    1337 if self.cfg.graphics.iterm2
                        && self.cfg.deny & crate::config::deny::GRAPHICS_DIRECT == 0
                        && head.osc_selector() == b"File" =>
                    {
                        let cap = self.cfg.graphics.limits.max_transfer_b64 + 4096;
                        (StrMode::ITerm, u32::try_from(cap).unwrap_or(u32::MAX))
                    }
                    1337 => (StrMode::UserVar, CAP_SMALL),
                    _ => return StrRoute::SKIP,
                };
                self.strm = mode;
                StrRoute { cap }
            }
            StrKind::Dcs => {
                let pr = head.dcs_private();
                let inter = head.dcs_intermediates();
                let fin = head.dcs_final();
                if pr == 0 && inter == b"$" && fin == Some(b'q') {
                    self.strm = StrMode::Decrqss;
                    return StrRoute { cap: CAP_DECRQSS };
                }
                if pr == 0 && inter == b"+" && fin == Some(b'q') {
                    self.strm = StrMode::Tcap;
                    return StrRoute { cap: CAP_SMALL };
                }
                if pr == 0 && inter.is_empty() && fin == Some(b'q') {
                    return match self.gfx_sixel_start(head.params()) {
                        Some(cap) => StrRoute { cap },
                        None => StrRoute::SKIP,
                    };
                }
                if pr == b'=' && inter.is_empty() && fin == Some(b's') {
                    match head.params().get(0) {
                        Some(1) => {
                            if self.sync.is_none() {
                                self.sync = self.now;
                            }
                        }
                        Some(2) => self.sync = None,
                        _ => {}
                    }
                }
                StrRoute::SKIP
            }
            StrKind::Apc if head.bytes().first() == Some(&b'M') => {
                self.strm = StrMode::Session;
                // Room for an `s=d` chunk (T-16 §2), as for a kitty chunk.
                let cap = self.cfg.graphics.limits.max_chunk_b64 + CAP_SMALL as usize;
                StrRoute {
                    cap: u32::try_from(cap).unwrap_or(u32::MAX),
                }
            }
            StrKind::Apc if head.bytes().first() == Some(&b'G') => match self.gfx_apc_route() {
                Some(cap) => StrRoute { cap },
                None => StrRoute::SKIP,
            },
            _ => StrRoute::SKIP,
        }
    }

    pub(crate) fn str_end(&mut self, end: StrEnd) {
        let mode = std::mem::replace(&mut self.strm, StrMode::None);
        let buf = std::mem::take(&mut self.sbuf);
        if mode == StrMode::Kitty {
            self.gfx_apc_end(&buf, end);
        } else if mode == StrMode::Sixel {
            self.gfx_sixel_end(end);
        } else if mode != StrMode::None && end.is_ok() {
            let term: &[u8] = if end == StrEnd::Bel {
                b"\x07"
            } else {
                b"\x1b\\"
            };
            self.osc_dispatch(mode, &buf, term);
        }
        self.sbuf = buf;
        self.sbuf.clear();
    }

    fn osc_dispatch(&mut self, mode: StrMode, buf: &[u8], term: &[u8]) {
        let class = match mode {
            StrMode::Title(_) => deny::TITLE_SET,
            StrMode::Palette | StrMode::Dyn(_) | StrMode::ResetPalette | StrMode::ResetDyn(_) => {
                deny::COLOUR
            }
            StrMode::Notify | StrMode::Notify777 | StrMode::Notify99 => deny::NOTIFICATION,
            StrMode::Link => deny::HYPERLINK,
            StrMode::UserVar => deny::USER_VAR,
            _ => 0,
        };
        if self.cfg.deny & class != 0 {
            // Denied: no observable effect and no reply (T-18 §11). A denied hyperlink
            // still closes any open one so later text is not linked by a stale URI.
            if mode == StrMode::Link {
                self.link = None;
            }
            return;
        }
        match mode {
            StrMode::Title(cmd) => {
                let t = sanitize(buf);
                if cmd != 1 {
                    self.title = t.clone();
                    self.events.title = Some(t.clone());
                }
                if cmd != 2 {
                    self.icon = t;
                }
            }
            StrMode::Palette => self.osc_palette(buf, term),
            StrMode::Dyn(cmd) => self.osc_dynamic(cmd, buf, term),
            StrMode::ResetPalette => {
                let def = color::default_palette(&self.cfg.ansi);
                let s = String::from_utf8_lossy(buf);
                if s.is_empty() {
                    *self.palette = def;
                } else {
                    for f in s.split(';') {
                        if let Ok(i) = f.parse::<usize>()
                            && i < 256
                        {
                            self.palette[i] = def[i];
                        }
                    }
                }
                self.events.colors_changed = true;
            }
            StrMode::ResetDyn(cmd) => {
                match cmd - 100 {
                    10 => self.fg = self.cfg.fg,
                    11 => self.bg = self.cfg.bg,
                    12 => self.cursor_color = self.cfg.cursor_color,
                    _ => return,
                }
                self.events.colors_changed = true;
            }
            StrMode::Cwd => {
                if let Ok(s) = String::from_utf8(buf.to_vec())
                    && s.starts_with("file://")
                    && !s.chars().any(char::is_control)
                {
                    self.cwd_uri = Some(s.clone());
                    self.events.cwd = Some(s);
                }
            }
            StrMode::Link => self.osc_link(buf),
            StrMode::Clip => self.osc_clip(buf),
            StrMode::Mark => self.osc_mark(buf),
            StrMode::UserVar => self.osc_user_var(buf),
            StrMode::ITerm => self.gfx_iterm(buf),
            StrMode::Session => self.media_apc(buf),
            StrMode::Notify => {
                let t = sanitize(buf);
                if !t.is_empty() {
                    self.events.push_notification(9, t);
                }
            }
            StrMode::Notify777 => {
                // `notify;title;body` (T-03 §3.1; text form is a spec gap, see T-03 report).
                let mut f = buf.splitn(3, |&b| b == b';');
                if f.next() == Some(b"notify") {
                    let title = sanitize(f.next().unwrap_or_default());
                    let body = sanitize(f.next().unwrap_or_default());
                    let t = match (title.is_empty(), body.is_empty()) {
                        (false, false) => format!("{title}: {body}"),
                        (false, true) => title,
                        _ => body,
                    };
                    if !t.is_empty() {
                        self.events.push_notification(777, t);
                    }
                }
            }
            StrMode::Notify99 => {
                // `metadata;payload`; title and body payloads each become a notification,
                // every other payload kind (close, query, alive, icon) is consumed.
                let mut f = buf.splitn(2, |&b| b == b';');
                let meta = f.next().unwrap_or_default();
                let Some(payload) = f.next() else { return };
                let (mut kind_ok, mut b64) = (true, false);
                for kv in meta.split(|&b| b == b':') {
                    match kv {
                        b"e=1" => b64 = true,
                        [b'p', b'=', v @ ..] => kind_ok = v == b"title" || v == b"body",
                        _ => {}
                    }
                }
                if !kind_ok {
                    return;
                }
                let t = if b64 {
                    match b64_decode(payload) {
                        Some(d) => sanitize(&d),
                        None => return,
                    }
                } else {
                    sanitize(payload)
                };
                if !t.is_empty() {
                    self.events.push_notification(99, t);
                }
            }
            StrMode::Progress => {
                let s = String::from_utf8_lossy(buf);
                let mut f = s.split(';');
                let _ = f.next();
                let st = f.next().unwrap_or("0");
                let val = f.next().unwrap_or("0");
                let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
                if digits(st) && (val.is_empty() || digits(val)) {
                    let st: u32 = st.parse().unwrap_or(u32::MAX);
                    let v: u32 = val
                        .parse()
                        .unwrap_or(if val.is_empty() { 0 } else { u32::MAX });
                    if st <= 4 {
                        let value = if st == 0 { 0 } else { v.min(100) as u8 };
                        self.events.progress = Some(Progress {
                            state: st as u8,
                            value,
                        });
                    }
                }
            }
            StrMode::Decrqss => self.decrqss(buf),
            StrMode::Tcap => self.xtgettcap(buf),
            StrMode::None | StrMode::Kitty | StrMode::Sixel => {}
        }
    }

    /// XTGETTCAP (T-19 §4): hex names, one reply each, stop at the first unknown or
    /// invalid name. Nothing received is echoed except as hex of a table name.
    fn xtgettcap(&mut self, buf: &[u8]) {
        for part in buf.split(|&b| b == b';') {
            let Some(name) = unhex(part) else {
                self.reply(b"\x1bP0+r\x1b\\".to_vec());
                return;
            };
            let value: Option<Option<&str>> = match name.as_slice() {
                b"TN" => Some(Some("xterm-256color")),
                b"Co" | b"colors" => Some(Some("256")),
                b"RGB" => Some(Some("8/8/8")),
                b"Tc" => Some(None),
                _ => None,
            };
            let Some(value) = value else {
                let r = format!("\x1bP0+r{}\x1b\\", hex_upper(&name));
                self.reply_str(r);
                return;
            };
            let r = match value {
                Some(v) => format!(
                    "\x1bP1+r{}={}\x1b\\",
                    hex_upper(&name),
                    hex_upper(v.as_bytes())
                ),
                None => format!("\x1bP1+r{}\x1b\\", hex_upper(&name)),
            };
            self.reply_str(r);
        }
    }

    fn osc_palette(&mut self, buf: &[u8], term: &[u8]) {
        let s = String::from_utf8_lossy(buf).into_owned();
        let mut it = s.split(';');
        while let (Some(idx), Some(spec)) = (it.next(), it.next()) {
            let Ok(i) = idx.parse::<usize>() else {
                continue;
            };
            if i > 255 {
                continue;
            }
            if spec == "?" {
                let mut r = format!("\x1b]4;{i};{}", color::format16(self.palette[i])).into_bytes();
                r.extend_from_slice(term);
                self.reply(r);
            } else if let Some(c) = color::parse(spec) {
                self.palette[i] = c;
                self.events.colors_changed = true;
            }
        }
    }

    fn osc_dynamic(&mut self, cmd: u32, buf: &[u8], term: &[u8]) {
        let s = String::from_utf8_lossy(buf).into_owned();
        for (k, spec) in s.split(';').enumerate() {
            let code = cmd + k as u32;
            if code > 12 {
                break;
            }
            let cur = match code {
                10 => self.fg,
                11 => self.bg,
                _ => self.cursor_color,
            };
            if spec == "?" {
                let mut r = format!("\x1b]{code};{}", color::format16(cur)).into_bytes();
                r.extend_from_slice(term);
                self.reply(r);
            } else if let Some(c) = color::parse(spec) {
                match code {
                    10 => self.fg = c,
                    11 => self.bg = c,
                    _ => self.cursor_color = c,
                }
                self.events.colors_changed = true;
            }
        }
    }

    fn osc_link(&mut self, buf: &[u8]) {
        let Some(pos) = buf.iter().position(|&b| b == b';') else {
            return;
        };
        let (params, uri) = (&buf[..pos], &buf[pos + 1..]);
        if uri.is_empty() {
            self.link = None;
            return;
        }
        let mut id = String::new();
        if let Ok(p) = std::str::from_utf8(params) {
            for kv in p.split(':') {
                if let Some(v) = kv.strip_prefix("id=") {
                    id = v.to_string();
                }
            }
        }
        let ok_uri = std::str::from_utf8(uri)
            .ok()
            .filter(|u| u.len() <= MAX_URI && !u.chars().any(char::is_control));
        self.link = match ok_uri {
            Some(u) if id.len() <= MAX_ID && !id.chars().any(char::is_control) => {
                Some((id, u.to_string()))
            }
            _ => None,
        };
    }

    fn osc_clip(&mut self, buf: &[u8]) {
        let Some(pos) = buf.iter().position(|&b| b == b';') else {
            return;
        };
        let (sel, data) = (&buf[..pos], &buf[pos + 1..]);
        let mut selection: String = sel
            .iter()
            .filter(|b| matches!(b, b'c' | b'p' | b'q' | b's' | b'0'..=b'7'))
            .take(8)
            .map(|&b| char::from(b))
            .collect();
        if selection.is_empty() {
            selection.push('c');
        }
        // `primary-selection` follows the clipboard classes and may be denied on its own.
        let primary = selection.contains(['p', 's']);
        let deny = self.cfg.deny;
        if data == b"?" {
            // Read: the term never answers. Main decides under the sequence policy (ask
            // locally, deny remotely, T-18 §3) and queues an encoded reply when allowed.
            if deny & deny::CLIPBOARD_READ != 0 || (primary && deny & deny::PRIMARY != 0) {
                return;
            }
            self.events.clip_read = Some(selection);
            return;
        }
        if !self.cfg.osc52_write
            || deny & deny::CLIPBOARD_WRITE != 0
            || (primary && deny & deny::PRIMARY != 0)
        {
            self.events.dropped.clipboard += 1;
            return;
        }
        let Some(bytes) = b64_decode(data) else {
            self.events.dropped.bad_base64 += 1;
            return;
        };
        self.events.push_clipboard(ClipboardSet {
            selection,
            data: bytes,
            program_origin: true,
        });
    }

    /// OSC 133 / 633 marks (T-14 §1). `A`-`D` raise a mark event; `L` and `P` do not.
    fn osc_mark(&mut self, buf: &[u8]) {
        let Some(&k) = buf.first() else {
            return;
        };
        if !matches!(k, b'A' | b'B' | b'C' | b'D' | b'L' | b'P')
            || (buf.len() > 1 && buf[1] != b';')
        {
            return;
        }
        let rest = std::str::from_utf8(buf.get(2..).unwrap_or(&[])).unwrap_or("");
        let (c, y) = (self.cur, self.y);
        let now = self.now;
        // Blocks are recorded on the primary screen (the one with history); marks on the
        // alternate screen only set the semantic class.
        let primary = c == 0;
        let at = if primary {
            let g = &self.panes[0].grid;
            g.pin_at(g.history_rows() + y, self.x.min(self.cols - 1))
        } else {
            None
        };
        let mut exit = None;
        let before = self
            .blocks
            .last()
            .map(|b| (b.id, b.state == shell::BlockState::Running));
        let block = match k {
            b'L' => {
                // Fresh line: CR LF unless already in column 0; opens no block.
                if self.x != 0 || self.wrap {
                    self.cursor_moved();
                    self.cr();
                    self.index();
                }
                return;
            }
            b'A' | b'P' => {
                let o = shell::parse_opts(k, rest);
                self.semantic = Semantic::Prompt;
                match o.kind.unwrap_or(shell::PromptKind::Primary) {
                    shell::PromptKind::Continuation | shell::PromptKind::Right => {
                        self.blocks.current_id()
                    }
                    shell::PromptKind::Primary if k == b'P' => self.blocks.current_id(),
                    shell::PromptKind::Primary => {
                        self.panes[c]
                            .grid
                            .set_row_flag(y, apoth_grid::flags::PROMPT_START, true);
                        self.bp_prompted = true;
                        if primary {
                            self.blocks.prompt(at, now)
                        } else {
                            0
                        }
                    }
                }
            }
            b'B' => {
                self.semantic = Semantic::Input;
                if primary { self.blocks.input(at) } else { 0 }
            }
            b'C' => {
                self.semantic = Semantic::Output;
                let o = shell::parse_opts(k, rest);
                if primary {
                    let cwd = self.cwd_uri.clone();
                    self.blocks.output(at, now, cwd.as_deref(), o.cmdline)
                } else {
                    0
                }
            }
            _ => {
                self.semantic = Semantic::Output;
                exit = rest.split(';').next().and_then(|s| s.parse::<i32>().ok());
                if primary {
                    self.blocks.end(at, now, exit);
                }
                0
            }
        };
        self.command_transitions(before);
        if block != 0 {
            self.panes[c].grid.set_row_block(y, block);
        }
        if k == b'P' {
            return;
        }
        let m = Mark {
            kind: char::from(k),
            exit,
            row: self.y as u32,
            col: self.x as u32,
        };
        self.events.push_mark(m);
    }

    /// Queue the command events one mark caused: the block that was running finished
    /// (explicitly by `D` or implicitly by a new prompt), and the newest block started.
    fn command_transitions(&mut self, before: Option<(u32, bool)>) {
        if let Some((id, true)) = before
            && self
                .blocks
                .get(id)
                .is_some_and(|b| b.state == shell::BlockState::Done)
        {
            self.events.push_command(CommandEvent {
                block: id,
                finished: true,
            });
        }
        if let Some(b) = self.blocks.last()
            && b.state == shell::BlockState::Running
            && before != Some((b.id, true))
        {
            let block = b.id;
            self.events.push_command(CommandEvent {
                block,
                finished: false,
            });
        }
    }

    /// `OSC 1337 ; SetUserVar=<name>=<base64>` (T-11 §9). Names are 1..=64 bytes of
    /// `[A-Za-z0-9_.-]`; values decode to at most 1 KiB of UTF-8 with no control characters.
    /// An empty value removes the variable. At most 64 names; a new name past that is
    /// dropped (and counted). Every other 1337 subcommand is ignored.
    fn osc_user_var(&mut self, buf: &[u8]) {
        let Some(rest) = buf.strip_prefix(b"SetUserVar=") else {
            return;
        };
        let (name, b64) = match rest.iter().position(|&b| b == b'=') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &b""[..]),
        };
        let name_ok = (1..=crate::USER_VAR_NAME_MAX).contains(&name.len())
            && name
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
        if !name_ok {
            return;
        }
        let Ok(name) = String::from_utf8(name.to_vec()) else {
            return;
        };
        let Some(raw) = b64_decode(b64) else {
            self.events.dropped.bad_base64 += 1;
            return;
        };
        if raw.len() > crate::USER_VAR_VALUE_MAX {
            return;
        }
        let Ok(val) = String::from_utf8(raw) else {
            return;
        };
        if val.chars().any(char::is_control) {
            return;
        }
        if val.is_empty() {
            if self.user_vars.remove(&name).is_some() {
                self.events.push_var(UserVar { name, value: None });
            }
            return;
        }
        if !self.user_vars.contains_key(&name) && self.user_vars.len() >= crate::USER_VARS_MAX {
            self.events.dropped.vars += 1;
            return;
        }
        if self.user_vars.get(&name) == Some(&val) {
            return;
        }
        self.user_vars.insert(name.clone(), val.clone());
        self.events.push_var(UserVar {
            name,
            value: Some(val),
        });
    }

    fn decrqss(&mut self, pt: &[u8]) {
        let body = match pt {
            b"m" => Some(format!("{}m", sgr::report(&self.pen))),
            b" q" => {
                let cs = self.cursor_style;
                let ps = u32::from(cs.shape) * 2 + if cs.blink { 1 } else { 2 };
                Some(format!("{ps} q"))
            }
            b"r" => Some(format!("{};{}r", self.top + 1, self.bottom + 1)),
            b"s" => Some(format!("{};{}s", self.left + 1, self.right + 1)),
            _ => None,
        };
        match body {
            Some(b) => self.reply_str(format!("\x1bP1$r{b}\x1b\\")),
            None => self.reply_str("\x1bP0$r\x1b\\".to_string()),
        }
    }
}

fn hex_upper(b: &[u8]) -> String {
    use std::fmt::Write;
    b.iter().fold(String::new(), |mut s, x| {
        let _ = write!(s, "{x:02X}");
        s
    })
}

/// Hex (either case, even length, non-empty) to bytes.
fn unhex(h: &[u8]) -> Option<Vec<u8>> {
    if h.is_empty() || !h.len().is_multiple_of(2) {
        return None;
    }
    h.chunks(2)
        .map(|p| {
            let hi = char::from(p[0]).to_digit(16)?;
            let lo = char::from(p[1]).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64() {
        assert_eq!(b64_decode(b"aGk="), Some(b"hi".to_vec()));
        assert_eq!(b64_decode(b"aGk"), Some(b"hi".to_vec()));
        assert_eq!(b64_decode(b""), Some(vec![]));
        assert_eq!(b64_decode(b"aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(b64_decode(b"a"), None);
        assert_eq!(b64_decode(b"a!b="), None);
    }

    #[test]
    fn sanitise_strips_controls_and_bidi() {
        assert_eq!(sanitize(b"a\x01b\x7fc"), "abc");
        assert_eq!(sanitize("a\u{202e}b\u{85}c".as_bytes()), "abc");
    }
}
