// SPDX-License-Identifier: AGPL-3.0-only
//! The working directory a program reports with OSC 7 (`file://host/path`, T-13 §6). Only a
//! local host (empty, `localhost` or ours) yields a path; it is percent-decoded and must be
//! absolute.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

/// The local path of an OSC 7 URI, when it names this host.
#[must_use]
pub(crate) fn local_path(uri: &str, hostname: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    if !(host.is_empty() || host == "localhost" || host.eq_ignore_ascii_case(hostname)) {
        return None;
    }
    let bytes = decode(path)?;
    if bytes.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

fn decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// This machine's host name (read once at startup).
#[must_use]
pub(crate) fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_uris_only() {
        assert_eq!(
            local_path("file:///home/u/my%20dir", "box"),
            Some(PathBuf::from("/home/u/my dir"))
        );
        assert_eq!(
            local_path("file://box/tmp", "box"),
            Some(PathBuf::from("/tmp"))
        );
        assert_eq!(
            local_path("file://localhost/tmp", "box"),
            Some(PathBuf::from("/tmp"))
        );
        assert_eq!(local_path("file://other/tmp", "box"), None);
        assert_eq!(local_path("file://box", "box"), None);
        assert_eq!(local_path("file:///a%00b", "box"), None);
        assert_eq!(local_path("file:///a%zz", "box"), None);
        assert_eq!(local_path("http://box/tmp", "box"), None);
    }
}
