// SPDX-License-Identifier: AGPL-3.0-only
//! `Config::apply_windowrule`.

use super::*;

impl Config {
    /// `windowrule "float" { app-id "pavucontrol|org.gnome.Calculator" }`
    /// (COMP-05 §4). An action or matcher this build cannot honour drops the
    /// whole rule with a warning, so a rule never applies in part.
    pub(crate) fn apply_windowrule(&mut self, node: &KdlNode) {
        let Some(action) = arg(node).and_then(KdlValue::as_string) else {
            self.reject(node, "windowrule needs an action argument");
            return;
        };
        let mut words = action.split_whitespace();
        let verb = words.next().unwrap_or_default();
        let param = words.next();
        // Ownership is per-action here, not per-node: `float` is cosmetic,
        // `sensitivity` and `no-agent` are the security surface. Refused in
        // both directions, same as a whole node.
        if let Some(owner) = schema::rule_owner(verb) {
            if !self.owned_here(node, &format!("windowrule {verb:?}"), owner) {
                return;
            }
        }
        let action = match (verb, param) {
            ("float", None) => RuleAction::Float,
            ("tile", None) => RuleAction::Tile,
            ("fullscreen", None) => RuleAction::Fullscreen,
            ("no-agent", None) => RuleAction::NoAgent,
            ("no-focus-steal", None) => RuleAction::NoFocusSteal,
            ("idle-inhibit", None) => RuleAction::IdleInhibit,
            ("size", Some(p)) => match parse_pair(p, 'x') {
                Some((w, h)) if w > 0 && h > 0 => RuleAction::Size(w, h),
                _ => {
                    self.reject(
                        node,
                        format!("windowrule size must be WxH in pixels (action={})", action),
                    );
                    return;
                }
            },
            ("position", Some(p)) => match parse_pair(p, ',') {
                Some((x, y)) => RuleAction::Position(x, y),
                None => {
                    self.reject(
                        node,
                        format!("windowrule position must be X,Y in pixels (action={})", action),
                    );
                    return;
                }
            },
            ("output", Some(p)) => RuleAction::Output(p.to_owned()),
            ("app-trust", Some("standard")) => RuleAction::Trust(AppTrust::Standard),
            ("app-trust", Some("trusted")) => RuleAction::Trust(AppTrust::Trusted),
            ("seat-compat", Some("lock")) => RuleAction::Seat(SeatCompat::Lock),
            ("seat-compat", Some("multi")) => RuleAction::Seat(SeatCompat::Multi),
            ("irreversible-capable", Some("true")) => RuleAction::IrreversibleCapable(true),
            ("irreversible-capable", Some("false")) => RuleAction::IrreversibleCapable(false),
            ("workspace", Some(p)) => match p.parse::<i32>() {
                Ok(n) if (1..=10).contains(&n) => RuleAction::Workspace(n),
                _ => {
                    self.reject(
                        node,
                        format!("windowrule workspace must be 1..=10 (action={})", action),
                    );
                    return;
                }
            },
            ("opacity", Some(p)) => match p.parse::<f32>() {
                Ok(v) if (0.0..=1.0).contains(&v) => RuleAction::Opacity(v),
                _ => {
                    self.reject(
                        node,
                        format!("windowrule opacity must be 0.0..=1.0 (action={})", action),
                    );
                    return;
                }
            },
            ("blur", Some(p)) => match BlurRule::parse(p) {
                Some(v) => RuleAction::Blur(v),
                None => {
                    self.reject(
                        node,
                        format!(
                            "windowrule blur must be true, false, off, blur, frost or glass (action={})",
                            action
                        ),
                    );
                    return;
                }
            },
            ("sensitivity", Some(p @ ("secret" | "private"))) => RuleAction::Sensitivity(p.to_owned()),
            ("sensitivity", Some(p)) => {
                // App-declared sensitivity may only raise a class, never lower.
                self.reject(
                    node,
                    format!("windowrule sensitivity may only raise (class={})", p),
                );
                return;
            }
            _ => {
                self.reject(
                    node,
                    format!("unimplemented windowrule action (action={})", action),
                );
                return;
            }
        };

        let mut matchers = Matchers::default();
        let Some(children) = node.children() else {
            self.reject(
                node,
                format!(
                    "windowrule with no matchers would hit every window (action={:?})",
                    action
                ),
            );
            return;
        };
        for n in children.nodes() {
            let name = n.name().value();
            if schema::find(schema::RULE_MATCHERS, name).is_none() {
                match schema::REFUSED_MATCHERS.iter().find(|(m, _)| *m == name) {
                    Some((_, why)) => self.reject(
                        n,
                        format!("refused windowrule matcher (matcher={}): {}", name, why),
                    ),
                    None => self.reject(n, format!("unimplemented windowrule matcher (matcher={})", name)),
                }
                return;
            }
            match name {
                "app-id" | "title" => {
                    let Some(pat) = arg(n).and_then(KdlValue::as_string).and_then(Pattern::parse) else {
                        self.reject(
                            n,
                            format!("matcher pattern is not a valid regex (matcher={})", name),
                        );
                        return;
                    };
                    if name == "app-id" {
                        matchers.app_id = Some(pat);
                    } else {
                        matchers.title = Some(pat);
                    }
                }
                "pid" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if v > 0 => matchers.pid = Some(v as i32),
                    _ => {
                        self.reject(n, "windowrule pid must be a positive integer");
                        return;
                    }
                },
                "xwayland" => matchers.xwayland = Some(arg(n).and_then(KdlValue::as_bool).unwrap_or(true)),
                "output" => match arg(n).and_then(KdlValue::as_string) {
                    Some(v) => matchers.output = Some(v.to_owned()),
                    None => {
                        self.reject(n, "windowrule output needs a name pattern");
                        return;
                    }
                },
                "cgroup" => match arg(n).and_then(KdlValue::as_string).and_then(Pattern::parse) {
                    Some(p) => matchers.cgroup = Some(p),
                    None => {
                        self.reject(n, "windowrule cgroup matcher is not a valid regex");
                        return;
                    }
                },
                "workspace" => match arg(n).and_then(KdlValue::as_integer) {
                    Some(v) if (1..=10).contains(&v) => matchers.workspace = Some(v as i32),
                    _ => {
                        self.reject(n, "windowrule workspace matcher must be 1..=10");
                        return;
                    }
                },
                other => {
                    self.reject(n, format!("unimplemented windowrule matcher (matcher={})", other));
                    return;
                }
            }
        }
        if matchers.is_empty() {
            self.reject(
                node,
                format!(
                    "windowrule with no matchers would hit every window (action={:?})",
                    action
                ),
            );
            return;
        }
        self.window_rules.push(WindowRule { action, matchers });
    }
}
