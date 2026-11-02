// SPDX-License-Identifier: AGPL-3.0-only
//! The pane socketpair: one `PaneRequest` in, one `PaneReply` out.
//!
//! Frame: `u32` little-endian length, then that many bytes of postcard. The
//! daemon is untrusted and so is its peer's length field: it is capped before
//! anything is allocated.

use std::io::{self, Read, Write};

use ecl_portal_proto::{PaneReply, PaneRequest};

/// Most bytes a frame may carry. A source list of a few hundred windows fits
/// many times over.
pub const MAX_FRAME: usize = 1 << 20;

/// `--fd N` out of the arguments (after the program name). `None` when absent
/// or not a plain non-negative number above stderr.
pub fn parse_fd(args: &[String]) -> Option<i32> {
    let at = args.iter().position(|a| a == "--fd")?;
    let n: i32 = args.get(at + 1)?.parse().ok()?;
    (n > 2).then_some(n)
}

/// Read one framed request.
pub fn read_request(r: &mut impl Read) -> io::Result<PaneRequest> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pane request too large",
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    postcard::from_bytes(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Write one framed reply.
pub fn write_reply(w: &mut impl Write, reply: &PaneReply) -> io::Result<()> {
    let body =
        postcard::to_stdvec(reply).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "reply too large"))?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecl_portal_proto::{IdentitySource, Requester};

    fn req() -> PaneRequest {
        PaneRequest::ScreenshotMode {
            requester: Requester {
                app_id: "org.example.App".into(),
                display_name: None,
                icon: None,
                source: IdentitySource::Host,
            },
        }
    }

    fn frame(body: &[u8]) -> Vec<u8> {
        let mut v = (body.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn a_request_round_trips_through_a_frame() {
        let bytes = frame(&postcard::to_stdvec(&req()).unwrap());
        assert_eq!(read_request(&mut bytes.as_slice()).unwrap(), req());
    }

    #[test]
    fn a_reply_is_length_prefixed_little_endian() {
        let mut out = Vec::new();
        write_reply(&mut out, &PaneReply::Cancelled).unwrap();
        let n = u32::from_le_bytes(out[..4].try_into().unwrap()) as usize;
        assert_eq!(n, out.len() - 4);
        assert_eq!(
            postcard::from_bytes::<PaneReply>(&out[4..]).unwrap(),
            PaneReply::Cancelled
        );
    }

    #[test]
    fn an_oversized_length_is_refused_before_allocating() {
        let bytes = u32::MAX.to_le_bytes().to_vec();
        let e = read_request(&mut bytes.as_slice()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_truncated_or_garbled_frame_is_an_error() {
        let mut short = frame(&postcard::to_stdvec(&req()).unwrap());
        short.pop();
        assert!(read_request(&mut short.as_slice()).is_err());
        assert!(read_request(&mut frame(&[0xff, 0xff, 0xff]).as_slice()).is_err());
        assert!(read_request(&mut [].as_slice()).is_err());
    }

    #[test]
    fn fd_argument() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_fd(&a(&["--fd", "7"])), Some(7));
        assert_eq!(parse_fd(&a(&["x", "--fd", "12"])), Some(12));
        assert_eq!(parse_fd(&a(&["--fd"])), None);
        assert_eq!(parse_fd(&a(&["--fd", "1"])), None);
        assert_eq!(parse_fd(&a(&["--fd", "-3"])), None);
        assert_eq!(parse_fd(&a(&["--fd", "x"])), None);
        assert_eq!(parse_fd(&a(&[])), None);
    }
}
