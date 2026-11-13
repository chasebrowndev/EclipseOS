// SPDX-License-Identifier: AGPL-3.0-only
//! MIME negotiation for clipboard, primary selection and drops (T-09 §6).
//!
//! Offer `text/plain;charset=utf-8`, `text/plain`, `UTF8_STRING`. Receive preferring UTF-8,
//! plus `text/uri-list` for file drops. Shell quoting and the paste guard sit above this crate.

use std::path::PathBuf;

/// The MIME types we offer, in order.
pub const OFFER: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// The file-drop MIME type.
pub const URI_LIST: &str = "text/uri-list";

/// What the receiver wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Want {
    /// Text only.
    Text,
    /// A list of files if offered, text otherwise.
    FilesOrText,
}

/// What the chosen type carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Payload {
    /// Text, UTF-8 (or ASCII-compatible plain text).
    Text,
    /// `text/uri-list`.
    UriList,
}

fn canonical(mime: &str) -> String {
    // Lower-case the type and drop spaces around parameters: "text/plain; charset=UTF-8"
    // and "text/plain;charset=utf-8" compare equal.
    let mut out = String::with_capacity(mime.len());
    for c in mime.chars() {
        if !c.is_whitespace() {
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

/// Is `mime` one of the types we offer? (Used to decide whether to honour a `send` request.)
pub fn is_offered(mime: &str) -> bool {
    let c = canonical(mime);
    OFFER.iter().any(|o| canonical(o) == c)
}

/// Choose the type to request from an offer. Returns the offered spelling.
pub fn choose_receive(offered: &[String], want: Want) -> Option<(&str, Payload)> {
    let find = |target: &str| {
        let t = canonical(target);
        offered
            .iter()
            .find(|o| canonical(o) == t)
            .map(String::as_str)
    };
    if want == Want::FilesOrText
        && let Some(m) = find(URI_LIST)
    {
        return Some((m, Payload::UriList));
    }
    for target in ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"] {
        if let Some(m) = find(target) {
            return Some((m, Payload::Text));
        }
    }
    None
}

/// Parse a `text/uri-list` body into local paths. Comment lines (`#`) and non-`file:` URIs
/// are skipped; `file://localhost/` and `file:///` forms are accepted; `%XX` is decoded.
pub fn parse_uri_list(body: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut out = Vec::new();
    for line in body.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let Some(rest) = line.strip_prefix(b"file://") else {
            continue;
        };
        let path = if rest.starts_with(b"/") {
            rest
        } else if let Some(p) = rest.strip_prefix(b"localhost") {
            if !p.starts_with(b"/") {
                continue;
            }
            p
        } else {
            continue; // a remote host
        };
        let decoded = percent_decode(path);
        if !decoded.is_empty() {
            out.push(PathBuf::from(std::ffi::OsString::from_vec(decoded)));
        }
    }
    out
}

fn percent_decode(s: &[u8]) -> Vec<u8> {
    let hex = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%'
            && i + 2 < s.len()
            && let (Some(h), Some(l)) = (hex(s[i + 1]), hex(s[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prefers_utf8_over_plain_over_nothing() {
        let o = v(&[
            "text/html",
            "text/plain",
            "UTF8_STRING",
            "text/plain;charset=utf-8",
        ]);
        assert_eq!(
            choose_receive(&o, Want::Text),
            Some(("text/plain;charset=utf-8", Payload::Text))
        );
        let o = v(&["text/plain", "UTF8_STRING"]);
        assert_eq!(
            choose_receive(&o, Want::Text),
            Some(("UTF8_STRING", Payload::Text))
        );
        let o = v(&["text/plain"]);
        assert_eq!(
            choose_receive(&o, Want::Text),
            Some(("text/plain", Payload::Text))
        );
        assert_eq!(choose_receive(&v(&["image/png"]), Want::Text), None);
        assert_eq!(choose_receive(&[], Want::Text), None);
    }

    #[test]
    fn matching_ignores_case_and_spacing() {
        let o = v(&["text/plain; charset=UTF-8"]);
        assert_eq!(
            choose_receive(&o, Want::Text).map(|x| x.1),
            Some(Payload::Text)
        );
    }

    #[test]
    fn file_drops_prefer_uri_list_only_when_asked() {
        let o = v(&["text/plain;charset=utf-8", "text/uri-list"]);
        assert_eq!(
            choose_receive(&o, Want::FilesOrText),
            Some(("text/uri-list", Payload::UriList))
        );
        assert_eq!(
            choose_receive(&o, Want::Text),
            Some(("text/plain;charset=utf-8", Payload::Text))
        );
        assert_eq!(choose_receive(&v(&["text/uri-list"]), Want::Text), None);
    }

    #[test]
    fn offered_set() {
        assert!(is_offered("text/plain;charset=utf-8"));
        assert!(is_offered("TEXT/PLAIN"));
        assert!(is_offered("UTF8_STRING"));
        assert!(!is_offered("text/html"));
    }

    #[test]
    fn uri_list_parsing() {
        let body = b"# comment\r\nfile:///home/u/a%20b.txt\r\nfile://localhost/tmp/x\nhttps://example.com/\nfile://otherhost/etc/passwd\n\nfile:///caf%C3%A9\n";
        let got = parse_uri_list(body);
        assert_eq!(
            got,
            vec![
                PathBuf::from("/home/u/a b.txt"),
                PathBuf::from("/tmp/x"),
                PathBuf::from("/café")
            ]
        );
    }

    #[test]
    fn bad_percent_escapes_pass_through() {
        assert_eq!(percent_decode(b"a%zzb%4"), b"a%zzb%4".to_vec());
        assert_eq!(percent_decode(b"%41"), b"A".to_vec());
    }
}
