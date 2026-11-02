// SPDX-License-Identifier: AGPL-3.0-only

//! `ComposeEmail` options to a `mailto:` URI (RFC 6068).

use crate::options::Options;

fn pct(s: &str, out: &mut String) {
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@' | b'+') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

fn addr_list(addrs: &[String]) -> String {
    let mut out = String::new();
    for (i, a) in addrs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        pct(a, &mut out);
    }
    out
}

/// Recipients named in the options (`addresses`, `cc`, `bcc`).
pub fn addresses(o: &Options) -> Vec<String> {
    o.strs("addresses").unwrap_or_default()
}

/// `mailto:to?subject=..&body=..&cc=..&bcc=..`. Header values are
/// percent-encoded, so CR/LF cannot inject extra headers. Attachments are not
/// representable in `mailto:` and are not forwarded.
pub fn mailto(o: &Options) -> String {
    let mut uri = format!("mailto:{}", addr_list(&addresses(o)));
    let mut sep = '?';
    let mut add = |k: &str, v: String| {
        uri.push(sep);
        sep = '&';
        uri.push_str(k);
        uri.push('=');
        uri.push_str(&v);
    };
    if let Some(s) = o.str("subject") {
        let mut e = String::new();
        pct(s, &mut e);
        add("subject", e);
    }
    if let Some(b) = o.str("body") {
        let mut e = String::new();
        pct(b, &mut e);
        add("body", e);
    }
    for k in ["cc", "bcc"] {
        if let Some(l) = o.strs(k).filter(|l| !l.is_empty()) {
            add(k, addr_list(&l));
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::val::Val;

    #[test]
    fn builds_mailto() {
        let o = Options::from_pairs([
            (
                "addresses".into(),
                Val::Strs(vec!["a@x.org".into(), "b@x.org".into()]),
            ),
            ("subject".into(), Val::Str("Hi there\r\nBcc: evil@x".into())),
            ("body".into(), Val::Str("a&b".into())),
            ("cc".into(), Val::Strs(vec!["c@x.org".into()])),
        ]);
        assert_eq!(
            mailto(&o),
            "mailto:a@x.org,b@x.org?subject=Hi%20there%0D%0ABcc%3A%20evil@x&body=a%26b&cc=c@x.org"
        );
    }

    #[test]
    fn bare() {
        assert_eq!(mailto(&Options::new()), "mailto:");
    }
}
