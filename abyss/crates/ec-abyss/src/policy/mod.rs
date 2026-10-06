// SPDX-License-Identifier: AGPL-3.0-only
//! The compositor's half of agent policy (COMP-11, S-01 §6). TCB.
//!
//! abyss never mints authority; it only verifies what `policyd` signed. This
//! module holds what one agent object may do — its verified grants and the
//! visibility compiled from them — and [`scene`] is the one place the scene
//! is filtered through that visibility.
//!
//! The protocol layer (`protocols/agent/`) owns the Wayland objects and calls
//! in here; it never reads a grant itself.

pub mod batch;
pub mod enforce;
pub mod lifecycle;
pub mod link;
pub mod scene;
pub mod table;

use ec_policy_eval::scope::SCENE_READ;
use ec_policy_eval::{Grant, SceneView, VerifyError, VerifyingKey};

/// Why a grant was not admitted to an agent object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmitError {
    /// No `policyd` key is pinned yet: the agents hook is on but `policyd`
    /// has not connected (COMP-01 §6, `POLICY_UNAVAILABLE`).
    PolicyUnavailable,
    /// The COSE_Sign1 failed [`Grant::verify`].
    Invalid(VerifyError),
    /// The principal is not an `agent:` principal, or differs from the one
    /// this agent object already speaks for (COMP-08 §1).
    WrongPrincipal,
    /// `policyd` has revoked this grant or its principal.
    Revoked,
}

/// One agent object's authority: the principal it speaks for and every live
/// grant it holds. Grants are additive and never merged (S-01 §4).
#[derive(Debug)]
pub struct Agent {
    principal: String,
    grants: Vec<Grant>,
    view: SceneView,
    read: SceneView,
    /// One per [`SEAT_CAPS`] entry, same order.
    acts: Vec<SceneView>,
}

/// What one request is filtered through, compiled from the live grants.
#[derive(Debug, Clone, Copy)]
pub struct Views<'a> {
    /// `scene.list`: what exists.
    pub list: &'a SceneView,
    /// `scene.read`: which of those may be read in detail. Only consulted
    /// through [`scene::readable`].
    pub read: &'a SceneView,
    /// Some live grant names `scene.read` at all, so a window [`read`]
    /// misses is `out_of_scope` rather than `no_capability` (S-01 §3).
    ///
    /// [`read`]: Views::read
    pub read_held: bool,
}

/// The acting capabilities a seat request can need (COMP-08 §4, S-01 §2).
/// Each gets its own visibility, compiled when the grants change, so the
/// request path looks one up and allocates nothing.
pub const SEAT_CAPS: [&str; 6] = [
    "seat.focus",
    "seat.key",
    "seat.text",
    "seat.pointer",
    "seat.touch",
    "click",
];

/// The principal prefix every agent grant carries (S-01 §4).
const AGENT_PREFIX: &str = "agent:";

impl Agent {
    /// `create_agent`: verifies the first grant and binds the object to its
    /// principal. `key` is the pinned `policyd` key, `None` until `policyd`
    /// has connected.
    pub fn admit(
        cose: &[u8],
        key: Option<&VerifyingKey>,
        now_ms: u64,
        revoked: impl Fn(&Grant) -> bool,
    ) -> Result<Agent, AdmitError> {
        let grant = verify(cose, key, now_ms, &revoked)?;
        let mut agent = Agent {
            principal: grant.principal.clone(),
            grants: vec![grant],
            view: SceneView::default(),
            read: SceneView::default(),
            acts: Vec::new(),
        };
        agent.recompile();
        Ok(agent)
    }

    /// `add_grant`: a further grant for the same principal.
    pub fn add_grant(
        &mut self,
        cose: &[u8],
        key: Option<&VerifyingKey>,
        now_ms: u64,
        revoked: impl Fn(&Grant) -> bool,
    ) -> Result<(), AdmitError> {
        let grant = verify(cose, key, now_ms, &revoked)?;
        if grant.principal != self.principal {
            return Err(AdmitError::WrongPrincipal);
        }
        self.grants.push(grant);
        self.recompile();
        Ok(())
    }

    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// The task this agent's grants were issued under. Every grant names
    /// one, and a principal has one live task at a time (A-04 §4), so the
    /// first grant's is the agent's.
    pub fn task_id(&self) -> Option<ec_policy_eval::Ulid> {
        self.grants.first().map(|g| g.task_id)
    }

    /// The views to filter this request through, after dropping every
    /// grant that has expired since the last request (S-01 §4: expiry is
    /// checked at request time, no grace). `None` when no grant is left: the
    /// agent then sees nothing and can do nothing.
    pub fn view_at(&mut self, now_ms: u64) -> Option<Views<'_>> {
        let before = self.grants.len();
        self.grants.retain(|g| g.is_valid_at(now_ms).is_ok());
        if self.grants.len() != before {
            self.recompile();
        }
        let read_held = self
            .grants
            .iter()
            .any(|g| g.capabilities.iter().any(|c| c.name == SCENE_READ));
        (!self.grants.is_empty()).then_some(Views {
            list: &self.view,
            read: &self.read,
            read_held,
        })
    }

    /// A revocation push from `policyd` (S-01 §6): drops every grant it
    /// names. Returns whether anything was dropped, so the caller can abort
    /// in-flight batches with `revoked`.
    pub fn revoke(&mut self, revoked: impl Fn(&Grant) -> bool) -> bool {
        let before = self.grants.len();
        self.grants.retain(|g| !revoked(g));
        let dropped = self.grants.len() != before;
        if dropped {
            self.recompile();
        }
        dropped
    }

    /// The visibility acting capability `cap` reaches, after dropping
    /// expired grants. `None` when no live grant names `cap` at all: the
    /// request is `no_capability`, not merely out of scope.
    pub fn act_view(&mut self, now_ms: u64, cap: &str) -> Option<&SceneView> {
        let before = self.grants.len();
        self.grants.retain(|g| g.is_valid_at(now_ms).is_ok());
        if self.grants.len() != before {
            self.recompile();
        }
        let i = SEAT_CAPS.iter().position(|c| *c == cap)?;
        let held = self
            .grants
            .iter()
            .any(|g| g.capabilities.iter().any(|c| c.name == cap));
        held.then(|| &self.acts[i])
    }

    /// The grant facts an `unless` clause may test (S-02 §3), written into
    /// `out` with no allocation. Facts past `out.len()` are dropped, which
    /// can only stop an `unless` from relaxing a rule: fail closed.
    pub fn grant_facts<'a>(&'a self, out: &mut [ec_policy_eval::check::GrantFact<'a>]) -> usize {
        let mut n = 0;
        for g in &self.grants {
            for c in &g.capabilities {
                if n == out.len() {
                    return n;
                }
                out[n] = ec_policy_eval::check::GrantFact {
                    capability: &c.name,
                    unattended: g.unattended,
                };
                n += 1;
            }
        }
        n
    }

    /// One line per live grant, for the emergency panel: its capabilities
    /// and their scopes as written (S-01 §4).
    pub fn describe_grants(&self) -> Vec<String> {
        self.grants
            .iter()
            .map(|g| {
                g.capabilities
                    .iter()
                    .map(|c| match c.scopes.as_slice() {
                        [] => c.name.clone(),
                        s => format!("{} {}", c.name, s.join(" ")),
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .collect()
    }

    fn recompile(&mut self) {
        self.view = SceneView::compile(&self.grants);
        self.read = SceneView::compile_for(SCENE_READ, &self.grants);
        self.acts = SEAT_CAPS
            .iter()
            .map(|c| SceneView::compile_for(c, &self.grants))
            .collect();
    }
}

fn verify(
    cose: &[u8],
    key: Option<&VerifyingKey>,
    now_ms: u64,
    revoked: &impl Fn(&Grant) -> bool,
) -> Result<Grant, AdmitError> {
    let key = key.ok_or(AdmitError::PolicyUnavailable)?;
    let grant = Grant::verify(cose, key, now_ms).map_err(AdmitError::Invalid)?;
    let name = grant.principal.strip_prefix(AGENT_PREFIX).unwrap_or("");
    if name.is_empty() {
        return Err(AdmitError::WrongPrincipal);
    }
    if revoked(&grant) {
        return Err(AdmitError::Revoked);
    }
    Ok(grant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_policy_eval::grant::{cose_sign1, protected_header, sig_structure};
    use ec_policy_eval::task::Ulid;
    use ec_policy_eval::{Capability, Constraints};
    use ed25519_dalek::{Signer, SigningKey};

    fn sk() -> SigningKey {
        SigningKey::from_bytes(&[3u8; 32])
    }

    fn grant(principal: &str, id: u8, expires_ms: u64) -> Grant {
        Grant {
            id: Ulid([id; 16]),
            principal: principal.into(),
            issued_ms: 0,
            expires_ms,
            issuer: "policyd".into(),
            task_id: Ulid([9; 16]),
            capabilities: vec![Capability {
                name: "scene.list".into(),
                scopes: vec![format!("handle:{id}")],
                quota: None,
            }],
            constraints: Constraints::default(),
            unattended: false,
        }
    }

    fn sign(g: &Grant, k: &SigningKey) -> Vec<u8> {
        let protected = protected_header(&k.verifying_key());
        let payload = g.encode();
        let sig = k.sign(&sig_structure(&protected, &payload));
        cose_sign1(&protected, &payload, &sig.to_bytes())
    }

    fn never(_: &Grant) -> bool {
        false
    }

    #[test]
    fn no_pinned_key_is_policy_unavailable() {
        let cose = sign(&grant("agent:a", 1, 100), &sk());
        assert_eq!(
            Agent::admit(&cose, None, 0, never).unwrap_err(),
            AdmitError::PolicyUnavailable
        );
    }

    #[test]
    fn a_grant_signed_by_another_key_is_refused() {
        let cose = sign(&grant("agent:a", 1, 100), &SigningKey::from_bytes(&[4u8; 32]));
        let key = sk().verifying_key();
        assert!(matches!(
            Agent::admit(&cose, Some(&key), 0, never),
            Err(AdmitError::Invalid(VerifyError::UnknownKey))
        ));
    }

    #[test]
    fn principal_must_be_an_agent_and_must_not_change() {
        let key = sk().verifying_key();
        for bad in ["human", "agent:", "policyd"] {
            let cose = sign(&grant(bad, 1, 100), &sk());
            assert_eq!(
                Agent::admit(&cose, Some(&key), 0, never).unwrap_err(),
                AdmitError::WrongPrincipal,
                "{bad}"
            );
        }
        let mut a = Agent::admit(&sign(&grant("agent:a", 1, 100), &sk()), Some(&key), 0, never).unwrap();
        let other = sign(&grant("agent:b", 2, 100), &sk());
        assert_eq!(
            a.add_grant(&other, Some(&key), 0, never).unwrap_err(),
            AdmitError::WrongPrincipal
        );
    }

    #[test]
    fn expiry_is_checked_per_request_and_shrinks_the_view() {
        let key = sk().verifying_key();
        let mut a = Agent::admit(&sign(&grant("agent:a", 1, 100), &sk()), Some(&key), 0, never).unwrap();
        a.add_grant(&sign(&grant("agent:a", 2, 200), &sk()), Some(&key), 0, never)
            .unwrap();
        assert!(a.view_at(99).is_some());
        assert_eq!(a.grants.len(), 2);
        assert!(a.view_at(100).is_some());
        assert_eq!(a.grants.len(), 1);
        assert!(a.view_at(200).is_none());
    }

    #[test]
    fn revocation_drops_the_named_grant_and_refuses_readmission() {
        let key = sk().verifying_key();
        let g1 = grant("agent:a", 1, 100);
        let mut a = Agent::admit(&sign(&g1, &sk()), Some(&key), 0, never).unwrap();
        let is_g1 = |g: &Grant| g.id == Ulid([1; 16]);
        assert!(a.revoke(is_g1));
        assert!(!a.revoke(is_g1));
        assert!(a.view_at(0).is_none());
        assert_eq!(
            a.add_grant(&sign(&g1, &sk()), Some(&key), 0, is_g1).unwrap_err(),
            AdmitError::Revoked
        );
    }
}
