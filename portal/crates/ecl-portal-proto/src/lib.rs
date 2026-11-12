// SPDX-License-Identifier: Apache-2.0

//! Wire types between `ecl-desktop-portal` and its dialog panes
//! (`ecl-portal-ui`), and the capture-confirm request sent to abyss over
//! `portal.sock` (ADR 0097). No I/O. Panes carry no authority: every reply is
//! a *choice*, never a grant.

use serde::{Deserialize, Serialize};

/// Frame version for the pane socketpair.
pub const VERSION: u16 = 1;

/// Who is asking, as far as the daemon can tell. Advisory only (ADR 0096).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requester {
    /// `.desktop` id, `host` when unknown.
    pub app_id: String,
    pub display_name: Option<String>,
    pub icon: Option<String>,
    /// How the id was derived (shown as a hint, never trusted).
    pub source: IdentitySource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentitySource {
    Flatpak,
    SecurityContext,
    DesktopFile,
    Host,
}

/// A thing that can be shared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Output {
        name: String,
        width: u32,
        height: u32,
    },
    Window {
        id: u64,
        title: String,
        app_id: String,
    },
}

/// Daemon -> pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneRequest {
    /// ScreenCast / Screenshot source chooser.
    PickSource {
        requester: Requester,
        sources: Vec<Source>,
        allow_multiple: bool,
        allow_audio: bool,
        allow_window: bool,
        allow_monitor: bool,
        remembered: Option<Source>,
    },
    /// Generic permission question (Location, Camera, Background, ...).
    Permission {
        requester: Requester,
        interface: String,
        title: String,
        detail: String,
        allow_remember: bool,
    },
    /// Screenshot interactive area/window/screen choice.
    ScreenshotMode { requester: Requester },
    /// Colour picker.
    PickColor { requester: Requester },
    /// Account details sheet (user name, avatar).
    Account {
        requester: Requester,
        reason: String,
    },
    /// Email composition hand-off confirmation.
    Email {
        requester: Requester,
        addresses: Vec<String>,
    },
}

/// Pane -> daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PaneReply {
    Sources {
        chosen: Vec<Source>,
        audio: bool,
        remember: bool,
    },
    Permission {
        allow: bool,
        remember: bool,
    },
    ScreenshotMode(ScreenshotMode),
    Color {
        r: f64,
        g: f64,
        b: f64,
    },
    Account {
        name: String,
        real_name: String,
        image: Option<String>,
    },
    Email {
        send: bool,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenshotMode {
    Screen,
    Window,
    Area,
}

/// Daemon -> abyss over `portal.sock` (ADR 0097). Fields are claims to display;
/// abyss re-derives the target itself and shows what *it* sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureConfirm {
    pub app_id_claim: String,
    pub stream: u64,
    pub target: Source,
    pub audio: bool,
    pub persistent: bool,
}

/// abyss -> daemon. `Granted` carries the grant id policyd minted; the daemon
/// presents it back with every capture session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureAnswer {
    Granted { grant: u64 },
    Denied,
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_round_trip() {
        let r = PaneReply::Sources {
            chosen: vec![Source::Output {
                name: "DP-1".into(),
                width: 2560,
                height: 1440,
            }],
            audio: false,
            remember: true,
        };
        let b = postcard::to_stdvec(&r).unwrap();
        assert_eq!(postcard::from_bytes::<PaneReply>(&b).unwrap(), r);
    }

    #[test]
    fn capture_answer_round_trip() {
        let a = CaptureAnswer::Granted { grant: 7 };
        let b = postcard::to_stdvec(&a).unwrap();
        assert_eq!(postcard::from_bytes::<CaptureAnswer>(&b).unwrap(), a);
    }

    /// abyss decodes and encodes these by hand (`trusted_ui/screencast.rs`,
    /// ADR 0097); its tests use exactly these bytes. Change one, change both.
    #[test]
    fn portal_sock_bytes_match_abyss() {
        let c = CaptureConfirm {
            app_id_claim: "com.discordapp.Discord".into(),
            stream: 7,
            target: Source::Output {
                name: "DP-1".into(),
                width: 1920,
                height: 1200,
            },
            audio: true,
            persistent: false,
        };
        let mut want = vec![22];
        want.extend_from_slice(b"com.discordapp.Discord");
        want.extend_from_slice(&[7, 0, 4]);
        want.extend_from_slice(b"DP-1");
        want.extend_from_slice(&[0x80, 0x0f, 0xb0, 0x09, 1, 0]);
        assert_eq!(postcard::to_stdvec(&c).unwrap(), want);
        let ans = |a: CaptureAnswer| postcard::to_stdvec(&a).unwrap();
        assert_eq!(ans(CaptureAnswer::Granted { grant: 300 }), [0, 0xac, 0x02]);
        assert_eq!(ans(CaptureAnswer::Denied), [1]);
        assert_eq!(ans(CaptureAnswer::Timeout), [2]);
    }
}
