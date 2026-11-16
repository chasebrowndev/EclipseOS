// SPDX-License-Identifier: Apache-2.0
//! P-04 §8 fixtures (ADR 0114: the C/ASan fixture re-expressed in Rust):
//! throughput, echo-off, OSC 133 command blocks, input hygiene, and the
//! ADR 0114 node model (per-surface ids, a parent above `terminal`).
//!
//! Every batch is applied to a mirror tree that enforces COMP-09 request
//! validity (no add of a live id, no move into a descendant, parents exist,
//! commit last, strings clean and within limits, at most 16 ext keys). The
//! mirror is a test oracle, not a second publisher.

use std::collections::HashMap;

use ec_term_pub::semantic::{OpKind, Role};
use ec_term_pub::{
    Batch, Config, Error, IdRange, NodeIds, Publisher, BATCH_STRUCTURAL, BATCH_URGENT, EXT_VALUE_MAX,
    MAX_COLS, MAX_LINE_BYTES, MAX_ROWS, PANE_IDS,
};

const MS: u64 = 1_000_000;

// ---------------------------------------------------------------- mirror

#[derive(Default)]
struct Node {
    parent: u32,
    role: u32,
    text: String,
    cursor: i32,
    ext: Vec<(String, String)>,
    children: Vec<u32>,
}

#[derive(Default)]
struct Mirror {
    n: HashMap<u32, Node>,
    root: u32,
    batches: u64,
    structural: u64,
    urgent: u64,
    value_ops: u64,
    ext_ops: u64,
}

fn clean(s: &str, max: usize) {
    assert!(s.len() <= max, "string of {} bytes over {max}", s.len());
    assert!(!s.chars().any(char::is_control), "control in {s:?}");
}

impl Mirror {
    /// The caller's own nodes: a `window` root and one `tab` per id.
    fn with_surface(window: u32, tabs: &[u32]) -> Mirror {
        let mut m = Mirror {
            root: window,
            ..Mirror::default()
        };
        m.n.insert(window, Node::default());
        for &t in tabs {
            m.n.insert(
                t,
                Node {
                    parent: window,
                    role: Role::Tab as u32,
                    ..Node::default()
                },
            );
            m.n.get_mut(&window).unwrap().children.push(t);
        }
        m
    }

    fn node(&self, id: u32) -> &Node {
        self.n.get(&id).unwrap_or_else(|| panic!("node {id} missing"))
    }

    fn ext(&self, id: u32, key: &str) -> Option<&str> {
        self.n
            .get(&id)?
            .ext
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn detach(&mut self, id: u32) {
        let p = std::mem::take(&mut self.n.get_mut(&id).unwrap().parent);
        if let Some(p) = self.n.get_mut(&p) {
            p.children.retain(|&c| c != id);
        }
    }

    fn attach(&mut self, id: u32, parent: u32, index: u32) {
        let p = self.n.get_mut(&parent).expect("parent exists");
        let at = (index as usize).min(p.children.len());
        p.children.insert(at, id);
        self.n.get_mut(&id).unwrap().parent = parent;
    }

    fn remove_subtree(&mut self, id: u32) {
        let n = self.n.remove(&id).expect("live");
        for c in n.children {
            self.remove_subtree(c);
        }
    }

    fn is_self_or_ancestor(&self, a: u32, mut b: u32) -> bool {
        while b != 0 {
            if a == b {
                return true;
            }
            b = self.n.get(&b).map_or(0, |n| n.parent);
        }
        false
    }

    fn apply(&mut self, b: &Batch<'_>) {
        let ops = b.ops();
        assert!(ops.len() >= 2);
        assert_eq!(ops.last().unwrap().kind, OpKind::Commit);
        self.batches += 1;
        if b.flags & BATCH_STRUCTURAL != 0 {
            self.structural += 1;
        }
        if b.flags & BATCH_URGENT != 0 {
            self.urgent += 1;
        }
        let mut saw_tree = false;
        for (i, op) in ops.iter().enumerate() {
            if op.kind != OpKind::SetExt {
                assert!(op.key.is_none());
            }
            match op.kind {
                OpKind::SetRoot => panic!("a pane publisher never sets the surface root"),
                OpKind::AddNode => {
                    assert!(self.n.contains_key(&op.parent), "parent {} missing", op.parent);
                    assert!(!self.n.contains_key(&op.node), "add of live id {}", op.node);
                    self.n.insert(
                        op.node,
                        Node {
                            cursor: -1,
                            ..Node::default()
                        },
                    );
                    self.attach(op.node, op.parent, op.index);
                    saw_tree = true;
                }
                OpKind::MoveNode => {
                    assert!(self.n.contains_key(&op.node) && self.n.contains_key(&op.parent));
                    assert_ne!(op.node, self.root);
                    assert!(!self.is_self_or_ancestor(op.node, op.parent));
                    self.detach(op.node);
                    self.attach(op.node, op.parent, op.index);
                    saw_tree = true;
                }
                OpKind::RemoveNode => {
                    assert!(self.n.contains_key(&op.node) && op.node != self.root);
                    self.detach(op.node);
                    self.remove_subtree(op.node);
                    saw_tree = true;
                }
                OpKind::SetRole => self.n.get_mut(&op.node).expect("live").role = op.role,
                OpKind::SetValueText => {
                    let text = b.str_of(op).expect("text");
                    clean(text, MAX_LINE_BYTES);
                    assert_eq!((op.sel_start, op.sel_end), (-1, -1));
                    let n = self.n.get_mut(&op.node).expect("live");
                    assert_eq!(n.role, Role::TerminalLine as u32);
                    n.text = text.to_owned();
                    n.cursor = op.cursor;
                    self.value_ops += 1;
                }
                OpKind::SetExt => {
                    let key = op.key.expect("key");
                    let val = b.str_of(op).expect("value");
                    clean(key, 31);
                    clean(val, EXT_VALUE_MAX);
                    assert!(!key.starts_with("ext."));
                    let n = self.n.get_mut(&op.node).expect("live");
                    match n.ext.iter_mut().find(|(k, _)| k == key) {
                        Some(e) => e.1 = val.to_owned(),
                        None => {
                            assert!(n.ext.len() < 16, "COMP-09 §7.3: 16 keys");
                            n.ext.push((key.to_owned(), val.to_owned()));
                        }
                    }
                    self.ext_ops += 1;
                }
                OpKind::Commit => assert_eq!(i, ops.len() - 1),
            }
        }
        assert_eq!(b.flags & BATCH_STRUCTURAL != 0, saw_tree);
    }
}

// ---------------------------------------------------------------- toy emulator

/// What an emulator does to the grid, reduced to lines.
struct Term {
    p: Publisher,
    m: Mirror,
    ids: IdRange,
    rows: usize,
    cols: usize,
    crow: usize,
    ccol: usize,
    lines: Vec<String>,
}

impl Term {
    fn new(rows: usize, cols: usize) -> Term {
        Term::with(Config::default(), rows, cols)
    }

    fn with(cfg: Config, rows: usize, cols: usize) -> Term {
        let mut ids = NodeIds::new();
        let window = ids.alloc().unwrap();
        let tab = ids.alloc().unwrap();
        let range = ids.range(PANE_IDS).unwrap();
        let p = Publisher::new(cfg, range, tab, 0, rows as u16, cols as u16).unwrap();
        Term {
            p,
            m: Mirror::with_surface(window, &[tab]),
            ids: range,
            rows,
            cols,
            crow: 0,
            ccol: 0,
            lines: vec![String::new(); rows],
        }
    }

    fn root(&self) -> u32 {
        self.p.terminal_id()
    }

    fn slot(&self, r: usize) -> u32 {
        self.ids.start() + 1 + r as u32
    }

    fn first_group(&self) -> u32 {
        self.ids.start() + 1 + u32::from(MAX_ROWS)
    }

    fn cursor(&mut self) {
        let col = self.ccol.min(self.cols - 1);
        self.p.set_cursor(self.crow as u16, col as u16).unwrap();
    }

    fn print(&mut self, s: &str) {
        let line = &mut self.lines[self.crow];
        line.truncate(self.ccol.min(line.len()));
        line.push_str(s);
        self.ccol += s.len();
        self.p.set_line(self.crow as u16, line.as_bytes()).unwrap();
        self.cursor();
    }

    fn newline(&mut self) {
        if self.crow == self.rows - 1 {
            self.lines.remove(0);
            self.lines.push(String::new());
            self.p.scroll(1);
        } else {
            self.crow += 1;
        }
        self.ccol = 0;
        self.cursor();
    }

    fn osc(&mut self, payload: &str, now: u64) {
        self.p.osc133(payload.as_bytes(), now);
    }

    /// Poll; apply. `Some((ops, flags, first op))` if a batch came out.
    fn poll(&mut self, now: u64) -> Option<(usize, u32)> {
        let b = self.p.poll(now)?;
        self.m.apply(&b);
        Some((b.ops().len(), b.flags))
    }

    /// Poll at the publisher's own deadline until it is idle.
    fn settle(&mut self, mut now: u64) -> u64 {
        for _ in 0..64 {
            let d = self.p.next_deadline();
            if d == u64::MAX {
                return now;
            }
            now = now.max(d);
            self.poll(now);
        }
        panic!("publisher never settled");
    }

    /// After a full publish, every published row equals the emulator's.
    fn check_mirror(&self) {
        for r in 0..self.rows {
            let n = self.m.node(self.slot(r));
            assert_eq!(n.role, Role::TerminalLine as u32);
            assert_eq!(n.text, self.lines[r].trim_end_matches(' '), "row {r}");
            let want = if r == self.crow { self.ccol as i32 } else { -1 };
            assert_eq!(n.cursor, want, "row {r} cursor");
        }
    }

    fn type_command(&mut self, a: &str, b: &str, c: &str, cmd: &str, now: u64) {
        self.osc(a, now);
        self.print("$ ");
        self.osc(b, now);
        self.print(cmd);
        self.newline();
        self.osc(c, now);
    }

    fn group_at(&self, k: usize) -> Option<u32> {
        self.m
            .node(self.root())
            .children
            .iter()
            .copied()
            .filter(|&c| self.m.node(c).role == Role::Group as u32)
            .nth(k)
    }

    fn ngroups(&self) -> usize {
        (0..).take_while(|&k| self.group_at(k).is_some()).count()
    }

    fn child_text(&self, g: u32, i: usize) -> &str {
        &self.m.node(self.m.node(g).children[i]).text
    }
}

// ---------------------------------------------------------------- fixtures

#[test]
fn config_defaults() {
    let c = Config::default();
    assert_eq!(c.min_interval_ns, 200 * MS, "5 Hz, P-04 §7");
    assert_eq!(c.idle_ns, 100 * MS, "P-04 §7");
    assert_eq!(c.downgrade_grace_ns, 500 * MS, "S-05 §5.2");
}

/// P-04 §8 throughput: 10 s of flat-out output. Bounded publish rate, bounded
/// memory, no generation storm, and the published grid still equals the
/// real one. `yes` repaints an identical screen, so line diffing has nothing
/// to send; `seq` changes every row, so the 5 Hz limit is what bounds it.
fn throughput(cmd: &str, distinct: bool) {
    let mut t = Term::new(24, 80);
    for _ in 0..30 {
        t.print("boot log line");
        t.newline();
    }
    let mut now = t.settle(0);
    t.type_command("A", "B", "C", cmd, now);
    now = t.settle(now + MS);
    assert_eq!(t.m.ext(t.first_group(), "command"), Some(cmd));

    let (start, pubs0, struct0) = (now, t.m.batches, t.m.structural);
    let (mut s1, mut max_ops, mut line) = (t.p.stats(), 0, 0u64);
    for ms in 1..=10_000u64 {
        now = start + ms * MS;
        for _ in 0..50 {
            // 50 000 lines/s
            if distinct {
                line += 1;
                t.print(&line.to_string());
            } else {
                t.print("y");
            }
            t.newline();
        }
        if let Some((len, _)) = t.poll(now) {
            max_ops = max_ops.max(len);
        }
        if ms == 1000 {
            s1 = t.p.stats();
        }
    }
    let s10 = t.p.stats();
    let (pubs, structural) = (t.m.batches - pubs0, t.m.structural - struct0);
    println!(
        "   {cmd}: {pubs} publishes, {structural} structural, max {max_ops} ops, {} B retained",
        s10.retained_bytes
    );
    assert!(pubs <= 51, "<= 5 Hz over 10 s");
    if distinct {
        assert!(pubs >= 45, "and it did keep publishing");
    } else {
        assert!(pubs <= 2, "identical screen: nothing new");
    }
    assert!(structural <= 2, "the old rows leaving; no storm");
    assert!(max_ops <= 3 * 24 + 8, "line deltas, not the log");
    assert_eq!(s10.retained_bytes, s1.retained_bytes, "flat after 1 s");
    assert!(s10.retained_bytes < 512 * 1024);

    // ^C: the block ends, the next prompt opens; the tree still matches.
    t.osc("D;130", now);
    t.osc("A", now);
    t.print("$ ");
    t.settle(now + MS);
    t.check_mirror();
    assert_eq!(t.m.ext(t.first_group(), "exit_code"), Some("130"));
}

#[test]
fn throughput_yes() {
    throughput("yes", false);
}

#[test]
fn throughput_seq() {
    throughput("seq 1 inf", true);
}

/// P-04 §8 echo-off: the raise is published before any keystroke can be,
/// nothing leaves while raised, and it holds for downgrade_grace.
fn echo_case(cmd: &str, prompt: &str, feedback: bool) {
    let mut t = Term::new(10, 80);
    let mut now = t.settle(0);
    t.type_command("A", "B", "C", cmd, now);
    now = t.settle(now + MS);
    let (values, exts, batches) = (t.m.value_ops, t.m.ext_ops, t.m.batches);

    // getpass(): ECHO off, then the prompt. The very next poll is the raise,
    // even though a publish went out under a millisecond ago.
    now += MS;
    t.p.set_echo(false, now);
    t.print(prompt);
    assert_eq!(t.p.next_deadline(), 0);
    let term = t.p.terminal_id();
    {
        let b = t.p.poll(now).expect("the raise");
        assert!(b.flags & BATCH_URGENT != 0);
        assert_eq!(b.ops().len(), 2);
        let op = &b.ops()[0];
        assert_eq!((op.kind, op.node), (OpKind::SetExt, term));
        assert_eq!((op.key, b.str_of(op)), (Some("echo"), Some("false")));
        t.m.apply(&b);
    }
    assert_eq!(t.m.ext(t.root(), "echo"), Some("false"));

    // Keystrokes for 2 s: nothing at all is published, prompt included.
    for _ in 0..8 {
        now += 250 * MS;
        if feedback {
            t.print("*");
        }
        assert_eq!(t.p.next_deadline(), u64::MAX);
        assert!(t.poll(now).is_none());
    }
    // Enter: echo back on. Still raised for the grace.
    now += 50 * MS;
    let on = now;
    t.p.set_echo(true, on);
    t.newline();
    t.print("done");
    assert_eq!(t.p.next_deadline(), on + 500 * MS);
    let mut at = on;
    while at < on + 500 * MS {
        assert!(t.poll(at).is_none());
        at += 25 * MS;
    }
    assert_eq!(t.m.value_ops, values);
    assert_eq!(t.m.ext_ops, exts + 1);
    assert_eq!(t.m.batches, batches + 1);

    now = on + 500 * MS;
    let (_, flags) = t.poll(now).expect("downgrade publish");
    assert_eq!(flags & BATCH_URGENT, 0);
    assert_eq!(t.m.ext(t.root(), "echo"), Some("true"));
    t.settle(now);
    t.check_mirror();
}

#[test]
fn echo_off_sudo() {
    echo_case("sudo true", "[sudo] password for user: ", true);
}

#[test]
fn echo_off_ssh() {
    echo_case("ssh host", "user@host's password: ", false);
}

#[test]
fn echo_off_read_s() {
    echo_case("read -s pw", "Password: ", false);
}

#[test]
fn echo_off_flap() {
    let mut t = Term::new(4, 40);
    // ECHO off before the first publish: the raise carries the terminal
    // node and `echo` only.
    t.p.set_echo(false, 0);
    {
        let b = t.p.poll(0).expect("raise");
        let k: Vec<_> = b.ops().iter().map(|o| o.kind).collect();
        assert_eq!(
            k,
            [OpKind::AddNode, OpKind::SetRole, OpKind::SetExt, OpKind::Commit]
        );
        t.m.apply(&b);
    }
    assert_eq!(t.m.value_ops, 0);
    // On, then off again inside the grace: one raise, grace restarts.
    t.p.set_echo(true, 100 * MS);
    assert!(t.poll(400 * MS).is_none());
    t.p.set_echo(false, 450 * MS);
    assert!(t.poll(450 * MS).is_none());
    assert!(t.poll(700 * MS).is_none());
    t.p.set_echo(true, 800 * MS);
    assert!(t.poll(1299 * MS).is_none());
    assert!(t.poll(1300 * MS).is_some());
    assert_eq!(t.m.urgent, 1);
    assert_eq!(t.m.ext(t.root(), "echo"), Some("true"));

    // A config cannot shorten the grace.
    let short = Config {
        downgrade_grace_ns: 0,
        ..Config::default()
    };
    let mut t = Term::with(short, 4, 40);
    assert!(t.poll(0).is_some());
    t.p.set_echo(false, 0);
    assert!(t.poll(0).is_some_and(|(_, f)| f & BATCH_URGENT != 0));
    t.p.set_echo(true, MS);
    assert!(t.poll(400 * MS).is_none());
    assert!(t.poll(501 * MS).is_some());
}

struct Shell {
    a: &'static str,
    b: &'static str,
    c: &'static str,
    d: fn(i32) -> String,
}

/// P-04 §8 OSC 133: command blocks, exit codes and prompt detection for the
/// marker dialects bash, zsh and fish emit.
fn osc_case(sh: &Shell) {
    let mut t = Term::new(12, 80);
    let mut now = t.settle(0);
    let runs: [(&str, Option<&str>, i32); 3] = [
        ("echo hello", Some("hello"), 0),
        ("false", None, 1),
        ("ls /nope", Some("ls: cannot access '/nope'"), 2),
    ];
    for &(cmd, out, exit) in &runs {
        t.type_command(sh.a, sh.b, sh.c, cmd, now);
        now = t.settle(now + MS);
        if let Some(o) = out {
            t.print(o);
            t.newline();
        }
        t.osc(&(sh.d)(exit), now);
        now = t.settle(now + MS);
    }
    t.osc(sh.a, now);
    t.print("$ ");
    t.osc(sh.b, now);
    t.print("gi");
    now = t.settle(now + MS);
    t.check_mirror();

    assert_eq!(t.ngroups(), 4);
    for (i, &(cmd, out, exit)) in runs.iter().enumerate() {
        let g = t.group_at(i).unwrap();
        assert_eq!(t.m.ext(g, "command"), Some(cmd));
        assert_eq!(t.m.ext(g, "exit_code"), Some(exit.to_string().as_str()));
        assert_eq!(t.m.ext(g, "is_prompt"), Some("false"));
        assert!(t.m.ext(g, "started").is_some() && t.m.ext(g, "ended").is_some());
        assert_eq!(t.child_text(g, 0), format!("$ {cmd}"));
        assert_eq!(t.m.node(g).children.len(), if out.is_some() { 2 } else { 1 });
        if let Some(o) = out {
            assert_eq!(t.child_text(g, 1), o);
        }
        assert_eq!(t.m.ext(t.m.node(g).children[0], "is_prompt"), Some("false"));
    }
    let cur = t.group_at(3).unwrap();
    assert_eq!(t.m.ext(cur, "is_prompt"), Some("true"));
    assert!(t.m.ext(cur, "command").is_none() && t.m.ext(cur, "exit_code").is_none());
    assert_eq!(t.m.node(cur).children.len(), 1);
    assert_eq!(t.child_text(cur, 0), "$ gi");
    assert_eq!(t.m.ext(t.m.node(cur).children[0], "is_prompt"), Some("true"));
    // Rows below the cursor are nobody's output.
    assert_eq!(t.m.node(t.slot(11)).parent, t.root());

    // Alternate screen: markers ignored, rows leave the groups, and come
    // back on exit.
    let s0 = t.m.structural;
    t.p.set_alt_screen(true);
    t.osc(sh.a, now);
    now = t.settle(now + MS);
    assert_eq!(t.m.structural, s0 + 1);
    assert_eq!(t.m.ext(t.root(), "alt_screen"), Some("true"));
    for i in 0..4 {
        assert!(t.m.node(t.group_at(i).unwrap()).children.is_empty());
    }
    t.p.set_alt_screen(false);
    t.settle(now + MS);
    assert_eq!(t.ngroups(), 4);
    assert_eq!(t.m.node(cur).children.len(), 1);
    t.check_mirror();
}

#[test]
fn osc133_bash() {
    osc_case(&Shell {
        a: "A",
        b: "B",
        c: "C",
        d: |e| format!("D;{e}"),
    });
}

#[test]
fn osc133_zsh() {
    osc_case(&Shell {
        a: "A;cl=m;aid=4242",
        b: "B",
        c: "C",
        d: |e| format!("133;D;{e};aid=4242"),
    });
}

#[test]
fn osc133_fish() {
    osc_case(&Shell {
        a: "A;special_key=1",
        b: "B",
        c: "C;cmdline_url=x",
        d: |e| format!("D;{e}"),
    });
}

#[test]
fn osc133_long_command() {
    let mut t = Term::new(8, 400);
    let now = t.settle(0);
    let cmd = "x".repeat(389);
    t.type_command("A", "B", "C", &cmd, now);
    t.settle(now + MS);
    let g = t.first_group();
    assert_eq!(t.m.ext(g, "command").map(str::len), Some(EXT_VALUE_MAX));
    assert_eq!(t.m.ext(g, "command_truncated"), Some("true"));
}

/// A program spamming OSC 133 cannot drive a publish or generation storm.
#[test]
fn osc133_flood() {
    let mut t = Term::new(24, 80);
    let mut now = 0;
    for ms in 1..=10_000u64 {
        now = ms * MS;
        for _ in 0..20 {
            t.osc("A", now);
            t.print("x");
            t.osc("C", now);
            t.newline();
            t.osc("D;0", now);
        }
        t.poll(now);
    }
    let s = t.p.stats();
    println!(
        "   flood: {} publishes, {} blocks live, {} B retained",
        t.m.batches, s.blocks, s.retained_bytes
    );
    assert!(t.m.batches <= 101, "10 Hz boundary cap, P-07 §3");
    assert!(s.blocks <= 64);
    assert!(s.retained_bytes < 1024 * 1024);
    t.settle(now);
    t.check_mirror();
}

/// Sizes, hostile input and many publishers driven through a resize (the
/// Rust form of the old ownership/lifetime fixture).
#[test]
fn sizes_and_hostile_input() {
    let mut ids = NodeIds::new();
    let tab = ids.alloc().unwrap();
    let r = ids.range(PANE_IDS).unwrap();
    for (rows, cols) in [(0, 80), (24, 0), (MAX_ROWS + 1, 80), (24, MAX_COLS + 1)] {
        assert_eq!(
            Publisher::new(Config::default(), r, tab, 0, rows, cols).err(),
            Some(Error::Range)
        );
    }
    let mut p = Publisher::new(Config::default(), r, tab, 0, 3, 10).unwrap();
    assert_eq!(p.set_line(3, b"x"), Err(Error::Range));
    assert_eq!(p.set_cursor(0, 10), Err(Error::Range));
    assert_eq!(p.resize(0, 10), Err(Error::Range));
    assert_eq!(p.set_line(0, b""), Ok(()));

    // Input is copied; hostile bytes (NUL, ESC, C1, bad UTF-8, oversize)
    // come out as safe text.
    let mut buf = b"abc".to_vec();
    p.set_line(0, &buf).unwrap();
    buf.copy_from_slice(b"zzz");
    p.set_line(1, b"a\0b\x1b]133;D\x07\xc2\x9b\xff\xfe end").unwrap();
    p.set_line(2, &[b'w'; 8000]).unwrap();
    let mut m = Mirror::with_surface(ids.alloc().unwrap(), &[tab]);
    {
        let b = p.poll(0).unwrap();
        m.apply(&b); // validates every string in the batch
    }
    let slot = |row: u32| r.start() + 1 + row;
    assert_eq!(m.node(slot(0)).text, "abc");
    assert_eq!(m.node(slot(2)).text.len(), MAX_LINE_BYTES);
    assert!(m.node(slot(1)).text.contains("end"));

    // Many publishers, each driven through a resize.
    for k in 0..50usize {
        let mut t = Term::new(5 + k % 7, 20 + k);
        let now = t.settle(0);
        t.type_command("A", "B", "C", "make", now);
        t.print("building");
        t.newline();
        t.p.resize(4, 30).unwrap();
        t.rows = 4;
        t.cols = 30;
        t.lines.truncate(4);
        t.crow = t.crow.min(3);
        t.ccol = 0;
        t.cursor();
        for row in 0..4 {
            let l = t.lines[row].clone();
            t.p.set_line(row as u16, l.as_bytes()).unwrap();
        }
        t.settle(now + MS);
        t.check_mirror();
    }
}

/// ADR 0114: two panes of one surface publish into one tree under their own
/// tabs, with disjoint ids; closing one and opening another never reuses an
/// id, and the caller can move a terminal between tabs.
#[test]
fn two_panes_share_one_surface() {
    let mut ids = NodeIds::new();
    let window = ids.alloc().unwrap();
    let (tab1, tab2) = (ids.alloc().unwrap(), ids.alloc().unwrap());
    let mut m = Mirror::with_surface(window, &[tab1, tab2]);
    let mut a = Publisher::new(Config::default(), ids.range(PANE_IDS).unwrap(), tab1, 0, 3, 20).unwrap();
    let mut b = Publisher::new(Config::default(), ids.range(PANE_IDS).unwrap(), tab2, 0, 2, 20).unwrap();
    assert!(!a.ids().contains(b.terminal_id()) && !b.ids().contains(a.terminal_id()));
    a.set_line(0, b"left").unwrap();
    b.set_line(0, b"right").unwrap();
    a.osc133(b"A", 0);
    b.osc133(b"A", 0);
    m.apply(&a.poll(0).unwrap());
    m.apply(&b.poll(0).unwrap());
    assert_eq!(m.node(tab1).children, [a.terminal_id()]);
    assert_eq!(m.node(tab2).children, [b.terminal_id()]);
    assert_eq!(m.node(a.terminal_id()).role, Role::Terminal as u32);
    assert_eq!(m.node(a.terminal_id() + 1).text, "left");
    assert_eq!(m.node(b.terminal_id() + 1).text, "right");

    // Pane b closes: the caller removes its subtree. A new pane gets new ids.
    let gone = b.ids();
    m.detach(b.terminal_id());
    m.remove_subtree(b.terminal_id());
    drop(b);
    let c = Publisher::new(Config::default(), ids.range(PANE_IDS).unwrap(), tab2, 0, 2, 20).unwrap();
    assert!(!gone.contains(c.terminal_id()) && !a.ids().contains(c.terminal_id()));
    let mut c = c;
    m.apply(&c.poll(0).unwrap());
    assert_eq!(m.node(tab2).children, [c.terminal_id()]);
    // Unrelated rows of pane a still publish.
    a.set_line(1, b"more").unwrap();
    m.apply(&a.poll(500 * MS).unwrap());
    assert_eq!(m.node(a.terminal_id() + 2).text, "more");
}
