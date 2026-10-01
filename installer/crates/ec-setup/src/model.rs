// SPDX-License-Identifier: AGPL-3.0-only
//! The wizard as a pure state machine (D-07 §4).
//!
//! [`Model::update`] takes a [`Message`], changes the model and returns the
//! [`Effect`]s the outside world has to perform. It never touches the
//! filesystem, the network, the socket or the clock, so step order, gating,
//! back and next, "Apply is disabled until the disk's name is typed" and "the
//! password is gone once it has been sent" are all ordinary unit tests. The iced
//! glue ([`crate::app`]) and the test driver ([`crate::exec`]) are the only two
//! things that run effects.
//!
//! The password lives in a [`Secret`]: a `Zeroizing` buffer with a redacted
//! `Debug`, so a stray `{:?}` of a message or the model cannot print it. It
//! leaves the model exactly once, inside [`Effect::Apply`], and both buffers
//! that held it are empty afterwards.

use crate::choices::{self, BarPosition, Blur, Choices, Mode, Tiling};
use crate::config::{self, WriteError};
use crate::data::{self, Layout, LANGUAGES};
use crate::helper::HelperEvent;
use crate::net::Snapshot;
use ec_setup_plan::{
    check_password, valid_by_id, valid_hostname, valid_username, Disk, Plan, Profile, Progress, Request,
    Stage, MIN_PASSWORD_CHARS,
};
use serde_json::{json, Value};
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

/// Text-input ids the view sets and effects may focus.
pub mod ids {
    pub const KB_TEST: &str = "kb-test";
    pub const KB_FILTER: &str = "kb-filter";
    pub const ZONE_SEARCH: &str = "zone-search";
    pub const PASSPHRASE: &str = "passphrase";
    pub const HOSTNAME: &str = "hostname";
    pub const USERNAME: &str = "username";
    pub const PASSWORD: &str = "password";
    pub const PASSWORD2: &str = "password2";
    pub const CONFIRM: &str = "confirm";
}

/// The screens of the install flow, in order. `number` is D-07 §4's own number:
/// steps 11 and 12 are not built yet, which is why the numbers jump. The
/// keyboard screen is D-07's step 1 shown on its own, and layout is split from
/// appearance (step 10), so each pair shares a number; the wizard asks layout
/// before components, which is not D-07's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Language,
    Keyboard,
    Timezone,
    Network,
    Disk,
    Identity,
    Profile,
    Mode,
    Layout,
    Components,
    Appearance,
    Apps,
    Review,
    Install,
}

impl Step {
    pub const ALL: [Step; 15] = [
        Step::Welcome,
        Step::Language,
        Step::Keyboard,
        Step::Timezone,
        Step::Network,
        Step::Disk,
        Step::Identity,
        Step::Profile,
        Step::Mode,
        Step::Layout,
        Step::Components,
        Step::Appearance,
        Step::Apps,
        Step::Review,
        Step::Install,
    ];

    pub fn number(self) -> u8 {
        match self {
            Step::Welcome => 0,
            Step::Language | Step::Keyboard => 1,
            Step::Timezone => 2,
            Step::Network => 3,
            Step::Disk => 4,
            Step::Identity => 5,
            Step::Profile => 6,
            Step::Mode => 7,
            Step::Components => 8,
            Step::Apps => 9,
            Step::Layout | Step::Appearance => 10,
            Step::Review => 13,
            Step::Install => 14,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome",
            Step::Language => "Language",
            Step::Keyboard => "Keyboard",
            Step::Timezone => "Time zone",
            Step::Network => "Network",
            Step::Disk => "Disk",
            Step::Identity => "Identity",
            Step::Profile => "Profile",
            Step::Mode => "Mode",
            Step::Layout => "Layout",
            Step::Components => "Components",
            Step::Appearance => "Appearance",
            Step::Apps => "Apps",
            Step::Review => "Review",
            Step::Install => "Install",
        }
    }

    /// The steps the progress indicator counts: everything between the welcome
    /// and the install itself.
    pub const COUNTED: usize = 13;

    /// 1-based place among the counted steps, or `None` on the welcome and
    /// while installing.
    pub fn position(self) -> Option<usize> {
        match self {
            Step::Welcome | Step::Install => None,
            s => Some(s.index()),
        }
    }

    pub fn index(self) -> usize {
        Step::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub fn next(self) -> Option<Step> {
        Step::ALL.get(self.index() + 1).copied()
    }

    pub fn prev(self) -> Option<Step> {
        self.index().checked_sub(1).map(|i| Step::ALL[i])
    }
}

/// A secret string. Zeroed on drop, redacted in `Debug`, never `Display`.
#[derive(Clone, Default)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(s: String) -> Secret {
        Secret(Zeroizing::new(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Overwrite the bytes, then empty the buffer.
    pub fn clear(&mut self) {
        self.0.zeroize();
    }

    /// Hand the buffer to a new owner that will wipe it; leave this one empty.
    pub fn take(&mut self) -> Zeroizing<String> {
        Zeroizing::new(std::mem::take(&mut *self.0))
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// A value fetched off the UI thread.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

/// Whether the keyboard layout reached the running compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum KbLive {
    #[default]
    Idle,
    Live,
    /// No control socket: the choice is kept and applies to the target only.
    Offline,
    /// The compositor refused the value (COMP-13 §1.4 validation).
    Refused,
}

/// The clock at the chosen zone, as `date` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneClock {
    pub time: String,
    pub offset: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Running,
    Done,
    Failed,
}

/// The install stages the progress screen lists, in the helper's order.
pub const STAGE_ORDER: [Stage; 10] = [
    Stage::Preflight,
    Stage::Confirm,
    Stage::Partition,
    Stage::Pacstrap,
    Stage::Configure,
    Stage::Bootloader,
    Stage::User,
    Stage::Seed,
    Stage::Units,
    Stage::Done,
];

pub fn stage_rank(s: Stage) -> usize {
    match s {
        Stage::Validate => 0,
        other => STAGE_ORDER.iter().position(|x| *x == other).map_or(0, |i| i + 1),
    }
}

/// D-07 §8: everything up to and including `Confirm` can fail with the disk
/// untouched.
pub fn touched_disk(s: Stage) -> bool {
    stage_rank(s) > stage_rank(Stage::Confirm)
}

#[derive(Clone, Debug)]
pub struct Install {
    pub phase: Phase,
    pub pct: u8,
    pub stage: Option<Stage>,
    /// The helper's short fixed-vocabulary message for the latest event.
    pub note: String,
    pub failed_at: Option<Stage>,
    /// `Some(err)` when the restart was refused.
    pub restart_error: Option<String>,
    pub restarting: bool,
}

impl Install {
    fn idle() -> Install {
        Install {
            phase: Phase::Idle,
            pct: 0,
            stage: None,
            note: String::new(),
            failed_at: None,
            restart_error: None,
            restarting: false,
        }
    }
}

/// Everything the model needs from the machine before it is built.
#[derive(Clone, Debug)]
pub struct Inputs {
    pub layouts: Vec<Layout>,
    pub keymaps: Vec<String>,
    /// NetworkManager already holds a saved Wi-Fi connection.
    pub saved_wifi: bool,
    pub zones: Vec<String>,
    pub zone_index: Vec<data::ZoneEntry>,
    pub default_zone: Option<String>,
    pub reduced_motion: bool,
    pub version: String,
    pub fake: bool,
}

impl Inputs {
    /// The machine's own lists.
    pub fn detect(reduced_motion: bool, fake: bool) -> Inputs {
        let zones = data::load_zones();
        Inputs {
            default_zone: data::current_zone(&zones),
            zone_index: data::load_zone_index(&zones),
            layouts: data::load_layouts(),
            keymaps: data::load_keymaps(),
            saved_wifi: data::has_saved_wifi(),
            zones,
            reduced_motion,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            fake,
        }
    }

    /// Fixed lists, no filesystem: tests and screenshots.
    pub fn builtin(fake: bool) -> Inputs {
        Inputs {
            layouts: data::builtin_layouts(),
            keymaps: data::builtin_keymaps(),
            saved_wifi: false,
            zone_index: data::build_index(&data::builtin_zones(), "", ""),
            zones: data::builtin_zones(),
            default_zone: Some("Europe/Berlin".into()),
            reduced_motion: false,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            fake,
        }
    }
}

/// A key the wizard handles itself, when no text field wants it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
    Up,
    Down,
    Tab,
    BackTab,
}

#[derive(Clone, Debug)]
pub enum Message {
    /// Step 0's hand-off: `ec_welcome::Message::Begin`.
    Begin,
    Next,
    Back,
    Key(Key),

    /// The step's "More options" disclosure, open or shut.
    More(bool),
    Language(usize),
    Layout(String),
    Variant(Option<String>),
    /// The console keymap the installed system boots with.
    Keymap(String),
    CarryNetwork(bool),
    KbFilter(String),
    KbTest(String),
    ConfigResult(&'static str, Result<(), WriteError>),

    /// What is typed in the time-zone search box.
    ZoneQuery(String),
    Zone(String),
    ZoneClock(String, Option<ZoneClock>),

    NetScanned(Snapshot),
    Rescan,
    SelectAp(String),
    Passphrase(Secret),
    Join,
    Joined(bool),

    DisksLoaded(Result<Vec<Disk>, String>),
    ReloadDisks,
    SelectDisk(String),

    Hostname(String),
    Username(String),
    Password(Secret),
    Password2(Secret),

    Profile(Profile),
    SetMode(Mode),
    SetTiling(Tiling),
    /// Slot index into [`choices::SLOTS`] and one of that slot's choices.
    SetSlot(usize, &'static str),
    SetRounded(bool),
    SetBlur(Blur),
    SetAnimations(bool),
    SetBarPosition(BarPosition),
    /// Tick or untick an application, by index into [`choices::APPS`].
    ToggleApp(usize),

    Confirm(String),
    Apply,

    Helper(HelperEvent),
    Restart,
    Restarted(Result<(), String>),

    /// Nothing to do: a focus task with no target, and Enter in the keyboard
    /// test field, which must not advance the wizard.
    Noop,
}

/// What the outside world has to do. Deliberately not `Clone`, and `Debug` is
/// written by hand: [`Effect::Apply`] and [`Effect::NetJoin`] carry secrets.
pub enum Effect {
    /// One `set_config_value` through [`crate::config::Writer`].
    Config {
        key: &'static str,
        value: Value,
    },
    LoadDisks,
    ReadClock(String),
    NetScan,
    NetJoin {
        ssid: String,
        secure: bool,
        passphrase: Zeroizing<String>,
    },
    Apply(Request),
    Reboot,
    Focus(&'static str),
    FocusNext,
    FocusPrev,
}

impl fmt::Debug for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Effect::Config { key, .. } => write!(f, "Config({key})"),
            Effect::LoadDisks => f.write_str("LoadDisks"),
            Effect::ReadClock(z) => write!(f, "ReadClock({z})"),
            Effect::NetScan => f.write_str("NetScan"),
            Effect::NetJoin { ssid, .. } => write!(f, "NetJoin({ssid}, ..)"),
            Effect::Apply(_) => f.write_str("Apply(..)"),
            Effect::Reboot => f.write_str("Reboot"),
            Effect::Focus(id) => write!(f, "Focus({id})"),
            Effect::FocusNext => f.write_str("FocusNext"),
            Effect::FocusPrev => f.write_str("FocusPrev"),
        }
    }
}

/// The console keymap for an xkb layout: the layout's own name, else the first
/// keymap that extends it, else `us`, else the first the machine has.
fn pick_keymap(all: &[String], layout: &str) -> String {
    let matches = data::keymaps_for(all, layout);
    matches
        .iter()
        .find(|k| k.as_str() == layout)
        .or(matches.first())
        .copied()
        .or_else(|| all.iter().find(|k| k.as_str() == "us"))
        .or(all.first())
        .cloned()
        .unwrap_or_else(|| "us".to_owned())
}

/// The most layouts shown at once; the filter narrows the rest.
pub const MAX_LAYOUT_ROWS: usize = 60;
/// WPA passphrases are 8 to 63 characters; anything shorter cannot connect.
pub const WPA_MIN: usize = 8;

pub struct Model {
    pub step: Step,
    pub fake: bool,
    pub reduced_motion: bool,
    pub version: String,

    /// The current step's "More options" is open. Shut on every step change.
    pub more: bool,

    // 1
    pub language: usize,
    pub layouts: Vec<Layout>,
    pub layout: String,
    pub variant: Option<String>,
    /// Console keymaps the machine has, and the one chosen for the plan.
    pub keymaps: Vec<String>,
    pub keymap: String,
    pub kb_filter: String,
    pub kb_test: String,
    pub kb_live: KbLive,

    // 2
    pub zones: Vec<String>,
    pub zone_index: Vec<data::ZoneEntry>,
    pub zone: String,
    pub zone_query: String,
    pub zone_clock: Option<ZoneClock>,

    // 3
    pub net: Load<Snapshot>,
    pub ap: Option<String>,
    pub passphrase: Secret,
    pub joining: bool,
    pub join_failed: bool,

    // 4
    pub disks: Load<Vec<Disk>>,
    pub disk: Option<String>,

    // 5
    pub hostname: String,
    pub username: String,
    pub password: Secret,
    pub password2: Secret,

    // 6
    pub profile: Profile,

    // 7 to 12: seeded from the profile, then edited
    pub choices: Choices,

    // 13
    pub carry_network: bool,
    pub saved_wifi: bool,
    pub confirm: String,

    // 14
    pub install: Install,
}

impl fmt::Debug for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Fields that could matter to a bug report, none that could be a secret.
        f.debug_struct("Model")
            .field("step", &self.step)
            .field("disk", &self.disk)
            .field("phase", &self.install.phase)
            .finish_non_exhaustive()
    }
}

impl Model {
    pub fn new(inputs: Inputs) -> Model {
        let zone = inputs
            .default_zone
            .clone()
            .filter(|z| inputs.zones.contains(z))
            .unwrap_or_else(|| "UTC".to_owned());
        let language = 0;
        let layout = LANGUAGES[language].layout.to_owned();
        let keymap = pick_keymap(&inputs.keymaps, &layout);
        Model {
            step: Step::Welcome,
            fake: inputs.fake,
            reduced_motion: inputs.reduced_motion,
            version: inputs.version,
            language,
            layouts: inputs.layouts,
            layout,
            variant: None,
            keymaps: inputs.keymaps,
            keymap,
            kb_filter: String::new(),
            kb_test: String::new(),
            kb_live: KbLive::Idle,
            more: false,
            zone_query: String::new(),
            zone,
            zones: inputs.zones,
            zone_index: inputs.zone_index,
            zone_clock: None,
            net: Load::Idle,
            ap: None,
            passphrase: Secret::default(),
            joining: false,
            join_failed: false,
            disks: Load::Idle,
            disk: None,
            hostname: "eclipse".to_owned(),
            username: String::new(),
            password: Secret::default(),
            password2: Secret::default(),
            profile: Profile::Standard,
            choices: Choices::for_profile(Profile::Standard),
            carry_network: true,
            saved_wifi: inputs.saved_wifi,
            confirm: String::new(),
            install: Install::idle(),
        }
    }

    /// Whether Review offers to carry Wi-Fi: a network was set up here, or
    /// NetworkManager already had a saved Wi-Fi connection.
    pub fn wifi_to_carry(&self) -> bool {
        self.saved_wifi
            || self.ap.is_some()
            || matches!(&self.net, Load::Ready(s) if matches!(&s.link, crate::net::Link::Online { medium, .. } if medium == "wifi"))
    }

    /// The console keymaps that suit the chosen layout.
    pub fn keymap_choices(&self) -> Vec<&String> {
        data::keymaps_for(&self.keymaps, &self.layout)
    }

    // ---------------------------------------------------------------- reads

    pub fn locale(&self) -> &'static str {
        LANGUAGES[self.language.min(LANGUAGES.len() - 1)].locale
    }

    /// Places matching the search box, best first. Empty when nothing is typed.
    pub fn zone_results(&self) -> Vec<&data::ZoneEntry> {
        data::search_zones(&self.zone_index, &self.zone_query)
    }

    /// The chosen zone as the search index knows it.
    pub fn zone_entry(&self) -> Option<&data::ZoneEntry> {
        self.zone_index.iter().find(|e| e.zone == self.zone)
    }

    pub fn current_layout(&self) -> Option<&Layout> {
        self.layouts.iter().find(|l| l.code == self.layout)
    }

    pub fn visible_layouts(&self) -> Vec<&Layout> {
        let q = self.kb_filter.trim().to_lowercase();
        self.layouts
            .iter()
            .filter(|l| {
                q.is_empty() || l.code.to_lowercase().contains(&q) || l.name.to_lowercase().contains(&q)
            })
            .take(MAX_LAYOUT_ROWS)
            .collect()
    }

    pub fn selected_disk(&self) -> Option<&Disk> {
        let id = self.disk.as_deref()?;
        match &self.disks {
            Load::Ready(v) => v.iter().find(|d| d.by_id == id),
            _ => None,
        }
    }

    pub fn ap_choice(&self) -> Option<&crate::net::Ap> {
        let ssid = self.ap.as_deref()?;
        match &self.net {
            Load::Ready(s) => s.aps.iter().find(|a| a.ssid == ssid),
            _ => None,
        }
    }

    pub fn online(&self) -> bool {
        matches!(&self.net, Load::Ready(s) if s.link.is_online())
    }

    pub fn can_join(&self) -> bool {
        match self.ap_choice() {
            Some(ap) => {
                !self.joining
                    && crate::net::ssid_ok(&ap.ssid)
                    && (!ap.secure || self.passphrase.len() >= WPA_MIN)
            }
            None => false,
        }
    }

    pub fn hostname_ok(&self) -> bool {
        valid_hostname(&self.hostname)
    }

    pub fn username_ok(&self) -> bool {
        valid_username(&self.username)
    }

    pub fn password_ok(&self) -> bool {
        check_password(self.password.as_str())
    }

    /// Characters typed so far, as the minimum counts them.
    pub fn password_chars(&self) -> usize {
        self.password.as_str().chars().count()
    }

    pub fn passwords_match(&self) -> bool {
        !self.password2.is_empty() && self.password.as_str() == self.password2.as_str()
    }

    /// The first thing wrong with the identity form, in a fixed sentence that
    /// never quotes what was typed.
    pub fn identity_problem(&self) -> Option<&'static str> {
        if !self.hostname_ok() {
            Some("Host names use lowercase letters, digits and hyphens, up to 63 characters.")
        } else if !self.username_ok() {
            Some("User names start with a lowercase letter, and cannot be root.")
        } else if self.password_chars() < MIN_PASSWORD_CHARS {
            Some("The password needs at least 8 characters.")
        } else if !self.password_ok() {
            Some("The password cannot contain line breaks.")
        } else if !self.passwords_match() {
            Some("The two passwords are not the same.")
        } else {
            None
        }
    }

    /// The disk's own name, typed again: the whole gate on Apply.
    pub fn confirmed(&self) -> bool {
        matches!(self.selected_disk(), Some(d) if !self.confirm.is_empty() && self.confirm == d.by_id)
    }

    pub fn can_apply(&self) -> bool {
        self.step == Step::Review
            && self.install.phase == Phase::Idle
            && self.confirmed()
            && self.identity_problem().is_none()
            && data::selectable(self.profile)
            && data::valid_zone(&self.zone)
            && self.zones.contains(&self.zone)
    }

    pub fn can_next(&self) -> bool {
        match self.step {
            Step::Welcome | Step::Review | Step::Install => false,
            Step::Language => true,
            Step::Keyboard => self.kb_live != KbLive::Refused,
            Step::Timezone => self.zones.contains(&self.zone),
            Step::Network => !self.joining,
            Step::Disk => self.selected_disk().is_some(),
            Step::Identity => self.identity_problem().is_none(),
            Step::Profile => data::selectable(self.profile),
            Step::Mode | Step::Layout | Step::Components | Step::Appearance | Step::Apps => true,
        }
    }

    pub fn can_back(&self) -> bool {
        match self.step {
            Step::Welcome | Step::Language => false,
            Step::Install => matches!(self.install.phase, Phase::Failed),
            _ => true,
        }
    }

    // --------------------------------------------------------------- update

    pub fn update(&mut self, message: Message) -> Vec<Effect> {
        let mut fx = Vec::new();
        match message {
            Message::Noop => {}
            Message::Begin => {
                if self.step == Step::Welcome {
                    self.enter(Step::Language, &mut fx);
                }
            }
            Message::Next => {
                if self.can_next() {
                    if let Some(s) = self.step.next() {
                        self.enter(s, &mut fx);
                    }
                } else {
                    self.surface_hostname(&mut fx);
                }
            }
            Message::Back => self.back(&mut fx),
            Message::Key(k) => self.key(k, &mut fx),

            // A hidden field that blocks Continue is never left hidden.
            Message::More(open) => {
                self.more = open || (self.step == Step::Identity && !self.hostname_ok());
            }
            Message::Language(i) => self.set_language(i, &mut fx),
            Message::Layout(code) => {
                if self.layouts.iter().any(|l| l.code == code) {
                    self.layout = code;
                    self.variant = None;
                    self.keymap = pick_keymap(&self.keymaps, &self.layout);
                    self.write_keyboard(&mut fx);
                }
            }
            Message::Keymap(k) => {
                if self.keymaps.contains(&k) {
                    self.keymap = k;
                }
            }
            Message::CarryNetwork(on) => self.carry_network = on,
            Message::Variant(v) => {
                let known = match (&v, self.current_layout()) {
                    (None, _) => true,
                    (Some(code), Some(l)) => l.variants.iter().any(|x| &x.code == code),
                    (Some(_), None) => false,
                };
                if known {
                    self.variant = v;
                    self.write_keyboard(&mut fx);
                }
            }
            Message::KbFilter(s) => self.kb_filter = s,
            Message::KbTest(s) => self.kb_test = s,
            Message::ConfigResult(key, result) => {
                if key == config::KB_LAYOUT || key == config::KB_VARIANT {
                    self.kb_live = match result {
                        Ok(()) if self.kb_live != KbLive::Refused || key == config::KB_LAYOUT => KbLive::Live,
                        Ok(()) => KbLive::Refused,
                        Err(WriteError::NoSocket) => KbLive::Offline,
                        Err(_) => KbLive::Refused,
                    };
                }
            }

            Message::ZoneQuery(q) => {
                self.zone_query = q;
                // Typing a place picks the best match, so Enter takes it. A
                // choice that is still among the results is left where it is.
                let pick = {
                    let results = self.zone_results();
                    match results.first() {
                        Some(first) if !results.iter().any(|e| e.zone == self.zone) => {
                            Some(first.zone.clone())
                        }
                        _ => None,
                    }
                };
                if let Some(z) = pick {
                    self.set_zone(z, &mut fx);
                }
            }
            Message::Zone(z) => self.set_zone(z, &mut fx),
            Message::ZoneClock(zone, clock) => {
                if zone == self.zone {
                    self.zone_clock = clock;
                }
            }

            Message::NetScanned(s) => {
                // A join in flight is not interrupted by a rescan result.
                if let Some(ssid) = &self.ap {
                    if !s.aps.iter().any(|a| &a.ssid == ssid) {
                        self.ap = None;
                        self.passphrase.clear();
                    }
                }
                self.net = Load::Ready(s);
            }
            Message::Rescan => {
                if !self.joining {
                    self.net = Load::Loading;
                    fx.push(Effect::NetScan);
                }
            }
            Message::SelectAp(ssid) => {
                let exists = matches!(&self.net, Load::Ready(s) if s.aps.iter().any(|a| a.ssid == ssid));
                if exists && !self.joining {
                    self.ap = Some(ssid);
                    self.passphrase.clear();
                    self.join_failed = false;
                    if self.ap_choice().is_some_and(|a| a.secure) {
                        fx.push(Effect::Focus(ids::PASSPHRASE));
                    }
                }
            }
            Message::Passphrase(s) => {
                self.passphrase = s;
                self.join_failed = false;
            }
            Message::Join => {
                if self.can_join() {
                    if let Some(ap) = self.ap_choice().cloned() {
                        self.joining = true;
                        self.join_failed = false;
                        fx.push(Effect::NetJoin {
                            ssid: ap.ssid,
                            secure: ap.secure,
                            passphrase: self.passphrase.take(),
                        });
                    }
                }
            }
            Message::Joined(ok) => {
                self.joining = false;
                self.passphrase.clear();
                if ok {
                    self.ap = None;
                    self.join_failed = false;
                    self.net = Load::Loading;
                    fx.push(Effect::NetScan);
                } else {
                    self.join_failed = true;
                }
            }

            Message::DisksLoaded(r) => match r {
                Ok(v) => {
                    if !self
                        .disk
                        .as_ref()
                        .is_some_and(|id| v.iter().any(|d| &d.by_id == id))
                    {
                        self.disk = None;
                        self.confirm.clear();
                    }
                    self.disks = Load::Ready(v);
                }
                Err(e) => self.disks = Load::Failed(e),
            },
            Message::ReloadDisks => {
                self.disks = Load::Loading;
                fx.push(Effect::LoadDisks);
            }
            Message::SelectDisk(id) => {
                if matches!(&self.disks, Load::Ready(v) if v.iter().any(|d| d.by_id == id)) {
                    if self.disk.as_deref() != Some(id.as_str()) {
                        self.confirm.clear();
                    }
                    self.disk = Some(id);
                }
            }

            Message::Hostname(s) => {
                self.hostname = s;
                if self.step == Step::Identity && !self.hostname_ok() {
                    self.more = true;
                }
            }
            Message::Username(s) => self.username = s,
            Message::Password(s) => self.password = s,
            Message::Password2(s) => self.password2 = s,

            Message::Profile(p) => {
                // Only Standard exists in this version (D-07 §4, step 6).
                if data::selectable(p) {
                    self.set_profile(p, &mut fx);
                }
            }
            Message::SetMode(v) => {
                self.choices.mode = v;
                self.write(choices::MODE, self.choices.mode_value(), &mut fx);
            }
            Message::SetTiling(v) => {
                self.choices.tiling = v;
                self.write(choices::LAYOUT, self.choices.tiling_value(), &mut fx);
            }
            Message::SetSlot(slot, id) => {
                if choices::SLOTS.get(slot).is_some_and(|s| s.choices.contains(&id)) {
                    self.choices.slots[slot] = id;
                    self.write(choices::SLOTS[slot].key, self.choices.slot_value(slot), &mut fx);
                }
            }
            Message::SetRounded(v) => {
                self.choices.rounded = v;
                self.write(choices::ROUNDING, self.choices.rounding_value(), &mut fx);
            }
            Message::SetBlur(v) => {
                self.choices.blur = v;
                self.write(choices::BLUR, self.choices.blur_value(), &mut fx);
            }
            Message::SetAnimations(v) => {
                self.choices.animations = v;
                self.write(choices::ANIMATIONS, self.choices.animations_value(), &mut fx);
            }
            Message::SetBarPosition(v) => {
                self.choices.bar_position = v;
                self.write(choices::BAR_POSITION, self.choices.bar_position_value(), &mut fx);
            }
            Message::ToggleApp(i) => {
                // Staged until Apply: an application is a package, not a setting.
                if let Some(on) = self.choices.apps.get_mut(i) {
                    *on = !*on;
                }
            }

            Message::Confirm(s) => self.confirm = s,
            Message::Apply => self.apply(&mut fx),

            Message::Helper(ev) => self.helper(ev),
            Message::Restart => {
                if self.install.phase == Phase::Done && !self.install.restarting {
                    self.install.restarting = true;
                    self.install.restart_error = None;
                    fx.push(Effect::Reboot);
                }
            }
            Message::Restarted(r) => {
                self.install.restarting = false;
                if let Err(e) = r {
                    self.install.restart_error = Some(e);
                }
            }
        }
        fx
    }

    fn enter(&mut self, step: Step, fx: &mut Vec<Effect>) {
        self.step = step;
        self.more = false;
        match step {
            Step::Welcome
            | Step::Language
            | Step::Profile
            | Step::Mode
            | Step::Layout
            | Step::Components
            | Step::Appearance
            | Step::Apps
            | Step::Install => {}
            Step::Keyboard => fx.push(Effect::Focus(ids::KB_TEST)),
            Step::Timezone => {
                fx.push(Effect::ReadClock(self.zone.clone()));
                fx.push(Effect::Focus(ids::ZONE_SEARCH));
            }
            Step::Network => {
                if matches!(self.net, Load::Idle | Load::Failed(_)) {
                    self.net = Load::Loading;
                    fx.push(Effect::NetScan);
                }
            }
            Step::Disk => {
                if matches!(self.disks, Load::Idle | Load::Failed(_)) {
                    self.disks = Load::Loading;
                    fx.push(Effect::LoadDisks);
                }
            }
            Step::Identity => fx.push(Effect::Focus(ids::USERNAME)),
            Step::Review => fx.push(Effect::Focus(ids::CONFIRM)),
        }
    }

    fn back(&mut self, fx: &mut Vec<Effect>) {
        if !self.can_back() {
            return;
        }
        if self.step == Step::Install {
            // The password was cleared when it was sent. Going back for another
            // attempt means typing it again, so land where it is asked.
            self.install = Install::idle();
            self.confirm.clear();
            self.enter(Step::Identity, fx);
        } else if let Some(p) = self.step.prev() {
            self.enter(p, fx);
        }
    }

    fn key(&mut self, key: Key, fx: &mut Vec<Effect>) {
        match key {
            Key::Tab => fx.push(Effect::FocusNext),
            Key::BackTab => fx.push(Effect::FocusPrev),
            Key::Escape => self.back(fx),
            Key::Enter => match self.step {
                Step::Review => self.apply(fx),
                Step::Install => {
                    if self.install.phase == Phase::Done && !self.install.restarting {
                        self.install.restarting = true;
                        self.install.restart_error = None;
                        fx.push(Effect::Reboot);
                    }
                }
                _ => {
                    if self.can_next() {
                        if let Some(s) = self.step.next() {
                            self.enter(s, fx);
                        }
                    } else {
                        self.surface_hostname(fx);
                    }
                }
            },
            Key::Up | Key::Down if self.step == Step::Review && self.wifi_to_carry() => {
                self.carry_network = !self.carry_network;
            }
            Key::Up => self.step_selection(-1, fx),
            Key::Down => self.step_selection(1, fx),
        }
    }

    /// On the identity step, an invalid host name is the first problem and
    /// lives under More options: open them and put the cursor there.
    fn surface_hostname(&mut self, fx: &mut Vec<Effect>) {
        if self.step == Step::Identity && !self.hostname_ok() {
            self.more = true;
            fx.push(Effect::Focus(ids::HOSTNAME));
        }
    }

    /// Arrow keys move the current step's selection.
    fn step_selection(&mut self, delta: i32, fx: &mut Vec<Effect>) {
        fn shift(len: usize, current: Option<usize>, delta: i32) -> Option<usize> {
            if len == 0 {
                return None;
            }
            Some(match current {
                None => 0,
                Some(i) => (i as i64 + delta as i64).clamp(0, len as i64 - 1) as usize,
            })
        }
        match self.step {
            Step::Language => {
                if let Some(i) = shift(LANGUAGES.len(), Some(self.language), delta) {
                    self.set_language(i, fx);
                }
            }
            Step::Keyboard => {
                // The list is not on screen under More options.
                if self.more {
                    return;
                }
                let visible: Vec<String> = self.visible_layouts().iter().map(|l| l.code.clone()).collect();
                let target = match visible.iter().position(|c| *c == self.layout) {
                    Some(i) => shift(visible.len(), Some(i), delta).map(|i| visible[i].clone()),
                    // The filter hides the current layout: one row from it in
                    // the whole list, not a jump to the top of the filtered one.
                    None => {
                        let cur = self.layouts.iter().position(|l| l.code == self.layout);
                        shift(self.layouts.len(), cur, delta).map(|i| self.layouts[i].code.clone())
                    }
                };
                if let Some(code) = target {
                    fx.extend(self.update(Message::Layout(code)));
                }
            }
            Step::Mode => {
                let cur = Mode::ALL.iter().position(|m| *m == self.choices.mode);
                if let Some(i) = shift(Mode::ALL.len(), cur, delta) {
                    fx.extend(self.update(Message::SetMode(Mode::ALL[i])));
                }
            }
            Step::Layout => {
                let cur = Tiling::ALL.iter().position(|t| *t == self.choices.tiling);
                if let Some(i) = shift(Tiling::ALL.len(), cur, delta) {
                    fx.extend(self.update(Message::SetTiling(Tiling::ALL[i])));
                }
            }
            Step::Timezone => {
                let zones: Vec<String> = self.zone_results().iter().map(|e| e.zone.clone()).collect();
                let cur = zones.iter().position(|z| *z == self.zone);
                if let Some(i) = shift(zones.len(), cur, delta) {
                    self.set_zone(zones[i].clone(), fx);
                }
            }
            Step::Network => {
                let target = match &self.net {
                    Load::Ready(s) => {
                        let cur = self
                            .ap
                            .as_ref()
                            .and_then(|a| s.aps.iter().position(|x| &x.ssid == a));
                        shift(s.aps.len(), cur, delta).map(|i| s.aps[i].ssid.clone())
                    }
                    _ => None,
                };
                if let Some(ssid) = target {
                    let _ = self
                        .update(Message::SelectAp(ssid))
                        .into_iter()
                        .map(|e| fx.push(e))
                        .count();
                }
            }
            Step::Disk => {
                let target = match &self.disks {
                    Load::Ready(v) => {
                        let cur = self
                            .disk
                            .as_ref()
                            .and_then(|a| v.iter().position(|x| &x.by_id == a));
                        shift(v.len(), cur, delta).map(|i| v[i].by_id.clone())
                    }
                    _ => None,
                };
                if let Some(id) = target {
                    let _ = self.update(Message::SelectDisk(id));
                }
            }
            _ => {}
        }
    }

    fn set_language(&mut self, i: usize, fx: &mut Vec<Effect>) {
        if i >= LANGUAGES.len() {
            return;
        }
        self.language = i;
        let want = LANGUAGES[i].layout;
        if self.layouts.iter().any(|l| l.code == want) {
            self.layout = want.to_owned();
            self.variant = None;
            self.keymap = pick_keymap(&self.keymaps, &self.layout);
        }
        self.write_keyboard(fx);
    }

    fn write_keyboard(&mut self, fx: &mut Vec<Effect>) {
        self.kb_live = KbLive::Idle;
        fx.push(Effect::Config {
            key: config::KB_LAYOUT,
            value: json!(self.layout),
        });
        fx.push(Effect::Config {
            key: config::KB_VARIANT,
            value: json!(self.variant.clone().unwrap_or_default()),
        });
    }

    /// One live write of a seed key, as the user leaves a value (D-07 §4).
    fn write(&self, key: &'static str, value: Value, fx: &mut Vec<Effect>) {
        fx.push(Effect::Config { key, value });
    }

    /// Picking a profile preselects every later step (COMP-17 §2.1). Picking the
    /// one already chosen keeps what the user has changed since.
    fn set_profile(&mut self, p: Profile, fx: &mut Vec<Effect>) {
        if p == self.profile {
            return;
        }
        self.profile = p;
        self.choices = Choices::for_profile(p);
        for (key, value) in self.choices.seeds() {
            self.write(key, value, fx);
        }
    }

    fn set_zone(&mut self, zone: String, fx: &mut Vec<Effect>) {
        if self.zones.contains(&zone) && data::valid_zone(&zone) {
            self.zone_clock = None;
            fx.push(Effect::ReadClock(zone.clone()));
            self.zone = zone;
        }
    }

    fn apply(&mut self, fx: &mut Vec<Effect>) {
        if !self.can_apply() {
            return;
        }
        let Some(disk) = self.selected_disk().map(|d| d.by_id.clone()) else {
            return;
        };
        if !valid_by_id(&disk) {
            return;
        }
        let plan = Plan {
            disk_by_id: disk,
            hostname: self.hostname.clone(),
            username: self.username.clone(),
            locale: self.locale().to_owned(),
            timezone: self.zone.clone(),
            keymap: self.keymap.clone(),
            carry_network: self.carry_network,
            profile: self.profile,
            candidates: self.choices.candidates(),
            // D-07 §4.4 "Stack": no step-12 toggle yet, so the profile decides.
            agents: self.profile.agents_default(),
        };
        // The password leaves the model here and nowhere else. Both buffers are
        // emptied, whether or not the helper ever answers.
        let password = self.password.take();
        self.password2.clear();
        self.passphrase.clear();
        let request = Request { plan, password };

        // The live file may never have been touched by a step that kept its
        // profile default, and the helper copies that file: write the whole
        // seed so what is copied is what the review showed.
        for (key, value) in self.choices.seeds() {
            self.write(key, value, fx);
        }
        fx.push(Effect::Config {
            key: "setup.profile",
            value: json!("standard"),
        });
        fx.push(Effect::Config {
            key: "setup.complete",
            value: json!(true),
        });
        fx.push(Effect::Apply(request));

        self.step = Step::Install;
        self.install = Install {
            phase: Phase::Running,
            ..Install::idle()
        };
    }

    fn helper(&mut self, ev: HelperEvent) {
        if self.install.phase != Phase::Running {
            return;
        }
        match ev {
            HelperEvent::Progress(Progress {
                stage,
                pct,
                msg,
                failed,
            }) => {
                self.install.pct = pct.min(100);
                self.install.stage = Some(stage);
                self.install.note = msg;
                if failed {
                    self.install.phase = Phase::Failed;
                    self.install.failed_at = Some(stage);
                } else if stage == Stage::Done {
                    self.install.phase = Phase::Done;
                    self.install.pct = 100;
                }
            }
            HelperEvent::Ended { clean } => {
                // The stream closed without a verdict: a denied authorisation
                // prompt, a crash, a missing helper. All read the same here,
                // and nothing the helper wrote to stderr is shown.
                if !clean || self.install.phase == Phase::Running {
                    self.install.phase = Phase::Failed;
                    self.install.failed_at = self.install.stage.or(Some(Stage::Validate));
                    self.install.note = "the installer stopped before it finished".to_owned();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{fake_snapshot, Link};

    fn model() -> Model {
        Model::new(Inputs::builtin(true))
    }

    /// Advance to `step` by the honest route, with fixture data.
    fn at(step: Step) -> Model {
        let mut m = model();
        m.update(Message::Begin);
        for target in [
            Step::Keyboard,
            Step::Timezone,
            Step::Network,
            Step::Disk,
            Step::Identity,
            Step::Profile,
            Step::Mode,
            Step::Layout,
            Step::Components,
            Step::Appearance,
            Step::Apps,
            Step::Review,
        ] {
            if m.step == step {
                break;
            }
            match m.step {
                Step::Network => {
                    m.update(Message::NetScanned(fake_snapshot(true)));
                }
                Step::Disk => {
                    m.update(Message::DisksLoaded(Ok(crate::helper::fake_disks())));
                    let id = crate::helper::fake_disks()[1].by_id.clone();
                    m.update(Message::SelectDisk(id));
                }
                Step::Identity => fill_identity(&mut m),
                _ => {}
            }
            m.update(Message::Next);
            assert_eq!(m.step, target, "could not reach {target:?}");
        }
        assert_eq!(m.step, step);
        m
    }

    fn fill_identity(m: &mut Model) {
        m.update(Message::Username("chase".into()));
        m.update(Message::Password(Secret::new("correct horse".into())));
        m.update(Message::Password2(Secret::new("correct horse".into())));
    }

    fn review_ready() -> Model {
        let mut m = at(Step::Review);
        let id = m.disk.clone().unwrap();
        m.update(Message::Confirm(id));
        m
    }

    #[test]
    fn steps_are_d07s_and_in_order() {
        let numbers: Vec<u8> = Step::ALL.iter().map(|s| s.number()).collect();
        assert_eq!(numbers, [0, 1, 1, 2, 3, 4, 5, 6, 7, 10, 8, 10, 9, 13, 14]);
        assert_eq!(Step::Welcome.prev(), None);
        assert_eq!(Step::Install.next(), None);
        for w in Step::ALL.windows(2) {
            assert_eq!(w[0].next(), Some(w[1]));
            assert_eq!(w[1].prev(), Some(w[0]));
        }
    }

    #[test]
    fn welcome_hands_off_to_language_only_from_step_zero() {
        let mut m = model();
        assert_eq!(m.step, Step::Welcome);
        assert!(!m.can_next(), "step 0 advances only on the welcome hand-off");
        m.update(Message::Next);
        assert_eq!(m.step, Step::Welcome);
        m.update(Message::Begin);
        assert_eq!(m.step, Step::Language);
        m.update(Message::Begin);
        assert_eq!(m.step, Step::Language, "a second Begin does nothing");
    }

    #[test]
    fn you_cannot_go_back_into_the_animation() {
        let mut m = at(Step::Language);
        assert!(!m.can_back());
        m.update(Message::Back);
        assert_eq!(m.step, Step::Language);
        m.update(Message::Key(Key::Escape));
        assert_eq!(m.step, Step::Language);
    }

    #[test]
    fn next_and_back_walk_the_whole_flow_and_undo_it() {
        let mut m = at(Step::Review);
        for expected in [
            Step::Apps,
            Step::Appearance,
            Step::Components,
            Step::Layout,
            Step::Mode,
            Step::Profile,
            Step::Identity,
            Step::Disk,
            Step::Network,
            Step::Timezone,
            Step::Keyboard,
            Step::Language,
        ] {
            m.update(Message::Back);
            assert_eq!(m.step, expected);
        }
    }

    #[test]
    fn entering_a_step_asks_for_what_it_needs_once() {
        let mut m = at(Step::Language);
        m.update(Message::Next);
        assert_eq!(m.step, Step::Keyboard);
        let fx = m.update(Message::Next);
        assert!(matches!(
            fx.as_slice(),
            [Effect::ReadClock(z), Effect::Focus(ids::ZONE_SEARCH)] if z == "Europe/Berlin"
        ));
        let fx = m.update(Message::Next);
        assert!(matches!(fx.as_slice(), [Effect::NetScan]));
        assert_eq!(m.net, Load::Loading);
        m.update(Message::NetScanned(fake_snapshot(false)));
        // Going back and forward again does not rescan a step that has answered.
        m.update(Message::Back);
        let fx = m.update(Message::Next);
        assert!(fx.is_empty());
    }

    #[test]
    fn network_is_skippable_even_offline_and_says_so() {
        let mut m = at(Step::Network);
        m.update(Message::NetScanned(Snapshot {
            link: Link::Offline,
            aps: vec![],
        }));
        assert!(!m.online());
        assert!(m.can_next(), "step 3 is skippable");
        m.update(Message::Next);
        assert_eq!(m.step, Step::Disk);
    }

    #[test]
    fn disk_needs_a_choice_from_the_helpers_list_and_never_a_free_path() {
        let mut m = at(Step::Disk);
        assert!(!m.can_next());
        m.update(Message::DisksLoaded(Ok(crate::helper::fake_disks())));
        assert!(
            !m.can_next(),
            "nothing is preselected: erasing must be deliberate"
        );
        m.update(Message::SelectDisk("/dev/sda".into()));
        assert_eq!(m.disk, None);
        m.update(Message::SelectDisk("../sda".into()));
        assert_eq!(m.disk, None);
        let id = crate::helper::fake_disks()[0].by_id.clone();
        m.update(Message::SelectDisk(id.clone()));
        assert_eq!(m.disk.as_deref(), Some(id.as_str()));
        assert!(m.can_next());
    }

    #[test]
    fn arrow_keys_move_the_disk_choice() {
        let mut m = at(Step::Disk);
        m.update(Message::DisksLoaded(Ok(crate::helper::fake_disks())));
        m.update(Message::Key(Key::Down));
        assert_eq!(m.disk, Some(crate::helper::fake_disks()[0].by_id.clone()));
        m.update(Message::Key(Key::Down));
        m.update(Message::Key(Key::Down));
        assert_eq!(m.disk, Some(crate::helper::fake_disks()[1].by_id.clone()));
        m.update(Message::Key(Key::Up));
        assert_eq!(m.disk, Some(crate::helper::fake_disks()[0].by_id.clone()));
    }

    #[test]
    fn a_failed_disk_listing_can_be_retried() {
        let mut m = at(Step::Disk);
        m.update(Message::DisksLoaded(Err(
            "the install helper is not available".into()
        )));
        assert!(matches!(m.disks, Load::Failed(_)));
        let fx = m.update(Message::ReloadDisks);
        assert!(matches!(fx.as_slice(), [Effect::LoadDisks]));
    }

    #[test]
    fn a_disk_that_vanishes_from_the_listing_is_dropped_from_the_choice() {
        let mut m = at(Step::Disk);
        m.update(Message::DisksLoaded(Ok(crate::helper::fake_disks())));
        m.update(Message::SelectDisk(crate::helper::fake_disks()[0].by_id.clone()));
        m.update(Message::Confirm("x".into()));
        m.update(Message::DisksLoaded(Ok(vec![
            crate::helper::fake_disks()[1].clone()
        ])));
        assert_eq!(m.disk, None);
        assert!(m.confirm.is_empty());
    }

    #[test]
    fn identity_gates_on_the_helpers_own_patterns_and_matching_passwords() {
        let mut m = at(Step::Identity);
        assert!(!m.can_next());
        for bad in ["root", "Chase", "1a", ""] {
            m.update(Message::Username(bad.into()));
            assert!(m.identity_problem().is_some(), "{bad:?}");
        }
        m.update(Message::Username("chase".into()));
        m.update(Message::Password(Secret::new("hunter22".into())));
        m.update(Message::Password2(Secret::new("hunter33".into())));
        assert_eq!(m.identity_problem(), Some("The two passwords are not the same."));
        assert!(!m.can_next());
        m.update(Message::Password2(Secret::new("hunter22".into())));
        assert!(m.can_next());
        m.update(Message::Hostname("Bad Host".into()));
        assert!(!m.can_next());
        m.update(Message::Hostname("eclipse-1".into()));
        assert!(m.can_next());
        m.update(Message::Password(Secret::new("a\nroot:x".into())));
        m.update(Message::Password2(Secret::new("a\nroot:x".into())));
        assert!(
            !m.can_next(),
            "a newline in a password is refused before it can be sent"
        );
    }

    #[test]
    fn problems_never_quote_the_input() {
        let mut m = at(Step::Identity);
        m.update(Message::Username("SECRETNAME".into()));
        assert!(!m.identity_problem().unwrap().contains("SECRETNAME"));
    }

    #[test]
    fn only_standard_is_selectable_and_the_default() {
        let mut m = at(Step::Profile);
        assert_eq!(m.profile, Profile::Standard);
        for p in [Profile::Minimal, Profile::Full, Profile::Agentic] {
            m.update(Message::Profile(p));
            assert_eq!(m.profile, Profile::Standard, "{p:?} must not be selectable");
        }
        m.update(Message::Profile(Profile::Standard));
        assert!(m.can_next());
    }

    /// The `Config` effects among `fx`, as (key, value).
    fn writes(fx: &[Effect]) -> Vec<(&'static str, Value)> {
        fx.iter()
            .filter_map(|e| match e {
                Effect::Config { key, value } => Some((*key, value.clone())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_choice_steps_start_from_the_profile_and_standard_is_continue_continue() {
        let mut m = at(Step::Profile);
        assert_eq!(m.choices, Choices::for_profile(Profile::Standard));
        for step in [
            Step::Mode,
            Step::Layout,
            Step::Components,
            Step::Appearance,
            Step::Apps,
            Step::Review,
        ] {
            assert!(m.can_next() || step == Step::Review);
            m.update(Message::Next);
            assert_eq!(m.step, step);
        }
        assert_eq!(m.choices.mode, Mode::Hybrid);
        assert_eq!(m.choices.tiling, Tiling::Radiant);
    }

    #[test]
    fn choosing_a_profile_reseeds_every_later_step_and_rewrites_the_seed() {
        for p in [Profile::Minimal, Profile::Full, Profile::Agentic] {
            let mut m = at(Step::Profile);
            m.choices.mode = Mode::De;
            m.choices.apps[3] = true;
            let mut fx = Vec::new();
            m.set_profile(p, &mut fx);
            assert_eq!(m.choices, Choices::for_profile(p), "{p:?}");
            let keys: Vec<&str> = writes(&fx).into_iter().map(|(k, _)| k).collect();
            assert_eq!(keys, choices::SEED_KEYS, "{p:?}");

            // Picking the same one again keeps what was changed since.
            m.choices.blur = Blur::Off;
            let mut fx = Vec::new();
            m.set_profile(p, &mut fx);
            assert!(fx.is_empty() && m.choices.blur == Blur::Off);
        }
    }

    #[test]
    fn each_choice_changes_the_model_writes_its_key_and_reaches_the_plan() {
        let mut m = review_ready();
        let mut seen = Vec::new();
        for msg in [
            Message::SetMode(Mode::De),
            Message::SetTiling(Tiling::Dwindle),
            Message::SetSlot(0, "waybar"),
            Message::SetSlot(1, "fuzzel"),
            Message::SetSlot(2, "none"),
            Message::SetSlot(3, "none"),
            Message::SetRounded(false),
            Message::SetBlur(Blur::Glass),
            Message::SetAnimations(true),
            Message::SetBarPosition(BarPosition::Bottom),
            Message::ToggleApp(0),
            Message::ToggleApp(7),
        ] {
            seen.extend(writes(&m.update(msg)));
        }
        assert_eq!(
            seen,
            [
                ("mode", json!("de")),
                ("general.layout", json!("dwindle")),
                ("components.bar", json!("waybar")),
                ("components.launcher", json!("fuzzel")),
                ("components.notifications", json!("none")),
                ("components.control-center", json!("none")),
                ("decoration.rounding", json!(0)),
                ("decoration.blur.mode", json!("glass")),
                ("animations.enabled", json!(true)),
                ("bar.position", json!("bottom")),
            ],
            "applications are packages, so toggling one writes nothing"
        );
        // A choice the slot does not offer is refused.
        assert!(m.update(Message::SetSlot(3, "waybar")).is_empty());
        assert!(m.update(Message::SetSlot(9, "waybar")).is_empty());
        assert_eq!(m.choices.slots[3], "none");

        let fx = m.update(Message::Apply);
        let plan = fx
            .iter()
            .find_map(|e| match e {
                Effect::Apply(r) => Some(r.plan.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            plan.candidates,
            ["waybar", "fuzzel", "app-browser", "app-printing"]
        );
        // The final seed carries every change.
        let last: std::collections::HashMap<_, _> = writes(&fx).into_iter().collect();
        assert_eq!(last["mode"], json!("de"));
        assert_eq!(last["bar.position"], json!("bottom"));
        assert_eq!(last["components.bar"], json!("waybar"));
    }

    #[test]
    fn arrows_move_the_mode_and_layout_selection() {
        let mut m = at(Step::Mode);
        m.update(Message::Key(Key::Down));
        assert_eq!(m.choices.mode, Mode::De);
        m.update(Message::Key(Key::Up));
        m.update(Message::Key(Key::Up));
        assert_eq!(m.choices.mode, Mode::Wm);
        let mut m = at(Step::Layout);
        m.update(Message::Key(Key::Down));
        assert_eq!(m.choices.tiling, Tiling::Dwindle);
    }

    #[test]
    fn apply_is_disabled_until_the_disks_name_is_typed_again() {
        let mut m = at(Step::Review);
        assert!(!m.can_apply());
        assert!(m.update(Message::Apply).is_empty());
        assert_eq!(m.step, Step::Review);

        let id = m.disk.clone().unwrap();
        // A prefix, a different case, a trailing space and the other disk's name
        // do not count.
        for wrong in [
            id[..id.len() - 1].to_owned(),
            id.to_uppercase(),
            format!("{id} "),
            crate::helper::fake_disks()[0].by_id.clone(),
        ] {
            m.update(Message::Confirm(wrong));
            assert!(!m.can_apply());
            assert!(m.update(Message::Apply).is_empty());
            assert!(m.update(Message::Key(Key::Enter)).is_empty());
            assert_eq!(m.step, Step::Review);
        }

        m.update(Message::Confirm(id));
        assert!(m.can_apply());
    }

    #[test]
    fn changing_the_disk_clears_the_typed_confirmation() {
        let mut m = review_ready();
        assert!(m.confirmed());
        for _ in 0..8 {
            m.update(Message::Back);
        }
        assert_eq!(m.step, Step::Disk);
        m.update(Message::SelectDisk(crate::helper::fake_disks()[0].by_id.clone()));
        assert!(m.confirm.is_empty());
    }

    #[test]
    fn apply_sends_one_request_and_empties_every_password_buffer() {
        let mut m = review_ready();
        let fx = m.update(Message::Apply);
        assert_eq!(m.step, Step::Install);
        assert_eq!(m.install.phase, Phase::Running);
        assert!(
            m.password.is_empty(),
            "the password buffer must be cleared after send"
        );
        assert!(m.password2.is_empty());
        assert!(m.passphrase.is_empty());

        let requests: Vec<&Request> = fx
            .iter()
            .filter_map(|e| match e {
                Effect::Apply(r) => Some(r),
                _ => None,
            })
            .collect();
        assert_eq!(requests.len(), 1);
        let r = requests[0];
        assert_eq!(r.password.as_str(), "correct horse");
        assert_eq!(r.plan.disk_by_id, crate::helper::fake_disks()[1].by_id);
        assert_eq!(r.plan.profile, Profile::Standard);
        assert_eq!(r.plan.locale, "en_US.UTF-8");
        assert_eq!(r.plan.timezone, "Europe/Berlin");
        assert_eq!(r.plan.username, "chase");
        // The plan carries catalog ids, never a package or a unit.
        assert_eq!(
            r.plan.candidates,
            ["ec-hyperion-bar", "ec-launcher", "ec-toasts", "ec-center"]
        );

        // The config writes come first, and only the wizard's own keys: the
        // whole seed, then the two `setup.` records.
        let keys: Vec<&str> = fx
            .iter()
            .filter_map(|e| match e {
                Effect::Config { key, .. } => Some(*key),
                _ => None,
            })
            .collect();
        let mut want: Vec<&str> = choices::SEED_KEYS.to_vec();
        want.extend(["setup.profile", "setup.complete"]);
        assert_eq!(keys, want);
        assert!(matches!(fx.last(), Some(Effect::Apply(_))));

        // A second Apply does nothing: nothing to send, and not in Review.
        assert!(m.update(Message::Apply).is_empty());
    }

    #[test]
    fn nothing_printable_from_the_model_or_the_effects_shows_a_secret() {
        let mut m = review_ready();
        m.update(Message::Password(Secret::new("tr0ub4dor-secret".into())));
        m.update(Message::Password2(Secret::new("tr0ub4dor-secret".into())));
        let printed = format!("{m:?} {:?}", Message::Password(m.password.clone()));
        assert!(!printed.contains("tr0ub4dor"));
        let fx = m.update(Message::Apply);
        assert!(!format!("{fx:?}").contains("tr0ub4dor"));
    }

    #[test]
    fn a_wifi_passphrase_is_cleared_after_join_and_never_survives_a_failure() {
        let mut m = at(Step::Network);
        m.update(Message::NetScanned(fake_snapshot(false)));
        m.update(Message::SelectAp("Lighthouse".into()));
        assert!(!m.can_join(), "too short to be a WPA passphrase");
        m.update(Message::Passphrase(Secret::new("hunter2".into())));
        assert!(!m.can_join());
        m.update(Message::Passphrase(Secret::new("correct horse".into())));
        assert!(m.can_join());
        let fx = m.update(Message::Join);
        assert!(m.passphrase.is_empty());
        assert!(m.joining);
        assert!(
            matches!(&fx[..], [Effect::NetJoin { ssid, secure: true, passphrase }]
            if ssid == "Lighthouse" && passphrase.as_str() == "correct horse")
        );
        assert!(!m.can_next(), "no leaving mid-join");
        m.update(Message::Joined(false));
        assert!(m.join_failed && !m.joining && m.passphrase.is_empty());
        m.update(Message::Joined(true));
        assert!(!m.join_failed);
    }

    #[test]
    fn an_open_network_needs_no_passphrase() {
        let mut m = at(Step::Network);
        m.update(Message::NetScanned(fake_snapshot(false)));
        m.update(Message::SelectAp("Cafe Meridian".into()));
        assert!(m.can_join());
    }

    #[test]
    fn choosing_a_language_writes_the_keyboard_live() {
        let mut m = at(Step::Language);
        let de = LANGUAGES.iter().position(|l| l.locale == "de_DE.UTF-8").unwrap();
        let fx = m.update(Message::Language(de));
        assert_eq!(m.layout, "de");
        assert_eq!(m.locale(), "de_DE.UTF-8");
        let writes: Vec<(&str, &Value)> = fx
            .iter()
            .filter_map(|e| match e {
                Effect::Config { key, value } => Some((*key, value)),
                _ => None,
            })
            .collect();
        assert_eq!(writes[0], ("input.kb-layout", &json!("de")));
        assert_eq!(writes[1], ("input.kb-variant", &json!("")));
        m.update(Message::Variant(Some("nodeadkeys".into())));
        assert_eq!(m.variant.as_deref(), Some("nodeadkeys"));
        m.update(Message::Variant(Some("no-such-variant".into())));
        assert_eq!(m.variant.as_deref(), Some("nodeadkeys"));
        m.update(Message::Layout("xx".into()));
        assert_eq!(m.layout, "de");
    }

    #[test]
    fn a_refused_keyboard_write_blocks_next_but_no_socket_does_not() {
        let mut m = at(Step::Keyboard);
        m.update(Message::ConfigResult(
            config::KB_LAYOUT,
            Err(WriteError::NoSocket),
        ));
        assert_eq!(m.kb_live, KbLive::Offline);
        assert!(m.can_next());
        m.update(Message::ConfigResult(
            config::KB_LAYOUT,
            Err(WriteError::Rejected),
        ));
        assert_eq!(m.kb_live, KbLive::Refused);
        assert!(!m.can_next());
        m.update(Message::Layout("us".into()));
        assert_eq!(m.kb_live, KbLive::Idle);
        m.update(Message::ConfigResult(config::KB_LAYOUT, Ok(())));
        assert_eq!(m.kb_live, KbLive::Live);
    }

    #[test]
    fn the_layout_filter_narrows_the_list() {
        let mut m = at(Step::Keyboard);
        m.update(Message::KbFilter("ger".into()));
        let v = m.visible_layouts();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].code, "de");
    }

    #[test]
    fn a_timezone_comes_only_from_the_list() {
        let mut m = at(Step::Timezone);
        let fx = m.update(Message::Zone("Asia/Tokyo".into()));
        assert_eq!(m.zone, "Asia/Tokyo");
        assert!(matches!(fx.as_slice(), [Effect::ReadClock(z)] if z == "Asia/Tokyo"));
        for bad in ["../../etc/passwd", "/etc/localtime", "Mars/Olympus", ""] {
            assert!(m.update(Message::Zone(bad.into())).is_empty());
            assert_eq!(m.zone, "Asia/Tokyo");
        }
        // A stale clock reading for another zone is dropped.
        m.update(Message::ZoneClock(
            "Europe/Berlin".into(),
            Some(ZoneClock {
                time: "00:00".into(),
                offset: "+0100".into(),
            }),
        ));
        assert_eq!(m.zone_clock, None);
    }

    #[test]
    fn typing_a_place_picks_the_best_match_and_reads_its_clock() {
        let mut m = at(Step::Timezone);
        assert_eq!(m.zone, "Europe/Berlin", "the guess is preselected");
        assert!(m.zone_results().is_empty(), "nothing typed, nothing listed");

        let fx = m.update(Message::ZoneQuery("New York".into()));
        assert_eq!(m.zone, "America/New_York");
        assert!(matches!(fx.as_slice(), [Effect::ReadClock(z)] if z == "America/New_York"));

        m.update(Message::ZoneQuery("us eastern".into()));
        assert_eq!(m.zone, "America/New_York", "a choice still in the results stays");

        m.update(Message::ZoneQuery("Tokyo".into()));
        assert_eq!(m.zone, "Asia/Tokyo");

        let fx = m.update(Message::ZoneQuery("us pacific".into()));
        assert_eq!(m.zone, "America/Los_Angeles");
        assert_eq!(fx.len(), 1);

        // No match, or an emptied box, leaves the choice alone.
        let fx = m.update(Message::ZoneQuery("atlantis".into()));
        assert!(fx.is_empty() && m.zone_results().is_empty());
        assert_eq!(m.zone, "America/Los_Angeles");
        m.update(Message::ZoneQuery(String::new()));
        assert_eq!(m.zone, "America/Los_Angeles");
        assert!(m.can_next());
    }

    #[test]
    fn arrow_keys_walk_the_search_results() {
        let mut m = at(Step::Timezone);
        m.update(Message::Zone("Asia/Tokyo".into()));
        m.update(Message::ZoneQuery("eur".into()));
        let first = m.zone_results()[0].zone.clone();
        assert_eq!(m.zone, first);
        m.update(Message::Key(Key::Down));
        let second = m.zone_results()[1].zone.clone();
        assert_eq!(m.zone, second);
        m.update(Message::Key(Key::Up));
        assert_eq!(m.zone, first);
    }

    #[test]
    fn more_options_shut_again_on_every_step_change() {
        let mut m = at(Step::Keyboard);
        m.update(Message::More(true));
        assert!(m.more);
        m.update(Message::Next);
        assert!(!m.more);
    }

    #[test]
    fn an_empty_disk_list_is_a_state_that_can_be_rescanned() {
        let mut m = at(Step::Disk);
        m.update(Message::DisksLoaded(Ok(vec![])));
        assert!(matches!(&m.disks, Load::Ready(v) if v.is_empty()));
        assert!(!m.can_next(), "no disk, no way forward");
        assert!(m.selected_disk().is_none());
        m.update(Message::Key(Key::Down));
        assert_eq!(m.disk, None);
        let fx = m.update(Message::ReloadDisks);
        assert!(matches!(fx.as_slice(), [Effect::LoadDisks]));
        assert_eq!(m.disks, Load::Loading);
        m.update(Message::DisksLoaded(Ok(crate::helper::fake_disks())));
        assert!(matches!(&m.disks, Load::Ready(v) if !v.is_empty()));
    }

    #[test]
    fn helper_progress_drives_the_install_to_done() {
        let mut m = review_ready();
        m.update(Message::Apply);
        let p = |stage, pct, failed| {
            Message::Helper(HelperEvent::Progress(Progress {
                stage,
                pct,
                msg: "x".into(),
                failed,
            }))
        };
        m.update(p(Stage::Pacstrap, 40, false));
        assert_eq!((m.install.phase, m.install.pct), (Phase::Running, 40));
        m.update(p(Stage::Done, 100, false));
        assert_eq!(m.install.phase, Phase::Done);
        m.update(Message::Helper(HelperEvent::Ended { clean: true }));
        assert_eq!(m.install.phase, Phase::Done);
        assert!(!m.can_back(), "no going back over a finished install");
    }

    #[test]
    fn a_failure_names_its_stage_and_back_lands_on_identity_to_retype_the_password() {
        let mut m = review_ready();
        m.update(Message::Apply);
        m.update(Message::Helper(HelperEvent::Progress(Progress {
            stage: Stage::Pacstrap,
            pct: 44,
            msg: "package download failed".into(),
            failed: true,
        })));
        assert_eq!(m.install.phase, Phase::Failed);
        assert_eq!(m.install.failed_at, Some(Stage::Pacstrap));
        assert!(touched_disk(Stage::Pacstrap));
        assert!(!touched_disk(Stage::Preflight));
        assert!(m.can_back());
        m.update(Message::Back);
        assert_eq!(m.step, Step::Identity);
        assert_eq!(m.install.phase, Phase::Idle);
        assert!(
            !m.can_next(),
            "the password was cleared, so it has to be entered again"
        );
    }

    #[test]
    fn a_stream_that_ends_without_a_verdict_is_a_failure_with_a_fixed_sentence() {
        let mut m = review_ready();
        m.update(Message::Apply);
        m.update(Message::Helper(HelperEvent::Ended { clean: false }));
        assert_eq!(m.install.phase, Phase::Failed);
        assert_eq!(m.install.note, "the installer stopped before it finished");
    }

    #[test]
    fn restart_is_offered_only_when_the_install_is_done() {
        let mut m = review_ready();
        m.update(Message::Apply);
        assert!(m.update(Message::Restart).is_empty());
        assert!(m.update(Message::Key(Key::Enter)).is_empty());
        m.update(Message::Helper(HelperEvent::Progress(Progress {
            stage: Stage::Done,
            pct: 100,
            msg: "installed".into(),
            failed: false,
        })));
        let fx = m.update(Message::Restart);
        assert!(matches!(fx.as_slice(), [Effect::Reboot]));
        // Not twice.
        assert!(m.update(Message::Restart).is_empty());
        m.update(Message::Restarted(Err("the restart was refused".into())));
        assert!(m.install.restart_error.is_some());
    }

    #[test]
    fn enter_advances_and_escape_goes_back_and_tab_moves_focus() {
        let mut m = at(Step::Language);
        m.update(Message::Key(Key::Enter));
        assert_eq!(m.step, Step::Keyboard);
        m.update(Message::Key(Key::Escape));
        assert_eq!(m.step, Step::Language);
        assert!(matches!(
            m.update(Message::Key(Key::Tab)).as_slice(),
            [Effect::FocusNext]
        ));
        assert!(matches!(
            m.update(Message::Key(Key::BackTab)).as_slice(),
            [Effect::FocusPrev]
        ));
    }

    #[test]
    fn entering_review_and_identity_focuses_the_field_that_is_wanted() {
        let mut m = at(Step::Disk);
        m.update(Message::DisksLoaded(Ok(crate::helper::fake_disks())));
        m.update(Message::SelectDisk(crate::helper::fake_disks()[1].by_id.clone()));
        let fx = m.update(Message::Next);
        assert!(matches!(fx.as_slice(), [Effect::Focus(ids::USERNAME)]));
    }

    #[test]
    fn stage_order_puts_confirm_before_the_first_write() {
        assert!(stage_rank(Stage::Confirm) < stage_rank(Stage::Partition));
        assert!(stage_rank(Stage::Validate) < stage_rank(Stage::Preflight));
        assert_eq!(STAGE_ORDER.last(), Some(&Stage::Done));
    }
    #[test]
    fn arrows_do_nothing_on_the_keyboard_step_while_the_list_is_hidden() {
        let mut m = at(Step::Keyboard);
        let before = m.layout.clone();
        m.update(Message::More(true));
        assert!(m.update(Message::Key(Key::Down)).is_empty());
        assert_eq!(m.layout, before);
    }

    #[test]
    fn arrows_move_one_row_from_a_layout_the_filter_hides() {
        let mut m = at(Step::Keyboard);
        m.update(Message::Layout("us".into()));
        let pos = m.layouts.iter().position(|l| l.code == "us").unwrap();
        m.update(Message::KbFilter("zzzz-no-such".into()));
        assert!(m.visible_layouts().is_empty());
        m.update(Message::Key(Key::Down));
        assert_eq!(m.layout, m.layouts[pos + 1].code);
        m.update(Message::Key(Key::Up));
        assert_eq!(m.layout, m.layouts[pos].code);
    }

    #[test]
    fn enter_in_the_keyboard_test_field_does_not_advance() {
        // The field submits Noop, which takes the key so the wizard does not.
        let mut m = at(Step::Keyboard);
        assert!(m.update(Message::Noop).is_empty());
        assert_eq!(m.step, Step::Keyboard);
    }

    #[test]
    fn a_bad_hidden_host_name_is_brought_into_view() {
        let mut m = at(Step::Identity);
        fill_identity(&mut m);
        m.hostname = "Bad Host".into();
        assert!(!m.more);

        // Enter from the last password field lands on the host name first.
        let fx = m.update(Message::Key(Key::Enter));
        assert_eq!(m.step, Step::Identity);
        assert!(m.more);
        assert!(fx
            .iter()
            .any(|e| matches!(e, Effect::Focus(id) if *id == ids::HOSTNAME)));

        // It cannot be hidden again while it is the problem.
        m.update(Message::More(false));
        assert!(m.more);

        // Continue does the same from a closed state.
        m.more = false;
        m.update(Message::Next);
        assert!(m.more);

        m.update(Message::Hostname("eclipse-1".into()));
        m.update(Message::More(false));
        assert!(!m.more);
        m.update(Message::Next);
        assert_eq!(m.step, Step::Profile);
    }
}
