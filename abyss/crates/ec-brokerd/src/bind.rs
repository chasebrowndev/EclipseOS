// SPDX-License-Identifier: AGPL-3.0-only
//! `bound_to` matching (S-08 §2.1): the broker, not the caller, decides
//! whether a secret may go to a destination.
//!
//! Three binding forms:
//!
//! * `host:<name>[:<port>]` — a DNS name, optionally `*.`-prefixed
//!   (subdomains only, never the apex), and a port. **No port means 443**,
//!   not "any port": a binding that quietly covers every service on a host is
//!   wider than the owner wrote.
//! * `url:<scheme>://<host>[:<port>]/<path-glob>` — same host rule; the port
//!   defaults to the scheme's; `*` in the path matches anything.
//! * `app:<app_id>` — a toplevel `app_id`, for `field_fill` into a native app.
//!
//! Every comparison fails closed: a target host with a trailing dot, uppercase
//! that does not fold, userinfo (`https://good.com@evil.com/`), a backslash,
//! an IP literal or any character outside `[a-z0-9.-]` matches nothing.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    Host {
        host: String,
        port: u16,
    },
    Url {
        scheme: String,
        host: String,
        port: u16,
        path: String,
    },
    App(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadBinding;

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "https" => Some(443),
        "http" => Some(80),
        _ => None,
    }
}

/// A host pattern or a target host: lowercase LDH labels, `*.` allowed only
/// in a pattern. Returns it folded, or `None`.
fn host_ok(h: &str, pattern: bool) -> Option<String> {
    let h = h.to_ascii_lowercase();
    let body = if pattern {
        h.strip_prefix("*.").unwrap_or(&h)
    } else {
        &h
    };
    if body.is_empty() || body.len() > 253 || body.starts_with('.') || body.ends_with('.') {
        return None;
    }
    let labels_ok = body.split('.').all(|l| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    });
    // An all-numeric final label is an IP literal or a number the resolver
    // might read as one. Not a name a binding can name.
    let last = body.rsplit('.').next().unwrap_or("");
    if !labels_ok || last.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(h)
}

fn host_match(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(apex) => {
            host.len() > apex.len() + 1
                && host.ends_with(apex)
                && host[..host.len() - apex.len()].ends_with('.')
        }
        None => pattern == host,
    }
}

/// Splits `host[:port]`. `None` on anything malformed, including `[`.
fn split_authority(a: &str, scheme_default: u16) -> Option<(&str, u16)> {
    if a.contains(['@', '\\', '[', ']']) {
        return None;
    }
    match a.split_once(':') {
        None => Some((a, scheme_default)),
        Some((h, p)) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let port: u16 = p.parse().ok()?;
            (port != 0).then_some((h, port))
        }
    }
}

/// `*` matches any run of bytes, everything else matches itself.
fn glob(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut i, mut star, mut mark) = (0, 0, None, 0);
    while i < s.len() {
        if p < pat.len() && pat[p] == b'*' {
            star = Some(p);
            mark = i;
            p += 1;
        } else if p < pat.len() && pat[p] == s[i] {
            p += 1;
            i += 1;
        } else if let Some(sp) = star {
            p = sp + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == b'*')
}

/// `scheme`, `authority`, `path` of an absolute URL, query and fragment
/// stripped from the path.
fn split_url(u: &str) -> Option<(String, &str, &str)> {
    let (scheme, rest) = u.split_once("://")?;
    if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (auth, tail) = rest.split_at(end);
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = &tail[..path_end];
    Some((
        scheme.to_ascii_lowercase(),
        auth,
        if path.is_empty() { "/" } else { path },
    ))
}

impl Binding {
    pub fn parse(s: &str) -> Result<Binding, BadBinding> {
        if let Some(h) = s.strip_prefix("host:") {
            let (host, port) = split_authority(h, 443).ok_or(BadBinding)?;
            return Ok(Binding::Host {
                host: host_ok(host, true).ok_or(BadBinding)?,
                port,
            });
        }
        if let Some(u) = s.strip_prefix("url:") {
            let (scheme, auth, path) = split_url(u).ok_or(BadBinding)?;
            let dp = default_port(&scheme).ok_or(BadBinding)?;
            let (host, port) = split_authority(auth, dp).ok_or(BadBinding)?;
            return Ok(Binding::Url {
                scheme,
                host: host_ok(host, true).ok_or(BadBinding)?,
                port,
                path: path.to_owned(),
            });
        }
        if let Some(a) = s.strip_prefix("app:") {
            if !a.is_empty() && a.len() <= 255 && a.bytes().all(|b| b.is_ascii_graphic()) {
                return Ok(Binding::App(a.to_owned()));
            }
        }
        Err(BadBinding)
    }

    pub fn to_text(&self) -> String {
        match self {
            Binding::Host { host, port } => format!("host:{host}:{port}"),
            Binding::Url {
                scheme,
                host,
                port,
                path,
            } => format!("url:{scheme}://{host}:{port}{path}"),
            Binding::App(a) => format!("app:{a}"),
        }
    }

    /// Whether a connection to `host:port` may carry the secret. A `Url`
    /// binding is not consulted: a connection has no path.
    pub fn matches_endpoint(&self, host: &str, port: u16) -> bool {
        match (self, host_ok(host, false)) {
            (Binding::Host { host: p, port: pp }, Some(h)) => *pp == port && host_match(p, &h),
            _ => false,
        }
    }

    /// Whether a page or request at `url` may be filled or substituted into.
    /// A `Host` binding covers every path on that host and port.
    pub fn matches_url(&self, url: &str) -> bool {
        let Some((scheme, auth, path)) = split_url(url) else {
            return false;
        };
        let Some(dp) = default_port(&scheme) else {
            return false;
        };
        let Some((h, port)) = split_authority(auth, dp) else {
            return false;
        };
        let Some(h) = host_ok(h, false) else {
            return false;
        };
        match self {
            Binding::Host { host, port: pp } => *pp == port && scheme == "https" && host_match(host, &h),
            Binding::Url {
                scheme: ps,
                host,
                port: pp,
                path: pat,
            } => {
                *ps == scheme && *pp == port && host_match(host, &h) && glob(pat.as_bytes(), path.as_bytes())
            }
            Binding::App(_) => false,
        }
    }

    pub fn matches_app(&self, app_id: &str) -> bool {
        matches!(self, Binding::App(a) if a == app_id)
    }

    pub fn is_web(&self) -> bool {
        !matches!(self, Binding::App(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(s: &str) -> Binding {
        Binding::parse(s).unwrap()
    }

    #[test]
    fn host_binding_matches_name_and_port_only() {
        let x = b("host:api.acme.com");
        assert!(x.matches_endpoint("api.acme.com", 443));
        assert!(x.matches_endpoint("API.ACME.COM", 443));
        assert!(!x.matches_endpoint("api.acme.com", 8443), "wrong port");
        assert!(!x.matches_endpoint("evil.com", 443));
        assert!(!x.matches_endpoint("api.acme.com.evil.com", 443));
        assert!(!x.matches_endpoint("xapi.acme.com", 443));
        assert!(!x.matches_endpoint("api.acme.com.", 443), "trailing dot");
        assert!(b("host:api.acme.com:8443").matches_endpoint("api.acme.com", 8443));
    }

    #[test]
    fn wildcard_covers_subdomains_not_the_apex() {
        let x = b("host:*.acme.com");
        assert!(x.matches_endpoint("a.acme.com", 443));
        assert!(x.matches_endpoint("a.b.acme.com", 443));
        assert!(!x.matches_endpoint("acme.com", 443));
        assert!(!x.matches_endpoint("notacme.com", 443));
        assert!(!x.matches_endpoint("evilacme.com", 443));
    }

    #[test]
    fn url_binding_checks_scheme_host_port_and_path() {
        let x = b("url:https://*.acme.com/*");
        assert!(x.matches_url("https://login.acme.com/signin?next=/"));
        assert!(!x.matches_url("http://login.acme.com/signin"), "scheme");
        assert!(!x.matches_url("https://login.acme.com:8443/"), "port");
        assert!(!x.matches_url("https://acme.com/"), "apex");
        assert!(!x.matches_url("https://good.acme.com@evil.com/"), "userinfo");
        assert!(!x.matches_url("https://evil.com/?h=login.acme.com"));
        assert!(!x.matches_url("https://evil.com\\@login.acme.com/"));
        let p = b("url:https://acme.com/login");
        assert!(p.matches_url("https://acme.com/login"));
        assert!(p.matches_url("https://acme.com/login?x=1#f"));
        assert!(!p.matches_url("https://acme.com/login/extra"));
        assert!(!p.matches_url("https://acme.com/admin"));
    }

    #[test]
    fn app_bindings_match_only_apps() {
        let a = b("app:org.mozilla.firefox");
        assert!(a.matches_app("org.mozilla.firefox"));
        assert!(!a.matches_app("org.mozilla.firefox2"));
        assert!(!a.matches_endpoint("org.mozilla.firefox", 443));
        assert!(!a.matches_url("https://org.mozilla.firefox/"));
    }

    #[test]
    fn malformed_bindings_and_targets_are_refused() {
        for s in [
            "",
            "host:",
            "host:*",
            "host:1.2.3.4",
            "host:a b.com",
            "host:a.com:0",
            "host:[::1]",
            "url:ftp://a.com/",
            "url:https://a@b.com/",
            "ssh:a",
            "app:",
        ] {
            assert!(Binding::parse(s).is_err(), "{s:?} should be refused");
        }
        assert!(!b("host:a.com").matches_endpoint("1.2.3.4", 443));
        assert!(!b("host:a.com").matches_endpoint("", 443));
    }

    #[test]
    fn text_round_trips() {
        for s in ["host:a.com", "url:https://*.a.com/x*", "app:foo"] {
            let one = b(s);
            assert_eq!(b(&one.to_text()), one);
        }
    }
}
