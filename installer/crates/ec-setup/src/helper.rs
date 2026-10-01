// SPDX-License-Identifier: AGPL-3.0-only
//! The root helper, from the client's side (D-07 §6), and the fake one.
//!
//! **Real.** Disks come from `ec-setup-helper list-disks`, which is
//! unprivileged and prints JSON `Vec<Disk>`; the wizard never lists `/dev`
//! itself and never lets the user type a device path. Apply is
//! `pkexec /usr/bin/ec-setup-helper apply` with exactly one JSON
//! [`Request`] on its stdin, then stdin closed, and one [`Progress`] JSON per
//! line coming back on stdout. Fixed argv, no shell, no environment, stderr
//! discarded (it could quote anything). The request is never logged, never
//! `Debug`-printed, and the serialised bytes that carry the password are
//! overwritten as soon as they have been written.
//!
//! **Fake.** `--fake-helper[=fail-at-<stage>]` swaps in an in-process
//! simulator: two invented disks, and a scripted run of realistic progress
//! events. It is what the whole flow is developed and tested against without
//! root, and it never is the default. It touches no disk and no network.

use ec_setup_plan::{
    check_password, valid_by_id, valid_hostname, valid_keymap, valid_username, Disk, Partition, Plan,
    Progress, Request, Stage,
};
use iced::futures::channel::mpsc;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;
use zeroize::Zeroizing;

/// Where the packaged helper lives. `pkexec` needs the absolute path: polkit
/// keys its policy on it (`org.freedesktop.policykit.exec.path`).
pub const HELPER_PATH: &str = "/usr/bin/ec-setup-helper";
/// `list-disks` is unprivileged, so it is found on `PATH` like any tool.
pub const HELPER_NAME: &str = "ec-setup-helper";

const GIB: u64 = 1 << 30;

/// A simulated run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeCfg {
    /// Stop with a failure at this stage.
    pub fail_at: Option<Stage>,
    /// Pause between events, so the progress screen is watchable.
    pub delay: Duration,
}

impl Default for FakeCfg {
    fn default() -> Self {
        FakeCfg {
            fail_at: None,
            delay: Duration::from_millis(450),
        }
    }
}

/// Everything that reaches outside the process: the helper, the network, the
/// clock, the reboot and the compositor's config. `fake` is `Some` only when
/// the user asked for `--fake-helper`, and then *all* of it is simulated, so a
/// dry run cannot change the machine it runs on.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub fake: Option<FakeCfg>,
}

impl Env {
    pub fn real() -> Env {
        Env { fake: None }
    }

    pub fn fake(cfg: FakeCfg) -> Env {
        Env { fake: Some(cfg) }
    }

    pub fn is_fake(&self) -> bool {
        self.fake.is_some()
    }
}

/// Parse `--fake-helper` and `--fake-helper=fail-at-<stage>`.
pub fn parse_fake_arg(arg: &str) -> Result<FakeCfg, String> {
    let mut cfg = FakeCfg::default();
    match arg.strip_prefix("--fake-helper") {
        Some("") => Ok(cfg),
        Some(rest) => {
            let stage = rest
                .strip_prefix("=fail-at-")
                .ok_or("--fake-helper: expected --fake-helper or --fake-helper=fail-at-<stage>")?;
            let stage = stage.replace('-', "_");
            let parsed: Stage = serde_json::from_value(serde_json::Value::String(stage))
                .map_err(|_| "--fake-helper: unknown stage".to_owned())?;
            cfg.fail_at = Some(parsed);
            Ok(cfg)
        }
        None => Err("not a --fake-helper argument".into()),
    }
}

// ------------------------------------------------------------------- disks

/// The disks the helper offers.
pub fn list_disks(env: &Env) -> Result<Vec<Disk>, String> {
    if env.is_fake() {
        return Ok(fake_disks());
    }
    let out = Command::new(HELPER_NAME)
        .arg("list-disks")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "the install helper is not available".to_owned())?;
    if !out.status.success() {
        return Err("the install helper could not list disks".into());
    }
    parse_disks(&out.stdout)
}

/// Parse the helper's listing. A disk whose by-id name would not be one path
/// component is dropped, not shown: nothing but a clean name ever becomes a
/// target.
pub fn parse_disks(stdout: &[u8]) -> Result<Vec<Disk>, String> {
    let disks: Vec<Disk> = serde_json::from_slice(stdout)
        .map_err(|_| "the install helper answered with something unreadable".to_owned())?;
    Ok(disks.into_iter().filter(|d| valid_by_id(&d.by_id)).collect())
}

/// Two invented disks: a laptop's NVMe with another system on it, and a blank
/// SATA drive. Invented on purpose: nothing here is read from the machine.
pub fn fake_disks() -> Vec<Disk> {
    let p = |fs: &str, label: &str, size_bytes: u64| Partition {
        fs: fs.into(),
        label: label.into(),
        size_bytes,
    };
    vec![
        Disk {
            by_id: "nvme-Samsung_SSD_970_EVO_1TB_S4EWNX0M123456".into(),
            model: "Samsung SSD 970 EVO 1TB".into(),
            size_bytes: 1_000_204_886_016,
            partitions: vec![
                p("vfat", "SYSTEM", GIB / 2),
                p("ntfs", "Windows", 300 * GIB),
                p("ntfs", "Recovery", 2 * GIB),
                p("ext4", "arch-root", 620 * GIB),
            ],
        },
        Disk {
            by_id: "ata-WDC_WD20EZAZ-00GGJB0_WD-WX12D34E5678".into(),
            model: "WDC WD20EZAZ-00GGJB0".into(),
            size_bytes: 2_000_398_934_016,
            partitions: vec![],
        },
    ]
}

// ------------------------------------------------------------------- apply

/// What the helper (or its simulation) sends back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HelperEvent {
    Progress(Progress),
    /// The stream is over. `clean` is the helper's own exit status: `false`
    /// covers a denied polkit prompt, a crash and a missing helper alike, and
    /// says nothing more (nothing it printed is quoted).
    Ended {
        clean: bool,
    },
}

/// Start an install. The [`Request`] is consumed and its password is zeroised
/// as soon as it has been written. Events arrive on the returned channel.
pub fn apply(env: &Env, request: Request) -> mpsc::UnboundedReceiver<HelperEvent> {
    let (tx, rx) = mpsc::unbounded();
    match env.fake.clone() {
        Some(cfg) => {
            std::thread::spawn(move || run_fake(cfg, request, tx));
        }
        None => {
            std::thread::spawn(move || run_real(request, tx));
        }
    }
    rx
}

fn run_real(request: Request, tx: mpsc::UnboundedSender<HelperEvent>) {
    let end = |clean: bool| {
        let _ = tx.unbounded_send(HelperEvent::Ended { clean });
    };
    let child = Command::new("pkexec")
        .arg(HELPER_PATH)
        .arg("apply")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return end(false);
    };

    // The one place the password is serialised. The buffer is wiped when this
    // block ends, whether or not the write succeeded.
    let wrote = {
        let body = serde_json::to_vec(&request).map(Zeroizing::new);
        drop(request);
        match (body, child.stdin.take()) {
            (Ok(body), Some(mut stdin)) => stdin.write_all(&body).and_then(|()| stdin.flush()).is_ok(),
            _ => false,
        }
        // `stdin` is dropped here, which closes the pipe: the helper sees EOF.
    };
    if !wrote {
        let _ = child.kill();
        let _ = child.wait();
        return end(false);
    }

    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            // A line that is not a `Progress` is dropped, never echoed.
            if let Ok(p) = serde_json::from_str::<Progress>(&line) {
                if tx.unbounded_send(HelperEvent::Progress(p)).is_err() {
                    break;
                }
            }
        }
    }
    let clean = child.wait().map(|s| s.success()).unwrap_or(false);
    end(clean);
}

fn run_fake(cfg: FakeCfg, request: Request, tx: mpsc::UnboundedSender<HelperEvent>) {
    let script = fake_script(&cfg, &request.plan, &request.password);
    drop(request);
    let failed = script.last().is_some_and(|p| p.failed);
    for p in script {
        if !cfg.delay.is_zero() {
            std::thread::sleep(cfg.delay);
        }
        if tx.unbounded_send(HelperEvent::Progress(p)).is_err() {
            return;
        }
    }
    let _ = tx.unbounded_send(HelperEvent::Ended { clean: !failed });
}

/// The whole simulated run, as data. Validates like the real helper would (a
/// bad plan fails at `validate`, before anything "happens"), then walks every
/// stage with realistic, fixed-vocabulary messages.
pub fn fake_script(cfg: &FakeCfg, plan: &Plan, password: &str) -> Vec<Progress> {
    let ev = |stage: Stage, pct: u8, msg: &str| Progress {
        stage,
        pct,
        msg: msg.to_owned(),
        failed: false,
    };
    let fail = |stage: Stage, pct: u8, msg: &str| Progress {
        stage,
        pct,
        msg: msg.to_owned(),
        failed: true,
    };

    if !valid_by_id(&plan.disk_by_id)
        || !valid_hostname(&plan.hostname)
        || !valid_username(&plan.username)
        || !valid_keymap(&plan.keymap)
        || !check_password(password)
    {
        return vec![fail(Stage::Validate, 0, "the plan was refused")];
    }

    let mut all: Vec<Progress> = vec![
        ev(Stage::Validate, 2, "plan accepted"),
        ev(Stage::Preflight, 5, "checking the network"),
        ev(Stage::Preflight, 8, "space and catalog ok"),
        ev(
            Stage::Confirm,
            10,
            "waiting for confirmation on the trusted screen",
        ),
        ev(Stage::Confirm, 12, "confirmed"),
        ev(Stage::Partition, 14, "writing the partition table"),
        ev(Stage::Partition, 18, "creating filesystems"),
    ];
    let total = 96u32;
    for n in (8..=total).step_by(8) {
        let pct = 20 + (n * 50 / total) as u8;
        all.push(ev(
            Stage::Pacstrap,
            pct,
            &format!("installing packages {n} of {total}"),
        ));
    }
    all.extend([
        ev(Stage::Configure, 74, "writing system configuration"),
        ev(Stage::Bootloader, 82, "installing the bootloader"),
        ev(Stage::User, 88, "creating the user"),
        ev(Stage::Seed, 94, "carrying your settings over"),
        ev(Stage::Units, 98, "enabling services"),
        ev(Stage::Done, 100, "installed"),
    ]);

    let Some(at) = cfg.fail_at else {
        return all;
    };
    // Cut the run at the first event of the failing stage and mark it failed,
    // as the real helper stops at the first failed stage.
    let mut out = Vec::new();
    for p in all {
        if p.stage == at {
            out.push(fail(at, p.pct, fail_message(at)));
            return out;
        }
        out.push(p);
    }
    out
}

fn fail_message(stage: Stage) -> &'static str {
    match stage {
        Stage::Validate => "the plan was refused",
        Stage::Preflight => "the network is not reachable",
        Stage::Confirm => "the erase was not confirmed",
        Stage::Partition => "could not partition the disk",
        Stage::Pacstrap => "package download failed",
        Stage::Configure => "could not write the system configuration",
        Stage::Bootloader => "could not install the bootloader",
        Stage::User => "could not create the user",
        Stage::Seed => "could not carry the settings over",
        Stage::Units => "could not enable services",
        Stage::Done => "unexpected failure",
    }
}

// ------------------------------------------------------------------ reboot

/// `systemctl reboot`, fixed argv. In a fake run nothing restarts.
pub fn reboot(env: &Env) -> Result<(), String> {
    if env.is_fake() {
        return Ok(());
    }
    Command::new("systemctl")
        .arg("reboot")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "could not run systemctl".to_owned())
        .and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err("the restart was refused".to_owned())
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_setup_plan::Profile;
    use iced::futures::StreamExt;

    fn plan() -> Plan {
        Plan {
            disk_by_id: "nvme-x".into(),
            hostname: "eclipse".into(),
            username: "chase".into(),
            locale: "en_US.UTF-8".into(),
            timezone: "UTC".into(),
            keymap: "us".into(),
            carry_network: true,
            profile: Profile::Standard,
            candidates: vec!["ec-hyperion-bar".into()],
            agents: false,
        }
    }

    #[test]
    fn fake_arg_parses() {
        assert_eq!(parse_fake_arg("--fake-helper").unwrap(), FakeCfg::default());
        let f = parse_fake_arg("--fake-helper=fail-at-pacstrap").unwrap();
        assert_eq!(f.fail_at, Some(Stage::Pacstrap));
        assert!(parse_fake_arg("--fake-helper=fail-at-nope").is_err());
        assert!(parse_fake_arg("--fake-helper=bogus").is_err());
        assert!(parse_fake_arg("--fake-helperx").is_err());
    }

    #[test]
    fn the_fake_run_walks_every_stage_in_order_and_ends_done() {
        let s = fake_script(&FakeCfg::default(), &plan(), "hunter22");
        assert!(s.iter().all(|p| !p.failed));
        let last = s.last().unwrap();
        assert_eq!((last.stage, last.pct), (Stage::Done, 100));
        // Monotone percentage, stages never go backwards.
        assert!(s.windows(2).all(|w| w[0].pct <= w[1].pct));
        assert!(s
            .windows(2)
            .all(|w| stage_rank(w[0].stage) <= stage_rank(w[1].stage)));
        for st in [Stage::Confirm, Stage::Partition, Stage::Pacstrap, Stage::Seed] {
            assert!(s.iter().any(|p| p.stage == st), "{st:?}");
        }
    }

    fn stage_rank(s: Stage) -> usize {
        [
            Stage::Validate,
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
        ]
        .iter()
        .position(|x| *x == s)
        .unwrap()
    }

    #[test]
    fn the_failing_variant_stops_at_the_named_stage() {
        let cfg = parse_fake_arg("--fake-helper=fail-at-pacstrap").unwrap();
        let s = fake_script(&cfg, &plan(), "hunter22");
        let last = s.last().unwrap();
        assert!(last.failed);
        assert_eq!(last.stage, Stage::Pacstrap);
        assert_eq!(s.iter().filter(|p| p.failed).count(), 1);
        assert!(s.iter().all(|p| p.stage != Stage::Done));
    }

    #[test]
    fn the_fake_refuses_a_bad_plan_like_the_real_helper_would() {
        let mut p = plan();
        p.username = "root".into();
        let s = fake_script(&FakeCfg::default(), &p, "hunter22");
        assert_eq!(s.len(), 1);
        assert!(s[0].failed && s[0].stage == Stage::Validate);
        let s = fake_script(&FakeCfg::default(), &plan(), "a\nb");
        assert!(s[0].failed);
    }

    #[test]
    fn no_message_carries_the_password_or_the_plan() {
        let s = fake_script(&FakeCfg::default(), &plan(), "hunter22-secret");
        for p in s {
            assert!(!p.msg.contains("hunter22"));
            assert!(!p.msg.contains("chase"));
        }
    }

    #[test]
    fn the_fake_channel_delivers_the_script_then_ends() {
        let env = Env::fake(FakeCfg {
            fail_at: None,
            delay: Duration::ZERO,
        });
        let rx = apply(
            &env,
            Request {
                plan: plan(),
                password: Zeroizing::new("hunter22".into()),
            },
        );
        let events: Vec<HelperEvent> = iced::futures::executor::block_on(rx.collect());
        assert!(matches!(events.last(), Some(HelperEvent::Ended { clean: true })));
        assert!(matches!(
            events[events.len() - 2],
            HelperEvent::Progress(Progress {
                stage: Stage::Done,
                failed: false,
                ..
            })
        ));
    }

    #[test]
    fn the_fake_disks_are_two_and_have_clean_names() {
        let d = fake_disks();
        assert_eq!(d.len(), 2);
        assert!(d.iter().all(|d| valid_by_id(&d.by_id)));
        assert!(!d[0].partitions.is_empty());
    }

    #[test]
    fn a_listing_drops_a_disk_that_is_not_one_clean_name() {
        let json = br#"[{"by_id":"nvme-ok","model":"m","size_bytes":1,"partitions":[]},
                        {"by_id":"../sda","model":"m","size_bytes":1,"partitions":[]}]"#;
        let d = parse_disks(json).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].by_id, "nvme-ok");
        assert!(parse_disks(b"not json").is_err());
    }

    #[test]
    fn a_fake_reboot_restarts_nothing() {
        assert!(reboot(&Env::fake(FakeCfg::default())).is_ok());
    }
}
