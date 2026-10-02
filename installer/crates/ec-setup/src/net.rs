// SPDX-License-Identifier: AGPL-3.0-only
//! Step 3: NetworkManager, through `nmcli` with fixed argv (D-07 §4).
//!
//! Three rules. No shell: every command is a fixed argument list. No secret on
//! argv: a passphrase reaches `nmcli --ask` on its **stdin** only, and the
//! buffer that carried it is overwritten straight after. And nothing `nmcli`
//! prints is shown to the user verbatim: its stderr can quote whatever it was
//! given, so the wizard says a fixed sentence instead.
//!
//! Wi-fi credentials belong to the *live* NetworkManager and are not carried to
//! the target (D-07 §4.3); nothing here writes any file.

use crate::helper::Env;
use std::io::Write;
use std::process::{Command, Stdio};
use zeroize::Zeroizing;

/// Whether the machine can reach anything.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Link {
    /// Not asked yet.
    #[default]
    Unknown,
    /// A non-loopback device is connected: `medium` is nmcli's device type
    /// (`ethernet`, `wifi`), `name` its connection.
    Online {
        medium: String,
        name: String,
    },
    Offline,
    /// `nmcli` is missing or NetworkManager is not running.
    NoManager,
}

impl Link {
    pub fn is_online(&self) -> bool {
        matches!(self, Link::Online { .. })
    }
}

/// One access point, best signal per SSID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ap {
    pub ssid: String,
    /// 0..=100.
    pub signal: u8,
    pub secure: bool,
    pub in_use: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub link: Link,
    pub aps: Vec<Ap>,
}

/// Split one line of `nmcli -t` output: fields are separated by `:`, and a
/// literal `:` or `\` inside a field is escaped with a backslash.
pub fn split_terse(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ':' => fields.push(std::mem::take(&mut cur)),
            other => cur.push(other),
        }
    }
    fields.push(cur);
    fields
}

/// `nmcli -t -f TYPE,STATE,CONNECTION device status`
pub fn parse_device_status(text: &str) -> Link {
    for line in text.lines() {
        let f = split_terse(line);
        let [ty, state, name] = [
            f.first().map_or("", String::as_str),
            f.get(1).map_or("", String::as_str),
            f.get(2).map_or("", String::as_str),
        ];
        if matches!(ty, "loopback" | "bridge" | "tun" | "dummy") {
            continue;
        }
        if state.starts_with("connected") && !state.starts_with("connecting") {
            return Link::Online {
                medium: ty.to_owned(),
                name: name.to_owned(),
            };
        }
    }
    Link::Offline
}

/// `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY device wifi list`
pub fn parse_wifi_list(text: &str) -> Vec<Ap> {
    let mut aps: Vec<Ap> = Vec::new();
    for line in text.lines() {
        let f = split_terse(line);
        if f.len() < 4 {
            continue;
        }
        let ssid = f[1].clone();
        if ssid.is_empty() {
            continue;
        }
        let ap = Ap {
            in_use: f[0].trim() == "*",
            signal: f[2].trim().parse().unwrap_or(0),
            secure: !f[3].trim().is_empty() && f[3].trim() != "--",
            ssid,
        };
        match aps.iter_mut().find(|a| a.ssid == ap.ssid) {
            Some(existing) => {
                if ap.in_use || (!existing.in_use && ap.signal > existing.signal) {
                    *existing = ap;
                }
            }
            None => aps.push(ap),
        }
    }
    aps.sort_by(|a, b| b.in_use.cmp(&a.in_use).then(b.signal.cmp(&a.signal)));
    aps
}

/// An SSID that can be handed to `nmcli` as an argument: not option-shaped, no
/// control characters, at most 32 bytes (the 802.11 limit).
pub fn ssid_ok(ssid: &str) -> bool {
    !ssid.is_empty() && ssid.len() <= 32 && !ssid.starts_with('-') && !ssid.chars().any(char::is_control)
}

fn nmcli(args: &[&str]) -> Option<String> {
    let out = Command::new("nmcli")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Ask NetworkManager where things stand. Blocking; run it off the UI thread.
pub fn scan(env: &Env) -> Snapshot {
    if env.is_fake() {
        return fake_snapshot(true);
    }
    let Some(status) = nmcli(&["-t", "-f", "TYPE,STATE,CONNECTION", "device", "status"]) else {
        return Snapshot {
            link: Link::NoManager,
            aps: Vec::new(),
        };
    };
    let aps = nmcli(&[
        "-t",
        "-f",
        "IN-USE,SSID,SIGNAL,SECURITY",
        "device",
        "wifi",
        "list",
    ])
    .map(|t| parse_wifi_list(&t))
    .unwrap_or_default();
    Snapshot {
        link: parse_device_status(&status),
        aps,
    }
}

/// Join a network. `passphrase` is written to nmcli's stdin and nowhere else.
/// Returns whether it connected. Blocking.
pub fn join(env: &Env, ssid: &str, secure: bool, passphrase: Zeroizing<String>) -> bool {
    if !ssid_ok(ssid) {
        return false;
    }
    if env.is_fake() {
        return !secure || passphrase.len() >= 8;
    }
    let mut cmd = Command::new("nmcli");
    cmd.args(["--wait", "30"]);
    if secure {
        // `--ask` reads the missing secret from stdin instead of argv.
        cmd.arg("--ask");
    }
    cmd.args(["device", "wifi", "connect", ssid])
        .stdin(if secure { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    if secure {
        if let Some(mut stdin) = child.stdin.take() {
            // The line that carries the secret is a Zeroizing buffer too.
            let mut line = Zeroizing::new(Vec::with_capacity(passphrase.len() + 1));
            line.extend_from_slice(passphrase.as_bytes());
            line.push(b'\n');
            let _ = stdin.write_all(&line);
        }
    }
    drop(passphrase);
    child.wait().map(|s| s.success()).unwrap_or(false)
}

/// What `--fake-helper` shows: online over wifi with a handful of neighbours.
pub fn fake_snapshot(online: bool) -> Snapshot {
    let ap = |ssid: &str, signal: u8, secure: bool, in_use: bool| Ap {
        ssid: ssid.into(),
        signal,
        secure,
        in_use,
    };
    Snapshot {
        link: if online {
            Link::Online {
                medium: "wifi".into(),
                name: "Lighthouse".into(),
            }
        } else {
            Link::Offline
        },
        aps: vec![
            ap("Lighthouse", 82, true, online),
            ap("Cafe Meridian", 61, false, false),
            ap("DIRECT-7f-Printer", 38, true, false),
            ap("Neighbour 5G", 27, true, false),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terse_fields_unescape_colons_and_backslashes() {
        assert_eq!(split_terse("a:b:c"), ["a", "b", "c"]);
        assert_eq!(split_terse(r"*:My\:Net:78:WPA2"), ["*", "My:Net", "78", "WPA2"]);
        assert_eq!(split_terse(r"a\\b:c"), [r"a\b", "c"]);
        assert_eq!(split_terse("::"), ["", "", ""]);
    }

    #[test]
    fn device_status_finds_a_connected_real_device() {
        let t = "loopback:connected (externally):lo\nethernet:unavailable:\nwifi:connected:Lighthouse\n";
        assert_eq!(
            parse_device_status(t),
            Link::Online {
                medium: "wifi".into(),
                name: "Lighthouse".into()
            }
        );
        assert_eq!(
            parse_device_status("loopback:connected (externally):lo\nwifi:disconnected:\n"),
            Link::Offline
        );
        assert_eq!(
            parse_device_status("wifi:connecting (getting IP configuration):x\n"),
            Link::Offline
        );
        assert_eq!(parse_device_status(""), Link::Offline);
    }

    #[test]
    fn wifi_list_dedupes_by_ssid_keeps_the_best_and_the_one_in_use_first() {
        let t =
            " :Cafe:40:WPA2\n *:Home:55:WPA2\n :Home:80:WPA2\n :Open:20:\n :Hidden-empty:::\n ::70:WPA2\n";
        // The IN-USE column is a single char; the fixture above pads it.
        let t = t.replace("\n ", "\n");
        let aps = parse_wifi_list(t.trim_start());
        assert_eq!(aps[0].ssid, "Home");
        assert!(aps[0].in_use);
        assert_eq!(aps.iter().filter(|a| a.ssid == "Home").count(), 1);
        assert!(aps.iter().any(|a| a.ssid == "Open" && !a.secure));
        assert!(aps.iter().all(|a| !a.ssid.is_empty()));
    }

    #[test]
    fn ssids_that_look_like_options_or_are_odd_are_refused() {
        assert!(ssid_ok("Lighthouse"));
        assert!(ssid_ok("Cafe Meridian"));
        for bad in ["", "-h", "--ask", "a\nb", &"x".repeat(33)] {
            assert!(!ssid_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_fake_join_needs_a_plausible_passphrase_and_touches_nothing() {
        let env = Env::fake(Default::default());
        assert!(join(
            &env,
            "Lighthouse",
            true,
            Zeroizing::new("correct horse".into())
        ));
        assert!(!join(&env, "Lighthouse", true, Zeroizing::new("short".into())));
        assert!(join(&env, "Cafe", false, Zeroizing::new(String::new())));
        assert!(!join(&env, "--ask", false, Zeroizing::new(String::new())));
    }
}
