// SPDX-License-Identifier: AGPL-3.0-only
//! The Add-ons pane (ADR 0066): which optional packages are installed, the
//! hooks they switch on, and what they ask for.
//!
//! Read-only by design. An add-on is installed by its package and nothing on
//! the socket turns one on, so this pane has no control that could. A
//! manifest's `capture "request"` is only shown here; the grant is a line in
//! `policy.kdl`, owned by the policy editor.

use iced::widget::{column, container, row, text, Column, Space};
use iced::{Alignment, Element, Length, Theme};

use ec_ipc::Addons;
use ec_ui::theme;
use ec_ui::tokens::{font, size, space};
use ec_ui::widget::{hairline, inset, micro_label, panel, status_cell, status_grid, value as mono};

use crate::app::{App, Message};

/// The hook named by the taskbar add-on (hyperion): the widget collection,
/// the premade catalog and command approval.
pub const TASKBAR_WIDGETS: &str = "taskbar-widgets";

/// Where a hook lives, per ADR 0066's table. Hooks are a host change with
/// their own review, so this list moves only when the ADR's table does; a
/// manifest naming anything else still shows, as a hook nothing here knows.
const HOOKS: [(&str, Host); 5] = [
    ("annotations", Host::Abyss),
    ("region-select", Host::Abyss),
    (TASKBAR_WIDGETS, Host::Abyss),
    ("agents", Host::Abyss),
    ("activity-lens", Host::Fog),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Host {
    Abyss,
    Fog,
    Unknown,
}

impl Host {
    fn as_str(self) -> &'static str {
        match self {
            Host::Abyss => "abyss",
            Host::Fog => "fog",
            Host::Unknown => "no host",
        }
    }
}

/// One hook as the grid shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hook {
    name: String,
    host: Host,
    on: bool,
    /// Names of the installed add-ons that name it.
    by: Vec<String>,
}

/// Every known hook, then any a manifest names that is not known, each with
/// whether it is on. abyss reports its own hooks (`hooks_on`); Fog's are on by
/// the same rule — some installed manifest names it — which abyss does not
/// report, so it is read off the manifests.
fn hooks(a: &Addons) -> Vec<Hook> {
    let by = |hook: &str| -> Vec<String> {
        a.addons
            .iter()
            .filter(|x| x.hooks.iter().any(|h| h == hook))
            .map(|x| x.name.clone())
            .collect()
    };
    let mut out: Vec<Hook> = HOOKS
        .iter()
        .map(|(name, host)| {
            let by = by(name);
            let on = match host {
                Host::Abyss => a.is_on(name),
                _ => !by.is_empty(),
            };
            Hook {
                name: (*name).to_owned(),
                host: *host,
                on,
                by,
            }
        })
        .collect();
    for x in &a.addons {
        for h in &x.hooks {
            if !out.iter().any(|k| k.name == *h) {
                out.push(Hook {
                    name: h.clone(),
                    host: Host::Unknown,
                    on: false,
                    by: by(h),
                });
            }
        }
    }
    out
}

/// Whether the taskbar add-on's hook is on. `None` (no answer from the
/// compositor) reads as on: the pane stays whole, and a refused write still
/// lands in the banner.
pub fn taskbar_widgets_on(app: &App) -> bool {
    widgets_on(app.addons.as_ref())
}

fn widgets_on(addons: Option<&Addons>) -> bool {
    addons.is_none_or(|a| a.is_on(TASKBAR_WIDGETS))
}

/// The header's two-line status chip.
pub fn status(app: &App) -> (String, String) {
    let Some(a) = &app.addons else {
        return ("not read".into(), "compositor unreachable".into());
    };
    let hs = hooks(a);
    let on = hs.iter().filter(|h| h.on).count();
    (
        match a.addons.len() {
            0 => "none installed".to_owned(),
            1 => "1 add-on installed".to_owned(),
            n => format!("{n} add-ons installed"),
        },
        format!("{on} of {} hooks on", hs.len()),
    )
}

fn caption<'a>(t: String) -> Element<'a, Message, Theme> {
    text(t)
        .font(font::DATA)
        .size(size::MICRO)
        .style(theme::text_tertiary)
        .into()
}

// Accent ledger: the one live yellow is the state of every hook that is on —
// what an installed add-on has switched on right now. Nothing else here is
// yellow; the list under the grid only reports.

/// Every block after the header: the hook grid, then the installed list.
pub fn blocks(app: &App) -> Vec<Element<'_, Message, Theme>> {
    let empty = Addons::default();
    let a = app.addons.as_ref().unwrap_or(&empty);
    vec![hero(app, a), installed(app, a)]
}

/// The hero: a status grid, one cell per hook.
fn hero<'a>(app: &App, a: &Addons) -> Element<'a, Message, Theme> {
    let hs = hooks(a);
    let cells = hs
        .iter()
        .map(|h| {
            let state = match (h.host, h.on) {
                (Host::Unknown, _) => "Not a hook",
                (_, true) => "On",
                (_, false) => "Off",
            };
            let who = if h.by.is_empty() {
                "no add-on names it".to_owned()
            } else {
                format!("by {}", h.by.join(", "))
            };
            status_cell(&h.name, state, &format!("{} \u{b7} {who}", h.host.as_str()), h.on)
        })
        .collect();
    let reading = if app.addons.is_none() {
        "compositor unreachable".to_owned()
    } else {
        "off unless an installed add-on names it".to_owned()
    };
    panel(
        app.glass_radius,
        column![
            row![
                micro_label("hooks"),
                Space::new().width(Length::Fill),
                caption(reading)
            ]
            .align_y(Alignment::Center),
            status_grid(cells, 2),
        ]
        .spacing(space::ROW_Y),
    )
    .into()
}

fn padded<'a>(e: impl Into<Element<'a, Message, Theme>>) -> Element<'a, Message, Theme> {
    container(e)
        .padding([space::ROW_Y, space::CARD])
        .width(Length::Fill)
        .into()
}

/// The installed add-ons: one row each, the hooks it names at the right, and
/// its capture request under them.
fn installed<'a>(app: &App, a: &Addons) -> Element<'a, Message, Theme> {
    let mut col = Column::new().push(padded(micro_label("installed")));
    if a.addons.is_empty() {
        let (head, body) = if app.addons.is_none() {
            (
                "Add-ons not read.",
                "The compositor did not answer; the list is empty until it does.",
            )
        } else {
            (
                "No add-ons installed.",
                "An add-on is a package. It ships a manifest in /usr/share/eclipse/addons/, and \
                 the hooks it names stay off until it is installed.",
            )
        };
        col = col.push(hairline()).push(padded(
            column![
                text(head)
                    .font(font::UI)
                    .size(size::BODY)
                    .style(theme::text_primary),
                text(body)
                    .font(font::UI)
                    .size(size::BODY_SMALL)
                    .style(theme::text_tertiary),
            ]
            .spacing(space::LINE_GAP),
        ));
    }
    for x in &a.addons {
        let mut right = Column::new()
            .push(mono(&x.hooks.join(" \u{b7} ")))
            .spacing(space::LINE_GAP)
            .align_x(Alignment::End);
        if x.capture_requested {
            right = right.push(
                text("requests screen capture (granted in policy.kdl)")
                    .font(font::UI)
                    .size(size::BODY_SMALL)
                    .style(theme::text_secondary),
            );
        }
        col = col.push(hairline()).push(padded(
            row![
                column![
                    text(x.name.clone())
                        .font(font::UI)
                        .size(size::BODY)
                        .style(theme::text_primary),
                    caption(x.id.clone()),
                ]
                .spacing(space::LINE_GAP),
                Space::new().width(Length::Fill),
                right,
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        ));
    }
    inset(col).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_ipc::Addon;

    fn eyes() -> Addon {
        Addon {
            id: "oracle-eyes".into(),
            name: "Oracle Eyes".into(),
            hooks: vec!["annotations".into(), "region-select".into()],
            capture_requested: true,
        }
    }

    #[test]
    fn abyss_hooks_are_on_only_when_abyss_says_so() {
        let a = Addons {
            addons: vec![eyes()],
            hooks_on: vec!["annotations".into()],
        };
        let hs = hooks(&a);
        let on = |n: &str| hs.iter().find(|h| h.name == n).map(|h| h.on);
        assert_eq!(on("annotations"), Some(true));
        assert_eq!(
            on("region-select"),
            Some(false),
            "named, but abyss did not turn it on"
        );
        assert_eq!(on(TASKBAR_WIDGETS), Some(false));
    }

    #[test]
    fn a_fog_hook_is_on_when_a_manifest_names_it() {
        let a = Addons {
            addons: vec![Addon {
                id: "fog-activity".into(),
                name: "Fog activity".into(),
                hooks: vec!["activity-lens".into(), "made-up".into()],
                capture_requested: false,
            }],
            hooks_on: vec![],
        };
        let hs = hooks(&a);
        assert!(hs.iter().any(|h| h.name == "activity-lens" && h.on));
        let odd = hs.iter().find(|h| h.name == "made-up").expect("shown");
        assert_eq!((odd.host, odd.on), (Host::Unknown, false));
    }

    #[test]
    fn the_agents_hook_is_an_abyss_hook() {
        let a = Addons {
            addons: vec![Addon {
                id: "agents".into(),
                name: "Agents".into(),
                hooks: vec!["agents".into()],
                capture_requested: false,
            }],
            hooks_on: vec!["agents".into()],
        };
        let hs = hooks(&a);
        let h = hs.iter().find(|h| h.name == "agents").expect("shown");
        assert_eq!((h.host, h.on), (Host::Abyss, true));
        assert_eq!(hs.iter().filter(|h| h.name == "agents").count(), 1);
    }

    #[test]
    fn no_answer_leaves_the_taskbar_pane_whole() {
        assert!(widgets_on(None));
        assert!(!widgets_on(Some(&Addons::default())));
        let on = Addons {
            addons: vec![],
            hooks_on: vec![TASKBAR_WIDGETS.into()],
        };
        assert!(widgets_on(Some(&on)));
    }
}
