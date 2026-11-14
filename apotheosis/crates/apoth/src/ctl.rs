// SPDX-License-Identifier: AGPL-3.0-only
//! The socket commands (T-12 §5): `ls`, `launch`, `split`, `focus`, `close`, `send-text`,
//! `send-keys`, `get-text`, `do`, `subscribe`, `version`. Each command becomes one or more
//! requests ([`plan`]), goes over the socket ([`Client`]) and is printed ([`render`]).
//! Parsing and printing are pure so the goldens below pin them. Exit codes: 0 ok, 1 error,
//! 2 usage, 3 denied, 4 no instance.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use apoth_action::{Action, Call, EventKind};
use apoth_proto::params::{
    ActionRun, Empty, Extent, Launch, PaneText, SendKeys, Subscribe, TargetOnly,
};
use apoth_proto::results::{LaunchReply, PaneTextReply, TreeReply, VersionReply};
use apoth_proto::{
    ENV_SOCKET, ErrorCode, LineDecoder, Params, Reply, Request, ServerMessage, WireError, WireEvent,
};
use serde_json::Value;

pub(crate) const OK: i32 = 0;
pub(crate) const ERROR: i32 = 1;
pub(crate) const USAGE: i32 = 2;

/// How long a connect or a reply may take before the instance counts as not answering.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Commands T-12 §5 names that this build does not have yet.
pub(crate) const NOT_YET: &[&str] = &[
    "notify",
    "set",
    "reset",
    "play",
    "ssh",
    "terminfo",
    "fonts",
    "doctor",
    "completions",
];

/// Socket commands.
pub(crate) const COMMANDS: &[&str] = &[
    "ls",
    "launch",
    "split",
    "focus",
    "close",
    "send-text",
    "send-keys",
    "get-text",
    "do",
    "subscribe",
    "version",
];

pub(crate) const HELP: &str = "\
Usage: apoth [--json] [--match TARGET] [--socket PATH] <command> [ARGS...]

Commands:
  ls                         list windows, tabs and panes
  launch [--cwd DIR] [--title T] [--wait] [-- COMMAND...]
                             open a tab in the running instance
  split [right|down|left|up] [--cwd DIR] [-- COMMAND...]
  focus [left|right|up|down|next|prev|last]
                             focus a neighbour, or the --match pane
  close [--force]            close the pane
  send-text TEXT             type TEXT into the pane
  send-keys CHORD...         press keys by xkb keysym (ctrl+c, Return); never fires binds
  get-text [--extent screen|scrollback|all] [--wrap]
  do 'ACTION; ACTION'        run actions (KDL `do` syntax)
  subscribe EVENT...         print events until interrupted
  version                    client and instance versions
  icat [OPTIONS] FILE...     show images in the pane (apoth icat --help)
  shell-init bash|zsh|fish   print the shell-integration snippet (defines `ap`)
  config ...                 edit or read the config file (no socket)

TARGET is self (the default inside a pane), focused, or id:N.
Exit codes: 0 ok, 1 error, 2 usage, 3 denied, 4 no instance.
";

/// Global options.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Opts {
    pub(crate) json: bool,
    pub(crate) target: Option<String>,
    pub(crate) socket: Option<PathBuf>,
}

/// How a command's last reply is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Show {
    Nothing,
    Tree,
    Text,
    Version,
    Launch { wait: bool },
    Events,
}

/// A command as requests.
#[derive(Debug, PartialEq)]
pub(crate) struct Plan {
    pub(crate) requests: Vec<Params>,
    pub(crate) show: Show,
}

/// Split global options from the command and its arguments.
pub(crate) fn parse_global(args: &[String]) -> Result<(Opts, &[String]), String> {
    let mut o = Opts::default();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--json" => o.json = true,
            "-m" | "--match" => {
                i += 1;
                o.target = Some(args.get(i).ok_or("--match needs a target")?.clone());
            }
            "--socket" => {
                i += 1;
                o.socket = Some(PathBuf::from(
                    args.get(i).ok_or("--socket needs a path")?.clone(),
                ));
            }
            _ => break,
        }
        i += 1;
    }
    Ok((o, &args[i..]))
}

/// Per-command flags may also follow the command; `--` ends them.
struct Rest<'a> {
    args: &'a [String],
    i: usize,
}

impl<'a> Rest<'a> {
    fn new(args: &'a [String]) -> Rest<'a> {
        Rest { args, i: 0 }
    }

    fn next(&mut self) -> Option<&'a str> {
        let a = self.args.get(self.i)?;
        self.i += 1;
        Some(a)
    }

    fn value(&mut self, flag: &str) -> Result<String, String> {
        self.next()
            .map(str::to_owned)
            .ok_or_else(|| format!("{flag} needs a value"))
    }

    fn tail(&mut self) -> Vec<String> {
        let t = self.args[self.i..].to_vec();
        self.i = self.args.len();
        t
    }
}

fn action_run(call: &Call, target: &Option<String>) -> Result<Params, String> {
    let text = call.format();
    // Checked here so a typo is a usage error, not a round trip.
    Action::parse(&text).map_err(|e| e.message)?;
    Ok(Params::ActionRun(ActionRun {
        action: text,
        target: target.clone(),
    }))
}

/// The requests for `cmd args` (global options already taken).
pub(crate) fn plan(cmd: &str, args: &[String], o: &mut Opts) -> Result<Plan, String> {
    let mut r = Rest::new(args);
    let flags_target = |r: &mut Rest, a: &str, o: &mut Opts| -> Result<bool, String> {
        match a {
            "-m" | "--match" => {
                o.target = Some(r.value(a)?);
                Ok(true)
            }
            "--json" => {
                o.json = true;
                Ok(true)
            }
            _ => Ok(false),
        }
    };
    let target = |o: &Opts| o.target.clone();
    let plan = match cmd {
        "ls" | "version" => {
            while let Some(a) = r.next() {
                if !flags_target(&mut r, a, o)? {
                    return Err(format!("{cmd} takes no argument {a:?}"));
                }
            }
            if cmd == "ls" {
                Plan {
                    requests: vec![Params::Tree(TargetOnly::default())],
                    show: Show::Tree,
                }
            } else {
                Plan {
                    requests: vec![Params::Version(Empty {})],
                    show: Show::Version,
                }
            }
        }
        "launch" => {
            let mut l = Launch::default();
            while let Some(a) = r.next() {
                match a {
                    "--cwd" => l.cwd = Some(absolute(&r.value(a)?)),
                    "--title" => l.title = Some(r.value(a)?),
                    "--wait" => l.wait = true,
                    "--hold" => l.hold = true,
                    "--profile" => l.profile = Some(r.value(a)?),
                    "--layout" => l.layout = Some(r.value(a)?),
                    "--" => {
                        l.argv = r.tail();
                        break;
                    }
                    _ if flags_target(&mut r, a, o)? => {}
                    _ => return Err(format!("launch: unexpected {a:?}")),
                }
            }
            if l.cwd.is_none() {
                l.cwd = std::env::current_dir()
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned());
            }
            l.activation_token = std::env::var("XDG_ACTIVATION_TOKEN").ok();
            l.env = launch_env();
            let wait = l.wait;
            Plan {
                requests: vec![Params::Launch(Box::new(l))],
                show: Show::Launch { wait },
            }
        }
        "split" => {
            let mut c = Call::new("pane.split");
            let mut dir = None;
            let mut argv = None;
            while let Some(a) = r.next() {
                match a {
                    "right" | "down" | "left" | "up" if dir.is_none() => dir = Some(a),
                    "--cwd" => c.named.push(("cwd".into(), absolute(&r.value(a)?))),
                    "--" => {
                        argv = Some(r.tail());
                        break;
                    }
                    _ if flags_target(&mut r, a, o)? => {}
                    _ => return Err(format!("split: unexpected {a:?}")),
                }
            }
            c.positional.push(dir.unwrap_or("right").to_owned());
            c.argv = argv.filter(|v| !v.is_empty());
            Plan {
                requests: vec![action_run(&c, &target(o))?],
                show: Show::Nothing,
            }
        }
        "focus" => {
            let mut c = None;
            while let Some(a) = r.next() {
                match a {
                    "left" | "right" | "up" | "down" if c.is_none() => {
                        let mut x = Call::new("pane.focus");
                        x.positional.push(a.to_owned());
                        c = Some(x);
                    }
                    "next" | "prev" | "last" if c.is_none() => {
                        c = Some(Call::new(format!("pane.focus-{a}")));
                    }
                    _ if flags_target(&mut r, a, o)? => {}
                    _ => return Err(format!("focus: unexpected {a:?}")),
                }
            }
            let c = match c {
                Some(c) => c,
                // A targeted no-op: the instance shows and focuses the target first.
                None if o.target.is_some() => Call::new("app.ignore"),
                None => return Err("focus needs a direction or --match".into()),
            };
            Plan {
                requests: vec![action_run(&c, &target(o))?],
                show: Show::Nothing,
            }
        }
        "close" => {
            let mut c = Call::new("pane.close");
            while let Some(a) = r.next() {
                match a {
                    "--force" => c.named.push(("force".into(), "true".into())),
                    _ if flags_target(&mut r, a, o)? => {}
                    _ => return Err(format!("close: unexpected {a:?}")),
                }
            }
            Plan {
                requests: vec![action_run(&c, &target(o))?],
                show: Show::Nothing,
            }
        }
        "send-text" => {
            let mut text = None;
            while let Some(a) = r.next() {
                match a {
                    "--" if text.is_none() => text = Some(r.tail().join(" ")),
                    _ if flags_target(&mut r, a, o)? => {}
                    _ if text.is_none() => text = Some(a.to_owned()),
                    _ => return Err("send-text takes one TEXT (quote it)".into()),
                }
            }
            let text = text.ok_or("send-text needs TEXT")?;
            let mut c = Call::new("input.send-text");
            c.positional.push(text);
            Plan {
                requests: vec![action_run(&c, &target(o))?],
                show: Show::Nothing,
            }
        }
        "send-keys" => {
            let mut keys = Vec::new();
            while let Some(a) = r.next() {
                if !flags_target(&mut r, a, o)? {
                    apoth_action::KeyChord::parse(a).map_err(|e| e.message)?;
                    keys.push(a.to_owned());
                }
            }
            if keys.is_empty() {
                return Err("send-keys needs at least one CHORD".into());
            }
            Plan {
                requests: vec![Params::InputSendKeys(SendKeys {
                    keys,
                    target: target(o),
                })],
                show: Show::Nothing,
            }
        }
        "get-text" => {
            let mut p = PaneText::default();
            while let Some(a) = r.next() {
                match a {
                    "--extent" => {
                        p.extent = match r.value(a)?.as_str() {
                            "screen" => Extent::Screen,
                            "scrollback" => Extent::Scrollback,
                            "all" => Extent::All,
                            "selection" => Extent::Selection,
                            other => return Err(format!("unknown extent {other:?}")),
                        }
                    }
                    "--wrap" => p.wrap = true,
                    "--sgr" => p.sgr = true,
                    _ if flags_target(&mut r, a, o)? => {}
                    _ => return Err(format!("get-text: unexpected {a:?}")),
                }
            }
            p.target = target(o);
            Plan {
                requests: vec![Params::PaneText(p)],
                show: Show::Text,
            }
        }
        "do" => {
            let mut script = None;
            while let Some(a) = r.next() {
                match a {
                    _ if flags_target(&mut r, a, o)? => {}
                    _ if script.is_none() => script = Some(a.to_owned()),
                    _ => return Err("do takes one script (quote it)".into()),
                }
            }
            let script = script.ok_or("do needs a script")?;
            let actions = Action::parse_script(&script).map_err(|e| e.message)?;
            if actions.is_empty() {
                return Err("do needs at least one action".into());
            }
            Plan {
                requests: actions
                    .iter()
                    .map(|a| {
                        Params::ActionRun(ActionRun {
                            action: a.to_string(),
                            target: target(o),
                        })
                    })
                    .collect(),
                show: Show::Nothing,
            }
        }
        "subscribe" => {
            let mut events = Vec::new();
            while let Some(a) = r.next() {
                if !flags_target(&mut r, a, o)? {
                    if EventKind::parse(a).is_none() {
                        return Err(format!("unknown event {a:?}"));
                    }
                    events.push(a.to_owned());
                }
            }
            if events.is_empty() {
                return Err("subscribe needs at least one EVENT".into());
            }
            Plan {
                requests: vec![Params::Subscribe(Subscribe {
                    events,
                    match_expr: target(o),
                })],
                show: Show::Events,
            }
        }
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok(plan)
}

/// The environment a launch carries (T-12 §5), bounded to what the request may hold.
pub(crate) fn launch_env() -> BTreeMap<String, String> {
    use apoth_proto::limits::{MAX_ELEM_BYTES, MAX_ENV, MAX_SHORT_BYTES};
    // Half the line limit, so the rest of the request always fits.
    let mut budget = apoth_proto::MAX_LINE_BYTES / 2;
    let mut m = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        let (Ok(k), Ok(v)) = (k.into_string(), v.into_string()) else {
            continue;
        };
        let cost = k.len() + v.len() + 8;
        if k.is_empty() || k.len() > MAX_SHORT_BYTES || v.len() > MAX_ELEM_BYTES || cost > budget {
            continue;
        }
        budget -= cost;
        m.insert(k, v);
        if m.len() == MAX_ENV {
            break;
        }
    }
    m
}

fn absolute(p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() {
        return p.to_owned();
    }
    std::env::current_dir()
        .map(|d| d.join(path).to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_owned())
}

// ------------------------------------------------------------------ transport

/// Why no instance was reached.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NoInstance(pub(crate) String);

/// Live sockets in `dir`, newest first.
pub(crate) fn candidates(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .filter_map(|p| {
            let t = std::fs::symlink_metadata(&p).ok()?.modified().ok()?;
            Some((t, p))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    v.into_iter().map(|(_, p)| p).collect()
}

/// Connect: `--socket`, else `$APOTHEOSIS_SOCKET`, else the newest live socket in
/// `$XDG_RUNTIME_DIR/apotheosis/`.
pub(crate) fn connect(explicit: Option<&Path>) -> Result<UnixStream, NoInstance> {
    if let Some(p) = explicit {
        return UnixStream::connect(p).map_err(|e| NoInstance(format!("{}: {e}", p.display())));
    }
    if let Some(p) = std::env::var_os(ENV_SOCKET).filter(|p| !p.is_empty())
        && let Ok(s) = UnixStream::connect(&p)
    {
        return Ok(s);
    }
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("apotheosis"))
    else {
        return Err(NoInstance("XDG_RUNTIME_DIR is not set".into()));
    };
    candidates(&dir)
        .iter()
        .find_map(|p| UnixStream::connect(p).ok())
        .ok_or_else(|| NoInstance("no running apotheosis instance".into()))
}

/// A blocking client.
#[derive(Debug)]
pub(crate) struct Client {
    stream: UnixStream,
    dec: LineDecoder,
    next_id: u64,
    lines: std::collections::VecDeque<Result<Vec<u8>, WireError>>,
    events: std::collections::VecDeque<WireEvent>,
}

impl Client {
    pub(crate) fn new(stream: UnixStream) -> io::Result<Client> {
        stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
        stream.set_write_timeout(Some(REPLY_TIMEOUT))?;
        Ok(Client {
            stream,
            dec: LineDecoder::new(),
            next_id: 1,
            lines: std::collections::VecDeque::new(),
            events: std::collections::VecDeque::new(),
        })
    }

    /// No read timeout (a `--wait` launch or a subscription may be idle indefinitely).
    pub(crate) fn unbounded(&mut self) -> io::Result<()> {
        self.stream.set_read_timeout(None)
    }

    fn read_message(&mut self) -> Result<ServerMessage, WireError> {
        let mut buf = [0u8; 16 * 1024];
        loop {
            if let Some(line) = self.lines.pop_front() {
                return ServerMessage::decode(&line?);
            }
            let n = match self.stream.read(&mut buf) {
                Ok(0) => {
                    return Err(WireError::new(
                        ErrorCode::NoInstance,
                        "the instance closed the connection",
                    ));
                }
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(WireError::new(
                        ErrorCode::NoInstance,
                        format!("the instance did not answer: {}", e.kind()),
                    ));
                }
            };
            self.lines.extend(self.dec.feed(&buf[..n]));
        }
    }

    /// One request, its reply. Events that arrive meanwhile are kept for [`Client::event`].
    pub(crate) fn call(&mut self, params: Params) -> Result<Value, WireError> {
        let id = self.send(params)?;
        self.reply(id)
    }

    /// Write one request; its id for [`Client::reply`].
    pub(crate) fn send(&mut self, params: Params) -> Result<u64, WireError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = Request { id, params }.to_line()?;
        self.stream.write_all(&line).map_err(|e| {
            WireError::new(
                ErrorCode::NoInstance,
                format!("cannot write to the instance: {}", e.kind()),
            )
        })?;
        Ok(id)
    }

    /// The socket, to poll for a deferred reply alongside another fd.
    pub(crate) fn stream(&self) -> &UnixStream {
        &self.stream
    }

    /// Lines already read but not yet taken (poll would not see them).
    pub(crate) fn buffered(&self) -> bool {
        !self.lines.is_empty()
    }

    /// The reply to request `id`.
    pub(crate) fn reply(&mut self, id: u64) -> Result<Value, WireError> {
        loop {
            match self.read_message()? {
                ServerMessage::Reply(Reply::Ok { id: rid, ok }) if rid == id => return Ok(ok),
                ServerMessage::Reply(Reply::Err { id: rid, error })
                    if rid == Some(id) || rid.is_none() =>
                {
                    return Err(error);
                }
                ServerMessage::Reply(_) => {}
                ServerMessage::Event(e) => self.events.push_back(e),
            }
        }
    }

    /// The next event.
    pub(crate) fn event(&mut self) -> Result<WireEvent, WireError> {
        if let Some(e) = self.events.pop_front() {
            return Ok(e);
        }
        loop {
            if let ServerMessage::Event(e) = self.read_message()? {
                return Ok(e);
            }
        }
    }
}

// ------------------------------------------------------------------ output

/// Print a reply. Returns the exit code (a `--wait` launch relays the child's).
pub(crate) fn render(show: Show, v: &Value, json: bool, out: &mut dyn Write) -> io::Result<i32> {
    if json {
        writeln!(out, "{v}")?;
        return Ok(match show {
            Show::Launch { wait: true } => launch_exit(v),
            _ => OK,
        });
    }
    match show {
        Show::Nothing | Show::Events => {}
        Show::Tree => {
            let t: TreeReply = serde_json::from_value(v.clone()).map_err(io::Error::other)?;
            for w in &t.windows {
                writeln!(out, "window {}", w.id)?;
                for tab in &w.tabs {
                    writeln!(
                        out,
                        "  tab {}  {}",
                        tab.id,
                        tab.title.as_deref().unwrap_or("-")
                    )?;
                    for p in &tab.panes {
                        let state = serde_json::to_value(p.state)
                            .ok()
                            .and_then(|s| s.as_str().map(str::to_owned))
                            .unwrap_or_default();
                        writeln!(
                            out,
                            "    pane {}  {}x{}  {state}  {}  {}",
                            p.id,
                            p.cols,
                            p.rows,
                            p.title.as_deref().unwrap_or("-"),
                            p.cwd.as_deref().unwrap_or("-"),
                        )?;
                    }
                }
            }
        }
        Show::Text => {
            let t: PaneTextReply = serde_json::from_value(v.clone()).map_err(io::Error::other)?;
            out.write_all(t.text.as_bytes())?;
            if t.truncated {
                // stdout carries the text only; the cut is reported on the exit path.
                return Ok(OK);
            }
        }
        Show::Version => {
            let r: VersionReply = serde_json::from_value(v.clone()).map_err(io::Error::other)?;
            writeln!(out, "apoth {}", env!("CARGO_PKG_VERSION"))?;
            writeln!(out, "apotheosis {} (protocol {})", r.version, r.proto)?;
        }
        Show::Launch { wait } => {
            let r: LaunchReply = serde_json::from_value(v.clone()).map_err(io::Error::other)?;
            if !wait {
                writeln!(out, "{}", r.pane)?;
            }
            return Ok(if wait { launch_exit(v) } else { OK });
        }
    }
    Ok(OK)
}

/// The exit code a `--wait` launch relays: the child's, or 128+n for death by signal n.
fn launch_exit(v: &Value) -> i32 {
    match v.get("exit").and_then(Value::as_i64) {
        Some(c) if c >= 0 => (c & 0xff) as i32,
        Some(c) => 128 + ((-c) & 0x7f) as i32,
        None => OK,
    }
}

/// One event as text: `name seq key=value ...` (JSON with `--json`).
pub(crate) fn render_event(e: &WireEvent, json: bool, out: &mut dyn Write) -> io::Result<()> {
    if json {
        return writeln!(out, "{}", e.to_value());
    }
    let mut s = format!(
        "{} {}",
        e.kind.as_str(),
        apoth_proto::event::cause_name(e.cause)
    );
    for (k, v) in &e.fields {
        s.push(' ');
        s.push_str(k);
        s.push('=');
        match v {
            None => s.push('-'),
            Some(apoth_action::FieldValue::Str(t)) => {
                s.push_str(&serde_json::Value::from(t.as_str()).to_string());
            }
            Some(apoth_action::FieldValue::Int(i)) => s.push_str(&i.to_string()),
            Some(apoth_action::FieldValue::Bool(b)) => {
                s.push_str(if *b { "true" } else { "false" })
            }
        }
    }
    writeln!(out, "{s}")
}

/// Print an error; its exit code.
pub(crate) fn fail(e: &WireError, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if json {
        let mut m = BTreeMap::new();
        m.insert("error", serde_json::to_value(e).unwrap_or(Value::Null));
        let _ = writeln!(out, "{}", serde_json::to_value(m).unwrap_or(Value::Null));
    } else {
        let _ = writeln!(err, "apoth: {}: {}", e.code, e.message);
    }
    i32::from(e.code.exit_code())
}

/// Run a socket command. `args` starts after the global options.
pub(crate) fn run(
    cmd: &str,
    args: &[String],
    mut o: Opts,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let p = match plan(cmd, args, &mut o) {
        Ok(p) => p,
        Err(m) => {
            let _ = writeln!(err, "apoth {cmd}: {m}");
            return USAGE;
        }
    };
    let stream = match connect(o.socket.as_deref()) {
        Ok(s) => s,
        Err(NoInstance(m)) => {
            return fail(&WireError::new(ErrorCode::NoInstance, m), o.json, out, err);
        }
    };
    let mut c = match Client::new(stream) {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "apoth: {e}");
            return ERROR;
        }
    };
    let hello = Params::Hello(apoth_proto::params::Hello {
        proto: apoth_proto::message::PROTO_VERSION,
        client: "apoth".into(),
        want: Vec::new(),
    });
    if let Err(e) = c.call(hello) {
        return fail(&e, o.json, out, err);
    }
    let long = matches!(p.show, Show::Events | Show::Launch { wait: true });
    if long && c.unbounded().is_err() {
        return ERROR;
    }
    let mut last = Value::Null;
    for req in p.requests {
        match c.call(req) {
            Ok(v) => last = v,
            Err(e) => return fail(&e, o.json, out, err),
        }
    }
    if p.show == Show::Events {
        loop {
            match c.event() {
                Ok(e) => {
                    if render_event(&e, o.json, out).is_err() || out.flush().is_err() {
                        return OK; // stdout closed: the reader is gone
                    }
                }
                Err(e) if e.code == ErrorCode::NoInstance => return OK,
                Err(e) => return fail(&e, o.json, out, err),
            }
        }
    }
    let truncated = p.show == Show::Text && last.get("truncated") == Some(&Value::Bool(true));
    match render(p.show, &last, o.json, out) {
        Ok(code) => {
            if truncated && !o.json {
                let _ = writeln!(
                    err,
                    "apoth: the text was cut to the newest part (size limit)"
                );
            }
            code
        }
        Err(e) => {
            let _ = writeln!(err, "apoth: {e}");
            ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    /// The wire line of each request of `apoth <args>`.
    fn wire(args: &[&str]) -> Result<Vec<String>, String> {
        let args = s(args);
        let (mut o, rest) = parse_global(&args)?;
        let (cmd, rest) = rest.split_first().ok_or("no command")?;
        let p = plan(cmd, rest, &mut o)?;
        Ok(p.requests
            .into_iter()
            .map(|params| {
                let l = Request { id: 1, params }.to_line().unwrap();
                String::from_utf8(l).unwrap().trim_end().to_owned()
            })
            .collect())
    }

    #[test]
    fn golden_requests() {
        let cases: &[(&[&str], &[&str])] = &[
            (&["ls"], &[r#"{"id":1,"method":"tree","params":{}}"#]),
            (
                &["version"],
                &[r#"{"id":1,"method":"version","params":{}}"#],
            ),
            (
                &["--match", "id:4", "split", "down", "--", "htop", "-d", "5"],
                &[
                    r#"{"id":1,"method":"action.run","params":{"action":"pane.split down -- htop -d 5","target":"id:4"}}"#,
                ],
            ),
            (
                &["split"],
                &[r#"{"id":1,"method":"action.run","params":{"action":"pane.split right"}}"#],
            ),
            (
                &["focus", "next"],
                &[r#"{"id":1,"method":"action.run","params":{"action":"pane.focus-next"}}"#],
            ),
            (
                &["focus", "-m", "id:7"],
                &[
                    r#"{"id":1,"method":"action.run","params":{"action":"app.ignore","target":"id:7"}}"#,
                ],
            ),
            (
                &["close", "--force"],
                &[r#"{"id":1,"method":"action.run","params":{"action":"pane.close force=true"}}"#],
            ),
            (
                &["send-text", "echo a;b"],
                &[
                    r#"{"id":1,"method":"action.run","params":{"action":"input.send-text \"echo a;b\""}}"#,
                ],
            ),
            (
                &["send-keys", "ctrl+c", "Return", "--match", "focused"],
                &[
                    r#"{"id":1,"method":"input.send-keys","params":{"keys":["ctrl+c","Return"],"target":"focused"}}"#,
                ],
            ),
            (
                &["get-text", "--extent", "all", "--wrap"],
                &[r#"{"id":1,"method":"pane.text","params":{"extent":"all","wrap":true}}"#],
            ),
            (
                &["do", "tab.next; pane.focus left"],
                &[
                    r#"{"id":1,"method":"action.run","params":{"action":"tab.next"}}"#,
                    r#"{"id":1,"method":"action.run","params":{"action":"pane.focus left"}}"#,
                ],
            ),
            (
                &["subscribe", "bell", "title-changed"],
                &[r#"{"id":1,"method":"subscribe","params":{"events":["bell","title-changed"]}}"#],
            ),
        ];
        for (args, want) in cases {
            let got = wire(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert_eq!(got, *want, "{args:?}");
        }
    }

    #[test]
    fn launch_carries_cwd_title_and_wait() {
        let args = s(&[
            "launch", "--cwd", "/tmp", "--title", "t", "--wait", "--", "make",
        ]);
        let mut o = Opts::default();
        let p = plan(&args[0], &args[1..], &mut o).unwrap();
        let Params::Launch(l) = &p.requests[0] else {
            panic!("not a launch");
        };
        assert_eq!(l.cwd.as_deref(), Some("/tmp"));
        assert_eq!(l.title.as_deref(), Some("t"));
        assert!(l.wait);
        assert_eq!(l.argv, ["make"]);
        assert_eq!(p.show, Show::Launch { wait: true });
    }

    #[test]
    fn usage_errors() {
        for args in [
            &["split", "sideways"][..],
            &["focus"],
            &["send-keys"],
            &["send-keys", "ctrl+nosuchkey+"],
            &["get-text", "--extent", "page"],
            &["do", "no.such-action"],
            &["subscribe", "not-an-event"],
            &["ls", "extra"],
            &["frobnicate"],
            &["--match"],
        ] {
            assert!(wire(args).is_err(), "{args:?} should be a usage error");
        }
    }

    #[test]
    fn golden_output() {
        let tree = serde_json::json!({"windows":[{"id":1,"tabs":[{"id":2,"title":"zsh",
            "panes":[{"id":3,"title":"vim","cwd":"/home/u","foreground":null,"cols":80,"rows":24,
            "state":"running"}]}]}]});
        let mut out = Vec::new();
        assert_eq!(render(Show::Tree, &tree, false, &mut out).unwrap(), 0);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "window 1\n  tab 2  zsh\n    pane 3  80x24  running  vim  /home/u\n"
        );
        let mut out = Vec::new();
        render(
            Show::Text,
            &serde_json::json!({"text":"a\nb\n","truncated":false}),
            false,
            &mut out,
        )
        .unwrap();
        assert_eq!(out, b"a\nb\n");
        let mut out = Vec::new();
        render(
            Show::Launch { wait: false },
            &serde_json::json!({"pane":9,"window":1}),
            false,
            &mut out,
        )
        .unwrap();
        assert_eq!(out, b"9\n");
        let mut out = Vec::new();
        let code = render(
            Show::Launch { wait: true },
            &serde_json::json!({"pane":9,"window":1,"exit":3}),
            false,
            &mut out,
        )
        .unwrap();
        assert_eq!((code, out.len()), (3, 0));
        assert_eq!(launch_exit(&serde_json::json!({"exit":-9})), 137);
        let mut out = Vec::new();
        render(Show::Nothing, &Value::Null, true, &mut out).unwrap();
        assert_eq!(out, b"null\n");
    }

    #[test]
    fn errors_map_to_exit_codes() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let e = WireError::new(ErrorCode::Denied, "needs consent");
        assert_eq!(fail(&e, false, &mut out, &mut err), 3);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "apoth: denied: needs consent\n"
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let e = WireError::new(ErrorCode::NoInstance, "none");
        assert_eq!(fail(&e, true, &mut out, &mut err), 4);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"error\":{\"code\":\"no-instance\",\"message\":\"none\"}}\n"
        );
    }

    #[test]
    fn event_text() {
        let e = WireEvent {
            seq: 4,
            subscription: Some(1),
            kind: EventKind::TitleChanged,
            cause: apoth_action::Cause::Sequence,
            fields: vec![
                (
                    "title".into(),
                    Some(apoth_action::FieldValue::Str("a \"b\"".into())),
                ),
                ("pane".into(), Some(apoth_action::FieldValue::Int(3))),
                ("cwd".into(), None),
            ],
        };
        let mut out = Vec::new();
        render_event(&e, false, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "title-changed sequence title=\"a \\\"b\\\"\" pane=3 cwd=-\n"
        );
    }

    #[test]
    fn socket_candidates_newest_first() {
        let d = std::env::temp_dir().join(format!("apoth-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let a = std::os::unix::net::UnixListener::bind(d.join("1.sock")).unwrap();
        std::fs::write(d.join("note.txt"), b"").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let b = std::os::unix::net::UnixListener::bind(d.join("2.sock")).unwrap();
        let c = candidates(&d);
        assert_eq!(c, [d.join("2.sock"), d.join("1.sock")]);
        drop((a, b));
        let _ = std::fs::remove_dir_all(&d);
    }
}
