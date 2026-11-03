// SPDX-License-Identifier: AGPL-3.0-only
//! The process scan: which tools are running, what each is doing, and how
//! far along a copy is.
//!
//! Only processes owned by the session user are read. `/proc/<pid>` is owned
//! by the process's euid, so a foreign process fails the uid check before any
//! of its files are opened. `comm` decides whether a process is a tool at all;
//! `cmdline` is read only for those, and only the parts that become a label or
//! a copy's source path leave this module. Argv is never logged.
//!
//! Everything that decides a label or a fraction is a pure function over
//! [`Proc`], so the tests drive it with fabricated processes and never touch
//! a live `/proc`. [`RealProc`] is the one place that reads the filesystem.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::time::Instant;

use super::{Job, Source};

/// Process names (`comm`) the scan reports. Each is at most 15 bytes, the
/// kernel's `comm` limit.
pub const TOOLS: &[&str] = &[
    "cargo", "rustc", "pacman", "cp", "mv", "rsync", "curl", "wget", "tar",
];

/// Most jobs one scan reports. The lowest pids win, so the set is stable.
pub const MAX_SCAN: usize = 16;

/// Longest label fragment taken from argv, in characters.
const MAX_NAME: usize = 60;
/// Longest URL basename shown, in characters.
const MAX_URL_NAME: usize = 40;
/// The cargo subcommands a label may name. Anything else shows as plain `cargo`.
pub const CARGO_SUBCOMMANDS: &[&str] = &[
    "build", "check", "test", "clippy", "run", "doc", "install", "bench", "fmt", "publish", "update", "fetch",
];

/// Cumulative I/O counters from `/proc/<pid>/io`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IoStat {
    pub rchar: u64,
    pub wchar: u64,
}

/// One candidate process, as the scan needs it. Built by a [`ProcSource`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    /// Kernel process name, the tool's identity.
    pub comm: String,
    /// Full argv, `argv[0]` included.
    pub argv: Vec<String>,
    /// Basename of the working directory. Read for `cargo` only, as a hint.
    pub cwd_name: Option<String>,
    pub io: Option<IoStat>,
    /// Size of the copy source when it is a local regular file. Read for
    /// `cp`, `mv` and `rsync` only.
    pub source_bytes: Option<u64>,
}

/// Where processes come from. The real one reads `/proc`; tests inject a list.
pub trait ProcSource: Send {
    fn procs(&mut self) -> Vec<Proc>;
}

/// `/proc`, restricted to processes owned by `uid`.
pub struct RealProc {
    uid: u32,
    root: PathBuf,
}

impl RealProc {
    pub fn new() -> Self {
        // SAFETY: getuid(2) cannot fail and takes no pointers.
        let uid = unsafe { libc::getuid() };
        Self {
            uid,
            root: PathBuf::from("/proc"),
        }
    }
}

impl Default for RealProc {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcSource for RealProc {
    fn procs(&mut self) -> Vec<Proc> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for ent in entries.flatten() {
            let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let dir = ent.path();
            // Foreign processes stop here: nothing under them is opened.
            match fs::metadata(&dir) {
                Ok(m) if m.uid() == self.uid => {}
                _ => continue,
            }
            let Ok(comm) = fs::read_to_string(dir.join("comm")) else {
                continue;
            };
            let comm = comm.trim_end().to_owned();
            if !is_tool(&comm) {
                continue;
            }
            let Ok(raw) = fs::read(dir.join("cmdline")) else {
                continue;
            };
            let raw = raw.strip_suffix(&[0]).unwrap_or(&raw);
            let argv: Vec<String> = raw
                .split(|b| *b == 0)
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .collect();
            let io = fs::read_to_string(dir.join("io")).ok().and_then(|s| parse_io(&s));
            let cwd = fs::read_link(dir.join("cwd")).ok();
            let cwd_name = match (comm.as_str(), &cwd) {
                ("cargo", Some(c)) => c.file_name().map(|n| n.to_string_lossy().into_owned()),
                _ => None,
            };
            let source_bytes = copy_source(&comm, &argv).and_then(|src| {
                let path = match (&cwd, src.starts_with('/')) {
                    (Some(c), false) => c.join(&src),
                    _ => PathBuf::from(&src),
                };
                fs::metadata(path).ok().filter(|m| m.is_file()).map(|m| m.len())
            });
            out.push(Proc {
                pid,
                comm,
                argv,
                cwd_name,
                io,
                source_bytes,
            });
        }
        out
    }
}

/// Parse `/proc/<pid>/io`. Missing counters make the whole reading unusable.
pub fn parse_io(text: &str) -> Option<IoStat> {
    let mut rchar = None;
    let mut wchar = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("rchar:") {
            rchar = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("wchar:") {
            wchar = v.trim().parse().ok();
        }
    }
    Some(IoStat {
        rchar: rchar?,
        wchar: wchar?,
    })
}

pub fn is_tool(comm: &str) -> bool {
    TOOLS.contains(&comm)
}

/// Arguments after argv[0] that are not flags. A flag's value is not
/// recognised as such: this is a label heuristic, not a parser.
fn operands(argv: &[String]) -> impl Iterator<Item = &str> {
    argv.iter()
        .skip(1)
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
}

/// The value of the first of `names` found, as `-o VALUE` or `--long=VALUE`.
fn flag_value<'a>(argv: &'a [String], names: &[&str]) -> Option<&'a str> {
    let mut it = argv.iter().skip(1).map(String::as_str);
    while let Some(a) = it.next() {
        if names.contains(&a) {
            return it.next();
        }
        for n in names {
            if let Some(v) = a.strip_prefix(n).and_then(|r| r.strip_prefix('=')) {
                return Some(v);
            }
        }
    }
    None
}

/// Last path component, ignoring a trailing slash.
pub fn base(path: &str) -> &str {
    let p = path.trim_end_matches('/');
    p.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(path)
}

/// At most [`MAX_NAME`] characters, control characters removed.
fn short(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect()
}

/// The file a `cp`, `mv` or `rsync` reads from: the second-to-last operand.
/// Remote `host:path` sources are returned too; they fail to stat later.
pub fn copy_source(comm: &str, argv: &[String]) -> Option<String> {
    if !matches!(comm, "cp" | "mv" | "rsync") {
        return None;
    }
    let ops: Vec<&str> = operands(argv).collect();
    if ops.len() < 2 {
        return None;
    }
    Some(ops[ops.len() - 2].to_owned())
}

/// The label a scanned process shows. Derived from argv only as far as the
/// label needs: a subcommand, a crate name, a package or file basename.
pub fn label(p: &Proc) -> String {
    match p.comm.as_str() {
        "cargo" => {
            // Only argv[1] can be the subcommand: anything after a flag may be
            // that flag's value, and values never reach a label.
            let mut s = match p.argv.get(1).map(String::as_str) {
                Some(sub) if CARGO_SUBCOMMANDS.contains(&sub) => format!("cargo {sub}"),
                _ => "cargo".into(),
            };
            if let Some(dir) = &p.cwd_name {
                s.push_str(&format!(" ({})", short(dir)));
            }
            s
        }
        "rustc" => match flag_value(&p.argv, &["--crate-name"]) {
            Some(name) => format!("rustc {}", short(name)),
            None => "rustc".into(),
        },
        "pacman" => match operands(&p.argv).next() {
            Some(first) => format!("pacman {}", short(base(first))),
            None => "pacman".into(),
        },
        tool @ ("cp" | "mv" | "rsync") => match operands(&p.argv).last() {
            Some(dest) => format!("{tool} \u{2192} {}", short(base(dest))),
            None => tool.into(),
        },
        tool @ ("curl" | "wget") => {
            let names: &[&str] = if tool == "curl" {
                &["-o", "--output"]
            } else {
                &["-O", "--output-document"]
            };
            let name = flag_value(&p.argv, names)
                .map(|f| base(f).to_owned())
                .or_else(|| url_basename(&p.argv));
            match name {
                Some(n) => format!("{tool} {}", short(&n)),
                None => tool.into(),
            }
        }
        "tar" => {
            let archive = operands(&p.argv).find(|a| is_archive(base(a)));
            match archive {
                Some(a) => format!("tar {}", short(base(a))),
                None => "tar".into(),
            }
        }
        other => other.into(),
    }
}

/// The last path segment of the first URL operand, at most [`MAX_URL_NAME`]
/// characters. The fragment and query go first, then the authority, which
/// may carry userinfo; neither reaches the label, since they often hold
/// credentials.
fn url_basename(argv: &[String]) -> Option<String> {
    let url = operands(argv).find(|a| a.contains("://"))?;
    let after_scheme = url.split_once("://")?.1;
    let no_frag_query = after_scheme.split(['?', '#']).next()?;
    let (_authority, path) = no_frag_query.split_once('/')?;
    let name = path.trim_end_matches('/').rsplit('/').next()?;
    (!name.is_empty()).then(|| name.chars().take(MAX_URL_NAME).collect())
}

fn is_archive(name: &str) -> bool {
    name.contains(".tar")
        || [".tgz", ".tzst", ".txz", ".tbz2"]
            .iter()
            .any(|e| name.ends_with(e))
}

/// `done / total`, clamped to `0.0..=1.0`. `None` when there is no total.
pub fn fraction(done: u64, total: u64) -> Option<f32> {
    if total == 0 {
        return None;
    }
    Some((done as f64 / total as f64).min(1.0) as f32)
}

/// A copy's fraction: bytes written against the source's size. `None` for
/// every other tool, and for a copy whose source size is unknown.
pub fn copy_fraction(p: &Proc) -> Option<f32> {
    if !matches!(p.comm.as_str(), "cp" | "mv" | "rsync") {
        return None;
    }
    fraction(p.io?.wchar, p.source_bytes?)
}

/// Turn the visible processes into scan jobs.
///
/// `started` remembers when each `(pid, comm)` was first seen, so elapsed
/// time survives between scans; entries for processes that have gone are
/// dropped. A standalone `rustc` is skipped while a `cargo` runs, because
/// cargo's job already covers the compile.
pub fn scan(procs: &[Proc], now: Instant, started: &mut HashMap<(u32, String), Instant>) -> Vec<Job> {
    let cargo_running = procs.iter().any(|p| p.comm == "cargo");
    let mut tracked: Vec<&Proc> = procs
        .iter()
        .filter(|p| is_tool(&p.comm))
        .filter(|p| !(p.comm == "rustc" && cargo_running))
        .collect();
    tracked.sort_by_key(|p| p.pid);

    let live: HashSet<(u32, String)> = tracked.iter().map(|p| (p.pid, p.comm.clone())).collect();
    started.retain(|k, _| live.contains(k));

    tracked
        .into_iter()
        .take(MAX_SCAN)
        .map(|p| {
            let at = *started.entry((p.pid, p.comm.clone())).or_insert(now);
            Job {
                id: format!("pid:{}", p.pid),
                label: label(p),
                source: Source::Scan,
                fraction: copy_fraction(p),
                started: at,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    fn proc(pid: u32, comm: &str, words: &[&str]) -> Proc {
        Proc {
            pid,
            comm: comm.into(),
            argv: argv(words),
            ..Proc::default()
        }
    }

    #[test]
    fn tools_are_the_listed_names() {
        for t in [
            "cargo", "rustc", "pacman", "cp", "mv", "rsync", "curl", "wget", "tar",
        ] {
            assert!(is_tool(t), "{t}");
            assert!(t.len() <= 15);
        }
        assert!(!is_tool("bash"));
        assert!(!is_tool("cargo-clippy"));
    }

    #[test]
    fn labels_from_argv() {
        let mut c = proc(1, "cargo", &["cargo", "build", "-j", "2"]);
        assert_eq!(label(&c), "cargo build");
        c.cwd_name = Some("EclipseOS".into());
        assert_eq!(label(&c), "cargo build (EclipseOS)");
        assert_eq!(label(&proc(2, "cargo", &["cargo"])), "cargo");

        assert_eq!(
            label(&proc(3, "rustc", &["rustc", "--crate-name", "ec_services", "-"])),
            "rustc ec_services"
        );
        assert_eq!(
            label(&proc(3, "rustc", &["rustc", "--crate-name=ec_ipc"])),
            "rustc ec_ipc"
        );
        assert_eq!(label(&proc(4, "rustc", &["rustc", "-V"])), "rustc");

        assert_eq!(
            label(&proc(5, "pacman", &["pacman", "-S", "ripgrep", "fd"])),
            "pacman ripgrep"
        );
        assert_eq!(
            label(&proc(
                5,
                "pacman",
                &["pacman", "-U", "/var/cache/x-1.0.pkg.tar.zst"]
            )),
            "pacman x-1.0.pkg.tar.zst"
        );
        assert_eq!(label(&proc(5, "pacman", &["pacman", "-Syu"])), "pacman");

        assert_eq!(
            label(&proc(6, "cp", &["cp", "-r", "/src/a.iso", "/mnt/usb/"])),
            "cp \u{2192} usb"
        );
        assert_eq!(
            label(&proc(6, "rsync", &["rsync", "-a", "src/", "host:/dst/file.bin"])),
            "rsync \u{2192} file.bin"
        );
        assert_eq!(label(&proc(6, "mv", &["mv"])), "mv");

        assert_eq!(
            label(&proc(
                7,
                "curl",
                &["curl", "-fsSL", "-o", "/tmp/x/out.tgz", "https://h/a"]
            )),
            "curl out.tgz"
        );
        assert_eq!(
            label(&proc(7, "wget", &["wget", "-O", "dl.bin", "https://h/a"])),
            "wget dl.bin"
        );
        assert_eq!(
            label(&proc(
                7,
                "curl",
                &["curl", "https://h.example/path/file.zip?token=SECRET#frag"]
            )),
            "curl file.zip"
        );
        assert_eq!(label(&proc(7, "curl", &["curl", "https://h.example/"])), "curl");

        assert_eq!(
            label(&proc(8, "tar", &["tar", "-xf", "/a/b/src.tar.zst", "-C", "/x"])),
            "tar src.tar.zst"
        );
        assert_eq!(label(&proc(8, "tar", &["tar", "-tv"])), "tar");
    }

    #[test]
    fn cargo_shows_only_an_allowed_subcommand_in_argv1() {
        let c = |words: &[&str]| label(&proc(1, "cargo", words));
        assert_eq!(c(&["cargo", "publish", "--token", "TOKENVALUE"]), "cargo publish");
        assert_eq!(c(&["cargo", "login", "sekret"]), "cargo");
        assert_eq!(c(&["cargo", "+nightly", "build"]), "cargo");
        assert_eq!(c(&["cargo", "--config", "x=build", "check"]), "cargo");
        assert_eq!(c(&["cargo", "-C", "/home/u/secret", "build"]), "cargo");
        assert_eq!(c(&["cargo", "frobnicate"]), "cargo");
        assert!(!c(&["cargo", "--config", "TOKENVALUE", "run"]).contains("TOKENVALUE"));
    }

    #[test]
    fn url_labels_drop_userinfo_query_and_fragment() {
        let u = |url: &str| label(&proc(7, "curl", &["curl", url]));
        assert_eq!(
            u("https://user:SECRET@h.example/dir/file.zip?token=SECRET#frag"),
            "curl file.zip"
        );
        assert_eq!(u("https://h.example/x.zip#frag?q=SECRET"), "curl x.zip");
        assert_eq!(u("https://user:SECRET@h.example/a/b/"), "curl b");
        assert_eq!(u("https://user:SECRET@h.example"), "curl");
        assert_eq!(u("https://user:SECRET@h.example?x=SECRET"), "curl");
        let long = format!("https://h.example/{}", "z".repeat(200));
        assert_eq!(u(&long), format!("curl {}", "z".repeat(MAX_URL_NAME)));
    }

    #[test]
    fn labels_are_bounded_and_strip_control_characters() {
        let long = "a".repeat(200);
        let l = label(&proc(1, "pacman", &["pacman", &long]));
        assert_eq!(l.chars().count(), "pacman ".len() + MAX_NAME);
        let l = label(&proc(1, "pacman", &["pacman", "x\u{7}y"]));
        assert_eq!(l, "pacman xy");
    }

    #[test]
    fn copy_source_is_the_second_to_last_operand() {
        assert_eq!(
            copy_source("cp", &argv(&["cp", "-r", "a", "b", "dst/"])).as_deref(),
            Some("b")
        );
        assert_eq!(copy_source("cp", &argv(&["cp", "only"])), None);
        assert_eq!(copy_source("curl", &argv(&["curl", "a", "b"])), None);
    }

    #[test]
    fn fraction_math() {
        assert_eq!(fraction(0, 0), None);
        assert_eq!(fraction(0, 100), Some(0.0));
        assert_eq!(fraction(50, 200), Some(0.25));
        assert_eq!(fraction(300, 200), Some(1.0));
    }

    #[test]
    fn copy_fraction_needs_both_io_and_source_size() {
        let mut p = proc(1, "cp", &["cp", "a", "b"]);
        assert_eq!(copy_fraction(&p), None);
        p.source_bytes = Some(1000);
        assert_eq!(copy_fraction(&p), None);
        p.io = Some(IoStat { rchar: 9, wchar: 250 });
        assert_eq!(copy_fraction(&p), Some(0.25));

        let mut c = proc(2, "curl", &["curl", "x"]);
        c.io = Some(IoStat { rchar: 0, wchar: 500 });
        c.source_bytes = Some(1000);
        assert_eq!(copy_fraction(&c), None, "curl has no size to measure against");
    }

    #[test]
    fn parses_io_counters() {
        let text = "rchar: 12\nwchar: 34\nsyscr: 1\nsyscw: 2\n";
        assert_eq!(parse_io(text), Some(IoStat { rchar: 12, wchar: 34 }));
        assert_eq!(parse_io("rchar: 1\n"), None);
    }

    #[test]
    fn scan_skips_rustc_under_cargo_and_keeps_start_times() {
        let mut started = HashMap::new();
        let t0 = Instant::now();
        let procs = vec![
            proc(10, "cargo", &["cargo", "build"]),
            proc(11, "rustc", &["rustc", "--crate-name", "a"]),
            proc(12, "bash", &["bash"]),
        ];
        let jobs = scan(&procs, t0, &mut started);
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "pid:10");
        assert_eq!(jobs[0].source, Source::Scan);
        assert_eq!(jobs[0].fraction, None);

        // A later scan keeps the original start time.
        let t1 = t0 + Duration::from_secs(3);
        let jobs = scan(&procs, t1, &mut started);
        assert_eq!(jobs[0].started, t0);

        // Once the process is gone its start time is forgotten.
        let jobs = scan(&[], t1, &mut started);
        assert!(jobs.is_empty());
        assert!(started.is_empty());
    }

    #[test]
    fn scan_reports_standalone_rustc_and_caps_the_count() {
        let mut started = HashMap::new();
        let now = Instant::now();
        let procs: Vec<Proc> = (1..=40).map(|pid| proc(pid, "tar", &["tar", "-t"])).collect();
        let jobs = scan(&procs, now, &mut started);
        assert_eq!(jobs.len(), MAX_SCAN);
        assert_eq!(jobs[0].id, "pid:1", "lowest pids win");

        let one = scan(
            &[proc(7, "rustc", &["rustc", "--crate-name", "q"])],
            now,
            &mut started,
        );
        assert_eq!(one[0].label, "rustc q");
    }
}
