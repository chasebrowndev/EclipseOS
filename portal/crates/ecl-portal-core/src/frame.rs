// SPDX-License-Identifier: AGPL-3.0-only

//! Length-prefixed postcard framing for the daemon <-> pane socketpair:
//! `u32` big-endian length, then that many postcard bytes.

use serde::{de::DeserializeOwned, Serialize};

/// A frame larger than this is a protocol violation (a pane is untrusted).
pub const MAX_FRAME: usize = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame too large ({0} bytes)")]
    TooLarge(usize),
    #[error("malformed frame: {0}")]
    Decode(#[from] postcard::Error),
}

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let body = postcard::to_stdvec(msg)?;
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(body.len()));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Validate a length header read off the wire.
pub fn check_len(header: [u8; 4]) -> Result<usize, FrameError> {
    let n = u32::from_be_bytes(header) as usize;
    if n > MAX_FRAME {
        Err(FrameError::TooLarge(n))
    } else {
        Ok(n)
    }
}

pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    Ok(postcard::from_bytes(body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecl_portal_proto::{PaneReply, PaneRequest, Requester};

    #[test]
    fn round_trip() {
        let r = PaneReply::Permission {
            allow: true,
            remember: false,
        };
        let f = encode(&r).unwrap();
        let n = check_len(f[..4].try_into().unwrap()).unwrap();
        assert_eq!(n, f.len() - 4);
        assert_eq!(decode::<PaneReply>(&f[4..]).unwrap(), r);
    }

    #[test]
    fn request_encodes() {
        let q = PaneRequest::PickColor {
            requester: Requester {
                app_id: "host".into(),
                display_name: None,
                icon: None,
                source: ecl_portal_proto::IdentitySource::Host,
            },
        };
        assert!(encode(&q).unwrap().len() > 4);
    }

    #[test]
    fn oversize_and_garbage_rejected() {
        assert!(check_len((MAX_FRAME as u32 + 1).to_be_bytes()).is_err());
        assert!(decode::<PaneReply>(&[0xff, 0xff, 0xff]).is_err());
    }
}
