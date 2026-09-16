// SPDX-License-Identifier: AGPL-3.0-only

//! Best-effort scrubbing of secret-shaped text, run on OCR output before
//! anything downstream sees it (spec §3.2, §6).
//!
//! **This is not a guarantee and must never be described as one.** There is
//! no per-application allow/deny list behind it (spec §2.2 dropped app
//! scoping), so this regex pass is the only thing standing between whatever
//! happened to be on screen and the model call. It catches shapes it knows.
//! A secret that does not look like one goes straight through.
//!
//! Two properties shape the implementation:
//!
//! * **Linear time.** The input is OCR of pixels an adversary chose. The
//!   `regex` crate has no backtracking, so a hostile page cannot turn a
//!   screenful of text into a stall — this is why nothing here hand-rolls a
//!   scanner or reaches for a backtracking engine.
//! * **Shape is preserved.** Each hit becomes a marker naming its class
//!   rather than vanishing, so the model still sees that a key was *there*
//!   and can answer about the page instead of about a hole in it.
//!
//! False positives cost more than they look: redacting ordinary prose makes
//! the product useless. Every pattern here is paired with a test asserting
//! an adjacent near-miss survives.

use std::sync::OnceLock;

use regex::{Captures, Match, Regex};

/// Compiled once per process. The set is small and every pattern is used on
/// every call, so there is nothing to gain from compiling them lazily one at
/// a time.
struct Patterns {
    pem: Regex,
    bearer: Regex,
    jwt: Regex,
    openai: Regex,
    github: Regex,
    aws: Regex,
    slack: Regex,
    labeled: Regex,
    hex: Regex,
    base64: Regex,
    card: Regex,
}

/// The prefix of every marker this module emits. Also the guard that makes
/// `redact` idempotent: a value that is already a marker is left alone.
const MARKER: &str = "[REDACTED:";

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        // Only the armour lines. The body is base64 and the base64 rule
        // below eats it; matching across lines would mean a dot-all pattern
        // over adversary-chosen text for no extra coverage.
        pem: Regex::new(r"-----(?:BEGIN|END) [A-Z ]{0,40}PRIVATE KEY-----").unwrap(),
        bearer: Regex::new(r"(?i)(authorization[ \t]*:[ \t]*)(bearer[ \t]+)?[^\r\n]+").unwrap(),
        // Three dot-separated base64url segments starting at a `{"` header.
        // The signature may be empty (`alg: none`), which is itself worth
        // redacting.
        jwt: Regex::new(r"eyJ[A-Za-z0-9_=-]{4,}\.[A-Za-z0-9_=-]{4,}\.[A-Za-z0-9_=-]*").unwrap(),
        openai: Regex::new(r"sk-(?:proj-)?[A-Za-z0-9_-]{16,}").unwrap(),
        github: Regex::new(r"gh[pousr]_[A-Za-z0-9]{20,}").unwrap(),
        aws: Regex::new(r"(?:AKIA|ASIA|AGPA|AIDA|AROA|ANPA)[0-9A-Z]{16}").unwrap(),
        slack: Regex::new(r"xox[baprs]-[A-Za-z0-9-]{10,}").unwrap(),
        // The label is kept deliberately: "there is a password field here"
        // is useful context and leaks nothing.
        labeled: Regex::new(
            r"(?i)(password|passwd|pwd|secret|token|api[-_ ]?key)([ \t]*[:=][ \t]*)([^\r\n]+)",
        )
        .unwrap(),
        hex: Regex::new(r"[0-9a-fA-F]{32,}").unwrap(),
        base64: Regex::new(r"[A-Za-z0-9+/]{40,}={0,2}").unwrap(),
        // Digit runs with optional single separators. Luhn (below) is what
        // decides; this pattern only finds candidates.
        card: Regex::new(r"[0-9](?:[ -]?[0-9]){12,18}").unwrap(),
    })
}

/// Replace secret-shaped substrings with class markers.
///
/// Best-effort only — see the module docs. Never present the output as
/// sanitised, only as scrubbed of the shapes listed here.
pub fn redact(text: &str) -> String {
    let p = patterns();

    // Order is load-bearing: the specific, self-identifying shapes go first
    // so a JWT is reported as a JWT rather than as a generic base64 run, and
    // the labeled-line rule runs after them so `token: eyJ…` keeps its label
    // instead of swallowing an already-placed marker.
    let mut out = p.pem.replace_all(text, "[REDACTED:private_key]").into_owned();
    out = p
        .bearer
        .replace_all(&out, |c: &Captures| {
            if already_redacted(c.get(0).map(|m| m.as_str()).unwrap_or_default()) {
                c[0].to_owned()
            } else {
                format!("{}[REDACTED:bearer]", &c[1])
            }
        })
        .into_owned();
    out = p.jwt.replace_all(&out, "[REDACTED:jwt]").into_owned();
    out = p.openai.replace_all(&out, "[REDACTED:api_key]").into_owned();
    out = p.github.replace_all(&out, "[REDACTED:api_key]").into_owned();
    out = p.aws.replace_all(&out, "[REDACTED:aws_key]").into_owned();
    out = p.slack.replace_all(&out, "[REDACTED:api_key]").into_owned();
    out = p
        .labeled
        .replace_all(&out, |c: &Captures| {
            if already_redacted(&c[3]) {
                c[0].to_owned()
            } else {
                format!("{}{}[REDACTED:secret]", &c[1], &c[2])
            }
        })
        .into_owned();
    out = replace_bounded(&p.hex, &out, "[REDACTED:hex]", |_| true);
    out = replace_bounded(&p.base64, &out, "[REDACTED:base64]", mixed_alphabet);
    out = replace_bounded(&p.card, &out, "[REDACTED:card]", luhn);
    out
}

fn already_redacted(value: &str) -> bool {
    value.trim_start().starts_with(MARKER)
}

/// `replace_all` with two extra conditions the regex itself cannot express:
/// the match must not be a fragment of a longer alphanumeric token, and
/// `accept` must agree. Both exist to protect near-misses — half of an
/// identifier is not a secret, and neither is a long ordinary number.
fn replace_bounded(re: &Regex, hay: &str, marker: &str, accept: fn(&str) -> bool) -> String {
    re.replace_all(hay, |c: &Captures| {
        let m: Match = c.get(0).expect("group 0 always exists");
        if standalone(hay, m) && accept(m.as_str()) {
            marker.to_owned()
        } else {
            m.as_str().to_owned()
        }
    })
    .into_owned()
}

fn standalone(hay: &str, m: Match) -> bool {
    let bytes = hay.as_bytes();
    let before_ok = m.start() == 0 || !bytes[m.start() - 1].is_ascii_alphanumeric();
    let after_ok = m.end() == bytes.len() || !bytes[m.end()].is_ascii_alphanumeric();
    before_ok && after_ok
}

/// Random base64 mixes cases and digits; a long lowercase URL slug or a
/// hyphen-free English compound does not. This is the difference between
/// scrubbing a key and scrubbing the page the user asked about.
fn mixed_alphabet(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_lowercase())
        && s.bytes().any(|b| b.is_ascii_uppercase())
        && s.bytes().any(|b| b.is_ascii_digit())
}

/// Luhn is the discriminator that lets ordinary 16-digit numbers — order
/// ids, timestamps pasted together, phone strings — survive.
fn luhn(s: &str) -> bool {
    let digits: Vec<u32> = s
        .bytes()
        .filter(|b| b.is_ascii_digit())
        .map(|b| u32::from(b - b'0'))
        .collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 {
                    x - 9
                } else {
                    x
                }
            } else {
                *d
            }
        })
        .sum();
    sum % 10 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_pattern() {
        let hit = redact("auth uses eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NSJ9.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk here");
        assert!(hit.contains("[REDACTED:jwt]"), "{hit}");
        // Near-miss: a dotted identifier that merely starts with the same
        // letters is ordinary code.
        let miss = "eyJson.parse.result";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn openai_key_pattern() {
        assert!(redact("key sk-abcdefghijklmnop0123456789").contains("[REDACTED:api_key]"));
        let miss = "the sk-1 flag";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn github_key_pattern() {
        assert!(redact("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123").contains("[REDACTED:api_key]"));
        let miss = "ghp_short";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn aws_key_pattern() {
        assert!(redact("AKIAIOSFODNN7EXAMPLE").contains("[REDACTED:aws_key]"));
        let miss = "AKIA is a prefix";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn slack_key_pattern() {
        assert!(redact("xoxb-123456789012-abcdefghijkl").contains("[REDACTED:api_key]"));
        let miss = "xoxo-hello";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn long_hex_pattern() {
        assert_eq!(
            redact("sha 0123456789abcdef0123456789abcdef done"),
            "sha [REDACTED:hex] done"
        );
        // 31 characters: below the floor, and a short commit hash must not
        // disappear from a page about git.
        let miss = "sha 0123456789abcdef0123456789abcde done";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn long_base64_pattern() {
        let hit = redact("blob QWxhZGRpbjpvcGVuIHNlc2FtZQ1234abcdEFGH5678ijkl==");
        assert!(hit.contains("[REDACTED:base64]"), "{hit}");
        // Near-miss: a long all-lowercase slug is prose, not entropy.
        let miss = "see supercalifragilisticexpialidociousandthensome for details";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn labeled_secret_lines() {
        assert_eq!(redact("password: hunter2"), "password: [REDACTED:secret]");
        assert_eq!(redact("passwd=swordfish"), "passwd=[REDACTED:secret]");
        assert_eq!(redact("API_KEY: abc"), "API_KEY: [REDACTED:secret]");
        assert_eq!(redact("token = xyz"), "token = [REDACTED:secret]");
        // Near-miss: prose mentioning the word keeps its sentence.
        let miss = "Your password must be at least twelve characters long.";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn card_pattern() {
        assert_eq!(redact("card 4111 1111 1111 1111 ok"), "card [REDACTED:card] ok");
        // Near-miss: same length, fails Luhn — an order number survives.
        let miss = "order 4111111111111112 ok";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn private_key_header_pattern() {
        let hit = redact("-----BEGIN RSA PRIVATE KEY-----");
        assert_eq!(hit, "[REDACTED:private_key]");
        let miss = "-----BEGIN CERTIFICATE-----";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn authorization_bearer_pattern() {
        assert_eq!(
            redact("Authorization: Bearer abc.def.ghi"),
            "Authorization: [REDACTED:bearer]"
        );
        let miss = "Authorization is handled by the gateway";
        assert_eq!(redact(miss), miss);
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = redact(
            "password: hunter2\nAuthorization: Bearer abc\nsk-abcdefghijklmnop0123456789\n4111 1111 1111 1111",
        );
        assert_eq!(redact(&once), once);
    }

    #[test]
    fn prose_passes_through_unchanged() {
        let prose = "The compositor owns presentation: Oracle-Eyes supplies a rectangle and a \
                     string, and everything about how that string is drawn — placement, \
                     wrapping, clamping — belongs to the other side of the socket.";
        assert_eq!(redact(prose), prose);
    }
}
