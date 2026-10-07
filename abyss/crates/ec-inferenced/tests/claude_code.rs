// SPDX-License-Identifier: AGPL-3.0-only
//! The Claude Code backend against `fake-claude` (a scripted stand-in for
//! `claude -p`), with the real tool shim in the loop: the fake launches the
//! shim named in `mcp.json` and speaks MCP to it, so the whole path
//! request -> session -> stream-json -> shim -> tool_result -> shim -> stream
//! is exercised. The sandbox is off (CI has no bwrap); the command line the
//! sandbox would use is pinned by unit tests in `src/claude_code.rs`.
//!
//! Run with `cargo test -p ec-inferenced --features fake-claude`.

use ec_inference_wire::{Backend, Completion, Failure, Request, Value};
use ec_inferenced::api::{failure, Http, HttpError, HttpResponse};
use ec_inferenced::claude_code::ClaudeConfig;
use ec_inferenced::credential::Credentials;
use ec_inferenced::router::Router;
use ec_inferenced::secret::ApiKey;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TOKEN: &str = "sk-ant-oat01-TEST-TOKEN-VALUE";
const NO_TOKEN: &str =
    "no Claude Code token stored: run `claude setup-token`, then `ec-secret add claude-code-token --bind host:api.anthropic.com`";

struct NoHttp;
impl Http for NoHttp {
    fn post_messages(&self, _: &ApiKey, _: &[u8]) -> Result<HttpResponse, HttpError> {
        Err(HttpError::Network("the API is not used here".into()))
    }
}

struct Creds(bool);
impl Credentials for Creds {
    fn api_key(&self, _: &str) -> Result<ApiKey, Failure> {
        Err(failure("no_credential", "no API key"))
    }
    fn claude_code_token(&self, _: &str) -> Result<ApiKey, Failure> {
        if self.0 {
            Ok(ApiKey::from_bytes(TOKEN.as_bytes()).unwrap())
        } else {
            Err(failure("no_credential", NO_TOKEN))
        }
    }
}

/// A scratch directory removed on drop.
struct Tmp(PathBuf);
impl Tmp {
    fn new(name: &str) -> Tmp {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "ecit-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn cc(&self) -> PathBuf {
        self.0.join("cc")
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(tmp: &Tmp) -> ClaudeConfig {
    ClaudeConfig {
        sandbox: false,
        base_dir: Some(tmp.cc()),
        claude_bin: Some(PathBuf::from(env!("CARGO_BIN_EXE_fake-claude"))),
        self_exe: PathBuf::from(env!("CARGO_BIN_EXE_ec-inferenced")),
        max_sessions: 8,
        idle: Duration::from_secs(900),
        reap_interval: Duration::from_millis(50),
        turn_timeout: Duration::from_secs(30),
        tool_batch_wait: Duration::from_millis(150),
        debug: false,
        extra_env: Vec::new(),
    }
}

fn router_with(cfg: ClaudeConfig, token: bool) -> Router {
    Router::with_claude(Arc::new(NoHttp), Arc::new(Creds(token)), cfg)
}

fn router(tmp: &Tmp) -> Router {
    router_with(config(tmp), true)
}

fn req(task: &str, system: &str, messages: Value, tools: Value) -> Request {
    Request {
        id: 1,
        task: task.into(),
        package: "ec-claude-code-agent".into(),
        backend: Backend::ClaudeCode,
        model: "claude-opus-5-5".into(),
        system: system.into(),
        messages,
        tools,
        max_tokens: 1000,
    }
}

fn user(t: &str) -> Value {
    json!({"role": "user", "content": t})
}

fn assistant(c: &Completion) -> Value {
    json!({"role": "assistant", "content": c.content})
}

fn text(c: &Completion) -> String {
    c.content
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["type"] == "text")
        .map(|b| b["text"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("")
}

fn tools_ab() -> Value {
    json!([
        {"name": "alpha", "description": "first", "input_schema": {"type": "object"}},
        {"name": "beta", "description": "second", "input_schema": {"type": "object"}},
    ])
}

fn pid_of(c: &Completion) -> u32 {
    text(c).strip_prefix("pid=").unwrap().parse().unwrap()
}

fn session_dirs(tmp: &Tmp) -> Vec<PathBuf> {
    std::fs::read_dir(tmp.cc())
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[test]
fn a_text_turn_ends_the_turn_with_usage_and_cost() {
    let tmp = Tmp::new("text");
    let r = router(&tmp);
    let c = r
        .complete(&req("t1", "", json!([user("hello")]), json!([])))
        .unwrap();
    assert_eq!(c.stop_reason, "end_turn");
    assert_eq!(text(&c), "got:hello");
    assert_eq!((c.input_tokens, c.output_tokens), (10, 4));
    assert_eq!(c.cost_usd, Some(0.0123));
    // The model that answered comes from system/init.
    assert_eq!(c.model, "fake-model");
    // `thinking` and unknown events are not in the reply.
    assert_eq!(c.content.as_array().unwrap().len(), 1);
}

#[test]
fn the_token_reaches_the_childs_environment() {
    let tmp = Tmp::new("env");
    let r = router(&tmp);
    let c = r
        .complete(&req("t1", "", json!([user("env")]), json!([])))
        .unwrap();
    assert_eq!(text(&c), format!("token_len={}", TOKEN.len()));
}

#[test]
fn the_session_directory_is_private_and_holds_the_expected_files() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = Tmp::new("perm");
    let r = router(&tmp);
    r.complete(&req("t1", "", json!([user("hello")]), json!([])))
        .unwrap();
    let dirs = session_dirs(&tmp);
    assert_eq!(dirs.len(), 1);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dirs[0]), 0o700);
    for sub in ["config", "home", "work"] {
        assert_eq!(mode(&dirs[0].join(sub)), 0o700, "{sub}");
    }
    assert_eq!(mode(&dirs[0].join("mcp.json")), 0o600);
    // The token is not written anywhere in it.
    for e in std::fs::read_dir(&dirs[0]).unwrap().flatten() {
        if e.path().is_file() {
            assert!(!std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .contains(TOKEN));
        }
    }
    // One connection per session: once the shim has connected, its socket
    // is gone.
    wait_for("the shim socket to go", || !dirs[0].join("shim.sock").exists());
}

#[test]
fn a_tool_turn_returns_stripped_tool_use_then_the_final_text() {
    let tmp = Tmp::new("tool");
    let r = router(&tmp);
    let tools = tools_ab();
    let m1 = json!([user("tool:alpha")]);
    let c1 = r.complete(&req("t1", "", m1.clone(), tools.clone())).unwrap();
    assert_eq!(c1.stop_reason, "tool_use");
    let blocks = c1.content.as_array().unwrap();
    assert_eq!(blocks[0], json!({"type": "text", "text": "calling"}));
    assert_eq!(blocks[1]["type"], "tool_use");
    assert_eq!(blocks[1]["name"], "alpha");
    assert_eq!(blocks[1]["input"], json!({"n": "alpha"}));
    let id = blocks[1]["id"].as_str().unwrap().to_owned();
    assert_eq!(blocks.len(), 2);

    let m2 = json!([
        user("tool:alpha"),
        assistant(&c1),
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "RESULT-A"}]},
    ]);
    let c2 = r.complete(&req("t1", "", m2, tools)).unwrap();
    assert_eq!(c2.stop_reason, "end_turn");
    assert_eq!(text(&c2), "done:RESULT-A");
    assert_eq!(r.claude().session_count(), 1);
    assert_eq!(session_dirs(&tmp).len(), 1);
}

#[test]
fn a_tool_result_marked_as_an_error_reaches_the_model_as_one() {
    let tmp = Tmp::new("toolerr");
    let r = router(&tmp);
    let tools = tools_ab();
    let c1 = r
        .complete(&req("t1", "", json!([user("tool:alpha")]), tools.clone()))
        .unwrap();
    let id = c1.content[1]["id"].as_str().unwrap().to_owned();
    let m2 = json!([
        user("tool:alpha"),
        assistant(&c1),
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id,
            "content": [{"type": "text", "text": "denied"}], "is_error": true}]},
    ]);
    let c2 = r.complete(&req("t1", "", m2, tools)).unwrap();
    assert_eq!(text(&c2), "done:denied");
}

#[test]
fn a_parallel_two_tool_turn_is_one_reply_with_both_calls() {
    for spec in ["tool:alpha,beta", "tool:alpha,beta:split"] {
        let tmp = Tmp::new("par");
        let r = router(&tmp);
        let tools = tools_ab();
        let c1 = r
            .complete(&req("t1", "", json!([user(spec)]), tools.clone()))
            .unwrap();
        assert_eq!(c1.stop_reason, "tool_use", "{spec}");
        let uses: Vec<&Value> = c1
            .content
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["type"] == "tool_use")
            .collect();
        assert_eq!(uses.len(), 2, "{spec}");
        assert_eq!(
            (uses[0]["name"].as_str(), uses[1]["name"].as_str()),
            (Some("alpha"), Some("beta"))
        );
        let (ia, ib) = (uses[0]["id"].as_str().unwrap(), uses[1]["id"].as_str().unwrap());
        assert_ne!(ia, ib);
        // Answered in the opposite order to the calls.
        let m2 = json!([
            user(spec),
            assistant(&c1),
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": ib, "content": "RB"},
                {"type": "tool_result", "tool_use_id": ia, "content": "RA"},
            ]},
        ]);
        let c2 = r.complete(&req("t1", "", m2, tools)).unwrap();
        assert_eq!(text(&c2), "done:RA,RB", "{spec}");
    }
}

#[test]
fn a_result_that_arrives_before_the_shims_call_is_held_for_it() {
    let tmp = Tmp::new("early");
    let r = router(&tmp);
    let tools = tools_ab();
    // `slow`: the fake delays its MCP call by 300 ms, so the agent's result
    // (sent at once) reaches the router first.
    let c1 = r
        .complete(&req("t1", "", json!([user("tool:alpha:slow")]), tools.clone()))
        .unwrap();
    let id = c1.content[1]["id"].as_str().unwrap().to_owned();
    let m2 = json!([
        user("tool:alpha:slow"),
        assistant(&c1),
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": "EARLY"}]},
    ]);
    let c2 = r.complete(&req("t1", "", m2, tools)).unwrap();
    assert_eq!(text(&c2), "done:EARLY");
}

#[test]
fn a_following_turn_reuses_the_session_and_a_non_append_restarts_with_a_transcript() {
    let tmp = Tmp::new("restart");
    let r = router(&tmp);
    let c1 = r
        .complete(&req("t1", "", json!([user("pid")]), json!([])))
        .unwrap();
    let pid1 = pid_of(&c1);
    // An append: same process.
    let m2 = json!([user("pid"), assistant(&c1), user("pid")]);
    let c2 = r.complete(&req("t1", "", m2, json!([]))).unwrap();
    assert_eq!(pid_of(&c2), pid1);
    // Not an append (the history differs): a new process, the old history
    // replayed as a labelled transcript ahead of the last turn.
    let m3 = json!([user("something else"), assistant(&c1), user("and now?")]);
    let c3 = r.complete(&req("t1", "", m3, json!([]))).unwrap();
    let t = text(&c3);
    assert!(t.starts_with("got:[Earlier conversation"), "{t}");
    assert!(t.contains("User: something else"), "{t}");
    assert!(t.contains(&format!("Assistant: pid={pid1}")), "{t}");
    assert!(t.ends_with("[End of earlier conversation]\n\nand now?"), "{t}");
    wait_for("the old process to be gone", || !alive(pid1));
    // Only the new session's directory is left.
    assert_eq!(session_dirs(&tmp).len(), 1);
}

#[test]
fn a_fresh_session_with_prior_history_replays_it() {
    let tmp = Tmp::new("resume");
    let r = router(&tmp);
    let m = json!([user("first"), {"role": "assistant", "content": "second"}, user("third")]);
    let c = r.complete(&req("t1", "", m, json!([]))).unwrap();
    let t = text(&c);
    assert!(
        t.contains("User: first") && t.contains("Assistant: second"),
        "{t}"
    );
    assert!(t.ends_with("third"), "{t}");
}

#[test]
fn changed_tools_restart_the_session() {
    let tmp = Tmp::new("tools");
    let r = router(&tmp);
    let c1 = r
        .complete(&req("t1", "", json!([user("pid")]), json!([])))
        .unwrap();
    let pid1 = pid_of(&c1);
    let m2 = json!([user("pid"), assistant(&c1), user("pid")]);
    // The tool list is fixed at MCP init, so a different one needs a new
    // process.
    let c2 = r.complete(&req("t1", "", m2, tools_ab())).unwrap();
    // A new process: it was told the earlier conversation as a transcript.
    assert!(
        text(&c2).starts_with("got:[Earlier conversation"),
        "{}",
        text(&c2)
    );
    wait_for("the old process to be gone", || !alive(pid1));
    assert_eq!(session_dirs(&tmp).len(), 1);
}

#[test]
fn a_session_that_lists_a_builtin_tool_is_refused_and_cleaned_up() {
    let tmp = Tmp::new("builtin");
    let r = router(&tmp);
    let f = r
        .complete(&req("t1", "FAKE:builtin", json!([user("hello")]), json!([])))
        .unwrap_err();
    assert_eq!(f.kind, "backend_unavailable");
    assert_eq!(f.message, "Claude Code exposed built-in tools; refusing");
    assert_eq!(r.claude().session_count(), 0);
    assert!(session_dirs(&tmp).is_empty());
}

#[test]
fn an_http_429_result_is_rate_limited() {
    let tmp = Tmp::new("429");
    let r = router(&tmp);
    let f = r
        .complete(&req("t1", "FAKE:429", json!([user("hello")]), json!([])))
        .unwrap_err();
    assert_eq!(f.kind, "rate_limited");
    assert!(session_dirs(&tmp).is_empty());
}

#[test]
fn a_child_crash_is_a_provider_error_and_the_next_request_starts_over() {
    let tmp = Tmp::new("crash");
    let r = router(&tmp);
    let f = r
        .complete(&req("t1", "", json!([user("exit")]), json!([])))
        .unwrap_err();
    assert_eq!(f.kind, "provider_error");
    assert!(session_dirs(&tmp).is_empty());
    let c = r
        .complete(&req("t1", "", json!([user("hello")]), json!([])))
        .unwrap();
    assert_eq!(text(&c), "got:hello");
}

#[test]
fn close_kills_the_process_and_removes_the_directory() {
    let tmp = Tmp::new("close");
    let r = router(&tmp);
    let c = r
        .complete(&req("t1", "", json!([user("pid")]), json!([])))
        .unwrap();
    let pid = pid_of(&c);
    assert!(alive(pid));
    r.close("t1");
    assert_eq!(r.claude().session_count(), 0);
    assert!(session_dirs(&tmp).is_empty());
    wait_for("the process to be gone", || !alive(pid));
    // A later request starts a new session.
    let c = r
        .complete(&req("t1", "", json!([user("pid")]), json!([])))
        .unwrap();
    assert_ne!(pid_of(&c), pid);
}

#[test]
fn an_idle_session_is_reaped() {
    let tmp = Tmp::new("idle");
    let mut cfg = config(&tmp);
    cfg.idle = Duration::from_millis(300);
    let r = router_with(cfg, true);
    let c = r
        .complete(&req("t1", "", json!([user("pid")]), json!([])))
        .unwrap();
    let pid = pid_of(&c);
    wait_for("the reaper", || r.claude().session_count() == 0);
    assert!(session_dirs(&tmp).is_empty());
    wait_for("the process to be gone", || !alive(pid));
}

#[test]
fn a_session_whose_child_exited_is_reaped_and_replaced() {
    let tmp = Tmp::new("exited");
    let r = router(&tmp);
    let c = r
        .complete(&req("t1", "", json!([user("pid")]), json!([])))
        .unwrap();
    let pid = pid_of(&c);
    // Kill the child behind the router's back.
    let st = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status()
        .unwrap();
    assert!(st.success());
    wait_for("the reaper", || r.claude().session_count() == 0);
    assert!(session_dirs(&tmp).is_empty());
}

#[test]
fn no_token_is_no_credential_with_the_remedy_and_starts_nothing() {
    let tmp = Tmp::new("notoken");
    let r = router_with(config(&tmp), false);
    let f = r
        .complete(&req("t1", "", json!([user("hello")]), json!([])))
        .unwrap_err();
    assert_eq!(f.kind, "no_credential");
    assert_eq!(f.message, NO_TOKEN);
    assert_eq!(r.claude().session_count(), 0);
    assert!(session_dirs(&tmp).is_empty());
}

#[test]
fn claude_not_installed_is_backend_unavailable() {
    let tmp = Tmp::new("noclaude");
    let mut cfg = config(&tmp);
    cfg.claude_bin = Some(tmp.0.join("nope"));
    let r = router_with(cfg, true);
    let f = r
        .complete(&req("t1", "", json!([user("hello")]), json!([])))
        .unwrap_err();
    assert_eq!(f.kind, "backend_unavailable");
    assert_eq!(f.message, "Claude Code is not installed (set ECLIPSE_CLAUDE_BIN)");
}

#[test]
fn the_ninth_session_is_refused() {
    let tmp = Tmp::new("cap");
    let mut cfg = config(&tmp);
    cfg.max_sessions = 2;
    let r = router_with(cfg, true);
    for t in ["a", "b"] {
        r.complete(&req(t, "", json!([user("hello")]), json!([])))
            .unwrap();
    }
    let f = r
        .complete(&req("c", "", json!([user("hello")]), json!([])))
        .unwrap_err();
    assert_eq!(f.kind, "backend_unavailable");
    assert_eq!(f.message, "too many Claude Code sessions are open");
    // An existing task is still served, and closing one frees a place.
    r.complete(&req("a", "", json!([user("again")]), json!([])))
        .unwrap();
    r.close("b");
    r.complete(&req("c", "", json!([user("hello")]), json!([])))
        .unwrap();
}

#[test]
fn dropping_the_router_removes_every_session() {
    let tmp = Tmp::new("drop");
    let r = router(&tmp);
    let mut pids = Vec::new();
    for t in ["a", "b"] {
        let c = r.complete(&req(t, "", json!([user("pid")]), json!([]))).unwrap();
        pids.push(pid_of(&c));
    }
    assert_eq!(session_dirs(&tmp).len(), 2);
    drop(r);
    wait_for("the directories to go", || session_dirs(&tmp).is_empty());
    for p in pids {
        wait_for("the process to be gone", || !alive(p));
    }
}

#[test]
fn a_bad_task_id_never_becomes_a_path() {
    let tmp = Tmp::new("badtask");
    let r = router(&tmp);
    let f = r
        .complete(&req("../evil", "", json!([user("hello")]), json!([])))
        .unwrap_err();
    assert_eq!(f.kind, "bad_request");
    assert!(session_dirs(&tmp).is_empty());
}

#[test]
fn stale_directories_are_swept_only_on_request() {
    let tmp = Tmp::new("sweep");
    let stale = tmp.cc().join("old-0");
    std::fs::create_dir_all(stale.join("config")).unwrap();
    let r = router(&tmp);
    // Serving requests never deletes what it did not create...
    r.complete(&req("t1", "", json!([user("hello")]), json!([])))
        .unwrap();
    assert!(stale.exists());
    // ...the daemon asks for the sweep once, at start-up.
    r.claude().sweep_stale();
    assert!(!stale.exists());
}
