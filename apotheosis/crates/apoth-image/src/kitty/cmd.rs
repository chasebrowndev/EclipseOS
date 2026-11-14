// SPDX-License-Identifier: AGPL-3.0-only
//! Kitty graphics control-data parser and command model (T-15 §1, GRAPHICS-01..13).
//!
//! Input is one APC body starting at its first byte: `G<key=value,...>[;<base64>]`.
//! Unknown keys are ignored (tolerant parser, GRAPHICS-03); a malformed value on a known
//! key is `EINVAL`, reported with whatever ids were readable so the client gets a reply.

use crate::ErrCode;

/// The three client-visible ids of a request. Zero means "not given".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Ids {
    /// `i=`
    pub id: u32,
    /// `I=` image number (terminal picks the id).
    pub number: u32,
    /// `p=`
    pub placement: u32,
}

/// Pixel format `f=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Rgb24,
    Rgba32,
    Png,
}

/// Transmission medium `t=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    Direct,
    File,
    TempFile,
    Shm,
}

/// Everything a transmission (`a=t|T|q|f`) carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransmitSpec {
    pub format: Format,
    pub medium: Medium,
    /// `o=z`
    pub compressed: bool,
    /// `s=` pixels
    pub width: u32,
    /// `v=` pixels
    pub height: u32,
    /// `S=` bytes to read from a file/shm medium (0 = to the end).
    pub size: u32,
    /// `O=` byte offset into a file/shm medium.
    pub offset: u32,
}

/// Relative placement parent (`P,Q,H,V`). Parsed only; limits are R2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Relative {
    pub parent_id: u32,
    pub parent_placement: u32,
    pub h: i32,
    pub v: i32,
}

/// A placement request (`a=p`, or the display half of `a=T`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceSpec {
    /// Source rectangle in physical pixels; zero w/h = to the edge.
    pub src_x: u32,
    pub src_y: u32,
    pub src_w: u32,
    pub src_h: u32,
    /// `c=`, `r=` cell box (0 = natural size).
    pub cols: u32,
    pub rows: u32,
    /// `X=`, `Y=` sub-cell pixel offsets.
    pub x_off: u32,
    pub y_off: u32,
    /// `z=`; below [`Z_BELOW_BACKGROUND`] draws under non-default cell backgrounds.
    pub z: i32,
    /// `C=1`: do not move the cursor.
    pub no_cursor_move: bool,
    /// `U=1`: virtual placement for Unicode placeholders.
    pub unicode: bool,
    pub relative: Option<Relative>,
}

/// Z-index threshold under which images draw below non-default cell backgrounds
/// (GRAPHICS-08).
pub const Z_BELOW_BACKGROUND: i32 = -1_073_741_824;

/// What a delete selects (`d=`); the free flag is the uppercase variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTarget {
    /// `a`: all placements visible on screen.
    Visible,
    /// `i`: by image id, optional placement id.
    Id { id: u32, placement: u32 },
    /// `n`: newest image with this number.
    Number { number: u32, placement: u32 },
    /// `c`: placements intersecting the cursor cell.
    Cursor,
    /// `f`: animation frames.
    Frames,
    /// `p`: placements intersecting cell (x, y), 1-based.
    Cell { x: u32, y: u32 },
    /// `q`: as `p`, restricted to z-index.
    CellZ { x: u32, y: u32, z: i32 },
    /// `r`: image ids in `lo..=hi`.
    IdRange { lo: u32, hi: u32 },
    /// `x`: placements intersecting column.
    Column(u32),
    /// `y`: placements intersecting row.
    Row(u32),
    /// `z`: placements with this z-index.
    Z(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteSpec {
    pub target: DeleteTarget,
    /// Uppercase selector: also free image data no placement references.
    pub free: bool,
}

/// `a=f` frame load. Parsed only (R2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSpec {
    pub transmit: TransmitSpec,
    pub x: u32,
    pub y: u32,
    /// `c=` base frame.
    pub base_frame: u32,
    /// `r=` frame to edit.
    pub edit_frame: u32,
    /// `z=` gap in ms.
    pub gap_ms: i32,
    /// `X=` composition mode (0 blend, 1 overwrite).
    pub composition: u32,
    /// `Y=` background colour (RGBA).
    pub background: u32,
}

/// `a=a` animation control. Parsed only (R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimSpec {
    /// `s=` state: 1 stop, 2 wait, 3 loop.
    pub state: u32,
    /// `v=` loop count.
    pub loops: u32,
    /// `r=` frame whose gap `z` edits.
    pub edit_frame: u32,
    /// `c=` current frame.
    pub current_frame: u32,
    pub gap_ms: i32,
}

/// `a=c` frame composition. Parsed only (R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposeSpec {
    pub src_frame: u32,
    pub dst_frame: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    /// `C=1`: replace instead of blend.
    pub replace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `a=t`
    Transmit(TransmitSpec),
    /// `a=T`
    TransmitPlace(TransmitSpec, PlaceSpec),
    /// `a=p`
    Place(PlaceSpec),
    /// `a=d`
    Delete(DeleteSpec),
    /// `a=q`: probe a load without storing. `e` is the optional capability probe.
    Query(TransmitSpec),
    Frame(FrameSpec),
    Animate(AnimSpec),
    Compose(ComposeSpec),
}

/// A parsed APC header (everything before the first `;`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub ids: Ids,
    /// `q=`: 0 reply always, 1 mute OK, 2 mute everything.
    pub quiet: u8,
    /// `m=1`: more chunks follow.
    pub more: bool,
    /// Only keys a continuation chunk may carry (`m`, `q`, `i`, `I`) were present.
    pub chunk_keys_only: bool,
    /// `e=` capability probe action, if present.
    pub probe: Option<u8>,
    pub command: Command,
}

/// A request that failed to parse; carries what is needed to reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError {
    pub ids: Ids,
    pub quiet: u8,
    pub code: ErrCode,
}

/// Parse one APC body (beginning with `G`). Returns the header and the base64 payload slice.
pub fn parse_apc(apc: &[u8]) -> Result<(Header, &[u8]), ParseError> {
    let fail = |ids, quiet| ParseError {
        ids,
        quiet,
        code: ErrCode::Einval,
    };
    let Some((&b'G', rest)) = apc.split_first() else {
        return Err(fail(Ids::default(), 0));
    };
    let (keys, payload) = match rest.iter().position(|&b| b == b';') {
        Some(p) => (&rest[..p], &rest[p + 1..]),
        None => (rest, &rest[rest.len()..]),
    };

    let mut k = Keys::default();
    let mut bad = false;
    let mut chunk_keys_only = true;
    for pair in keys.split(|&b| b == b',') {
        if pair.is_empty() {
            continue;
        }
        let (key, val) = match pair {
            [key, b'=', val @ ..] => (*key, val),
            _ => {
                bad = true;
                continue;
            }
        };
        if !matches!(key, b'm' | b'q' | b'i' | b'I') {
            chunk_keys_only = false;
        }
        if !k.set(key, val) {
            bad = true;
        }
    }
    let ids = Ids {
        id: k.id,
        number: k.number,
        placement: k.placement,
    };
    if bad || k.quiet > 2 || k.id != 0 && k.number != 0 {
        // i= together with I= is invalid per the protocol.
        return Err(fail(ids, k.quiet.min(2)));
    }
    let command = k.command().map_err(|code| ParseError {
        ids,
        quiet: k.quiet,
        code,
    })?;
    Ok((
        Header {
            ids,
            quiet: k.quiet,
            more: k.more == 1,
            chunk_keys_only,
            probe: k.probe,
            command,
        },
        payload,
    ))
}

/// Keys the terminal supports for each action; the reply to an `e=` probe.
pub fn supported_keys(action: u8) -> &'static str {
    match action {
        b't' | b'T' => "a,t,f,o,m,q,i,I,p,s,v,S,O,c,r,x,y,w,h,X,Y,z,C,U",
        b'q' => "a,t,f,o,i,I,s,v,S,O",
        b'p' => "a,i,I,p,x,y,w,h,c,r,X,Y,z,C,U,P,Q,H,V",
        b'd' => "a,d,i,I,p,x,y,z,q",
        _ => "",
    }
}

#[derive(Default)]
struct Keys {
    action: Option<u8>,
    medium: Option<u8>,
    format: Option<u32>,
    compress: Option<u8>,
    more: u8,
    quiet: u8,
    id: u32,
    number: u32,
    placement: u32,
    s: u32,
    v: u32,
    size: u32,
    offset: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    c: u32,
    r: u32,
    xo: u32,
    yo: u32,
    z: i32,
    z_raw: bool,
    cursor_keep: u32,
    unicode: u32,
    delete: Option<u8>,
    parent_id: u32,
    parent_placement: u32,
    rel_h: i32,
    rel_v: i32,
    has_parent: bool,
    probe: Option<u8>,
}

fn num(val: &[u8]) -> Option<i64> {
    if val.is_empty() || val.len() > 12 {
        return None;
    }
    let (neg, digits) = match val {
        [b'-', rest @ ..] => (true, rest),
        _ => (false, val),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut n: i64 = 0;
    for d in digits {
        n = n * 10 + i64::from(d - b'0');
    }
    Some(if neg { -n } else { n })
}

fn unum(val: &[u8]) -> Option<u32> {
    u32::try_from(num(val)?).ok()
}

fn snum(val: &[u8]) -> Option<i32> {
    i32::try_from(num(val)?).ok()
}

impl Keys {
    /// Returns false when a known key has an invalid value.
    fn set(&mut self, key: u8, val: &[u8]) -> bool {
        let ch = || (val.len() == 1).then(|| val[0]);
        match key {
            b'a' => ch().map(|c| self.action = Some(c)).is_some(),
            b't' => ch().map(|c| self.medium = Some(c)).is_some(),
            b'o' => ch().map(|c| self.compress = Some(c)).is_some(),
            b'd' => ch().map(|c| self.delete = Some(c)).is_some(),
            b'e' => ch().map(|c| self.probe = Some(c)).is_some(),
            b'f' => unum(val).map(|n| self.format = Some(n)).is_some(),
            b'm' => unum(val)
                .filter(|&n| n <= 1)
                .map(|n| self.more = n as u8)
                .is_some(),
            b'q' => unum(val)
                .filter(|&n| n <= 2)
                .map(|n| self.quiet = n as u8)
                .is_some(),
            b'i' => unum(val).map(|n| self.id = n).is_some(),
            b'I' => unum(val).map(|n| self.number = n).is_some(),
            b'p' => unum(val).map(|n| self.placement = n).is_some(),
            b's' => unum(val).map(|n| self.s = n).is_some(),
            b'v' => unum(val).map(|n| self.v = n).is_some(),
            b'S' => unum(val).map(|n| self.size = n).is_some(),
            b'O' => unum(val).map(|n| self.offset = n).is_some(),
            b'x' => unum(val).map(|n| self.x = n).is_some(),
            b'y' => unum(val).map(|n| self.y = n).is_some(),
            b'w' => unum(val).map(|n| self.w = n).is_some(),
            b'h' => unum(val).map(|n| self.h = n).is_some(),
            b'c' => unum(val).map(|n| self.c = n).is_some(),
            b'r' => unum(val).map(|n| self.r = n).is_some(),
            b'X' => unum(val).map(|n| self.xo = n).is_some(),
            b'Y' => unum(val).map(|n| self.yo = n).is_some(),
            b'z' => snum(val)
                .map(|n| {
                    self.z = n;
                    self.z_raw = true;
                })
                .is_some(),
            b'C' => unum(val).map(|n| self.cursor_keep = n).is_some(),
            b'U' => unum(val).map(|n| self.unicode = n).is_some(),
            b'P' => unum(val)
                .map(|n| {
                    self.parent_id = n;
                    self.has_parent = true;
                })
                .is_some(),
            b'Q' => unum(val).map(|n| self.parent_placement = n).is_some(),
            b'H' => snum(val).map(|n| self.rel_h = n).is_some(),
            b'V' => snum(val).map(|n| self.rel_v = n).is_some(),
            _ => true, // unknown keys are ignored
        }
    }

    fn transmit(&self) -> Result<TransmitSpec, ErrCode> {
        let format = match self.format.unwrap_or(32) {
            24 => Format::Rgb24,
            32 => Format::Rgba32,
            100 => Format::Png,
            _ => return Err(ErrCode::Einval),
        };
        let medium = match self.medium.unwrap_or(b'd') {
            b'd' => Medium::Direct,
            b'f' => Medium::File,
            b't' => Medium::TempFile,
            b's' => Medium::Shm,
            _ => return Err(ErrCode::Einval),
        };
        let compressed = match self.compress {
            None => false,
            Some(b'z') => true,
            Some(_) => return Err(ErrCode::Einval),
        };
        Ok(TransmitSpec {
            format,
            medium,
            compressed,
            width: self.s,
            height: self.v,
            size: self.size,
            offset: self.offset,
        })
    }

    fn place(&self) -> Result<PlaceSpec, ErrCode> {
        if self.cursor_keep > 1 || self.unicode > 1 {
            return Err(ErrCode::Einval);
        }
        Ok(PlaceSpec {
            src_x: self.x,
            src_y: self.y,
            src_w: self.w,
            src_h: self.h,
            cols: self.c,
            rows: self.r,
            x_off: self.xo,
            y_off: self.yo,
            z: self.z,
            no_cursor_move: self.cursor_keep == 1,
            unicode: self.unicode == 1,
            relative: self.has_parent.then_some(Relative {
                parent_id: self.parent_id,
                parent_placement: self.parent_placement,
                h: self.rel_h,
                v: self.rel_v,
            }),
        })
    }

    fn delete(&self) -> Result<DeleteSpec, ErrCode> {
        let sel = self.delete.unwrap_or(b'a');
        let free = sel.is_ascii_uppercase();
        let target = match sel.to_ascii_lowercase() {
            b'a' => DeleteTarget::Visible,
            b'i' => DeleteTarget::Id {
                id: self.id,
                placement: self.placement,
            },
            b'n' => DeleteTarget::Number {
                number: self.number,
                placement: self.placement,
            },
            b'c' => DeleteTarget::Cursor,
            b'f' => DeleteTarget::Frames,
            b'p' => DeleteTarget::Cell {
                x: self.x,
                y: self.y,
            },
            b'q' => DeleteTarget::CellZ {
                x: self.x,
                y: self.y,
                z: self.z,
            },
            b'r' => {
                if self.x > self.y {
                    return Err(ErrCode::Einval);
                }
                DeleteTarget::IdRange {
                    lo: self.x,
                    hi: self.y,
                }
            }
            b'x' => DeleteTarget::Column(self.x),
            b'y' => DeleteTarget::Row(self.y),
            b'z' => DeleteTarget::Z(self.z),
            _ => return Err(ErrCode::Einval),
        };
        Ok(DeleteSpec { target, free })
    }

    fn command(&self) -> Result<Command, ErrCode> {
        Ok(match self.action.unwrap_or(b't') {
            b't' => Command::Transmit(self.transmit()?),
            b'T' => Command::TransmitPlace(self.transmit()?, self.place()?),
            b'p' => Command::Place(self.place()?),
            b'd' => Command::Delete(self.delete()?),
            b'q' => Command::Query(self.transmit()?),
            b'f' => Command::Frame(FrameSpec {
                transmit: self.transmit()?,
                x: self.x,
                y: self.y,
                base_frame: self.c,
                edit_frame: self.r,
                gap_ms: if self.z_raw { self.z } else { 40 },
                composition: self.xo,
                background: self.yo,
            }),
            b'a' => Command::Animate(AnimSpec {
                state: self.s,
                loops: self.v,
                edit_frame: self.r,
                current_frame: self.c,
                gap_ms: self.z,
            }),
            b'c' => Command::Compose(ComposeSpec {
                src_frame: self.r,
                dst_frame: self.c,
                x: self.x,
                y: self.y,
                w: self.w,
                h: self.h,
                dst_x: self.xo,
                dst_y: self.yo,
                replace: self.cursor_keep == 1,
            }),
            _ => return Err(ErrCode::Einval),
        })
    }
}
