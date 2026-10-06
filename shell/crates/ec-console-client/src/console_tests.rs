// SPDX-License-Identifier: AGPL-3.0-only
//! Tests for `console`, against a fake agentd on a socketpair.

use super::*;
use std::thread;

/// A fake server on a socketpair: answers each request through `answer`,
/// first writing `before` lines (notifications) ahead of the reply.
fn serve(
    server: UnixStream,
    before: Vec<Value>,
    answer: impl Fn(&str, &Value) -> std::result::Result<Value, Value> + Send + 'static,
) -> thread::JoinHandle<Vec<Value>> {
    thread::spawn(move || {
        let mut r = BufReader::new(server.try_clone().expect("clone"));
        let mut w = server;
        let mut seen = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line).expect("read") == 0 {
                return seen;
            }
            let req: Value = serde_json::from_str(&line).expect("json");
            for b in &before {
                writeln!(w, "{b}").expect("w");
            }
            let reply = match answer(req["method"].as_str().expect("m"), &req["params"]) {
                Ok(v) => json!({"jsonrpc": "2.0", "id": req["id"], "result": v}),
                Err(e) => json!({"jsonrpc": "2.0", "id": req["id"], "error": e}),
            };
            seen.push(req);
            writeln!(w, "{reply}").expect("w");
        }
    })
}

fn pair() -> (Console, UnixStream) {
    let (c, s) = UnixStream::pair().expect("pair");
    (Console::from_stream(c).expect("console"), s)
}

#[test]
fn request_shapes_match_the_contract() {
    assert_eq!(
        serde_json::from_str::<Value>(&request_line(3, "pause_task", json!({"task_id": "t1"}))).unwrap(),
        json!({"jsonrpc": "2.0", "id": 3, "method": "pause_task", "params": {"task_id": "t1"}})
    );
    let r: Value = serde_json::from_str(&request_line(1, "list_packages", Value::Null)).unwrap();
    assert!(r.get("params").is_none());
    assert_eq!(list_tasks_params(None), Value::Null);
    assert_eq!(list_tasks_params(Some("paused")), json!({"state": "paused"}));
    assert_eq!(
        conversation_read_params("t", Some(9)),
        json!({"task_id": "t", "since": 9})
    );
    assert_eq!(
        conversation_post_params("t", "hi", Some(4)),
        json!({"task_id": "t", "text": "hi", "reply_to": 4})
    );
    assert_eq!(
        conversation_post_params("t", "hi", None),
        json!({"task_id": "t", "text": "hi"})
    );
}

#[test]
fn typed_calls_round_trip() {
    let (c, server) = pair();
    let h = serve(server, vec![], |m, p| match m {
        "list_tasks" => Ok(json!({"available": false, "decisions_pending": 2, "tasks": [{
            "task_id": "t1", "package": "ec-ref-agent", "version": "0.1.0", "statement": "do it",
            "state": "active", "reason": "", "deadline_ms": 7200000, "started_ms": 5, "closed_ms": null,
            "depth": 0, "continuation": "", "awaiting_reply": true, "pending_decisions": 2,
            "min_trust": "standard", "counters": {}, "future_field": 1}]})),
        "get_task" => Ok(json!({"task_id": p["task_id"], "state": "paused",
            "children": [{"task_id": "t2", "parent": "t1"}]})),
        "conversation_read" => Ok(json!({"closed": false, "awaiting_reply": true, "messages": [
            {"schema": "conversation.v1", "msg_id": 1, "time_ms": 10, "kind": "say", "text": "hello",
             "trust": {"min_trust": "untrusted", "head": "agent"}},
            {"msg_id": 2, "time_ms": 11, "kind": "human", "text": "yo", "reply_to": 1}]})),
        "conversation_post" => Ok(json!({"msg_id": 3, "time_ms": 12})),
        "list_sessions" => Ok(
            json!({"sessions": [{"task_id": "t0", "package": "p", "statement": "s",
            "reason": "done", "closed_ms": 5, "min_trust": "standard"}]}),
        ),
        "list_packages" => Ok(
            json!({"packages": [{"id": "ec-ref-agent", "name": "Reference Agent",
            "publisher": "eclipse", "version": "0.1.0"}]}),
        ),
        "subscribe" => Ok(
            json!({"subscribed": true, "events": ["message"], "available": true,
            "decisions_pending": 1}),
        ),
        "pause_task" | "cancel_task" => Ok(json!({"ok": true})),
        "delete_session" => Ok(json!({"deleted": true})),
        _ => Ok(json!({})),
    });
    let l = c.list_tasks(None).unwrap();
    assert!(!l.available);
    assert_eq!(l.decisions_pending, 2);
    let t = &l.tasks[0];
    assert_eq!(
        (t.task_id.as_str(), t.awaiting_reply, t.pending_decisions),
        ("t1", true, 2)
    );
    assert_eq!(
        (t.version.as_str(), t.started_ms, t.closed_ms),
        ("0.1.0", 5, None)
    );
    let g = c.get_task("t1").unwrap();
    assert_eq!(g.state, "paused");
    assert_eq!(g.children[0].parent.as_deref(), Some("t1"));
    let conv = c.conversation_read("t1", None).unwrap();
    assert!(conv.awaiting_reply && !conv.closed);
    let m = &conv.messages;
    assert_eq!(
        m[0].trust,
        Trust {
            min_trust: "untrusted".into(),
            head: "agent".into()
        }
    );
    assert_eq!((m[0].msg_id, m[0].time_ms), (1, 10));
    assert_eq!(m[1].reply_to, Some(1));
    let p = c.conversation_post("t1", "hey", None).unwrap();
    assert_eq!((p.msg_id, p.time_ms), (3, 12));
    assert_eq!(c.list_sessions(None).unwrap()[0].reason, "done");
    assert_eq!(c.list_packages().unwrap()[0].publisher, "eclipse");
    let sub = c.subscribe().unwrap();
    assert_eq!(
        (sub.events, sub.decisions_pending),
        (vec!["message".to_owned()], 1)
    );
    c.pause_task("t1").unwrap();
    c.cancel_task("t1", CancelMode::Drain).unwrap();
    c.delete_session("t0").unwrap();
    c.show_decisions().unwrap();
    drop(c);
    let seen = h.join().unwrap();
    let cancel = seen.iter().find(|r| r["method"] == "cancel_task").unwrap();
    assert_eq!(cancel["params"], json!({"task_id": "t1", "mode": "drain"}));
}

#[test]
fn errors_are_matched_by_code_and_carry_the_reason() {
    let (c, server) = pair();
    let _h = serve(server, vec![], |m, _| match m {
        "get_task" => Err(json!({"code": -32002, "message": "whatever", "data": {"reason": "x"}})),
        "show_decisions" => Err(json!({"code": -32004, "message": "slow down"})),
        "pause_task" => Err(json!({"code": -32003, "message": "u"})),
        "conversation_post" => Err(json!({"code": -32006, "message": "c"})),
        _ => Err(json!({"code": -32000, "message": "d", "data": {"reason": "policy_unavailable"}})),
    });
    assert!(c.get_task("nope").unwrap_err().is_not_found());
    assert!(c.show_decisions().unwrap_err().is_rate_limited());
    assert!(c.pause_task("t").unwrap_err().is_unavailable());
    assert!(c.conversation_post("t", "x", None).unwrap_err().is_closed());
    let e = c.cancel_task("t", CancelMode::Immediate).unwrap_err();
    assert!(e.is_denied());
    assert_eq!(e.reason(), Some("policy_unavailable"));
}

#[test]
fn events_parse_and_do_not_disturb_replies() {
    let (c, server) = pair();
    let ev = |name: &str, data: Value| json!({"jsonrpc": "2.0", "method": "event", "params": {"event": name, "data": data}});
    let before = vec![
        ev(
            "task_started",
            json!({"task_id": "t1", "package": "p", "statement": "s", "deadline_ms": 9}),
        ),
        ev(
            "message",
            json!({"task_id": "t1", "msg_id": 1, "time_ms": 4, "kind": "ask", "text": "q?",
                "trust": {"min_trust": "untrusted", "head": "agent"}}),
        ),
        ev("awaiting_reply", json!({"task_id": "t1", "awaiting": true})),
        ev("decisions_pending", json!({"count": 3})),
        ev(
            "task_state",
            json!({"task_id": "t1", "state": "paused", "reason": "human"}),
        ),
        ev("task_closed", json!({"task_id": "t1", "reason": "done"})),
        ev("mystery", json!({})),
    ];
    let _h = serve(server, before, |_, _| Ok(json!({})));
    c.subscribe().unwrap();
    let got: Vec<Event> = (0..7)
        .map(|_| c.events().recv_timeout(Duration::from_secs(2)).unwrap())
        .collect();
    assert!(matches!(&got[0], Event::TaskStarted { task_id, deadline_ms: 9, .. } if task_id == "t1"));
    match &got[1] {
        Event::Message(m) => {
            assert_eq!(
                (m.kind.as_str(), m.task_id.as_str(), m.msg_id, m.time_ms),
                ("ask", "t1", 1, 4)
            );
            assert_eq!(m.trust.head, "agent");
        }
        e => panic!("{e:?}"),
    }
    assert_eq!(
        got[2],
        Event::AwaitingReply {
            task_id: "t1".into(),
            awaiting: true
        }
    );
    assert_eq!(got[3], Event::DecisionsPending { count: 3 });
    assert_eq!(
        got[4],
        Event::TaskState {
            task_id: "t1".into(),
            state: "paused".into(),
            reason: "human".into()
        }
    );
    assert_eq!(
        got[5],
        Event::TaskClosed {
            task_id: "t1".into(),
            reason: "done".into()
        }
    );
    assert!(matches!(&got[6], Event::Unknown { name, .. } if name == "mystery"));
}

#[test]
fn disconnect_fails_waiters_and_ends_the_event_stream() {
    let (c, server) = pair();
    drop(server);
    assert!(matches!(
        c.list_packages(),
        Err(Error::Disconnected | Error::Io(_))
    ));
    let mut last = None;
    while let Ok(e) = c.events().recv_timeout(Duration::from_secs(2)) {
        last = Some(e);
    }
    assert_eq!(last, Some(Event::Disconnected));
}
