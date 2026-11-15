// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-secret-prompt` — one password field, then exit (ADR 0053).
//!
//! The taskbar starts this process with the network or device to prompt for,
//! and Settings starts it with the account an API key is for; neither sees
//! what is typed. The value goes from the field straight to the status
//! service's action, or to `ec-secret`'s stdin, and is overwritten. Its whole window is
//! classified `secret` by the owner windowrule on its exact app-id, so the
//! compositor redacts it from capture.

pub mod app;
pub mod service;
pub mod terminal;

use std::fmt;

/// The exact app-id `/etc/eclipse/policy.kdl` matches to classify this window
/// `secret`. Changing it silently drops the classification.
pub const APP_ID: &str = "ec-secret-prompt";

/// What BlueZ asked for. A PIN is legacy pairing's free text; a passkey is
/// the six-digit number SSP shows on the other device. The last three carry
/// no typed secret at all: `Confirm` is SSP numeric comparison (the human says
/// whether both screens show the same number), `Authorize` is Just Works (the
/// human says yes or no), and `Show` is the other way round from a passkey —
/// the device has a keyboard and the human types *our* code into it, so the
/// window only displays it and answers nothing.
#[derive(Clone, PartialEq, Eq)]
pub enum Code {
    Pin,
    Passkey,
    /// The passkey both sides are showing, 0–999999.
    Confirm(u32),
    Authorize,
    /// The PIN or zero-padded passkey to type on the device.
    Show(String),
}

/// Numbers are not secrets, but a pairing code is still nobody's business in
/// a log: `Debug` names the kind and never the code.
impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Code::Pin => "Pin",
            Code::Passkey => "Passkey",
            Code::Confirm(_) => "Confirm(<redacted>)",
            Code::Authorize => "Authorize",
            Code::Show(_) => "Show(<redacted>)",
        })
    }
}

/// What the secret is for. Names and addresses are not secret and may be
/// shown and logged; the value typed for them is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Wifi {
        ssid: String,
    },
    Bluetooth {
        addr: String,
        code: Code,
    },
    /// An API key for a Claude account, stored as `anthropic-api-key[.account]`
    /// by `ec-secret account add-key` (ADR 0077). The account name is not
    /// secret; the key is.
    ApiKey {
        account: String,
    },
    /// A password a program in an Apotheosis pane asks for (ADR 0053 amendment of
    /// 2026-11-15). The request comes on stdin, never argv; the answer goes back on
    /// stdout. `Debug` of the model prints counts only.
    Terminal(Box<terminal::TerminalAsk>),
}

/// Program text made safe to draw: terminal escape sequences, control
/// characters and bidi overrides go (a reversed run could make `rm` read as
/// something else), whitespace runs fold to one space, and past `max`
/// characters it ends in an ellipsis.
pub fn clean(s: &str, max: usize) -> String {
    let bidi =
        |c: char| matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    let mut out: Vec<char> = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                // CSI: parameters up to a final byte in @..~.
                Some('[') => while chars.next().is_some_and(|b| !('@'..='~').contains(&b)) {},
                // OSC, DCS and friends: up to BEL or ST.
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    while let Some(b) = chars.next() {
                        if b == '\u{7}' || (b == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let c = if c.is_whitespace() { ' ' } else { c };
        if (c.is_control() || bidi(c)) || (c == ' ' && out.last().is_none_or(|l| *l == ' ')) {
            continue;
        }
        out.push(c);
    }
    while out.last() == Some(&' ') {
        out.pop();
    }
    if out.len() > max {
        out.truncate(max.saturating_sub(1));
        out.push('…');
    }
    out.into_iter().collect()
}

/// [`clean`], but past `max` it keeps the end and starts with an ellipsis: a
/// path's tail names the thing.
pub fn clean_tail(s: &str, max: usize) -> String {
    let full = clean(s, usize::MAX);
    let n = full.chars().count();
    if n <= max {
        return full;
    }
    let keep = max.saturating_sub(1);
    std::iter::once('…').chain(full.chars().skip(n - keep)).collect()
}

/// Longest network, device or account name under the heading.
const SUBJECT_MAX: usize = 36;

/// Longest program name in a heading: it has to stay one line.
const PROGRAM_MAX: usize = 32;

/// The program's name as the heading may say it. It is program text (argv[0]
/// when unchecked), so it is cleaned, cut at its first word and capped: a name
/// cannot forge a second line, write a sentence of its own into the heading,
/// or run off the window. The whole argv is still shown, as reported.
fn program_name(t: &terminal::TerminalAsk) -> String {
    let full = clean(t.program(), usize::MAX);
    let word = full.split(' ').next().unwrap_or_default();
    if word.is_empty() {
        "A program".to_owned()
    } else {
        clean(word, PROGRAM_MAX)
    }
}

/// Longest account name (ADR 0077): 1–32 of `[A-Za-z0-9_-]`.
const ACCOUNT_MAX: usize = 32;
/// Longest key the field takes. `ec-secret` reads at most 8 KiB a line; a
/// real key is about a hundred characters, so this only stops a runaway
/// paste.
const KEY_MAX: usize = 4096;

/// An account name `ec-secret` accepts. `default` is one by this rule too.
pub fn valid_account(name: &str) -> bool {
    (1..=ACCOUNT_MAX).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Legacy PINs are at most 16 characters (Bluetooth Core, Vol 3 Part C §3.2.1).
const PIN_MAX: usize = 16;
/// A passkey is 0–999999, typed as up to six digits.
const PASSKEY_DIGITS: usize = 6;

const USAGE: &str = "usage: ec-secret-prompt wifi <ssid> | bt <addr> pin|passkey|authorize \
                     | bt <addr> confirm <passkey> | bt <addr> show <code> | api-key <account> \
                     | terminal (request on stdin)";

/// One to six ASCII digits, as a passkey.
fn passkey(value: &str) -> Option<u32> {
    let digits = (1..=PASSKEY_DIGITS).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit());
    digits.then(|| value.parse().ok()).flatten()
}

impl Target {
    /// `wifi <ssid>`, `bt <addr> pin|passkey|authorize`, `bt <addr> confirm
    /// <passkey>` or `bt <addr> show <code>` — everything after argv[0].
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let bt = |addr: &String, code| {
            Ok(Target::Bluetooth {
                addr: addr.clone(),
                code,
            })
        };
        match args {
            [kind, ssid] if kind == "wifi" && !ssid.is_empty() => Ok(Target::Wifi { ssid: ssid.clone() }),
            [kind, account] if kind == "api-key" && valid_account(account) => Ok(Target::ApiKey {
                account: account.clone(),
            }),
            [kind, _] if kind == "api-key" => {
                Err("api-key takes an account name: 1 to 32 letters, digits, - or _".to_owned())
            }
            [kind, addr, code] if kind == "bt" && !addr.is_empty() => match code.as_str() {
                "pin" => bt(addr, Code::Pin),
                "passkey" => bt(addr, Code::Passkey),
                "authorize" => bt(addr, Code::Authorize),
                other => Err(format!(
                    "unknown code kind {other:?}: expected pin, passkey or authorize"
                )),
            },
            [kind, addr, mode, value] if kind == "bt" && !addr.is_empty() => match mode.as_str() {
                // The code itself is never echoed back in the refusal.
                "confirm" => match passkey(value) {
                    Some(n) => bt(addr, Code::Confirm(n)),
                    None => Err("confirm takes a passkey of one to six digits".to_owned()),
                },
                "show" if !value.is_empty() && value.chars().count() <= PIN_MAX => {
                    bt(addr, Code::Show(value.clone()))
                }
                "show" => Err("show takes a code of one to sixteen characters".to_owned()),
                other => Err(format!("unknown mode {other:?}: expected confirm or show")),
            },
            _ => Err(USAGE.to_owned()),
        }
    }

    /// Whether the human types anything. The yes/no and display-only modes
    /// have no field.
    pub fn takes_input(&self) -> bool {
        !matches!(
            self,
            Target::Bluetooth {
                code: Code::Confirm(_) | Code::Authorize | Code::Show(_),
                ..
            }
        )
    }

    /// Whether the window has an answer to give. `Show` has none: the device
    /// takes the code, and BlueZ opened no request for us to answer.
    pub fn answers(&self) -> bool {
        !matches!(
            self,
            Target::Bluetooth {
                code: Code::Show(_),
                ..
            }
        )
    }

    /// The code the window displays instead of a field: the six digits to
    /// compare, or the code to type on the device.
    pub fn shown(&self) -> Option<String> {
        match self {
            Target::Bluetooth {
                code: Code::Confirm(n),
                ..
            } => Some(format!("{n:06}")),
            Target::Bluetooth {
                code: Code::Show(c), ..
            } => Some(c.clone()),
            _ => None,
        }
    }

    /// The window title. The heading says the rest.
    pub fn title(&self) -> &'static str {
        match self {
            Target::Bluetooth {
                code: Code::Confirm(_) | Code::Authorize,
                ..
            } => "Confirm pairing",
            Target::Bluetooth {
                code: Code::Show(_), ..
            } => "Pairing code",
            Target::ApiKey { .. } => "Add API key",
            Target::Terminal(_) => "Terminal password",
            _ => "Enter password",
        }
    }

    /// The sheet's one sentence. A Terminal heading names the program,
    /// cleaned and capped.
    pub fn heading(&self) -> String {
        match self {
            Target::Wifi { .. } => "Wi-Fi needs a password".to_owned(),
            Target::Bluetooth { code: Code::Pin, .. } => "Bluetooth needs a PIN".to_owned(),
            Target::Bluetooth {
                code: Code::Passkey, ..
            } => "Bluetooth needs a passkey".to_owned(),
            Target::Bluetooth {
                code: Code::Confirm(_),
                ..
            } => "Does the other device show this code?".to_owned(),
            Target::Bluetooth {
                code: Code::Authorize,
                ..
            } => "Pair with this device?".to_owned(),
            Target::Bluetooth {
                code: Code::Show(_), ..
            } => "Type this code on the device".to_owned(),
            Target::ApiKey { .. } => "Add an API key".to_owned(),
            // `sudo` asks for the user's own password; anything else for one it names.
            Target::Terminal(t) if t.elevated() => format!("{} needs your password", program_name(t)),
            Target::Terminal(t) => format!("{} needs a password", program_name(t)),
        }
    }

    /// The heading's second line for a network, device or account: `("for",
    /// name)`. A Terminal request builds its own (the command it will run).
    pub fn subject(&self) -> Option<(&'static str, String)> {
        match self {
            Target::Wifi { ssid } => Some(("for", clean(ssid, SUBJECT_MAX))),
            Target::Bluetooth { addr, .. } => Some(("for", clean(addr, SUBJECT_MAX))),
            Target::ApiKey { account } => Some(("for", account.clone())),
            Target::Terminal(_) => None,
        }
    }

    /// The symbolic icon at the sheet's head.
    pub fn glyph(&self) -> &'static str {
        match self {
            Target::Wifi { .. } => "network-wireless",
            Target::Bluetooth { .. } => "bluetooth-active",
            Target::ApiKey { .. } => "dialog-password",
            Target::Terminal(t) if t.elevated() => "system-lock-screen",
            Target::Terminal(_) => "dialog-password",
        }
    }

    /// The verb on the one committing button.
    pub fn verb(&self) -> &'static str {
        match self {
            Target::Wifi { .. } => "Connect",
            Target::ApiKey { .. } => "Save",
            // `sudo` asks to act as root: the human allows it.
            Target::Terminal(t) if t.elevated() => "Allow",
            Target::Terminal(_) => "Send",
            Target::Bluetooth {
                code: Code::Show(_), ..
            } => "Done",
            Target::Bluetooth { .. } => "Pair",
        }
    }

    /// The committing button's label while its attempt is with the service.
    pub fn working(&self) -> &'static str {
        match self {
            Target::Wifi { .. } => "Connecting…",
            Target::Bluetooth { .. } => "Pairing…",
            Target::ApiKey { .. } => "Saving…",
            Target::Terminal(_) => "Sending…",
        }
    }

    /// The field's placeholder: what a complete value looks like. The heading
    /// already says which kind of secret this is.
    pub fn hint(&self) -> &'static str {
        match self {
            Target::Wifi { .. } => "8 to 63 characters",
            Target::ApiKey { .. } => "paste the key",
            Target::Terminal(_) => "password",
            Target::Bluetooth { code: Code::Pin, .. } => "up to 16 characters",
            Target::Bluetooth {
                code: Code::Passkey, ..
            } => "6 digits",
            Target::Bluetooth { .. } => "",
        }
    }

    /// Whether `typed` may stand in the field at all. A keystroke that would
    /// make it inadmissible is refused rather than accepted and complained
    /// about, so a passkey field simply does not take a letter.
    pub fn admits(&self, typed: &str) -> bool {
        match self {
            Target::Wifi { .. } => true,
            Target::ApiKey { .. } => typed.len() <= KEY_MAX,
            Target::Terminal(_) => typed.len() <= terminal::MAX_SECRET,
            Target::Bluetooth { code: Code::Pin, .. } => typed.chars().count() <= PIN_MAX,
            Target::Bluetooth {
                code: Code::Passkey, ..
            } => typed.len() <= PASSKEY_DIGITS && typed.bytes().all(|b| b.is_ascii_digit()),
            Target::Bluetooth { .. } => typed.is_empty(),
        }
    }

    /// Whether `typed` is complete enough to send. WPA2-PSK passphrases are
    /// 8–63 characters (IEEE 802.11-2020 §J.4.1); a 64-hex PSK is also 64.
    pub fn ready(&self, typed: &str) -> bool {
        match self {
            Target::Wifi { .. } => (8..=64).contains(&typed.chars().count()),
            // A paste often carries a stray space or newline; the key is sent
            // trimmed, so only what is left counts.
            Target::ApiKey { .. } => !typed.trim().is_empty(),
            // An empty password is the program's to refuse, not ours.
            Target::Terminal(_) => true,
            Target::Bluetooth {
                code: Code::Pin | Code::Passkey,
                ..
            } => !typed.is_empty(),
            Target::Bluetooth { .. } => true,
        }
    }
}

/// The text in the field. Every copy this process owns is overwritten when it
/// is replaced or dropped, and `Debug` never shows it.
///
/// The overwrite is best-effort by nature: iced's field lays out the bullets,
/// not the value, but a message carrying a keystroke's worth of text passes
/// through the runtime, and what the runtime does with its own buffers is out
/// of reach. What this promises is that no copy *we* hold outlives its use.
#[derive(Default)]
pub struct Buffer(String);

impl Buffer {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Take `new` as the field's value, overwriting the old one.
    pub fn replace(&mut self, new: String) {
        wipe(std::mem::replace(&mut self.0, new));
    }

    pub fn clear(&mut self) {
        self.replace(String::new());
    }

    /// Move the value out, leaving the field empty. The caller now owns the
    /// only copy and must `wipe` it.
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl fmt::Debug for Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Buffer(<redacted>)")
    }
}

impl Drop for Buffer {
    fn drop(&mut self) {
        wipe(std::mem::take(&mut self.0));
    }
}

/// Overwrite every byte the string's buffer owns, spare capacity included.
/// `black_box` keeps the optimiser from proving the writes dead. A plain
/// overwrite, since `zeroize` is not a dependency of this workspace.
pub fn wipe(value: String) {
    let mut bytes = value.into_bytes();
    let capacity = bytes.capacity();
    bytes.clear();
    bytes.resize(capacity, 0);
    std::hint::black_box(&mut bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parses_both_shapes_and_refuses_the_rest() {
        assert_eq!(
            Target::parse(&args(&["wifi", "Hollow Point"])),
            Ok(Target::Wifi {
                ssid: "Hollow Point".to_owned()
            })
        );
        assert_eq!(
            Target::parse(&args(&["bt", "AA:03", "passkey"])),
            Ok(Target::Bluetooth {
                addr: "AA:03".to_owned(),
                code: Code::Passkey
            })
        );
        assert!(Target::parse(&args(&["bt", "AA:03", "pairme"])).is_err());
        assert!(Target::parse(&args(&["wifi"])).is_err());
        assert!(Target::parse(&args(&["wifi", ""])).is_err());
    }

    #[test]
    fn parses_the_yes_no_and_show_modes() {
        let bt = |code| {
            Ok(Target::Bluetooth {
                addr: "AA:03".to_owned(),
                code,
            })
        };
        assert_eq!(
            Target::parse(&args(&["bt", "AA:03", "confirm", "42"])),
            bt(Code::Confirm(42))
        );
        assert_eq!(
            Target::parse(&args(&["bt", "AA:03", "authorize"])),
            bt(Code::Authorize)
        );
        assert_eq!(
            Target::parse(&args(&["bt", "AA:03", "show", "0042"])),
            bt(Code::Show("0042".to_owned()))
        );
        assert!(Target::parse(&args(&["bt", "AA:03", "confirm", "1234567"])).is_err());
        assert!(Target::parse(&args(&["bt", "AA:03", "confirm", "12a"])).is_err());
        assert!(Target::parse(&args(&["bt", "AA:03", "confirm", ""])).is_err());
        assert!(Target::parse(&args(&["bt", "AA:03", "confirm"])).is_err());
        assert!(Target::parse(&args(&["bt", "AA:03", "show", ""])).is_err());
        assert!(Target::parse(&args(&["bt", "AA:03", "authorize", "x"])).is_err());
    }

    #[test]
    fn a_confirmation_shows_six_digits_and_takes_no_input() {
        let t = Target::Bluetooth {
            addr: "AA:03".to_owned(),
            code: Code::Confirm(42),
        };
        assert_eq!(t.shown().as_deref(), Some("000042"));
        assert!(!t.takes_input());
        assert!(t.answers());
        assert!(t.ready(""));
        assert!(!t.admits("1"));
        let shown = Target::Bluetooth {
            addr: "AA:03".to_owned(),
            code: Code::Show("0042".to_owned()),
        };
        assert!(!shown.answers());
    }

    #[test]
    fn a_passkey_field_only_takes_six_digits() {
        let t = Target::Bluetooth {
            addr: "AA:03".to_owned(),
            code: Code::Passkey,
        };
        assert!(t.admits("012345"));
        assert!(!t.admits("0123456"));
        assert!(!t.admits("12a"));
    }

    #[test]
    fn an_api_key_target_takes_an_account_name_only() {
        assert_eq!(
            Target::parse(&args(&["api-key", "work"])),
            Ok(Target::ApiKey {
                account: "work".to_owned()
            })
        );
        assert!(Target::parse(&args(&["api-key", "default"])).is_ok());
        assert!(Target::parse(&args(&["api-key", "a_b-9"])).is_ok());
        assert!(Target::parse(&args(&["api-key", ""])).is_err());
        assert!(Target::parse(&args(&["api-key", "has space"])).is_err());
        assert!(Target::parse(&args(&["api-key", "dot.ted"])).is_err());
        assert!(Target::parse(&args(&["api-key", &"a".repeat(33)])).is_err());
        assert!(Target::parse(&args(&["api-key"])).is_err());
        let t = Target::ApiKey {
            account: "work".to_owned(),
        };
        assert_eq!(t.heading(), "Add an API key");
        assert_eq!(t.subject(), Some(("for", "work".to_owned())));
        assert!(t.takes_input() && t.answers());
        assert!(!t.ready("  \n"));
        assert!(t.ready(" k \n"));
    }

    #[test]
    fn a_hostile_program_name_cannot_forge_the_heading() {
        let mut ask = terminal::TerminalAsk::parse(
            br#"{"v":1,"via":"askpass","verified":{"window":"","tab":"","pane":"","pane_id":1,"in_foreground":true,"ancestry":[],"pid":1,"exe":null,"uid":1000,"euid":0},"unverified":{"argv":["x"],"cwd":null,"prompt":"","host":null}}"#,
            false,
        )
        .unwrap();
        ask.claims.argv[0] =
            "sudo wants your password.\n\u{1b}[2K\u{202E}Type it here:\r".to_owned() + &"A".repeat(400);
        let h = Target::Terminal(Box::new(ask.clone())).heading();
        assert_eq!(h, "sudo needs your password");
        ask.claims.argv[0] = "\u{202E}\u{1b}[31m".to_owned() + &"A".repeat(400);
        let h = Target::Terminal(Box::new(ask)).heading();
        assert!(
            !h.chars()
                .any(|c| c.is_control() || ('\u{202A}'..='\u{202E}').contains(&c)),
            "{h:?}"
        );
        let name = h.strip_suffix(" needs your password").unwrap();
        assert_eq!(name.chars().count(), PROGRAM_MAX);
        assert!(name.ends_with('…') && name.starts_with('A'));
    }

    #[test]
    fn a_wifi_passphrase_is_eight_to_sixty_four() {
        let t = Target::Wifi { ssid: "x".to_owned() };
        assert!(!t.ready("short"));
        assert!(t.ready("eightchr"));
        assert!(!t.ready(&"a".repeat(65)));
    }

    #[test]
    fn debug_never_shows_the_value() {
        let mut b = Buffer::default();
        b.replace("hunter22".to_owned());
        assert!(!format!("{b:?}").contains("hunter22"));
        let msg = app::Message::Typed("hunter22".to_owned());
        assert!(!format!("{msg:?}").contains("hunter22"));
        let t = Target::Bluetooth {
            addr: "AA:03".to_owned(),
            code: Code::Confirm(424242),
        };
        assert!(!format!("{t:?}").contains("424242"));
        assert!(!format!("{:?}", Code::Show("9911".to_owned())).contains("9911"));
    }
}
