// SPDX-License-Identifier: AGPL-3.0-only
//! The command-widget approvals store (ADR 0067 "Approvals"; COMP-10 §3.11).
//!
//! `$XDG_STATE_HOME/eclipse/widget-approvals.kdl` (falling back to
//! `~/.local/state`, like `outputs/persist.rs`), one line per widget:
//!
//! ```kdl
//! approve "load" "<64 hex digits>"
//! ```
//!
//! It maps a widget name to the one hash the owner accepted. It is state, not
//! config: no GUI edits it, `get_config` does not serve it, and the config
//! watcher does not watch it. Read at config apply; written only by
//! [`persist`] (off the event loop) or [`record_approval`] (blocking).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::OnceLock;

use kdl::KdlDocument;

use super::widget_hash::WidgetHash;

#[cfg(any(test, feature = "test-hooks"))]
thread_local! {
    static TEST_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Tests only: use `path` as the store on this thread (`None` = no store).
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub fn set_test_path(path: Option<PathBuf>) {
    TEST_PATH.with(|p| *p.borrow_mut() = path);
}

fn path() -> Option<PathBuf> {
    #[cfg(any(test, feature = "test-hooks"))]
    {
        TEST_PATH.with(|p| p.borrow().clone())
    }
    #[cfg(not(any(test, feature = "test-hooks")))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .filter(|x| !x.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
        Some(base.join("eclipse/widget-approvals.kdl"))
    }
}

/// The store entry for the Oracle Eyes model command (owner decision 3,
/// ADR 0067's path), beside the widget entries. Its hash is
/// [`super::widget_hash::model_command_hash`], derived under its own context,
/// so a widget that happens to carry this name can never approve the model
/// command, nor the reverse: the worst it does is overwrite the entry and
/// make the owner be asked again.
pub const MODEL_COMMAND_KEY: &str = "oracle-eyes:model-command";

/// Recorded approvals, name → accepted hash, one entry per name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Approvals(Vec<(String, WidgetHash)>);

impl Approvals {
    pub fn get(&self, name: &str) -> Option<WidgetHash> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, h)| *h)
    }

    fn set(&mut self, name: &str, hash: WidgetHash) {
        match self.0.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = hash,
            None => self.0.push((name.to_owned(), hash)),
        }
    }
}

/// Read the store. Missing or unreadable is "nothing approved"; a malformed
/// line approves nothing and the rest still load (fail-closed per line).
pub fn read() -> Approvals {
    match path() {
        Some(path) => read_at(&path),
        None => Approvals::default(),
    }
}

fn read_at(path: &Path) -> Approvals {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Approvals::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "reading widget approvals; treating none as approved");
            Approvals::default()
        }
    }
}

fn parse(text: &str) -> Approvals {
    let doc: KdlDocument = match text.parse() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "widget approvals are not KDL; treating none as approved");
            return Approvals::default();
        }
    };
    let mut out = Approvals::default();
    for node in doc.nodes() {
        let strs: Option<Vec<&str>> = node
            .entries()
            .iter()
            .map(|e| e.name().is_none().then(|| e.value().as_string()).flatten())
            .collect();
        match (node.name().value(), strs.as_deref(), node.children()) {
            ("approve", Some([name, hex]), None) if !name.is_empty() => match WidgetHash::from_hex(hex) {
                Some(h) => out.set(name, h),
                None => tracing::warn!(name, "widget approval with a malformed hash; ignored"),
            },
            (other, _, _) => tracing::warn!(node = other, "unrecognised line in widget approvals; ignored"),
        }
    }
    out
}

fn render(a: &Approvals) -> String {
    let mut s = String::from(
        "// Command widgets the owner approved (ADR 0067). Written by abyss's approval\n\
         // prompt only; a line here lets exactly that definition run.\n",
    );
    for (name, hash) in &a.0 {
        s.push_str("approve ");
        s.push_str(&super::edit::quote(name));
        s.push_str(" \"");
        s.push_str(&hash.to_hex());
        s.push_str("\"\n");
    }
    s
}

/// Record that the owner accepted `hash` for widget `name`, replacing any
/// earlier approval for that name. Atomic (temp file + rename), mode 0600.
/// Blocks on fsync: not for the event loop, which uses [`persist`].
///
/// **Only the Trusted UI approval surface (`trusted_ui/`, ADR 0067) may call
/// this**, and only on the owner's Accept/Allow. No socket method, no config
/// path and nothing else in this crate reaches it; a test scans the source to
/// keep it that way. The caller re-applies the config afterwards
/// (`withhold::reapply`) so the widget goes live.
pub fn record_approval(name: &str, hash: WidgetHash) -> std::io::Result<()> {
    let path = path().ok_or_else(no_path)?;
    write_at(&path, name, hash)
}

/// [`record_approval`] without blocking: the store path is resolved here, the
/// read-modify-write runs on one long-lived writer thread, in send order, so
/// quick successive approvals never lose an update. A failed write is logged
/// there (`tracing::error`, widget name only). `Err` only for immediate
/// failures: no store path, the writer could not start, or it is gone.
///
/// The caller records the approval for this session in
/// `WidgetApprovals::granted` and re-applies, so the widget goes live without
/// waiting for the disk.
///
/// **Only the Trusted UI approval surface (`trusted_ui/`, ADR 0067) may call
/// this**, and only on the owner's Accept/Allow; the same source-scan test
/// as [`record_approval`] enforces it.
pub fn persist(name: &str, hash: WidgetHash) -> std::io::Result<()> {
    let path = path().ok_or_else(no_path)?;
    writer()?
        .send(Job::Write(path, name.to_owned(), hash))
        .map_err(|_| std::io::Error::other("approvals writer is gone"))
}

/// Tests only: block until the writer has finished every [`persist`] sent so
/// far (from any thread).
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub fn wait_written() {
    let (tx, rx) = mpsc::channel();
    writer().unwrap().send(Job::Flush(tx)).unwrap();
    rx.recv().unwrap();
}

enum Job {
    Write(PathBuf, String, WidgetHash),
    #[cfg(any(test, feature = "test-hooks"))]
    Flush(mpsc::Sender<()>),
}

fn no_path() -> std::io::Error {
    std::io::Error::other("no XDG_STATE_HOME or HOME")
}

static WRITER: OnceLock<mpsc::Sender<Job>> = OnceLock::new();

/// The writer's queue, starting the thread on first use.
fn writer() -> std::io::Result<&'static mpsc::Sender<Job>> {
    if let Some(tx) = WRITER.get() {
        return Ok(tx);
    }
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("abyss-approvals".into())
        .spawn(move || {
            for job in rx {
                match job {
                    Job::Write(path, name, hash) => {
                        if let Err(e) = write_at(&path, &name, hash) {
                            tracing::error!(name, error = %e, "widget approval not saved; it lasts this session only");
                        }
                    }
                    #[cfg(any(test, feature = "test-hooks"))]
                    Job::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        })?;
    // A racing first caller (tests only; the loop is one thread) may have
    // won: its sender is kept, ours drops and our idle thread exits.
    let _ = WRITER.set(tx);
    Ok(WRITER.get().expect("set above"))
}

fn write_at(path: &Path, name: &str, hash: WidgetHash) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let mut all = read_at(path);
    all.set(name, hash);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("kdl.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    let written = f
        .write_all(render(&all).as_bytes())
        .and_then(|()| f.sync_all())
        .and_then(|()| std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hx(b: u8) -> WidgetHash {
        WidgetHash([b; 32])
    }

    #[test]
    fn parse_takes_good_lines_and_skips_bad_ones() {
        let text = format!(
            "approve \"load\" \"{}\"\napprove \"x\" \"nothex\"\napprove \"y\"\nallow \"z\" \"{}\"\n\
             approve \"\" \"{}\"\napprove \"load\" \"{}\"\napprove name=\"w\" \"{}\"\n",
            hx(1).to_hex(),
            hx(2).to_hex(),
            hx(3).to_hex(),
            hx(4).to_hex(),
            hx(5).to_hex(),
        );
        let a = parse(&text);
        assert_eq!(a, Approvals(vec![("load".into(), hx(4))]));
        assert_eq!(parse("{{{"), Approvals::default());
    }

    #[test]
    fn record_writes_0600_atomically_and_replaces_by_name() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("abyss-approvals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("eclipse/widget-approvals.kdl");
        set_test_path(Some(file.clone()));
        assert_eq!(read(), Approvals::default());
        record_approval("load", hx(1)).unwrap();
        record_approval("we\"ird", hx(2)).unwrap();
        record_approval("load", hx(3)).unwrap();
        let a = read();
        assert_eq!(a.get("load"), Some(hx(3)));
        assert_eq!(a.get("we\"ird"), Some(hx(2)));
        assert_eq!(a.0.len(), 2);
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!file.with_extension("kdl.tmp").exists());
        set_test_path(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_writes_off_thread_in_order() {
        let dir = std::env::temp_dir().join(format!("abyss-approvals-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("eclipse/widget-approvals.kdl");
        set_test_path(Some(file.clone()));
        persist("load", hx(1)).unwrap();
        wait_written();
        assert_eq!(read().get("load"), Some(hx(1)));
        // Two in a row, no wait between: both land, the later wins per name.
        persist("cpu", hx(2)).unwrap();
        persist("load", hx(3)).unwrap();
        wait_written();
        let a = read();
        assert_eq!(
            (a.get("load"), a.get("cpu"), a.0.len()),
            (Some(hx(3)), Some(hx(2)), 2)
        );
        set_test_path(None);
        assert!(
            persist("load", hx(4)).is_err(),
            "no store path is an immediate error"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ADR 0067: only the Trusted UI approval surface writes approvals or
    /// edits the owner's config on a prompt's answer. Outside `trusted_ui/`
    /// and test modules, the only mention of each is its own definition.
    #[test]
    fn only_the_approval_surface_calls_the_answer_helpers() {
        fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        // This crate holds the helpers; the compositor crate holds their only
        // legitimate caller (`trusted_ui/`) and everything else to police.
        let own = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let abyss = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ec-abyss/src");
        let mut files = Vec::new();
        walk(&own, &mut files);
        walk(&abyss, &mut files);
        assert!(files.len() > 20, "scanned too little: {}", abyss.display());
        let mut defs = 0;
        for f in files {
            if f.strip_prefix(&abyss).is_ok_and(|p| p.starts_with("trusted_ui")) {
                continue;
            }
            let text = std::fs::read_to_string(&f).unwrap();
            // Everything above the unit-test module (tests may call anything).
            let live = text.split("\nmod tests {").next().unwrap_or("");
            for line in live.lines() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                for name in [
                    "record_approval",
                    "persist",
                    "revert_widget",
                    "remove_widget",
                    "revert_model_command",
                ] {
                    let call = format!("{name}(");
                    if !t.contains(&call) {
                        continue;
                    }
                    if t.contains(&format!("fn {call}")) {
                        defs += 1;
                        continue;
                    }
                    panic!("{}: `{name}` called outside trusted_ui/: {t}", f.display());
                }
            }
        }
        assert_eq!(defs, 5, "the five helpers were not all found");
    }
}
