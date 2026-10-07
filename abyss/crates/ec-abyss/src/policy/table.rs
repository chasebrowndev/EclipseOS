// SPDX-License-Identifier: AGPL-3.0-only
//! The live enforcement table (COMP-11 §2). TCB.
//!
//! `policyd` pushes a signed table over the link; [`receive`] believes it
//! only when the signature verifies against the key pinned this session and
//! its version moves forward. Then it is swapped in whole: abyss is
//! single-threaded, so no request is ever evaluated against half of two
//! tables. A rejected table leaves the previous one live and raises a
//! trusted-UI notice, never a client-drawn one.
//!
//! With no table at all, agents cannot connect (COMP-01 §6 degraded mode):
//! `create_agent` answers `POLICY_UNAVAILABLE`, exactly as with no key.

use ec_policy_eval::check::Table;
use ec_policy_eval::table::{self as wire, TableError};

use crate::state::AbyssState;

/// Why a pushed table was not taken. Each keeps the previous one live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// Nothing pinned yet: no key to verify against.
    NoKey,
    Signature,
    /// The version does not move forward (rollback or replay).
    Rollback {
        live: u64,
        offered: u64,
    },
    /// Signed, but not a table this build can read whole.
    Malformed(TableError),
}

/// Verify, check monotonicity, decode. Pure, so every rule is a test.
pub fn accept(
    key: Option<&ec_policy_eval::VerifyingKey>,
    live: Option<u64>,
    bytes: &[u8],
    sig: &[u8; 64],
) -> Result<Table, Rejected> {
    let key = key.ok_or(Rejected::NoKey)?;
    wire::verify(key, bytes, sig).map_err(|_| Rejected::Signature)?;
    let table = wire::decode(bytes).map_err(Rejected::Malformed)?;
    if let Some(live) = live {
        if table.version <= live {
            return Err(Rejected::Rollback {
                live,
                offered: table.version,
            });
        }
    }
    Ok(table)
}

/// A table message from `policyd`.
pub fn receive(state: &mut AbyssState, bytes: &[u8], sig: &[u8; 64]) {
    let live = state.policy_table.as_ref().map(|t| t.version);
    match accept(state.policy_key.as_ref(), live, bytes, sig) {
        Ok(table) => {
            let version = table.version;
            tracing::info!(
                version,
                deny = table.rules.deny.len(),
                prompt = table.rules.prompt.len(),
                defer = table.rules.defer.len(),
                allow = table.rules.allow.len(),
                "enforcement table live"
            );
            state.policy_table = Some(table);
            crate::audit::agent(state, crate::audit::policy(version, wire::hash(bytes)));
        }
        Err(why) => {
            tracing::error!(?why, "enforcement table rejected; the previous one stays live");
            crate::trusted_ui::notice::table_rejected(state);
        }
    }
}

/// Whether agents may be admitted: a key and a table (COMP-01 §6).
pub fn live(state: &AbyssState) -> bool {
    state.policy_key.is_some() && state.policy_table.is_some()
}

/// Tests only: a live table that defers to the grant (`default-allow`).
#[cfg(test)]
pub(crate) fn install_for_test(state: &mut AbyssState) {
    use ec_policy_eval::check::{CompiledRule, Phases, Pred};
    let mut rules = Phases::default();
    rules.allow.push(CompiledRule {
        id: "default-allow".into(),
        preds: vec![Pred::Capability(vec![ec_policy_eval::scope::Glob::new("*")])],
        unless: Vec::new(),
    });
    state.policy_table = Some(Table {
        version: 1,
        rules,
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_policy_eval::check::{CompiledRule, Phases, Pred};
    use ec_policy_eval::scope::Glob;
    use ed25519_dalek::SigningKey;

    fn table(version: u64) -> Table {
        let mut rules = Phases::default();
        rules.allow.push(CompiledRule {
            id: "default-allow".into(),
            preds: vec![Pred::Capability(vec![Glob::new("*")])],
            unless: Vec::new(),
        });
        Table {
            version,
            rules,
            ..Default::default()
        }
    }

    #[test]
    fn only_a_signed_forward_table_is_taken() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let pk = key.verifying_key();
        let b = wire::encode(&table(10));
        let sig = wire::sign(&key, &b);
        assert_eq!(accept(Some(&pk), None, &b, &sig).map(|t| t.version), Ok(10));
        assert_eq!(accept(None, None, &b, &sig).err(), Some(Rejected::NoKey));
        let other = SigningKey::from_bytes(&[6; 32]);
        assert_eq!(
            accept(Some(&pk), None, &b, &wire::sign(&other, &b)).err(),
            Some(Rejected::Signature)
        );
        assert_eq!(
            accept(Some(&pk), Some(10), &b, &sig).err(),
            Some(Rejected::Rollback {
                live: 10,
                offered: 10
            })
        );
        assert_eq!(
            accept(Some(&pk), Some(11), &b, &sig).err(),
            Some(Rejected::Rollback {
                live: 11,
                offered: 10
            })
        );
        assert!(accept(Some(&pk), Some(9), &b, &sig).is_ok());
        // Signed garbage is signed, and still refused.
        let junk = vec![0xa0];
        assert!(matches!(
            accept(Some(&pk), None, &junk, &wire::sign(&key, &junk)),
            Err(Rejected::Malformed(_))
        ));
    }
}
