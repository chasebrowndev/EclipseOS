// SPDX-License-Identifier: AGPL-3.0-only
//! Test driver: runs [`Effect`]s synchronously against the fake environment, so
//! the whole wizard, from Begin to Restart, is an ordinary `#[test]` with no
//! iced, no window, no root and no desktop to disturb.

use crate::config::{self, Writer};
use crate::helper::{self, Env, FakeCfg};
use crate::model::{Effect, Inputs, Message, Model, Phase, Secret, Step, ZoneClock};
use crate::{net, sys};
use eclipse_setup_plan::Stage;
use iced::futures::StreamExt;
use std::collections::VecDeque;
use std::time::Duration;

pub struct Driver {
    pub model: Model,
    pub env: Env,
    pub writer: Writer,
    /// One line per effect, from the hand-written `Debug` that hides secrets.
    pub log: Vec<String>,
}

impl Driver {
    pub fn fake(fail_at: Option<Stage>) -> Driver {
        Driver {
            model: Model::new(Inputs::builtin(true)),
            env: Env::fake(FakeCfg {
                fail_at,
                delay: Duration::ZERO,
            }),
            writer: Writer::recording(),
            log: Vec::new(),
        }
    }

    /// Send a message and run everything it causes, to quiescence.
    pub fn send(&mut self, message: Message) {
        let mut queue: VecDeque<Message> = VecDeque::from([message]);
        while let Some(m) = queue.pop_front() {
            for effect in self.model.update(m) {
                self.log.push(format!("{effect:?}"));
                queue.extend(self.run(effect));
            }
        }
    }

    fn run(&mut self, effect: Effect) -> Vec<Message> {
        match effect {
            Effect::Config { key, value } => {
                let r = if config::tolerates_unknown(key) {
                    self.writer.set_setup(key, value)
                } else {
                    self.writer.set(key, value)
                };
                vec![Message::ConfigResult(key, r)]
            }
            Effect::LoadDisks => vec![Message::DisksLoaded(helper::list_disks(&self.env))],
            Effect::ReadClock(zone) => {
                // `date` is real and harmless; a fake clock keeps the test hermetic.
                let clock = sys::zone_clock(&zone).or_else(|| {
                    Some(ZoneClock {
                        time: "12:00".into(),
                        offset: "+0000".into(),
                    })
                });
                vec![Message::ZoneClock(zone, clock)]
            }
            Effect::NetScan => vec![Message::NetScanned(net::scan(&self.env))],
            Effect::NetJoin {
                ssid,
                secure,
                passphrase,
            } => {
                vec![Message::Joined(net::join(&self.env, &ssid, secure, passphrase))]
            }
            Effect::Apply(request) => {
                let rx = helper::apply(&self.env, request);
                iced::futures::executor::block_on(rx.collect::<Vec<_>>())
                    .into_iter()
                    .map(Message::Helper)
                    .collect()
            }
            Effect::Reboot => vec![Message::Restarted(helper::reboot(&self.env))],
            Effect::Focus(_) | Effect::FocusNext | Effect::FocusPrev => Vec::new(),
        }
    }

    /// Walk the honest route up to the review, with a valid identity.
    pub fn walk_to_review(&mut self) {
        self.send(Message::Begin);
        self.send(Message::Next); // language -> keyboard
        self.send(Message::Next); // -> time zone
        self.send(Message::Next); // -> network
        self.send(Message::Next); // -> disk
        assert_eq!(self.model.step, Step::Disk);
        let id = helper::fake_disks()[1].by_id.clone();
        self.send(Message::SelectDisk(id));
        self.send(Message::Next);
        self.send(Message::Username("chase".into()));
        self.send(Message::Password(Secret::new("correct horse".into())));
        self.send(Message::Password2(Secret::new("correct horse".into())));
        // Identity, profile and the five choice steps are all Continue.
        for _ in 0..7 {
            self.send(Message::Next);
        }
        assert_eq!(self.model.step, Step::Review);
    }

    pub fn arm(&mut self) {
        let id = self.model.disk.clone().expect("a disk is chosen");
        self.send(Message::Confirm(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crate::data::LANGUAGES;

    fn keys(d: &Driver) -> Vec<&str> {
        d.writer.recorded().iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn the_whole_flow_runs_against_the_fake_helper_and_restarts() {
        let mut d = Driver::fake(None);
        d.send(Message::Begin);
        let de = LANGUAGES.iter().position(|l| l.locale == "de_DE.UTF-8").unwrap();
        d.send(Message::Language(de));
        d.walk_to_review();
        assert!(!d.model.can_apply());
        d.arm();
        assert!(d.model.can_apply());
        d.send(Message::Apply);

        assert_eq!(d.model.install.phase, Phase::Done);
        assert_eq!(d.model.install.pct, 100);
        assert!(d.model.password.is_empty() && d.model.password2.is_empty());

        d.send(Message::Restart);
        assert!(d.model.install.restart_error.is_none());
        assert!(d.log.iter().any(|l| l == "Reboot"));

        // Nothing in the log carries the password.
        assert!(!d.log.join("\n").contains("correct horse"));
    }

    #[test]
    fn every_config_write_is_on_the_allowlist_and_none_is_policy() {
        let mut d = Driver::fake(None);
        d.send(Message::Begin);
        let de = LANGUAGES.iter().position(|l| l.locale == "de_DE.UTF-8").unwrap();
        d.send(Message::Language(de));
        d.send(Message::Variant(Some("nodeadkeys".into())));
        d.walk_to_review();
        d.arm();
        d.send(Message::Apply);

        let written = keys(&d);
        assert!(written.contains(&"input.kb-layout"));
        assert!(written.contains(&"input.kb-variant"));
        assert!(written.contains(&"setup.complete"));
        for k in &written {
            assert!(config::is_allowed(k), "{k}");
            assert!(!k.contains("policy"), "{k}");
            assert!(!k.contains("phrase"), "{k}");
        }
        // The layout is written before the variant, every time.
        let first_layout = written.iter().position(|k| *k == "input.kb-layout").unwrap();
        let first_variant = written.iter().position(|k| *k == "input.kb-variant").unwrap();
        assert!(first_layout < first_variant);
    }

    #[test]
    fn a_failure_at_pacstrap_is_reported_at_pacstrap_and_can_be_retried() {
        let mut d = Driver::fake(Some(Stage::Pacstrap));
        d.walk_to_review();
        d.arm();
        d.send(Message::Apply);
        assert_eq!(d.model.install.phase, Phase::Failed);
        assert_eq!(d.model.install.failed_at, Some(Stage::Pacstrap));
        assert!(crate::model::touched_disk(Stage::Pacstrap));

        // Restart is not on offer after a failure.
        d.send(Message::Restart);
        assert!(!d.log.iter().any(|l| l == "Reboot"));

        d.send(Message::Back);
        assert_eq!(d.model.step, Step::Identity);
        assert!(!d.model.can_next(), "the password has to be typed again");
    }

    #[test]
    fn a_failure_before_the_disk_is_touched_says_so() {
        let mut d = Driver::fake(Some(Stage::Preflight));
        d.walk_to_review();
        d.arm();
        d.send(Message::Apply);
        assert_eq!(d.model.install.phase, Phase::Failed);
        assert!(!crate::model::touched_disk(d.model.install.failed_at.unwrap()));
    }

    #[test]
    fn the_fake_run_never_writes_anywhere_the_recorder_did_not_see() {
        // A dry run must not reach the real socket: the writer in the driver is
        // the recording one, and the app builds the same for `--fake-helper`.
        let d = Driver::fake(None);
        assert!(matches!(d.writer, Writer::Record(_)));
        assert!(d.env.is_fake());
    }
}
