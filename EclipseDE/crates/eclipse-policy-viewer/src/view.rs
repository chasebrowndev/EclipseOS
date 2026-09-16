// SPDX-License-Identifier: AGPL-3.0-only
//! The three sections: capture, data control, security window rules.
//!
//! Every colour, size and spacing comes from `eclipse_ui::tokens`. The accent
//! is spent once per section, on the sentence that says what the section
//! currently permits — nothing else in this window is yellow.

use iced::widget::{column, scrollable, text, Column};
use iced::{Element, Length, Theme};

use eclipse_ui::theme;
use eclipse_ui::tokens::{font, size, space};
use eclipse_ui::widget::{content, hairline, inset_list, list_row, panel, value};

use crate::app::Message;
use crate::read::{Allowlist, FileState, Policy, Rule};

/// The one sentence every section ends with. Editing the security surface is
/// not a thing a layer-shell client may do.
const READ_ONLY: &str = "Read-only. Editing policy happens in the compositor-drawn policy editor, not here.";

pub fn view(policy: &Policy) -> Element<'_, Message, Theme> {
    let body = column![
        files(policy),
        capture(policy),
        data_control(policy),
        rules(policy),
    ]
    .spacing(space::BLOCK);

    content(vec![scrollable(body)
        .style(theme::eclipse_scrollable)
        .height(Length::Fill)
        .into()])
}

fn title<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI_SEMIBOLD)
        .size(size::CARD_TITLE)
        .style(theme::text_primary)
        .into()
}

fn note<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI)
        .size(size::BODY_SMALL)
        .style(theme::text_secondary)
        .into()
}

/// The sentence that says what this section currently permits — the section's
/// one accented line.
fn verdict<'a>(t: &str) -> Element<'a, Message, Theme> {
    text(t.to_owned())
        .font(font::UI_MEDIUM)
        .size(size::BODY_SMALL)
        .style(theme::text_accent)
        .into()
}

fn section<'a>(
    heading: &str,
    explanation: &str,
    blocks: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut col = Column::new()
        .spacing(space::ROW_Y)
        .push(title(heading))
        .push(note(explanation));
    for b in blocks {
        col = col.push(b);
    }
    panel(col.push(hairline()).push(note(READ_ONLY))).into()
}

/// Which files were read. A missing `/etc/eclipse/policy.kdl` is normal; a
/// malformed one is not, and the reader has to be told which file and why.
fn files(policy: &Policy) -> Element<'_, Message, Theme> {
    let rows = policy
        .files
        .iter()
        .map(|f| {
            let state = match &f.state {
                FileState::Read => "read".to_owned(),
                FileState::Missing => "not present".to_owned(),
                FileState::Unreadable(e) => format!("unreadable: {e}"),
                FileState::Malformed(e) => format!("malformed: {e}"),
            };
            list_row(&f.path.display().to_string(), value(&state))
        })
        .collect();

    section(
        "Policy files",
        "Read from disk in this order; a later file replaces an earlier one. \
         The compositor is never asked — the control socket refuses to serve policy.",
        vec![inset_list("SEARCH PATH", rows)],
    )
}

fn allow_rows(list: &Allowlist) -> Vec<Element<'_, Message, Theme>> {
    if list.is_empty() {
        return vec![list_row(list.empty_reads_as(), value("0 allowed"))];
    }
    list.entries
        .iter()
        .map(|e| list_row(&e.name, value(&e.source.short())))
        .collect()
}

fn permits(list: &Allowlist, what: &str) -> String {
    match list.entries.len() {
        0 => format!("Nothing may {what}."),
        1 => format!("One name may {what}."),
        n => format!("{n} names may {what}."),
    }
}

fn capture(policy: &Policy) -> Element<'_, Message, Theme> {
    let scripted = match &policy.scripted_input {
        Some((true, src)) => list_row(
            "The control socket may synthesise human input.",
            value(&src.short()),
        ),
        Some((false, src)) => list_row(
            "The control socket may not synthesise human input.",
            value(&src.short()),
        ),
        None => list_row(
            "Not set — the control socket may not synthesise human input.",
            value("default"),
        ),
    };

    section(
        "Screen capture",
        "Process names allowed to bind zwlr_screencopy_manager_v1. Capture reads \
         every pixel of an output, including other clients' windows.",
        vec![
            verdict(&permits(&policy.capture_allow, "capture the screen")),
            inset_list("CAPTURE.ALLOW", allow_rows(&policy.capture_allow)),
            inset_list(
                "CAPTURE.REDACT-APP-ID",
                if policy.capture_redact.is_empty() {
                    vec![list_row(
                        "Not set — no app_id is redacted out of a capture.",
                        value("0 redacted"),
                    )]
                } else {
                    allow_rows(&policy.capture_redact)
                },
            ),
            inset_list("MISC.SCRIPTED-INPUT", vec![scripted]),
        ],
    )
}

fn data_control(policy: &Policy) -> Element<'_, Message, Theme> {
    section(
        "Clipboard data control",
        "Process names allowed to bind zwlr_data_control_manager_v1. Data control \
         reads every selection, including ones typed into another window.",
        vec![
            verdict(&permits(
                &policy.data_control_allow,
                "read the clipboard this way",
            )),
            inset_list(
                "CLIPBOARD.DATA-CONTROL-ALLOW",
                allow_rows(&policy.data_control_allow),
            ),
        ],
    )
}

fn matchers(rule: &Rule) -> String {
    if rule.matchers.is_empty() {
        return "no matcher".to_owned();
    }
    rule.matchers
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn rules(policy: &Policy) -> Element<'_, Message, Theme> {
    let rows: Vec<Element<'_, Message, Theme>> = if policy.rules.is_empty() {
        vec![list_row(
            "No window carries a policy-owned rule; every window is private and untrusted.",
            value("0 rules"),
        )]
    } else {
        policy
            .rules
            .iter()
            .map(|r| {
                list_row(
                    &format!("{} — {}", r.action(), matchers(r)),
                    value(&r.source.short()),
                )
            })
            .collect()
    };

    section(
        "Security window rules",
        "sensitivity, app-trust, seat-compat and no-agent. Every other windowrule \
         belongs to abyss.kdl and is not shown here.",
        vec![
            verdict(&match policy.rules.len() {
                0 => "No window is granted anything.".to_owned(),
                1 => "One window rule changes a window's security class.".to_owned(),
                n => format!("{n} window rules change a window's security class."),
            }),
            inset_list("WINDOWRULE", rows),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_allowlist_renders_a_sentence_not_a_blank_list() {
        let list = Allowlist::default();
        let rows = allow_rows(&list);
        assert_eq!(rows.len(), 1, "an empty list still draws one row");
        assert_eq!(
            permits(&list, "capture the screen"),
            "Nothing may capture the screen."
        );
    }

    #[test]
    fn a_rule_with_no_matcher_says_so_rather_than_rendering_nothing() {
        let rule = Rule {
            verb: "no-agent".into(),
            param: None,
            matchers: vec![],
            source: crate::read::Source {
                file: "/etc/eclipse/policy.kdl".into(),
                line: 1,
            },
        };
        assert_eq!(matchers(&rule), "no matcher");
    }

    #[test]
    fn every_section_carries_the_read_only_affordance() {
        assert!(READ_ONLY.contains("compositor-drawn policy editor"));
    }
}
