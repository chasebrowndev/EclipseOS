// SPDX-License-Identifier: AGPL-3.0-only

//! OpenURI plumbing without `xdg-open`: URI validation, MIME lookup from
//! `globs2`, default-handler lookup from `mimeapps.list` / `mimeinfo.cache`,
//! and `Exec` field-code expansion. All pure; the daemon feeds in file text.

use std::collections::HashMap;

use crate::filechooser::glob_match;

/// `scheme` of an absolute URI, if well formed (`[A-Za-z][A-Za-z0-9+.-]*:`).
pub fn scheme_of(uri: &str) -> Option<&str> {
    let (s, rest) = uri.split_once(':')?;
    let mut b = s.bytes();
    let ok = b.next().is_some_and(|c| c.is_ascii_alphabetic())
        && b.all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-'));
    (ok && !rest.contains('\0') && uri.len() <= 8192).then_some(s)
}

pub fn scheme_mime(scheme: &str) -> String {
    format!("x-scheme-handler/{}", scheme.to_ascii_lowercase())
}

/// Desktop-file ids per MIME type, in priority order, plus removals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MimeApps {
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, Vec<String>>,
}

fn split_ids(v: &str) -> Vec<String> {
    v.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

impl MimeApps {
    /// Parse `mimeapps.list` (`[Default Applications]`, `[Added Associations]`,
    /// `[Removed Associations]`).
    pub fn parse(text: &str) -> Self {
        let mut m = MimeApps::default();
        let mut group = "";
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                group = line;
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let target = match group {
                "[Default Applications]" => &mut m.defaults,
                "[Added Associations]" => &mut m.added,
                "[Removed Associations]" => &mut m.removed,
                _ => continue,
            };
            target
                .entry(k.trim().to_owned())
                .or_default()
                .extend(split_ids(v));
        }
        m
    }

    /// Parse `mimeinfo.cache` (`[MIME Cache]`): treated as added associations.
    pub fn parse_cache(text: &str) -> Self {
        let mut m = MimeApps::default();
        let mut in_cache = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_cache = line == "[MIME Cache]";
            } else if in_cache {
                if let Some((k, v)) = line.split_once('=') {
                    m.added
                        .entry(k.trim().to_owned())
                        .or_default()
                        .extend(split_ids(v));
                }
            }
        }
        m
    }

    /// Fold in a lower-priority file: `self` keeps precedence.
    pub fn merge_lower(&mut self, lower: &MimeApps) {
        for (dst, src) in [
            (&mut self.defaults, &lower.defaults),
            (&mut self.added, &lower.added),
            (&mut self.removed, &lower.removed),
        ] {
            for (k, ids) in src {
                let e = dst.entry(k.clone()).or_default();
                for id in ids {
                    if !e.contains(id) {
                        e.push(id.clone());
                    }
                }
            }
        }
    }

    /// Candidate desktop ids for `mime`: defaults, then added associations,
    /// minus removed ones, without duplicates.
    pub fn candidates(&self, mime: &str) -> Vec<String> {
        let removed = self.removed.get(mime);
        let mut out: Vec<String> = Vec::new();
        for id in self
            .defaults
            .get(mime)
            .into_iter()
            .chain(self.added.get(mime))
            .flatten()
        {
            if removed.is_some_and(|r| r.contains(id)) || out.contains(id) {
                continue;
            }
            out.push(id.clone());
        }
        out
    }
}

/// MIME type of `filename` from shared-mime-info `globs2` text
/// (`weight:mime:glob[:flags]`). Highest weight wins; ties go to the longer
/// glob. Matching is case-insensitive unless the entry says `cs`.
pub fn mime_from_globs2(globs2: &str, filename: &str) -> Option<String> {
    let lower = filename.to_ascii_lowercase();
    let mut best: Option<(u32, usize, &str)> = None;
    for line in globs2.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut f = line.split(':');
        let (Some(w), Some(mime), Some(glob)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let Ok(w) = w.parse::<u32>() else { continue };
        let cs = f.next().is_some_and(|fl| fl.split(',').any(|x| x == "cs"));
        let hit = if cs {
            glob_match(glob.as_bytes(), filename.as_bytes())
        } else {
            glob_match(glob.to_ascii_lowercase().as_bytes(), lower.as_bytes())
        };
        if hit && best.is_none_or(|(bw, bl, _)| (w, glob.len()) > (bw, bl)) {
            best = Some((w, glob.len(), mime));
        }
    }
    best.map(|(_, _, m)| m.to_owned())
}

/// Split an `Exec` value into words (desktop-entry quoting rules).
fn exec_words(exec: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                have = true;
            }
            '\\' if quoted => {
                cur.push(chars.next()?);
                have = true;
            }
            c if c.is_whitespace() && !quoted => {
                if have {
                    words.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if quoted {
        return None;
    }
    if have {
        words.push(cur);
    }
    Some(words)
}

/// Expand an `Exec` line into an argv. `%u/%U` take `uri`, `%f/%F` take
/// `path`; other field codes vanish, `%%` is `%`. Returns `None` if the line
/// needs a path and there is none, or is empty/malformed. With no target field
/// code at all, the URI is appended (browsers and mail clients expect it).
pub fn expand_exec(exec: &str, uri: &str, path: Option<&str>) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut used = false;
    for w in exec_words(exec)? {
        if let Some(code) = w.strip_prefix('%').filter(|c| c.len() == 1) {
            match code {
                "u" | "U" => {
                    out.push(uri.to_owned());
                    used = true;
                }
                "f" | "F" => {
                    out.push(path?.to_owned());
                    used = true;
                }
                "%" => out.push("%".into()),
                _ => {}
            }
            continue;
        }
        // Codes embedded in a word (`--url=%u`).
        let mut s = String::new();
        let mut it = w.chars();
        while let Some(c) = it.next() {
            if c != '%' {
                s.push(c);
                continue;
            }
            match it.next() {
                Some('u') | Some('U') => {
                    s.push_str(uri);
                    used = true;
                }
                Some('f') | Some('F') => {
                    s.push_str(path?);
                    used = true;
                }
                Some('%') => s.push('%'),
                _ => {}
            }
        }
        out.push(s);
    }
    if out.is_empty() {
        return None;
    }
    if !used {
        out.push(match path {
            Some(p) => p.to_owned(),
            None => uri.to_owned(),
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes() {
        assert_eq!(scheme_of("https://x.org/a"), Some("https"));
        assert_eq!(scheme_of("mailto:a@b"), Some("mailto"));
        assert_eq!(scheme_of("-evil:x"), None);
        assert_eq!(scheme_of("noscheme"), None);
        assert_eq!(scheme_of("1http://x"), None);
        assert_eq!(scheme_mime("HTTPS"), "x-scheme-handler/https");
    }

    #[test]
    fn mimeapps_precedence() {
        let mut user = MimeApps::parse(
            "[Default Applications]\nx-scheme-handler/https=foot.desktop;\n[Removed Associations]\nx-scheme-handler/https=bad.desktop;\n",
        );
        let sys = MimeApps::parse_cache(
            "[MIME Cache]\nx-scheme-handler/https=bad.desktop;firefox.desktop;foot.desktop;\n",
        );
        user.merge_lower(&sys);
        assert_eq!(
            user.candidates("x-scheme-handler/https"),
            vec!["foot.desktop", "firefox.desktop"]
        );
        assert!(user.candidates("text/plain").is_empty());
    }

    #[test]
    fn globs() {
        let g = "# c\n50:image/png:*.png\n50:text/plain:*.txt\n10:application/x-x:*.tar.gz\n50:application/gzip:*.gz\n50:text/x-c:*.C:cs\n";
        assert_eq!(mime_from_globs2(g, "A.PNG").as_deref(), Some("image/png"));
        assert_eq!(
            mime_from_globs2(g, "x.tar.gz").as_deref(),
            Some("application/gzip")
        );
        assert_eq!(mime_from_globs2(g, "a.c"), None);
        assert_eq!(mime_from_globs2(g, "a.C").as_deref(), Some("text/x-c"));
        assert_eq!(mime_from_globs2(g, "none"), None);
    }

    #[test]
    fn exec_expansion() {
        assert_eq!(
            expand_exec("/usr/bin/firefox %u", "https://x", None),
            Some(vec!["/usr/bin/firefox".into(), "https://x".into()])
        );
        assert_eq!(
            expand_exec("viewer --class \"My App\" %f %i", "file:///a", Some("/a")),
            Some(vec![
                "viewer".into(),
                "--class".into(),
                "My App".into(),
                "/a".into()
            ])
        );
        assert_eq!(
            expand_exec("app --url=%u 100%%", "u:1", None),
            Some(vec!["app".into(), "--url=u:1".into(), "100%".into()])
        );
        assert_eq!(expand_exec("viewer %f", "https://x", None), None);
        assert_eq!(
            expand_exec("app", "https://x", None),
            Some(vec!["app".into(), "https://x".into()])
        );
        assert_eq!(expand_exec("", "u:1", None), None);
        assert_eq!(expand_exec("app \"unterminated", "u:1", None), None);
    }

    #[test]
    fn exec_has_no_shell_semantics() {
        let v = expand_exec("app %u", "https://x/;rm -rf ~", None).unwrap();
        assert_eq!(v, vec!["app", "https://x/;rm -rf ~"]);
    }
}
