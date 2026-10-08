// SPDX-License-Identifier: AGPL-3.0-only
//! A-08 §13 "No task without a slot": no path in agentd creates, previews,
//! unpauses or resumes a task, answers a prompt, or touches grants. Enforced
//! by grepping the source, so adding such a path fails here first.

use std::path::Path;

fn sources() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "rs") {
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            ));
        }
    }
    assert!(out.len() > 5);
    out
}

/// The part of a file that is not a `#[cfg(test)]` module.
fn non_test(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap()
}

#[test]
fn the_console_surface_has_no_task_creating_method() {
    let banned = [
        "\"create_task\"",
        "\"preview_task\"",
        "\"resume_session\"",
        "\"unpause_task\"",
        "\"answer_prompt\"",
        "\"set_draft\"",
        "\"grant",
        "ToPolicyd::CreateTask",
        "ToPolicyd::PreviewTask",
        "ToPolicyd::UnpauseTask",
        "ToPolicyd::OpenTask",
        "ToPolicyd::Mint",
        "ToPolicyd::Defer",
        "ToPolicyd::Revoke",
        "ToPolicyd::InstallBegin",
        "ToPolicyd::InstallAnswer",
    ];
    for (name, src) in sources() {
        for b in banned {
            assert!(!non_test(&src).contains(b), "{name} contains {b}");
        }
    }
}

#[test]
fn the_only_messages_agentd_sends_policyd_are_pause_cancel_and_exited() {
    for (name, src) in sources() {
        let s = non_test(&src);
        let mut rest = s;
        while let Some(i) = rest.find("ToPolicyd::") {
            rest = &rest[i + "ToPolicyd::".len()..];
            let variant: String = rest.chars().take_while(|c| c.is_alphanumeric()).collect();
            assert!(
                matches!(variant.as_str(), "PauseTask" | "CancelTask" | "Exited" | "decode"),
                "{name} uses ToPolicyd::{variant}"
            );
        }
    }
}

#[test]
fn console_methods_are_exactly_the_spec_table() {
    let core = sources().into_iter().find(|(n, _)| n == "core.rs").unwrap().1;
    let body = non_test(&core);
    let start = body.find("fn console_call").unwrap();
    let end = body[start..].find("fn m_list_tasks").unwrap() + start;
    let mut methods: Vec<String> = Vec::new();
    for line in body[start..end].lines() {
        let t = line.trim();
        if t.starts_with('"') && t.contains("=>") {
            for part in t.split("=>").next().unwrap().split('|') {
                methods.push(part.trim().trim_matches('"').to_owned());
            }
        }
    }
    methods.sort();
    let mut want = [
        "list_packages",
        "list_tasks",
        "get_task",
        "conversation_read",
        "conversation_post",
        "pause_task",
        "cancel_task",
        "show_decisions",
        "unlock_secrets",
        "list_sessions",
        "delete_session",
        "subscribe",
        "unsubscribe",
    ]
    .map(str::to_owned)
    .to_vec();
    want.sort();
    assert_eq!(methods, want);
}
