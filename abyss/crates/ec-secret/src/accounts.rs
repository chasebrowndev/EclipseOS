// SPDX-License-Identifier: AGPL-3.0-only
//! Account names and the brokerd secrets that hold them (ADR 0077).
//!
//! The same rules as `ec_inference_wire::{account_ok, secret_name}`, which
//! ec-inferenced reads the secrets back by; a test pins the two together.

use ec_brokerd::wire::MetaView;

/// A Claude Code OAuth token from `claude setup-token`.
pub const TOKEN_BASE: &str = "claude-code-token";
/// An Anthropic API key.
pub const KEY_BASE: &str = "anthropic-api-key";
/// Where both are released.
pub const BINDING: &str = "host:api.anthropic.com";

/// 1 to 32 of `[A-Za-z0-9_-]`. (The wire's empty default is `default` here.)
pub fn name_ok(a: &str) -> bool {
    !a.is_empty()
        && a.len() <= 32
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `base` for the default account, else `base.account`. `None` for a bad name.
pub fn secret_name(base: &str, account: &str) -> Option<String> {
    if !name_ok(account) {
        return None;
    }
    Some(if account == "default" {
        base.to_owned()
    } else {
        format!("{base}.{account}")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    ClaudeCode,
    ApiKey,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::ClaudeCode => "claude-code",
            Kind::ApiKey => "api-key",
        }
    }

    pub fn base(self) -> &'static str {
        match self {
            Kind::ClaudeCode => TOKEN_BASE,
            Kind::ApiKey => KEY_BASE,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub kind: Kind,
    pub rotations: u64,
}

/// The accounts among brokerd's secrets; secrets that are not one are left
/// out. The default account first, then by name; an account holding both kinds
/// appears once per kind.
pub fn from_secrets(items: &[MetaView]) -> Vec<Account> {
    let mut out = Vec::new();
    for i in items {
        for kind in [Kind::ClaudeCode, Kind::ApiKey] {
            let name = if i.name == kind.base() {
                "default"
            } else if let Some(a) = i.name.strip_prefix(kind.base()).and_then(|r| r.strip_prefix('.')) {
                // `.default` is not a name we write; leave it alone.
                if !name_ok(a) || a == "default" {
                    continue;
                }
                a
            } else {
                continue;
            };
            out.push(Account {
                name: name.to_owned(),
                kind,
                rotations: i.rotation_counter,
            });
        }
    }
    out.sort_by(|a, b| (a.name != "default", &a.name, a.kind).cmp(&(b.name != "default", &b.name, b.kind)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(name: &str, rot: u64) -> MetaView {
        MetaView {
            name: name.into(),
            id: [0; 16],
            kind: "bearer".into(),
            bound_to: vec![BINDING.into()],
            modes: vec!["proxy_header".into()],
            requires_prompt: false,
            rotation_counter: rot,
        }
    }

    #[test]
    fn names_follow_the_inferenced_rules() {
        for base in [TOKEN_BASE, KEY_BASE] {
            for a in ["default", "work", "a-b_C9", &"x".repeat(32)] {
                assert_eq!(
                    secret_name(base, a),
                    ec_inference_wire::secret_name(base, a),
                    "{base} {a}"
                );
            }
            for bad in ["", "a.b", "has space", &"x".repeat(33)] {
                assert_eq!(secret_name(base, bad), None, "{bad:?}");
            }
        }
        assert_eq!(
            ec_inference_wire::secret_name(TOKEN_BASE, "default").as_deref(),
            Some("claude-code-token")
        );
    }

    #[test]
    fn only_accounts_are_listed_and_the_default_comes_first() {
        let items = [
            meta("zeta-thing", 0),
            meta("anthropic-api-key.work", 1),
            meta("claude-code-token", 0),
            meta("claude-code-token.work", 3),
            meta("anthropic-api-key.bad name", 0),
            meta("claude-code-token.default", 0),
            meta("claude-code-tokenx", 0),
            meta("anthropic-api-key.alpha", 2),
        ];
        let got: Vec<(String, &str, u64)> = from_secrets(&items)
            .into_iter()
            .map(|a| (a.name, a.kind.as_str(), a.rotations))
            .collect();
        assert_eq!(
            got,
            [
                ("default".to_string(), "claude-code", 0),
                ("alpha".to_string(), "api-key", 2),
                ("work".to_string(), "claude-code", 3),
                ("work".to_string(), "api-key", 1),
            ]
        );
    }
}
