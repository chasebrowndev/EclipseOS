// SPDX-License-Identifier: AGPL-3.0-only
//! The sequence policy table and consent (T-18 §3, §10; SECURITY-03..09, -31), and the
//! hyperlink open rules (T-14 §5). Pure: main asks, the term enforces the deny word.
//!
//! * [`Policy`] holds the T-18 §3 defaults with the `security.sequence` overrides. The
//!   local column takes an override as given; the remote column only ever tightens (a pane
//!   is remote here by process detection, T-18 §10 test 2, so no override loosens it).
//! * [`Policy::deny_word`] is the `apoth_term::deny` bit set the io thread installs before
//!   each feed: a denied class has no effect and no reply.
//! * [`Consent`] is the consent bar's rate limit: one pending question per class per pane,
//!   repeated asks within 10 s of a deny are auto-denied, and "always for this pane" sticks
//!   for the pane's life.
//!
//! Nothing here sees content; diagnostics name classes only.

use std::time::{Duration, Instant};

use apoth_term::deny;

/// `allow | ask | deny`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Decision {
    Allow,
    Ask,
    Deny,
}

impl Decision {
    fn parse(s: &str) -> Option<Decision> {
        Some(match s {
            "allow" => Decision::Allow,
            "ask" => Decision::Ask,
            "deny" => Decision::Deny,
            _ => return None,
        })
    }
}

/// The T-18 §3 classes, in table order. Ids are `apoth_config::lang::SEQUENCE_CLASSES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Class {
    ClipboardWrite,
    ClipboardRead,
    PrimarySelection,
    TitleSet,
    TitleReport,
    WindowOpsReport,
    WindowOpsControl,
    Color,
    Font,
    Notifications,
    NotificationActivation,
    PointerShape,
    GraphicsDirect,
    GraphicsFile,
    MediaPath,
    UserVars,
    Hyperlinks,
    InputModes,
}

/// Class, id, local default, remote default.
const TABLE: &[(Class, &str, Decision, Decision)] = {
    use Decision::{Allow, Ask, Deny};
    &[
        (Class::ClipboardWrite, "clipboard-write", Allow, Allow),
        (Class::ClipboardRead, "clipboard-read", Ask, Deny),
        // "As clipboard": the stricter of the two clipboard rows applies on top (see `decide`).
        (Class::PrimarySelection, "primary-selection", Allow, Allow),
        (Class::TitleSet, "title-set", Allow, Allow),
        (Class::TitleReport, "title-report", Allow, Allow),
        (Class::WindowOpsReport, "window-ops-report", Allow, Allow),
        (Class::WindowOpsControl, "window-ops-control", Deny, Deny),
        (Class::Color, "color", Allow, Allow),
        (Class::Font, "font", Deny, Deny),
        (Class::Notifications, "notifications", Allow, Allow),
        (
            Class::NotificationActivation,
            "notification-activation",
            Ask,
            Deny,
        ),
        (Class::PointerShape, "pointer-shape", Allow, Allow),
        (Class::GraphicsDirect, "graphics-direct", Allow, Allow),
        (Class::GraphicsFile, "graphics-file", Ask, Deny),
        (Class::MediaPath, "media-path", Ask, Deny),
        (Class::UserVars, "user-vars", Allow, Allow),
        (Class::Hyperlinks, "hyperlinks", Allow, Allow),
        (Class::InputModes, "input-modes", Allow, Allow),
    ]
};

impl Class {
    pub(crate) fn id(self) -> &'static str {
        TABLE
            .iter()
            .find(|r| r.0 == self)
            .map_or("unknown", |r| r.1)
    }

    /// The consent bar's words for the class ("Allow {label}?").
    pub(crate) fn label(self) -> &'static str {
        match self {
            Class::ClipboardRead => "this program to read the clipboard",
            Class::ClipboardWrite => "this program to set the clipboard",
            Class::PrimarySelection => "this program to use the primary selection",
            Class::NotificationActivation => {
                "this program to learn when you click its notifications"
            }
            Class::GraphicsFile => "this program to show images from files",
            Class::MediaPath => "this program to play media from a path",
            _ => "this request",
        }
    }

    fn parse(s: &str) -> Option<Class> {
        TABLE.iter().find(|r| r.1 == s).map(|r| r.0)
    }
}

/// The table with overrides applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Policy {
    /// `(local, remote)` per class, table order.
    rows: Vec<(Decision, Decision)>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            rows: TABLE.iter().map(|r| (r.2, r.3)).collect(),
        }
    }
}

impl Policy {
    /// From `security.sequence` entries `class → [local, remote?]` (validated by the
    /// loader; anything unknown here is ignored). The local value replaces the default;
    /// the remote value (or the local one when only one is given) may only tighten.
    pub(crate) fn from_entries<'a>(
        entries: impl IntoIterator<Item = (&'a str, &'a [String])>,
    ) -> Policy {
        let mut p = Policy::default();
        for (k, v) in entries {
            let Some(c) = Class::parse(k) else { continue };
            let Some(local) = v.first().and_then(|s| Decision::parse(s)) else {
                continue;
            };
            let remote = v.get(1).and_then(|s| Decision::parse(s)).unwrap_or(local);
            let i = p.idx(c);
            p.rows[i].0 = local;
            p.rows[i].1 = p.rows[i].1.max(remote);
        }
        p
    }

    fn idx(&self, c: Class) -> usize {
        TABLE.iter().position(|r| r.0 == c).unwrap_or(0)
    }

    /// The decision for `c` in a local or remote pane.
    pub(crate) fn decide(&self, c: Class, remote: bool) -> Decision {
        let r = self.rows[self.idx(c)];
        if remote { r.1 } else { r.0 }
    }

    /// Primary access in a direction ("primary-selection access: as clipboard"): the stricter of the primary row and the clipboard
    /// row for that direction.
    pub(crate) fn decide_primary(&self, read: bool, remote: bool) -> Decision {
        let dir = if read {
            Class::ClipboardRead
        } else {
            Class::ClipboardWrite
        };
        self.decide(dir, remote)
            .max(self.decide(Class::PrimarySelection, remote))
    }

    /// The `apoth_term::deny` bits for a pane: every class the term enforces whose
    /// decision is `deny`. `ask` classes are raised to main (OSC 52 read) or, for classes
    /// the term cannot ask about, treated as deny.
    pub(crate) fn deny_word(&self, remote: bool) -> u32 {
        let d = |c: Class| self.decide(c, remote) == Decision::Deny;
        let ask_or_deny = |c: Class| self.decide(c, remote) != Decision::Allow;
        let mut w = 0;
        if ask_or_deny(Class::TitleSet) {
            w |= deny::TITLE_SET;
        }
        if ask_or_deny(Class::TitleReport) {
            w |= deny::TITLE_REPORT;
        }
        if ask_or_deny(Class::WindowOpsReport) {
            w |= deny::WINDOW_REPORT;
        }
        if ask_or_deny(Class::Color) {
            w |= deny::COLOUR;
        }
        if ask_or_deny(Class::Notifications) {
            w |= deny::NOTIFICATION;
        }
        if ask_or_deny(Class::Hyperlinks) {
            w |= deny::HYPERLINK;
        }
        if ask_or_deny(Class::UserVars) {
            w |= deny::USER_VAR;
        }
        // Clipboard writes are not asked (a write is not a question the user can judge per
        // sequence); `ask` therefore means deny for the term.
        if ask_or_deny(Class::ClipboardWrite) {
            w |= deny::CLIPBOARD_WRITE;
        }
        if d(Class::ClipboardRead) {
            w |= deny::CLIPBOARD_READ;
        }
        if self.decide_primary(true, remote) == Decision::Deny
            || self.decide_primary(false, remote) != Decision::Allow
        {
            w |= deny::PRIMARY;
        }
        // Graphics: one sequence carries the image, so `ask` cannot be answered per
        // sequence and means deny; file media is asked per transmission (T-15 §5).
        if ask_or_deny(Class::GraphicsDirect) {
            w |= deny::GRAPHICS_DIRECT;
        }
        match self.decide(Class::GraphicsFile, remote) {
            Decision::Deny => w |= deny::GRAPHICS_FILE,
            Decision::Ask => w |= deny::GRAPHICS_FILE_ASK,
            Decision::Allow => {}
        }
        w
    }
}

/// What to do with a question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ask {
    /// "Always for this pane" was answered before: allow.
    Granted,
    /// Asked within 10 s of a deny: deny without asking.
    AutoDeny,
    /// A question for this class and pane is already pending: drop this one.
    Busy,
    /// Show the consent bar.
    Show,
}

/// A consent answer (seat input on the bar only; never an Action, T-18 §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Once,
    Always,
    Deny,
}

/// Auto-deny window after a deny.
pub(crate) const DENY_COOLDOWN: Duration = Duration::from_secs(10);

/// The consent rate limit (T-18 §3).
#[derive(Debug, Default)]
pub(crate) struct Consent {
    pending: Vec<(u64, Class)>,
    denied: Vec<(u64, Class, Instant)>,
    always: Vec<(u64, Class)>,
}

impl Consent {
    pub(crate) fn ask(&mut self, pane: u64, c: Class, now: Instant) -> Ask {
        if self.always.contains(&(pane, c)) {
            return Ask::Granted;
        }
        self.denied
            .retain(|d| now.saturating_duration_since(d.2) < DENY_COOLDOWN);
        if self.denied.iter().any(|d| d.0 == pane && d.1 == c) {
            return Ask::AutoDeny;
        }
        if self.pending.contains(&(pane, c)) {
            return Ask::Busy;
        }
        self.pending.push((pane, c));
        Ask::Show
    }

    /// Record the answer to a pending question.
    pub(crate) fn answer(&mut self, pane: u64, c: Class, a: Answer, now: Instant) {
        self.pending.retain(|p| *p != (pane, c));
        match a {
            Answer::Once => {}
            Answer::Always => {
                if !self.always.contains(&(pane, c)) {
                    self.always.push((pane, c));
                }
            }
            Answer::Deny => self.denied.push((pane, c, now)),
        }
    }

    /// Withdraw a pending question that was never shown (another bar held the slot).
    pub(crate) fn cancel(&mut self, pane: u64, c: Class) {
        self.pending.retain(|p| *p != (pane, c));
    }

    /// The pane closed: forget it.
    pub(crate) fn forget(&mut self, pane: u64) {
        self.pending.retain(|p| p.0 != pane);
        self.denied.retain(|d| d.0 != pane);
        self.always.retain(|a| a.0 != pane);
    }
}

/// Why a link is not opened (no URI in the message: it is program content).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkRefused {
    Scheme,
    RemoteFile,
    Malformed,
}

impl LinkRefused {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            LinkRefused::Scheme => "scheme not allowed",
            LinkRefused::RemoteFile => "file link names another host",
            LinkRefused::Malformed => "malformed link",
        }
    }
}

/// T-14 §5: the scheme is on `links.schemes`; `file://` names no host, `localhost` or this
/// host; no controls or spaces.
pub(crate) fn check_link(uri: &str, schemes: &[String], hostname: &str) -> Result<(), LinkRefused> {
    if uri.is_empty()
        || uri.len() > apoth_grid::MAX_URI
        || uri.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(LinkRefused::Malformed);
    }
    let Some(colon) = uri.find(':') else {
        return Err(LinkRefused::Malformed);
    };
    let scheme = &uri[..colon];
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return Err(LinkRefused::Malformed);
    }
    if !schemes.iter().any(|s| s.eq_ignore_ascii_case(scheme)) {
        return Err(LinkRefused::Scheme);
    }
    if scheme.eq_ignore_ascii_case("file") {
        let rest = &uri[colon + 1..];
        if let Some(after) = rest.strip_prefix("//") {
            let host = after.split('/').next().unwrap_or("");
            if !(host.is_empty()
                || host.eq_ignore_ascii_case("localhost")
                || host.eq_ignore_ascii_case(hostname))
            {
                return Err(LinkRefused::RemoteFile);
            }
        }
    }
    Ok(())
}

/// The full URI is shown before opening when it differs from the visible text (T-14 §5).
pub(crate) fn needs_reveal(uri: &str, visible: &str) -> bool {
    uri != visible.trim()
}

/// `links.open-command` with `{url}` substituted (a template with no `{url}` gets the URI
/// appended as its own argument). Each argument stays one argv element: no shell.
pub(crate) fn open_argv(template: &[String], uri: &str) -> Option<Vec<String>> {
    if template.is_empty() || template[0].is_empty() {
        return None;
    }
    let mut seen = false;
    let mut v: Vec<String> = template
        .iter()
        .map(|a| {
            if a.contains("{url}") {
                seen = true;
                a.replace("{url}", uri)
            } else {
                a.clone()
            }
        })
        .collect();
    if !seen {
        v.push(uri.to_string());
    }
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_ids_match_the_config_vocabulary() {
        for (_, id, _, _) in TABLE {
            assert!(
                apoth_config::lang::SEQUENCE_CLASSES.contains(id),
                "{id} is not a config sequence class"
            );
        }
        assert_eq!(TABLE.len(), apoth_config::lang::SEQUENCE_CLASSES.len());
    }

    #[test]
    fn defaults_follow_t18_section_3() {
        let p = Policy::default();
        assert_eq!(p.decide(Class::ClipboardWrite, false), Decision::Allow);
        assert_eq!(p.decide(Class::ClipboardWrite, true), Decision::Allow);
        assert_eq!(p.decide(Class::ClipboardRead, false), Decision::Ask);
        assert_eq!(p.decide(Class::ClipboardRead, true), Decision::Deny);
        assert_eq!(p.decide(Class::WindowOpsControl, false), Decision::Deny);
        assert_eq!(p.decide(Class::Font, true), Decision::Deny);
        assert_eq!(
            p.decide(Class::NotificationActivation, true),
            Decision::Deny
        );
        assert_eq!(
            p.deny_word(false),
            deny::GRAPHICS_FILE_ASK,
            "nothing the term enforces is denied locally; image files are asked"
        );
        assert_eq!(
            p.deny_word(true),
            deny::CLIPBOARD_READ | deny::PRIMARY | deny::GRAPHICS_FILE
        );
    }

    #[test]
    fn overrides_set_local_and_only_tighten_remote() {
        let allow = vec!["allow".to_string(), "allow".to_string()];
        let deny_local = vec!["deny".to_string()];
        let p = Policy::from_entries([
            ("clipboard-read", allow.as_slice()),
            ("title-set", deny_local.as_slice()),
            ("bogus", allow.as_slice()),
        ]);
        assert_eq!(p.decide(Class::ClipboardRead, false), Decision::Allow);
        assert_eq!(
            p.decide(Class::ClipboardRead, true),
            Decision::Deny,
            "an override never loosens a detected remote pane"
        );
        assert_eq!(p.decide(Class::TitleSet, false), Decision::Deny);
        assert_eq!(p.decide(Class::TitleSet, true), Decision::Deny);
        assert_ne!(p.deny_word(false) & deny::TITLE_SET, 0);
    }

    #[test]
    fn primary_is_never_looser_than_the_clipboard() {
        let p = Policy::default();
        assert_eq!(p.decide_primary(true, false), Decision::Ask);
        assert_eq!(p.decide_primary(false, false), Decision::Allow);
        let deny = vec!["deny".to_string()];
        let p = Policy::from_entries([("clipboard-write", deny.as_slice())]);
        assert_eq!(p.decide_primary(false, false), Decision::Deny);
        assert_ne!(p.deny_word(false) & deny::PRIMARY, 0);
        assert_ne!(p.deny_word(false) & deny::CLIPBOARD_WRITE, 0);
    }

    #[test]
    fn consent_rate_limit() {
        let mut c = Consent::default();
        let t0 = Instant::now();
        assert_eq!(c.ask(1, Class::ClipboardRead, t0), Ask::Show);
        assert_eq!(
            c.ask(1, Class::ClipboardRead, t0),
            Ask::Busy,
            "one pending per class"
        );
        assert_eq!(c.ask(2, Class::ClipboardRead, t0), Ask::Show, "per pane");
        c.answer(1, Class::ClipboardRead, Answer::Deny, t0);
        assert_eq!(
            c.ask(1, Class::ClipboardRead, t0 + Duration::from_secs(9)),
            Ask::AutoDeny
        );
        assert_eq!(
            c.ask(1, Class::ClipboardRead, t0 + Duration::from_secs(11)),
            Ask::Show
        );
        c.answer(2, Class::ClipboardRead, Answer::Always, t0);
        assert_eq!(c.ask(2, Class::ClipboardRead, t0), Ask::Granted);
        c.forget(2);
        assert_eq!(c.ask(2, Class::ClipboardRead, t0), Ask::Show);
    }

    #[test]
    fn link_rules() {
        let s: Vec<String> = ["http", "https", "mailto", "ftp", "file"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(check_link("https://example.com/x", &s, "box"), Ok(()));
        assert_eq!(check_link("HTTPS://example.com", &s, "box"), Ok(()));
        assert_eq!(
            check_link("javascript:alert(1)", &s, "box"),
            Err(LinkRefused::Scheme)
        );
        assert_eq!(
            check_link("ssh://host", &s, "box"),
            Err(LinkRefused::Scheme)
        );
        assert_eq!(check_link("file:///etc/hosts", &s, "box"), Ok(()));
        assert_eq!(check_link("file://localhost/etc", &s, "box"), Ok(()));
        assert_eq!(check_link("file://box/etc", &s, "box"), Ok(()));
        assert_eq!(
            check_link("file://evil.example/share", &s, "box"),
            Err(LinkRefused::RemoteFile)
        );
        assert_eq!(
            check_link("https://a b", &s, "box"),
            Err(LinkRefused::Malformed)
        );
        assert_eq!(
            check_link("https://a\x1b", &s, "box"),
            Err(LinkRefused::Malformed)
        );
        assert_eq!(
            check_link("noscheme", &s, "box"),
            Err(LinkRefused::Malformed)
        );
        assert!(needs_reveal("https://evil.example", "https://bank.example"));
        assert!(!needs_reveal("https://a.example", " https://a.example "));
    }

    #[test]
    fn open_argv_keeps_one_argument() {
        let t = vec!["xdg-open".to_string(), "{url}".to_string()];
        assert_eq!(
            open_argv(&t, "https://x/$(rm -rf ~)"),
            Some(vec![
                "xdg-open".to_string(),
                "https://x/$(rm -rf ~)".to_string()
            ])
        );
        let t = vec!["firefox".to_string()];
        assert_eq!(
            open_argv(&t, "https://x"),
            Some(vec!["firefox".to_string(), "https://x".to_string()])
        );
        assert_eq!(open_argv(&[], "https://x"), None);
    }
}
