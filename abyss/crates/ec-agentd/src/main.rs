// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-agentd`: the agent daemon, M11 skeleton (A-01, COMP-08).
//!
//! Dials the privileged socket, admits one agent from a COSE_Sign1 grant
//! file (`create_agent`, COMP-08 §1), and with `--list` prints what
//! `list_toplevels` returns (COMP-08 §3). Everything it learns is already
//! filtered by abyss; this process decides nothing.
//!
//! Usage: `ec-agentd [--list] <grant.cose>`

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use ec_protocols::agent::client::{
    eclipse_agent_manager_v1::{self, EclipseAgentManagerV1},
    eclipse_agent_v1::{self, EclipseAgentV1},
    eclipse_scene_v1::{self, EclipseSceneV1},
};
use wayland_client::{
    backend::WaylandError,
    protocol::wl_registry::{self, WlRegistry},
    Connection, Dispatch, DispatchError, QueueHandle,
};

const MANAGER: &str = "eclipse_agent_manager_v1";
/// `req_id` of the one listing this skeleton asks for.
const LIST_REQ: u32 = 1;

/// The status names of COMP-08 §2.1, by value.
const STATUS: [&str; 26] = [
    "ok",
    "no_capability",
    "out_of_scope",
    "rate_limited",
    "stale_generation",
    "focus_lost",
    "client_gone",
    "client_timeout",
    "no_such_action",
    "sensitivity_denied",
    "policy_denied",
    "prompt_denied",
    "prompt_timeout",
    "revoked",
    "paused",
    "invalid_argument",
    "quota_exceeded",
    "duplicate",
    "deferred_timeout",
    "class_changed",
    "broker_locked",
    "secret_rotated",
    "batch_exhausted",
    "circuit_breaker",
    "provenance_required",
    "task_closed",
];

#[derive(Default)]
struct App {
    manager: Option<(u32, u32)>,
    listed: Vec<String>,
    done: bool,
    refused: Option<(u32, String)>,
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            if interface == MANAGER {
                app.manager = Some((name, version));
            }
        }
    }
}

impl Dispatch<EclipseAgentManagerV1, ()> for App {
    fn event(
        _: &mut Self,
        _: &EclipseAgentManagerV1,
        event: eclipse_agent_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_agent_manager_v1::Event::Revoked { principal, reason } = event {
            eprintln!("ec-agentd: {principal} revoked (reason {reason})");
        }
    }
}

impl Dispatch<EclipseAgentV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &EclipseAgentV1,
        event: eclipse_agent_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_agent_v1::Event::Result { status, detail, .. } = event {
            app.refused = Some((status, detail));
            app.done = true;
        }
    }
}

impl Dispatch<EclipseSceneV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &EclipseSceneV1,
        event: eclipse_scene_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            eclipse_scene_v1::Event::Toplevel {
                req_id: LIST_REQ,
                handle,
                app_id,
                title,
                workspace,
                output,
                ..
            } => app
                .listed
                .push(format!("{handle}\t{app_id}\t{workspace}\t{output}\t{title}")),
            eclipse_scene_v1::Event::ToplevelsDone { req_id: LIST_REQ } => app.done = true,
            eclipse_scene_v1::Event::Result {
                req_id: LIST_REQ,
                status,
                detail,
                ..
            } => {
                app.refused = Some((status, detail));
                app.done = true;
            }
            _ => {}
        }
    }
}

fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ECLIPSE_AGENT_SOCKET") {
        return Some(PathBuf::from(p));
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(base).join("eclipse").join("ec-agent.sock"))
}

fn status_name(s: u32) -> &'static str {
    STATUS.get(s as usize).copied().unwrap_or("unknown")
}

/// A protocol error from abyss, said plainly. `create_agent` refusals
/// arrive this way (COMP-08 §1).
fn explain(e: DispatchError) -> String {
    match e {
        DispatchError::Backend(WaylandError::Protocol(p)) => {
            let what = match (p.object_interface.as_str(), p.code) {
                (MANAGER | "eclipse_agent_v1", 0) => "invalid grant",
                (MANAGER | "eclipse_agent_v1", 1) => "policyd unavailable",
                _ => "protocol error",
            };
            format!("{what}: {}", p.message)
        }
        other => other.to_string(),
    }
}

fn run(list: bool, grant_path: &str) -> Result<(), String> {
    let grant = std::fs::read(grant_path).map_err(|e| format!("reading {grant_path}: {e}"))?;
    let path = socket_path().ok_or("XDG_RUNTIME_DIR unset and no ECLIPSE_AGENT_SOCKET")?;
    let stream = UnixStream::connect(&path).map_err(|e| format!("connecting to {}: {e}", path.display()))?;
    let conn = Connection::from_socket(stream).map_err(|e| e.to_string())?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let registry = conn.display().get_registry(&qh, ());
    let mut app = App::default();
    queue.roundtrip(&mut app).map_err(explain)?;
    let (name, _) = app
        .manager
        .ok_or("no eclipse_agent_manager_v1 on this socket (is the agents add-on on?)")?;
    let manager: EclipseAgentManagerV1 = registry.bind(name, 1, &qh, ());
    let agent = manager.create_agent(grant, &qh, ());
    let scene = agent.get_scene(&qh, ());
    queue.roundtrip(&mut app).map_err(explain)?;
    if let Some((status, detail)) = app.refused.take() {
        return Err(format!("agent refused: {} {detail}", status_name(status)));
    }
    eprintln!("ec-agentd: agent admitted");

    if list {
        scene.list_toplevels(LIST_REQ, String::new());
        while !app.done {
            queue.blocking_dispatch(&mut app).map_err(explain)?;
        }
        if let Some((status, detail)) = app.refused.take() {
            return Err(format!("list_toplevels: {} {detail}", status_name(status)));
        }
        for line in &app.listed {
            println!("{line}");
        }
        return Ok(());
    }
    // A daemon from here: hold the agent until abyss closes the connection.
    loop {
        queue.blocking_dispatch(&mut app).map_err(explain)?;
    }
}

fn main() -> ExitCode {
    let mut list = false;
    let mut grant = None;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--list" => list = true,
            "-h" | "--help" => {
                println!("usage: ec-agentd [--list] <grant.cose>");
                return ExitCode::SUCCESS;
            }
            _ if grant.is_none() && !a.starts_with('-') => grant = Some(a),
            other => {
                eprintln!("ec-agentd: unexpected argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(grant) = grant else {
        eprintln!("usage: ec-agentd [--list] <grant.cose>");
        return ExitCode::from(2);
    };
    match run(list, &grant) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ec-agentd: {e}");
            ExitCode::FAILURE
        }
    }
}
