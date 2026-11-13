// SPDX-License-Identifier: Apache-2.0
//! Registry completeness, exhaustive round-trip, class table and consent tests
//! (T-11 §10, M4-applicable part).

use std::collections::{HashMap, HashSet};

use apoth_action::argv::Placeholder;
use apoth_action::enums::CaptureDest;
use apoth_action::text::Call;
use apoth_action::{
    Action, ActionInfo, Class, ClassContext, ClassSet, ConsentError, ErrorKind, FAMILIES, MacroDef,
    MacroSet, NoContext, OverrideClass, ParamKind, REGISTRY, Refusal, lookup, rule_consent, search,
};

fn kebab(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

#[test]
fn registry_is_complete() {
    assert!(
        REGISTRY.len() >= 100,
        "R1 floor is ~100 actions, have {}",
        REGISTRY.len()
    );
    let bare: HashSet<&str> = ["capture", "pipe", "launch", "open", "notify"].into();
    let mut ids = HashSet::new();
    for i in REGISTRY {
        assert!(ids.insert(i.id), "duplicate id {}", i.id);
        assert!(!i.title.trim().is_empty(), "{}: title", i.id);
        assert!(!i.doc.trim().is_empty(), "{}: doc", i.id);
        assert!(
            FAMILIES.contains(&i.family()),
            "{}: family not in closed set",
            i.id
        );
        match i.id.split_once('.') {
            Some((fam, verb)) => {
                assert!(kebab(fam) && kebab(verb), "{}: not family.verb kebab", i.id);
                assert!(!verb.contains('.'), "{}", i.id);
            }
            None => assert!(
                bare.contains(i.id),
                "{}: bare id outside the closed bare set",
                i.id
            ),
        }
        assert!(
            !i.classes.is_empty() || i.is_argument_dependent(),
            "{}: no class",
            i.id
        );
        let mut names = HashSet::new();
        for p in i.params {
            assert!(kebab(p.name), "{}: param {}", i.id, p.name);
            assert!(names.insert(p.name), "{}: duplicate param {}", i.id, p.name);
        }
        assert!(
            i.params
                .iter()
                .filter(|p| p.kind == ParamKind::Argv)
                .count()
                <= 1,
            "{}: more than one argv",
            i.id
        );
    }
}

#[test]
fn every_family_but_r2_prompt_has_members() {
    for fam in FAMILIES {
        let n = REGISTRY.iter().filter(|i| i.family() == *fam).count();
        if *fam == "prompt" {
            assert_eq!(n, 0, "prompt.* is R2 sheets");
        } else {
            assert!(n > 0, "family {fam} is empty");
        }
    }
}

#[test]
fn spec_examples_exist() {
    for id in [
        "input.send-text",
        "input.send-bytes",
        "input.send-keys",
        "input.paste",
        "clipboard.copy",
        "clipboard.paste-to",
        "clipboard.clear",
        "selection.select",
        "selection.extend",
        "selection.clear",
        "selection.copy",
        "scroll.page-up",
        "search.start",
        "cursor.jump-char",
        "hints.start",
        "hints.cancel",
        "block.jump",
        "block.select-output",
        "block.copy-output",
        "block.pipe-output",
        "block.open-output",
        "block.copy-command",
        "mark.set",
        "palette.toggle",
        "interface.focus-next",
        "pane.split",
        "pane.close",
        "pane.focus",
        "pane.move",
        "workspace.switch",
        "layout.apply",
        "layout.save",
        "layout.next",
        "broadcast.set",
        "keyboard.toggle-inhibit",
        "config.set",
        "config.reset",
        "config.reload",
        "config.open",
        "config.save-override",
        "config.persist",
        "var.set",
        "var.unset",
        "var.get",
        "session.save",
        "session.restore",
        "scrollback.clear",
        "scrollback.save",
        "capture",
        "pipe",
        "launch",
        "open",
        "coprocess.run",
        "macro.run",
        "notify",
        "media.play",
        "media.pause",
        "media.seek",
        "media.hide",
        "media.delete",
        "media.save",
        "graphics.hide",
        "graphics.delete",
        "graphics.save",
        "debug.toggle-inspector",
        "debug.toggle-key-overlay",
        "app.quit",
        "app.new-instance",
        "key-table.push",
        "key-table.pop",
        "tab.new",
        "window.new",
    ] {
        assert!(lookup(id).is_some(), "{id} missing");
    }
}

#[test]
fn consent_and_sheet_answers_have_no_id() {
    let forbidden = [
        "allow",
        "deny",
        "consent",
        "askpass",
        "paste-guard",
        "confirm",
        "sheet",
        "approve",
        "grant",
    ];
    for i in REGISTRY {
        for w in forbidden {
            assert!(!i.id.contains(w), "{} looks like a consent answer", i.id);
        }
    }
    for name in [
        "consent.allow-once",
        "consent.allow-always",
        "consent.deny",
        "paste-guard.confirm",
        "askpass.submit",
        "sheet.answer",
        "consent.allow",
        "rule.allow-once",
    ] {
        let e = Action::parse(name).unwrap_err();
        assert_eq!(e.kind, ErrorKind::UnknownAction, "{name}");
    }
}

fn sample(kind: ParamKind) -> String {
    match kind {
        ParamKind::Text => "x".into(),
        ParamKind::Bool => "true".into(),
        ParamKind::Int | ParamKind::Uint => "1".into(),
        ParamKind::Float => "0.5".into(),
        ParamKind::Duration => "10s".into(),
        ParamKind::Argv => unreachable!(),
        ParamKind::Hex => "00ff".into(),
        ParamKind::Keys => "ctrl+a".into(),
        ParamKind::Char => "x".into(),
        ParamKind::Enum(names) => names[0].to_string(),
    }
}

fn call_for(info: &ActionInfo, all: bool) -> Call {
    let mut c = Call::new(info.id);
    for p in info.params {
        if !all && !p.required {
            continue;
        }
        if p.kind == ParamKind::Argv {
            c.argv = Some(vec!["echo".into(), "{url}".into()]);
        } else {
            c.named.push((p.name.to_string(), sample(p.kind)));
        }
    }
    c
}

fn roundtrip(a: &Action) {
    let call = a.to_call();
    let text = call.format();
    let b = Action::parse(&text).unwrap_or_else(|e| panic!("`{text}`: {e}"));
    assert_eq!(a, &b, "{text}");
    assert_eq!(b.to_call(), call, "canonical form is stable: {text}");
    assert_eq!(Action::format_script(std::slice::from_ref(a)), text);
    assert_eq!(a.to_string(), text);
    let c: Action = text.parse().unwrap();
    assert_eq!(&c, a);
}

#[test]
fn every_action_builds_formats_and_reparses_equal() {
    let mut skipped_all = Vec::new();
    for info in REGISTRY {
        let a = Action::from_call(&call_for(info, false))
            .unwrap_or_else(|e| panic!("{} required-only: {e}", info.id));
        assert_eq!(a.id(), info.id);
        assert_eq!(a.info().id, info.id);
        roundtrip(&a);
        match Action::from_call(&call_for(info, true)) {
            Ok(a) => {
                assert_eq!(a.id(), info.id);
                roundtrip(&a);
            }
            Err(e) => {
                assert_eq!(e.kind, ErrorKind::Invalid, "{}: {e}", info.id);
                skipped_all.push(info.id);
            }
        }
    }
    // only the cross-field-constrained ones may be skipped in the all-parameters pass
    for id in &skipped_all {
        assert!(["capture", "launch"].contains(id), "{id}");
    }
}

#[test]
fn script_roundtrip_with_chaining() {
    let src = "pane.split right -- htop; input.send-text \"a;b\\n\"; key-table.push leader one-shot=true timeout=2s";
    let v = Action::parse_script(src).unwrap();
    assert_eq!(v.len(), 3);
    let again = Action::parse_script(&Action::format_script(&v)).unwrap();
    assert_eq!(v, again);
    assert!(Action::parse("a.b; c.d").is_err());
    assert_eq!(Action::parse_script("").unwrap().len(), 0);
}

#[test]
fn launch_and_capture_all_options() {
    let a = Action::parse(
        "launch split cwd=last-reported workdir=/tmp env=\"A=1\" env=\"B=2\" stdin=selection hold=true \
         location=down bias=30 profile=dev title=t -- nvim {file}",
    )
    .unwrap();
    roundtrip(&a);
    assert!(Action::parse("launch tab location=down").is_err());
    assert!(Action::parse("launch split bias=0").is_err());
    assert!(Action::parse("launch split bias=100").is_err());
    assert!(Action::parse("launch tab env=\"1A=2\"").is_err());
    assert!(Action::parse("launch tab env=novalue").is_err());
    for dest in CaptureDest::NAMES {
        let ok = match *dest {
            "clipboard" => "capture visible clipboard",
            "file" => "capture visible file path=/tmp/o",
            "pager" => "capture scrollback pager",
            "editor" => "capture selection editor -- vi {file}",
            "command" => "capture last-command-output command -- cat",
            _ => unreachable!(),
        };
        roundtrip(&Action::parse(ok).unwrap());
    }
    assert!(Action::parse("capture visible file").is_err());
    assert!(Action::parse("capture visible command").is_err());
    assert!(Action::parse("capture visible clipboard path=/x").is_err());
}

#[test]
fn parse_errors_have_kinds_and_suggestions() {
    let e = Action::parse("pane.splt right").unwrap_err();
    assert_eq!(e.kind, ErrorKind::UnknownAction);
    assert_eq!(e.suggestion.as_deref(), Some("pane.split"));
    let e = Action::parse("pane.split").unwrap_err();
    assert_eq!(e.kind, ErrorKind::MissingParam);
    let e = Action::parse("pane.split right argvv=1").unwrap_err();
    assert_eq!(e.kind, ErrorKind::UnknownParam);
    let e = Action::parse("pane.split right cwd=/ extra extra2").unwrap_err();
    assert_eq!(e.kind, ErrorKind::UnknownParam);
    let e = Action::parse("pane.split sideways").unwrap_err();
    assert_eq!(e.kind, ErrorKind::BadValue);
    assert!(Action::parse("pane.close force=true force=true").is_err());
    assert!(Action::parse("tab.close -- ls").is_err());
    assert!(Action::parse("pane.split right -- {url}").is_err());
    assert!(Action::parse("launch tab -- sh -c \"echo {url}\"").is_err());
}

#[test]
fn typed_values_are_validated() {
    assert!(Action::parse("tab.select 0").is_err());
    assert!(Action::parse("tab.select 3").is_ok());
    assert!(Action::parse("font.size-set -1").is_err());
    assert!(Action::parse("font.size-set 11.5").is_ok());
    assert!(Action::parse("font.size-set nan").is_err());
    assert!(Action::parse("media.volume 1.5").is_err());
    assert!(Action::parse("input.send-bytes zz").is_err());
    assert!(Action::parse("input.send-keys ctrl+").is_err());
    assert!(Action::parse("input.send-csi \"\\x01\"").is_err());
    assert!(Action::parse("var.set \"bad name\" v").is_err());
    let big = "x".repeat(1025);
    assert!(Action::parse(&format!("var.set n {big}")).is_err());
    assert!(Action::parse(&format!("var.set n {}", &big[..1024])).is_ok());
    assert!(Action::parse("config.set Font.Size 1").is_err());
    assert!(Action::parse("cursor.jump-char ab").is_err());
    assert!(Action::parse("key-table.push t timeout=99h").is_err());
}

#[test]
fn text_budget_is_enforced() {
    let big = "x".repeat(64 * 1024 + 1);
    assert!(Action::parse(&format!("input.send-text \"{big}\"")).is_err());
}

fn cls(a: &str) -> ClassSet {
    Action::parse(a)
        .unwrap_or_else(|e| panic!("{a}: {e}"))
        .classify(&NoContext)
        .classes
}

fn set(c: &[Class]) -> ClassSet {
    ClassSet::of(c)
}

#[test]
fn class_table_matches_spec() {
    use Class::*;
    let table: &[(&str, &[Class])] = &[
        ("input.send-text hi", &[Input]),
        ("input.send-bytes 00", &[Input]),
        ("input.send-keys ctrl+a", &[Input]),
        ("input.paste", &[Input, Clipboard]),
        ("clipboard.copy x", &[Clipboard]),
        ("clipboard.clear", &[Clipboard]),
        ("clipboard.paste-to", &[Clipboard]),
        ("selection.select word", &[ReadContent]),
        ("selection.clear", &[ReadContent]),
        ("selection.copy", &[ReadContent, Clipboard]),
        ("scroll.page-up", &[Layout]),
        ("search.start", &[Layout]),
        ("copy-mode.enter", &[Layout]),
        ("key-table.pop", &[Layout]),
        ("cursor.jump-char x", &[Layout]),
        ("hints.start", &[Layout]),
        ("block.jump next", &[Layout]),
        ("block.select-output", &[Layout]),
        ("block.copy-output", &[ReadContent, Clipboard]),
        ("block.copy-command", &[ReadContent, Clipboard]),
        ("block.pipe-output -- cat", &[Spawn, ReadContent]),
        ("block.open-output", &[Spawn, ReadContent]),
        ("block.open-output -- less", &[Spawn, ReadContent]),
        ("mark.set", &[Layout]),
        ("bookmark.add x", &[Layout]),
        ("palette.toggle", &[Layout]),
        ("interface.focus-next", &[Layout]),
        ("pane.split right", &[Layout]),
        ("pane.split right -- htop", &[Layout, Spawn]),
        ("pane.close", &[Layout]),
        ("tab.new", &[Layout]),
        ("tab.new -- sh", &[Layout, Spawn]),
        ("window.new -- sh", &[Layout, Spawn]),
        ("workspace.switch w", &[Layout]),
        ("layout.save x", &[Layout]),
        ("layout.next", &[Layout]),
        ("broadcast.set all", &[Input]),
        ("keyboard.toggle-inhibit", &[Input]),
        ("font.size-set 12", &[Appearance]),
        ("font.family-set mono", &[Appearance]),
        ("theme.set dark", &[Appearance]),
        ("config.reload", &[Appearance]),
        ("config.open", &[Spawn]),
        ("config.open -- micro {file}", &[Spawn]),
        ("config.save-override scope=pane", &[ConfigWrite]),
        ("var.set a b", &[Layout]),
        ("var.unset a", &[Layout]),
        ("var.get a", &[ReadContent]),
        ("session.save", &[Layout]),
        ("session.restore", &[Spawn]),
        ("scrollback.clear", &[Layout]),
        ("scrollback.save /tmp/x", &[ReadContent, Spawn]),
        ("capture visible clipboard", &[ReadContent, Clipboard]),
        ("capture visible file path=/tmp/x", &[ReadContent, Spawn]),
        ("capture visible pager", &[ReadContent, Spawn]),
        ("capture visible editor", &[ReadContent, Spawn]),
        ("capture visible command -- cat", &[ReadContent, Spawn]),
        ("pipe selection -- cat", &[Spawn, ReadContent]),
        ("launch tab", &[Spawn]),
        ("launch tab -- sh", &[Spawn]),
        ("open http://x", &[Spawn]),
        ("coprocess.run -- cat", &[Spawn, ReadContent, Input]),
        ("notify hello", &[Layout]),
        ("media.play", &[Layout]),
        ("media.seek 5", &[Layout]),
        ("media.save /x", &[ReadContent, Spawn]),
        ("graphics.delete", &[Layout]),
        ("graphics.save /x", &[ReadContent, Spawn]),
        ("debug.toggle-inspector", &[ReadContent]),
        ("debug.toggle-key-overlay", &[ReadContent]),
        ("app.quit", &[Layout]),
        ("app.new-instance", &[Layout]),
    ];
    for (a, expected) in table {
        assert_eq!(cls(a), set(expected), "{a}");
    }
}

/// Deliberate tightenings beyond the T-11 §1 table, recorded in CLAUDE.md.
#[test]
fn recorded_class_tightenings() {
    use Class::*;
    assert_eq!(cls("input.paste-selection"), set(&[Input, Clipboard]));
    assert_eq!(cls("pane.signal-child int"), set(&[Input]));
}

#[test]
fn user_action_ids_are_namespaced() {
    assert!(apoth_action::is_user_action_id("User.my-thing"));
    assert!(apoth_action::is_user_action_id("User.a.b"));
    assert!(!apoth_action::is_user_action_id("User."));
    assert!(!apoth_action::is_user_action_id("pane.split"));
    assert!(!apoth_action::is_user_action_id("User.has space"));
    assert!(!apoth_action::is_user_action_id("user.x"));
}

#[test]
fn every_registry_base_class_is_a_subset_of_classify() {
    for info in REGISTRY {
        let a = Action::from_call(&call_for(info, false)).unwrap();
        let c = a.classify(&NoContext);
        assert!(set(info.classes).is_subset(c.classes), "{}", info.id);
    }
}

#[test]
fn argv_content_placeholders_need_read_content() {
    assert!(cls("launch tab -- echo {selection}").contains(Class::ReadContent));
    assert!(cls("pipe visible -- echo {match}").contains(Class::ReadContent));
    assert!(cls("open x -- xdg-open {url}").contains(Class::ReadContent));
    assert!(!cls("launch tab -- echo {cwd} {file} {line} {pane-id}").contains(Class::ReadContent));
    assert!(cls("launch background stdin=visible -- cat").contains(Class::ReadContent));
    let _ = Placeholder::Selection;
}

struct Ctx {
    keys: HashMap<&'static str, OverrideClass>,
    layouts: HashMap<&'static str, bool>,
}

impl ClassContext for Ctx {
    fn override_class(&self, key: &str) -> Option<OverrideClass> {
        self.keys.get(key).copied()
    }
    fn layout_has_argv(&self, name: &str) -> Option<bool> {
        self.layouts.get(name).copied()
    }
}

fn ctx() -> Ctx {
    Ctx {
        keys: HashMap::from([
            ("font.size", OverrideClass::Appearance),
            ("scrollback.lines", OverrideClass::Behaviour),
            ("clipboard.osc52-write", OverrideClass::Security),
            ("socket.grants", OverrideClass::Grant),
        ]),
        layouts: HashMap::from([("plain", false), ("cmds", true)]),
    }
}

#[test]
fn config_set_class_follows_override_class() {
    let c = ctx();
    let k = |a: &str| Action::parse(a).unwrap().classify(&c);
    for verb in ["config.set", "config.reset"] {
        let tail = if verb == "config.set" { " 1" } else { "" };
        let a = k(&format!("{verb} font.size{tail}"));
        assert_eq!(a.classes, set(&[Class::Appearance]));
        assert_eq!(a.refused, None);
        let a = k(&format!("{verb} scrollback.lines{tail}"));
        assert_eq!(a.classes, set(&[Class::ConfigWrite]));
        assert_eq!(a.refused, None);
        let a = k(&format!("{verb} clipboard.osc52-write{tail}"));
        assert_eq!(a.refused, Some(Refusal::RuntimeSecurityKey));
        assert!(a.refused.unwrap().to_string().contains("security"));
        let a = k(&format!("{verb} socket.grants{tail}"));
        assert_eq!(a.refused, Some(Refusal::GrantKey));
        assert!(a.refused.unwrap().to_string().contains("grant"));
        // unknown key: fail-closed as grant
        let a = k(&format!("{verb} no.such-key{tail}"));
        assert_eq!(a.refused, Some(Refusal::GrantKey));
    }
    // persist: security needs config-write (not refused), grant is refused
    let p = k("config.persist clipboard.osc52-write true");
    assert_eq!(p.classes, set(&[Class::ConfigWrite]));
    assert_eq!(p.refused, None);
    let p = k("config.persist socket.grants x");
    assert_eq!(p.refused, Some(Refusal::GrantKey));
    // closed context: everything unknown, so refused
    assert!(
        Action::parse("config.set font.size 1")
            .unwrap()
            .classify(&NoContext)
            .refused
            .is_some()
    );
}

#[test]
fn layout_apply_spawn_only_with_argv() {
    let c = ctx();
    let k = |a: &str| Action::parse(a).unwrap().classify(&c).classes;
    assert_eq!(k("layout.apply plain"), set(&[Class::Layout]));
    assert_eq!(k("layout.apply cmds"), set(&[Class::Layout, Class::Spawn]));
    assert!(k("layout.apply ghost").contains(Class::Spawn));
}

fn def(name: &str, members: &[&str]) -> MacroDef {
    MacroDef {
        name: name.into(),
        members: members.iter().map(|m| Action::parse(m).unwrap()).collect(),
    }
}

#[test]
fn rule_consent_through_macros() {
    let s = MacroSet::build(vec![
        def("wrap-launch", &["notify hi", "launch tab -- sh"]),
        def("wrap-capture", &["capture visible command -- cat"]),
        def("wrap-split", &["pane.split right -- top"]),
        def("typer", &["input.send-text x"]),
        def("harmless", &["notify hi", "font.size-increase", "tab.next"]),
        def("reader", &["var.get x"]),
    ])
    .unwrap();
    let c = ctx();
    let cx = s.with(&c);
    let consent = |name: &str, input: bool, spawn: bool| {
        let a = Action::parse(&format!("macro.run {name}")).unwrap();
        rule_consent(&a.classify(&cx), input, spawn)
    };
    for m in ["wrap-launch", "wrap-split"] {
        assert_eq!(
            consent(m, true, false),
            Err(ConsentError::NeedsAllowSpawn),
            "{m}"
        );
    }
    // capture to a command also reads content: refused whatever the opt-ins
    assert!(consent("wrap-capture", false, false).is_err());
    // wrap-capture reads content: never allowed from a rule, even with both opt-ins
    assert_eq!(
        consent("wrap-capture", true, true),
        Err(ConsentError::Never(Class::ReadContent))
    );
    assert_eq!(consent("wrap-launch", false, true), Ok(()));
    assert_eq!(consent("wrap-split", false, true), Ok(()));
    assert_eq!(
        consent("typer", false, true),
        Err(ConsentError::NeedsAllowInput(Class::Input))
    );
    assert_eq!(consent("typer", true, false), Ok(()));
    assert_eq!(consent("harmless", false, false), Ok(()));
    assert_eq!(
        consent("reader", true, true),
        Err(ConsentError::Never(Class::ReadContent))
    );
}

#[test]
fn palette_and_socket_reach_every_action() {
    assert_eq!(search("").len(), REGISTRY.len());
    for info in REGISTRY {
        assert!(
            search(info.id).iter().any(|i| i.id == info.id),
            "{}",
            info.id
        );
        assert!(lookup(info.id).is_some());
        // the socket's `action.run` takes the same textual/typed form
        let a = Action::from_call(&call_for(info, false)).unwrap();
        assert_eq!(a.id(), info.id);
    }
    let hits = search("split");
    assert!(hits.iter().any(|i| i.id == "pane.split"));
    assert!(search("zzzzqqq").is_empty());
}
