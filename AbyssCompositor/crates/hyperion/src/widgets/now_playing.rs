// SPDX-License-Identifier: AGPL-3.0-only
//! Now Playing: art, title over artist, and the visualizer; the transport is
//! the revealed section.
//!
//! The visualizer is the bar's live value while something plays — the one
//! accent this widget spends, and only while the core is showing. With no
//! player the widget has no presence at all and animates out to zero width;
//! the last track stays drawn under the clip while it goes, so the cell
//! closes over its content instead of emptying first.

use iced::widget::{image, Row};
use iced::Alignment;

use eclipse_services::media::{NowPlaying, Playback};
use eclipse_ui::tokens::bar;
use eclipse_ui::widget::{self as parts, ShellFrame, TRANSPORT_W};

use super::{Action, Feed as Routed, Parts, Spans};
use crate::app::Message;

/// `bar.widgets.now-playing.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cfg {
    pub art: bool,
    pub visualizer: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            art: true,
            visualizer: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct State {
    /// What the media service last said; `None` is nothing playing.
    pub player: Option<NowPlaying>,
    /// The last track drawn, kept while the widget animates out.
    pub shown: Option<NowPlaying>,
    /// The decoded cover, by the service's art key, so an unchanged cover is
    /// never decoded twice.
    art: Option<(String, image::Handle)>,
    pub levels: [f32; bar::VIZ_BANDS],
}

impl State {
    /// A cover that did not come from the service: a preview's fixture.
    pub fn set_art(&mut self, key: &str, handle: image::Handle) {
        self.art = Some((key.to_owned(), handle));
    }

    pub fn playing(&self) -> bool {
        self.player
            .as_ref()
            .is_some_and(|p| p.status == Playback::Playing)
    }
}

#[derive(Debug, Clone)]
pub enum Feed {
    Player(Option<NowPlaying>),
    Levels([f32; bar::VIZ_BANDS]),
    Previous,
    PlayPause,
    Next,
}

pub fn update(state: &mut State, feed: Feed) -> Option<Action> {
    match feed {
        Feed::Player(player) => {
            if let Some(np) = &player {
                state.art = match &np.art {
                    Some(a) if state.art.as_ref().is_some_and(|(k, _)| *k == a.key) => state.art.take(),
                    Some(a) => Some((a.key.clone(), parts::art_handle(&a.bytes))),
                    None => None,
                };
                state.shown = Some(np.clone());
            } else {
                state.levels = [0.0; bar::VIZ_BANDS];
            }
            state.player = player;
            None
        }
        Feed::Levels(levels) => {
            state.levels = levels;
            None
        }
        Feed::Previous => Some(Action::Previous),
        Feed::PlayPause => Some(Action::PlayPause),
        Feed::Next => Some(Action::Next),
    }
}

pub fn spans(state: &State, cfg: &Cfg) -> Spans {
    let mut core = bar::MEDIA_TEXT_W;
    if cfg.art {
        core += bar::ART + bar::WIDGET_GAP;
    }
    if cfg.visualizer {
        core += bar::WIDGET_GAP + bar::VIZ_W;
    }
    Spans {
        core,
        revealed: TRANSPORT_W,
        present: state.player.is_some(),
    }
}

fn msg(f: Feed) -> Message {
    Message::Widget(Routed::NowPlaying(f))
}

pub fn view<'a>(state: &'a State, cfg: &Cfg, frame: ShellFrame) -> Parts<'a> {
    let Some(np) = state.shown.as_ref() else {
        return Parts::empty();
    };
    let playing = np.status == Playback::Playing;
    let subtitle = np.artist.as_deref().or(np.album.as_deref()).unwrap_or(&np.player);
    let mut core = Row::new().spacing(bar::WIDGET_GAP).align_y(Alignment::Center);
    if cfg.art {
        core = core.push(parts::art_thumb(state.art.as_ref().map(|(_, h)| h), false));
    }
    core = core.push(parts::track_label(&np.title, subtitle, bar::MEDIA_TEXT_W));
    if cfg.visualizer {
        // The accent is the live value: playing, and actually on screen.
        core = core.push(parts::viz_bars(&state.levels, playing && frame.open > 0.0));
    }
    let transport = parts::transport(
        playing,
        np.can_prev.then(|| msg(Feed::Previous)),
        np.can_pause.then(|| msg(Feed::PlayPause)),
        np.can_next.then(|| msg(Feed::Next)),
    );
    Parts {
        core: core.into(),
        revealed: Some(transport),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str) -> NowPlaying {
        NowPlaying {
            player: "spotify".into(),
            title: title.into(),
            artist: Some("M83".into()),
            album: None,
            art: None,
            status: Playback::Playing,
            can_prev: true,
            can_next: true,
            can_pause: true,
        }
    }

    /// Nothing playing is no presence, but the last track stays drawn so the
    /// cell closes over it rather than emptying first.
    #[test]
    fn a_stopped_player_leaves_its_last_track_to_animate_out() {
        let mut s = State::default();
        update(&mut s, Feed::Player(Some(track("Midnight City"))));
        assert!(spans(&s, &Cfg::default()).present);
        update(&mut s, Feed::Player(None));
        assert!(!spans(&s, &Cfg::default()).present);
        assert_eq!(s.shown.as_ref().map(|t| t.title.as_str()), Some("Midnight City"));
    }

    #[test]
    fn switching_art_and_visualizer_off_narrows_the_core() {
        let s = State::default();
        let full = spans(&s, &Cfg::default()).core;
        let bare = spans(
            &s,
            &Cfg {
                art: false,
                visualizer: false,
            },
        )
        .core;
        assert_eq!(bare, bar::MEDIA_TEXT_W);
        assert!(full > bare);
    }

    #[test]
    fn transport_presses_become_media_actions() {
        let mut s = State::default();
        assert_eq!(update(&mut s, Feed::PlayPause), Some(Action::PlayPause));
        assert_eq!(update(&mut s, Feed::Next), Some(Action::Next));
    }
}
