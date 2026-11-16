// SPDX-License-Identifier: Apache-2.0
//! The armed askpass channel (T-22 §3, SECURITY-30): what `apoth askpass` sends on the
//! dedicated `askpass-<random>.sock` and what the instance answers. Framing only.
//!
//! Request (one per connection, the client then shuts its write side):
//! `APASS1 <kind> <token>\n<prompt bytes>` — `kind` is `secret`, `confirm` or `none`
//! (ssh's `SSH_ASKPASS_PROMPT`), `token` the 64 lowercase hex digits of the 256-bit
//! one-time token, the prompt at most [`MAX_PROMPT`] bytes (program text: the remote's).
//!
//! Answer: one status byte, then for [`Status::Ok`] the secret (at most [`MAX_SECRET`]
//! bytes, no newline). The secret is never logged or kept by either side.
//!
//! The token is never part of any other message; this module has no `Debug` that prints it.

/// Environment variable naming the armed socket (set on the ssh child only).
pub const ENV_SOCK: &str = "APOTHEOSIS_ASKPASS_SOCK";
/// Environment variable carrying the one-time token (set on the ssh child only).
pub const ENV_TOKEN: &str = "APOTHEOSIS_ASKPASS_TOKEN";
/// Token length in bytes (256 bits).
pub const TOKEN_BYTES: usize = 32;
/// Hex digits of a token.
pub const TOKEN_HEX: usize = 2 * TOKEN_BYTES;
/// Largest prompt carried (T-18 §5 budget; ssh prompts are a line or a host-key block).
pub const MAX_PROMPT: usize = 4096;
/// Largest secret (T-18 §5 budget).
pub const MAX_SECRET: usize = 1024;
/// Largest request on the wire.
pub const MAX_REQUEST: usize = MAGIC.len() + 8 + 1 + TOKEN_HEX + 1 + MAX_PROMPT;
/// Host label budget (the armed `apoth ssh` destination).
pub const MAX_HOST: usize = 255;

const MAGIC: &str = "APASS1 ";

/// What ssh asks for (`SSH_ASKPASS_PROMPT`, ssh-askpass(1)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A passphrase, password or a typed answer (host-key `yes`).
    Secret,
    /// Allow or deny; the answer is the exit status.
    Confirm,
    /// Information only (a touch-the-key notice): no answer.
    None,
}

impl Kind {
    /// The word on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Secret => "secret",
            Kind::Confirm => "confirm",
            Kind::None => "none",
        }
    }

    /// From `SSH_ASKPASS_PROMPT` (unset or anything else = a secret).
    #[must_use]
    pub fn from_env(v: Option<&str>) -> Kind {
        match v {
            Some("confirm") => Kind::Confirm,
            Some("none") => Kind::None,
            _ => Kind::Secret,
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s {
            "secret" => Some(Kind::Secret),
            "confirm" => Some(Kind::Confirm),
            "none" => Some(Kind::None),
            _ => None,
        }
    }
}

/// The answer's status byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    /// Answered: the secret follows (empty for confirm / none).
    Ok = 0,
    /// The user cancelled.
    Cancelled = 1,
    /// Refused: bad token, wrong ancestry, one already pending, or not armed.
    Refused = 3,
}

impl Status {
    /// From the first answer byte.
    #[must_use]
    pub fn from_byte(b: u8) -> Option<Status> {
        match b {
            0 => Some(Status::Ok),
            1 => Some(Status::Cancelled),
            3 => Some(Status::Refused),
            _ => None,
        }
    }
}

/// A decoded request. Borrowed; the token is compared with [`token_eq`], never printed.
pub struct Request<'a> {
    /// What ssh asks for.
    pub kind: Kind,
    /// The token as sent (hex).
    pub token: &'a [u8],
    /// The prompt (untrusted program text; may be invalid UTF-8).
    pub prompt: &'a [u8],
}

impl std::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("kind", &self.kind)
            .field("prompt_len", &self.prompt.len())
            .finish_non_exhaustive()
    }
}

/// Why a request was malformed (no content).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Malformed;

/// Encode a request. `token_hex` must be [`TOKEN_HEX`] bytes; the prompt is cut to
/// [`MAX_PROMPT`] bytes.
#[must_use]
pub fn encode_request(kind: Kind, token_hex: &[u8], prompt: &[u8]) -> Vec<u8> {
    let prompt = &prompt[..prompt.len().min(MAX_PROMPT)];
    let mut v = Vec::with_capacity(MAGIC.len() + 9 + token_hex.len() + 1 + prompt.len());
    v.extend_from_slice(MAGIC.as_bytes());
    v.extend_from_slice(kind.as_str().as_bytes());
    v.push(b' ');
    v.extend_from_slice(token_hex);
    v.push(b'\n');
    v.extend_from_slice(prompt);
    v
}

/// Decode a whole request (everything the client sent before shutting its write side).
pub fn decode_request(b: &[u8]) -> Result<Request<'_>, Malformed> {
    if b.len() > MAX_REQUEST {
        return Err(Malformed);
    }
    let rest = b.strip_prefix(MAGIC.as_bytes()).ok_or(Malformed)?;
    let nl = rest.iter().position(|&c| c == b'\n').ok_or(Malformed)?;
    let (head, prompt) = (&rest[..nl], &rest[nl + 1..]);
    let sp = head.iter().position(|&c| c == b' ').ok_or(Malformed)?;
    let kind = std::str::from_utf8(&head[..sp])
        .ok()
        .and_then(Kind::parse)
        .ok_or(Malformed)?;
    let token = &head[sp + 1..];
    if token.len() != TOKEN_HEX
        || !token
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    {
        return Err(Malformed);
    }
    if prompt.len() > MAX_PROMPT {
        return Err(Malformed);
    }
    Ok(Request {
        kind,
        token,
        prompt,
    })
}

/// Lowercase hex of a token.
#[must_use]
pub fn token_hex(t: &[u8; TOKEN_BYTES]) -> [u8; TOKEN_HEX] {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; TOKEN_HEX];
    for (i, b) in t.iter().enumerate() {
        out[2 * i] = D[usize::from(b >> 4)];
        out[2 * i + 1] = D[usize::from(b & 15)];
    }
    out
}

/// Constant-time equality (length is public).
#[must_use]
pub fn token_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    std::hint::black_box(d) == 0
}

/// Overwrite a buffer that held a secret. Best effort without `unsafe`: the write is kept
/// alive by `black_box`, so the optimiser cannot drop it as dead.
pub fn wipe(b: &mut [u8]) {
    b.fill(0);
    std::hint::black_box(&*b);
}

/// Fuzz body: arbitrary bytes through the request decoder never panic and a decoded
/// request re-encodes to the same bytes.
pub fn fuzz_askpass(data: &[u8]) {
    if let Ok(r) = decode_request(data) {
        let again = encode_request(r.kind, r.token, r.prompt);
        assert_eq!(again, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: [u8; TOKEN_BYTES] = [0xab; TOKEN_BYTES];

    #[test]
    fn round_trip_and_limits() {
        let hex = token_hex(&T);
        assert_eq!(&hex[..4], b"abab");
        for kind in [Kind::Secret, Kind::Confirm, Kind::None] {
            let w = encode_request(kind, &hex, b"pw for x: ");
            let r = decode_request(&w).unwrap();
            assert_eq!(r.kind, kind);
            assert!(token_eq(r.token, &hex));
            assert_eq!(r.prompt, b"pw for x: ");
        }
        // A prompt over the budget is cut by the encoder, refused by the decoder.
        let long = vec![b'x'; MAX_PROMPT + 10];
        let w = encode_request(Kind::Secret, &hex, &long);
        assert_eq!(decode_request(&w).unwrap().prompt.len(), MAX_PROMPT);
        let mut over = w.clone();
        over.push(b'y');
        assert!(decode_request(&over).is_err());
    }

    #[test]
    fn malformed_requests_are_refused() {
        let hex = token_hex(&T);
        let mut upper = encode_request(Kind::Secret, &hex, b"p");
        upper[MAGIC.len() + 7] = b'A';
        for bad in [
            &b""[..],
            b"APASS1 secret\n",
            b"APASS2 secret 00\n",
            b"APASS1 nope 0000\nprompt",
            &upper,
        ] {
            assert!(decode_request(bad).is_err());
        }
        let short = encode_request(Kind::Secret, &hex[..10], b"p");
        assert!(decode_request(&short).is_err());
        assert!(
            format!(
                "{:?}",
                decode_request(&encode_request(Kind::Secret, &hex, b"p"))
            )
            .contains("prompt_len")
        );
    }

    #[test]
    fn token_compare_and_wipe() {
        assert!(token_eq(b"abc", b"abc"));
        assert!(!token_eq(b"abc", b"abd"));
        assert!(!token_eq(b"abc", b"ab"));
        let mut s = *b"hunter2";
        wipe(&mut s);
        assert_eq!(s, [0; 7]);
        assert_eq!(Kind::from_env(Some("confirm")), Kind::Confirm);
        assert_eq!(Kind::from_env(None), Kind::Secret);
        assert_eq!(Status::from_byte(3), Some(Status::Refused));
        assert_eq!(Status::from_byte(9), None);
    }

    #[test]
    fn fuzz_body_survives_its_seeds() {
        let hex = token_hex(&T);
        fuzz_askpass(&encode_request(Kind::Confirm, &hex, b"Allow?"));
        fuzz_askpass(b"APASS1 secret ");
        fuzz_askpass(&[0xff; 300]);
    }
}
