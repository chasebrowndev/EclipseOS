// SPDX-License-Identifier: Apache-2.0
//! Golden JSON round-trips for every message kind, and rejection tests (T-12 §6,
//! wire-level subset).

use apoth_action::{Class, ClassSet, NoContext};
use apoth_proto::event::WireEvent;
use apoth_proto::params::*;
use apoth_proto::results::*;
use apoth_proto::{
    Cap, ErrorCode, MAX_LINE_BYTES, METHODS, Method, Params, Reply, Request, ServerMessage,
};
use serde_json::{Value, json};

fn req(line: &str) -> Request {
    Request::decode(line.as_bytes()).unwrap_or_else(|e| panic!("{line}: {}", e.error))
}

fn rej(line: &str) -> (Option<u64>, ErrorCode) {
    let e = Request::decode(line.as_bytes()).expect_err(line);
    (e.id, e.error.code)
}

/// Decode, check the typed form, re-encode and compare JSON values.
fn golden(line: &str, expect: Params) {
    let r = req(line);
    assert_eq!(r.params, expect, "{line}");
    let want: Value = serde_json::from_str(line).unwrap();
    let got = r.to_value();
    // `params` may be omitted on the wire when empty.
    let mut want = want;
    if want.get("params").is_none() {
        want["params"] = json!({});
    }
    assert_eq!(got, want, "{line}");
    let again = Request::decode(&r.to_line().unwrap()[..r.to_line().unwrap().len() - 1]).unwrap();
    assert_eq!(again, r);
}

#[test]
fn golden_every_request_kind() {
    golden(
        r#"{"id":1,"method":"hello","params":{"proto":1,"client":"apoth","want":["observe","input"]}}"#,
        Params::Hello(Hello {
            proto: 1,
            client: "apoth".into(),
            want: vec![Cap(Class::Observe), Cap(Class::Input)],
        }),
    );
    golden(
        r#"{"id":2,"method":"action.run","params":{"action":"pane.focus-next","target":"self"}}"#,
        Params::ActionRun(ActionRun {
            action: "pane.focus-next".into(),
            target: Some("self".into()),
        }),
    );
    golden(
        r#"{"id":3,"method":"input.send-keys","params":{"keys":["ctrl+c","a"],"target":"last"}}"#,
        Params::InputSendKeys(SendKeys {
            keys: vec!["ctrl+c".into(), "a".into()],
            target: Some("last".into()),
        }),
    );
    golden(
        r#"{"id":4,"method":"tree","params":{}}"#,
        Params::Tree(TargetOnly::default()),
    );
    golden(
        r#"{"id":5,"method":"pane.info","params":{"target":"id:7"}}"#,
        Params::PaneInfo(TargetOnly {
            target: Some("id:7".into()),
        }),
    );
    golden(
        r#"{"id":6,"method":"pane.processes","params":{}}"#,
        Params::PaneProcesses(TargetOnly::default()),
    );
    golden(
        r#"{"id":7,"method":"pane.text","params":{"extent":"all","sgr":true,"wrap":true}}"#,
        Params::PaneText(PaneText {
            target: None,
            extent: Extent::All,
            sgr: true,
            wrap: true,
        }),
    );
    golden(
        r#"{"id":8,"method":"pane.blocks","params":{"limit":5}}"#,
        Params::PaneBlocks(PaneBlocks {
            target: None,
            limit: Some(5),
        }),
    );
    golden(
        r#"{"id":9,"method":"config.get","params":{"key":"font.size"}}"#,
        Params::ConfigGet(ConfigGet {
            key: "font.size".into(),
        }),
    );
    golden(
        r#"{"id":10,"method":"config.effective","params":{"key":"font.size","target":"self"}}"#,
        Params::ConfigEffective(ConfigEffective {
            key: Some("font.size".into()),
            target: Some("self".into()),
        }),
    );
    golden(
        r#"{"id":11,"method":"keyboard.state","params":{}}"#,
        Params::KeyboardState(Empty {}),
    );
    golden(
        r#"{"id":12,"method":"graphics.list","params":{}}"#,
        Params::GraphicsList(TargetOnly::default()),
    );
    golden(
        r#"{"id":13,"method":"media.list","params":{}}"#,
        Params::MediaList(TargetOnly::default()),
    );
    golden(
        r#"{"id":14,"method":"wayland.info","params":{}}"#,
        Params::WaylandInfo(Empty {}),
    );
    golden(
        r#"{"id":15,"method":"render.status","params":{}}"#,
        Params::RenderStatus(Empty {}),
    );
    golden(
        r#"{"id":16,"method":"version","params":{}}"#,
        Params::Version(Empty {}),
    );
    golden(
        r#"{"id":17,"method":"subscribe","params":{"events":["bell","command-finished"],"match":"status:!0"}}"#,
        Params::Subscribe(Subscribe {
            events: vec!["bell".into(), "command-finished".into()],
            match_expr: Some("status:!0".into()),
        }),
    );
    golden(
        r#"{"id":18,"method":"unsubscribe","params":{"subscription":3}}"#,
        Params::Unsubscribe(Unsubscribe { subscription: 3 }),
    );
    golden(
        r#"{"id":19,"method":"pane.watch","params":{"since":42}}"#,
        Params::PaneWatch(PaneWatch {
            target: None,
            since: Some(42),
        }),
    );
    golden(
        r#"{"id":20,"method":"config.set","params":{"key":"font.size","value":"14"}}"#,
        Params::ConfigSet(ConfigSet {
            key: "font.size".into(),
            value: "14".into(),
        }),
    );
    golden(
        r#"{"id":21,"method":"config.reset","params":{"key":"font.size"}}"#,
        Params::ConfigReset(ConfigReset {
            key: "font.size".into(),
        }),
    );
    golden(
        r#"{"id":22,"method":"config.reload","params":{}}"#,
        Params::ConfigReload(Empty {}),
    );
    golden(
        r#"{"id":23,"method":"config.persist","params":{"key":"font.size","value":"14"}}"#,
        Params::ConfigPersist(ConfigSet {
            key: "font.size".into(),
            value: "14".into(),
        }),
    );
    golden(
        r#"{"id":24,"method":"config.save-override","params":{}}"#,
        Params::ConfigSaveOverride(ConfigSaveOverride::default()),
    );
    golden(
        r#"{"id":25,"method":"notify","params":{"title":"done","body":"build finished"}}"#,
        Params::Notify(Notify {
            title: "done".into(),
            body: Some("build finished".into()),
            urgency: None,
        }),
    );
    golden(
        r#"{"id":26,"method":"launch","params":{"argv":["htop"],"cwd":"/tmp","env":{"A":"b"},"activation_token":"tok","overrides":{"font.size":"12"},"title":"t","app_id":"x.y","profile":"p","layout":"l","hold":true,"wait":true}}"#,
        Params::Launch(Box::new(Launch {
            argv: vec!["htop".into()],
            cwd: Some("/tmp".into()),
            env: [("A".to_owned(), "b".to_owned())].into(),
            activation_token: Some("tok".into()),
            overrides: [("font.size".to_owned(), "12".to_owned())].into(),
            title: Some("t".into()),
            app_id: Some("x.y".into()),
            profile: Some("p".into()),
            layout: Some("l".into()),
            hold: true,
            wait: true,
        })),
    );
}

#[test]
fn every_method_has_a_golden_request() {
    // 26 golden cases above, one per table row.
    assert_eq!(METHODS.len(), 26);
}

#[test]
fn params_may_be_omitted() {
    let r = req(r#"{"id":4,"method":"tree"}"#);
    assert_eq!(r.params, Params::Tree(TargetOnly::default()));
}

#[test]
fn golden_replies_and_errors() {
    let r = Reply::ok(
        1,
        &HelloReply {
            proto: 1,
            version: "0.1.0".into(),
            granted: vec![Cap(Class::Observe), Cap(Class::Layout)],
            features: vec!["events".into()],
        },
    )
    .unwrap();
    assert_eq!(
        r.to_value(),
        json!({"id":1,"ok":{"proto":1,"version":"0.1.0","granted":["observe","layout"],"features":["events"]}})
    );
    let line = r.to_line().unwrap();
    assert_eq!(line.last(), Some(&b'\n'));
    assert_eq!(
        ServerMessage::decode(&line[..line.len() - 1]).unwrap(),
        ServerMessage::Reply(r)
    );

    let e = Reply::error(Some(9), ErrorCode::Denied, "needs input");
    assert_eq!(
        e.to_value(),
        json!({"id":9,"error":{"code":"denied","message":"needs input"}})
    );
    let line = e.to_line().unwrap();
    assert_eq!(
        ServerMessage::decode(&line[..line.len() - 1]).unwrap(),
        ServerMessage::Reply(e)
    );

    let e = Reply::error(None, ErrorCode::BadRequest, "x");
    assert_eq!(e.to_value()["id"], Value::Null);
    let line = e.to_line().unwrap();
    assert_eq!(
        ServerMessage::decode(&line[..line.len() - 1]).unwrap(),
        ServerMessage::Reply(e)
    );
}

#[test]
fn golden_results_roundtrip_with_nulls() {
    let tree = TreeReply {
        windows: vec![WindowNode {
            id: 1,
            tabs: vec![TabNode {
                id: 2,
                title: None,
                panes: vec![PaneNode {
                    id: 3,
                    title: None,
                    cwd: None,
                    foreground: Some(ForegroundProc {
                        pid: 10,
                        argv: None,
                    }),
                    cols: 80,
                    rows: 24,
                    state: PaneState::Credential,
                }],
            }],
        }],
    };
    let v = serde_json::to_value(&tree).unwrap();
    assert_eq!(
        v,
        json!({"windows":[{"id":1,"tabs":[{"id":2,"title":null,"panes":[{"id":3,"title":null,"cwd":null,
            "foreground":{"pid":10,"argv":null},"cols":80,"rows":24,"state":"credential"}]}]}]})
    );
    assert_eq!(serde_json::from_value::<TreeReply>(v).unwrap(), tree);

    let p = ProcessesReply {
        processes: vec![ProcessInfo {
            pid: 1,
            ppid: 0,
            foreground: true,
            argv: Some(vec!["sh".into()]),
        }],
    };
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(serde_json::from_value::<ProcessesReply>(v).unwrap(), p);

    let l = LaunchReply {
        pane: 1,
        window: 2,
        exit: Some(3),
    };
    assert_eq!(
        serde_json::to_value(&l).unwrap(),
        json!({"pane":1,"window":2,"exit":3})
    );
    let s = SubscribeReply { subscription: 4 };
    assert_eq!(serde_json::to_value(&s).unwrap(), json!({"subscription":4}));
    let t = PaneTextReply {
        text: "hi".into(),
        truncated: false,
    };
    assert_eq!(
        serde_json::to_value(&t).unwrap(),
        json!({"text":"hi","truncated":false})
    );
    let v = VersionReply {
        proto: 1,
        oldest: 1,
        version: "x".into(),
    };
    assert_eq!(
        serde_json::from_value::<VersionReply>(serde_json::to_value(&v).unwrap()).unwrap(),
        v
    );
}

#[test]
fn golden_events() {
    use apoth_action::{Cause, Event, EventKind, FieldValue};
    let mut ev = Event::new(EventKind::CommandFinished, Cause::Sequence);
    ev.set("command", FieldValue::Str("make".into())).unwrap();
    ev.set("status", FieldValue::Int(2)).unwrap();
    ev.set("pane", FieldValue::Int(3)).unwrap();

    let w = WireEvent::from_event(5, Some(1), &ev, false);
    assert_eq!(
        w.to_value(),
        json!({"event":"command-finished","seq":5,"subscription":1,"cause":"sequence",
            "command":"make","status":2,"pane":3})
    );
    assert_eq!(WireEvent::from_value(w.to_value()).unwrap(), w);
    let line = ServerMessage::Event(w.clone()).to_line().unwrap();
    assert_eq!(
        ServerMessage::decode(&line[..line.len() - 1]).unwrap(),
        ServerMessage::Event(w)
    );

    // Redaction: content fields null, others kept.
    let r = WireEvent::from_event(6, None, &ev, true);
    let v = r.to_value();
    assert_eq!(v["command"], Value::Null);
    assert_eq!(v["status"], json!(2));
    assert!(v.get("subscription").is_none());
    assert_eq!(WireEvent::from_value(v).unwrap(), r);
}

#[test]
fn event_rejections() {
    let bad = [
        json!({"event":"nope","seq":1,"cause":"user"}),
        json!({"event":"bell","seq":-1,"cause":"user"}),
        json!({"event":"bell","seq":1,"cause":"alien"}),
        json!({"event":"bell","seq":1,"cause":"user","extra":1}),
        json!({"event":"command-finished","seq":1,"cause":"user","status":"x"}),
        json!({"event":"command-finished","seq":1,"cause":"user","status":null}),
        json!({"event":"bell","cause":"user"}),
    ];
    for b in bad {
        assert!(WireEvent::from_value(b.clone()).is_err(), "{b}");
    }
}

#[test]
fn rejects_malformed_and_unknown() {
    assert_eq!(rej("not json").1, ErrorCode::BadRequest);
    assert_eq!(rej("[1,2]").1, ErrorCode::BadRequest);
    assert_eq!(rej("").1, ErrorCode::BadRequest);
    assert_eq!(rej(r#"{"method":"tree"}"#).1, ErrorCode::BadRequest);
    assert_eq!(
        rej(r#"{"id":"1","method":"tree"}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":1.5,"method":"tree"}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(rej(r#"{"id":-1,"method":"tree"}"#).1, ErrorCode::BadRequest);
    // Unknown envelope field, id still recovered for the reply.
    assert_eq!(
        rej(r#"{"id":3,"method":"tree","extra":1}"#),
        (Some(3), ErrorCode::BadRequest)
    );
    // Unknown method.
    assert_eq!(
        rej(r#"{"id":4,"method":"nope"}"#),
        (Some(4), ErrorCode::UnknownMethod)
    );
    // Unknown params field, on every shape.
    assert_eq!(
        rej(r#"{"id":5,"method":"tree","params":{"x":1}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":5,"method":"version","params":{"x":1}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":5,"method":"pane.text","params":{"extent":"all","bogus":true}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":5,"method":"pane.text","params":{"extent":"wide"}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":5,"method":"tree","params":[]}"#).1,
        ErrorCode::BadRequest
    );
    // Hello: unknown capability, missing client, bad proto type.
    assert_eq!(
        rej(r#"{"id":1,"method":"hello","params":{"proto":1,"client":"c","want":["root"]}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":1,"method":"hello","params":{"proto":1}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":1,"method":"hello","params":{"proto":"1","client":"c"}}"#).1,
        ErrorCode::BadRequest
    );
    // Subscribe: unknown event, empty list.
    assert_eq!(
        rej(r#"{"id":1,"method":"subscribe","params":{"events":["nope"]}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":1,"method":"subscribe","params":{"events":[]}}"#).1,
        ErrorCode::BadRequest
    );
    // Keys: empty, bad chord. Config key shape. Empty target.
    assert_eq!(
        rej(r#"{"id":1,"method":"input.send-keys","params":{"keys":[]}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":1,"method":"input.send-keys","params":{"keys":["hyper+a"]}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":1,"method":"config.get","params":{"key":"Font Size"}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":1,"method":"tree","params":{"target":""}}"#).1,
        ErrorCode::BadRequest
    );
    // Launch: NUL, bad env name.
    assert_eq!(
        rej(r#"{"id":1,"method":"launch","params":{"argv":["a\u0000b"]}}"#).1,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rej(r#"{"id":1,"method":"launch","params":{"env":{"A=B":"c"}}}"#).1,
        ErrorCode::BadRequest
    );
}

#[test]
fn consent_and_sheet_names_are_unknown_actions_and_not_methods() {
    for name in [
        "consent.allow-once",
        "consent.allow-always",
        "consent.deny",
        "allow-once",
        "allow-always",
        "deny",
        "paste-guard.confirm",
        "askpass.submit",
        "sheet.answer",
    ] {
        let line = json!({"id":1,"method":"action.run","params":{"action":name}}).to_string();
        let (id, code) = rej(&line);
        assert_eq!((id, code), (Some(1), ErrorCode::UnknownAction), "{name}");
        let line = json!({"id":1,"method":name,"params":{}}).to_string();
        assert_eq!(rej(&line).1, ErrorCode::UnknownMethod, "{name}");
    }
    assert_eq!(
        rej(r#"{"id":1,"method":"action.run","params":{"action":"pane.focus-next; pane.close"}}"#)
            .1,
        ErrorCode::BadRequest
    );
}

#[test]
fn grant_keys_never_writable() {
    for m in ["config.set", "config.persist"] {
        for key in [
            "socket.grants",
            "socket.key-stream",
            "socket.interpreters",
            "socket",
        ] {
            let line = json!({"id":1,"method":m,"params":{"key":key,"value":"x"}}).to_string();
            assert_eq!(rej(&line).1, ErrorCode::Denied, "{m} {key}");
        }
    }
    assert_eq!(
        rej(r#"{"id":1,"method":"config.reset","params":{"key":"socket.grants"}}"#).1,
        ErrorCode::Denied
    );
    assert_eq!(
        rej(r#"{"id":1,"method":"launch","params":{"overrides":{"socket.grants":"x"}}}"#).1,
        ErrorCode::Denied
    );
    // No field smuggles a bypass: strictness covers it.
    assert_eq!(
        rej(r#"{"id":1,"method":"config.set","params":{"key":"font.size","value":"1","force":true}}"#).1,
        ErrorCode::BadRequest
    );
}

#[test]
fn oversize_line_rejected() {
    let big = vec![b' '; MAX_LINE_BYTES + 1];
    let e = Request::decode(&big).unwrap_err();
    assert_eq!(e.error.code, ErrorCode::LineTooLong);
    assert_eq!(
        e.reply().to_value()["error"]["code"],
        json!("line-too-long")
    );
    assert_eq!(
        ServerMessage::decode(&big).unwrap_err().code,
        ErrorCode::LineTooLong
    );
    // Within the line limit but over a field budget.
    let line = json!({"id":1,"method":"tree","params":{"target":"x".repeat(5000)}}).to_string();
    assert_eq!(rej(&line).1, ErrorCode::BadRequest);
}

#[test]
fn server_message_rejections() {
    for l in [
        r#"{"id":1}"#,
        r#"{"id":1,"ok":1,"error":{"code":"denied","message":""}}"#,
        r#"{"id":1,"ok":1,"extra":2}"#,
        r#"{"id":1,"error":{"code":"nope","message":""}}"#,
        r#"{"event":"bell","seq":1,"cause":"user","ok":1}"#,
        r#"[1]"#,
        "garbage",
    ] {
        assert!(ServerMessage::decode(l.as_bytes()).is_err(), "{l}");
    }
}

#[test]
fn classification_via_apoth_action() {
    let c = |line: &str| req(line).params.classify(&NoContext);
    assert_eq!(
        c(r#"{"id":1,"method":"action.run","params":{"action":"pane.focus-next"}}"#).classes,
        ClassSet::of(&[Class::Layout])
    );
    assert!(
        c(r#"{"id":1,"method":"action.run","params":{"action":"input.send-keys ctrl+c"}}"#)
            .classes
            .contains(Class::Input)
    );
    assert_eq!(
        c(r#"{"id":1,"method":"pane.text","params":{}}"#).classes,
        ClassSet::of(&[Class::ReadContent])
    );
    assert_eq!(
        c(r#"{"id":1,"method":"config.reload"}"#).classes,
        ClassSet::of(&[Class::Appearance])
    );
    assert_eq!(
        c(r#"{"id":1,"method":"config.persist","params":{"key":"font.size","value":"1"}}"#).classes,
        ClassSet::of(&[Class::ConfigWrite])
    );
    // Unknown keys fail closed under the empty context: refused, never a bare grant.
    assert!(
        c(r#"{"id":1,"method":"config.set","params":{"key":"font.size","value":"1"}}"#)
            .refused
            .is_some()
    );
    assert!(
        c(r#"{"id":1,"method":"notify","params":{"title":"x"}}"#)
            .classes
            .contains(Class::Layout)
    );
    assert!(
        c(r#"{"id":1,"method":"launch","params":{}}"#)
            .classes
            .contains(Class::Spawn)
    );
}

#[test]
fn input_injecting_marker() {
    for r in METHODS {
        assert_eq!(r.method.injects_input(), r.classes.contains(Class::Input));
    }
    assert!(Method::InputSendKeys.injects_input());
    let r = req(r#"{"id":1,"method":"action.run","params":{"action":"input.send-text hi"}}"#);
    assert!(apoth_proto::injects_input(
        r.params.classify(&NoContext).classes
    ));
}

#[test]
fn exit_codes_and_env_names() {
    assert_eq!(ErrorCode::Denied.exit_code(), 3);
    assert_eq!(ErrorCode::NoInstance.exit_code(), 4);
    assert_eq!(apoth_proto::ENV_SOCKET, "APOTHEOSIS_SOCKET");
    assert_eq!(apoth_proto::ENV_PANE, "APOTHEOSIS_PANE");
    assert_eq!(apoth_proto::ENV_WINDOW, "APOTHEOSIS_WINDOW");
    assert_eq!(apoth_proto::ENV_VERSION, "APOTHEOSIS_VERSION");
    assert_eq!(apoth_proto::DEFAULT_TARGET, "self");
}

#[test]
fn version_negotiation() {
    assert_eq!(apoth_proto::negotiate(1).unwrap(), 1);
    assert_eq!(
        apoth_proto::negotiate(99).unwrap(),
        apoth_proto::PROTO_VERSION
    );
    assert_eq!(
        apoth_proto::negotiate(0).unwrap_err().code,
        ErrorCode::UnsupportedVersion
    );
}

#[test]
fn corpus_seeds_never_panic() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus/proto_json");
    let mut n = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let data = std::fs::read(entry.unwrap().path()).unwrap();
        apoth_proto::fuzz_parse(&data);
        n += 1;
    }
    assert!(n >= 8);
}

#[test]
fn fuzz_entry_handles_junk() {
    for d in [
        &b""[..],
        b"\n\n\n",
        b"{",
        b"\xff\xfe",
        b"{\"id\":1,\"method\":\"tree\"}\n{",
    ] {
        apoth_proto::fuzz_parse(d);
    }
}

#[test]
fn launch_overrides_classified_per_key() {
    use apoth_action::{ClassContext, OverrideClass, Refusal};
    struct Ctx;
    impl ClassContext for Ctx {
        fn override_class(&self, k: &str) -> Option<OverrideClass> {
            match k {
                "font.size" => Some(OverrideClass::Appearance),
                "scrollback.lines" => Some(OverrideClass::Behaviour),
                "clipboard.read" => Some(OverrideClass::Security),
                _ => None,
            }
        }
    }
    let c = |k: &str| {
        let line =
            format!(r#"{{"id":1,"method":"launch","params":{{"overrides":{{"{k}":"1"}}}}}}"#);
        req(&line).params.classify(&Ctx)
    };
    let a = c("font.size");
    assert!(a.classes.contains(Class::Spawn) && a.classes.contains(Class::Appearance));
    assert!(a.refused.is_none() && !a.classes.contains(Class::ConfigWrite));
    for k in ["scrollback.lines", "clipboard.read"] {
        let r = c(k);
        assert!(r.classes.contains(Class::Spawn) && r.classes.contains(Class::ConfigWrite));
        assert!(r.refused.is_none());
    }
    let g = c("mystery.key");
    assert!(g.classes.contains(Class::ConfigWrite));
    assert_eq!(g.refused, Some(Refusal::GrantKey));
}
