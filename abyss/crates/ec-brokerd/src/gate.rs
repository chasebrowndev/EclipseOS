// SPDX-License-Identifier: AGPL-3.0-only
//! Who may ask brokerd for what. Authorisation enforcement: owner review.
//!
//! brokerd does not decide policy (policyd does) and does not see agents.
//! Its callers are a closed set of processes, each of which is trusted to
//! have done its own half of the check, and each of which may only ask for
//! the operations its role needs. An unrecognised peer is not a [`Peer`] at
//! all: the transport drops the connection before a byte is parsed.
//!
//! The table is a `match` with no wildcard arm over [`Op`], so adding an
//! operation without deciding who may call it does not compile.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    /// abyss: trusted UI, `field_fill`, session lock/end.
    Compositor,
    /// The per-agent egress proxy (S-09): `proxy_header` substitution.
    Proxy,
    /// The sandbox launcher in `agentd`: `materialize`, TOTP issuance.
    Agentd,
    /// The owner, through `ec-secret` on a local TTY (S-08 §2).
    Owner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Status,
    BeginUnlock,
    AnswerUnlock,
    Init,
    Lock,
    Add,
    Rotate,
    Revoke,
    List,
    Substitute,
    FieldFill,
    Materialize,
    Totp,
}

pub fn allowed(peer: Peer, op: Op) -> bool {
    use Op::*;
    use Peer::*;
    match op {
        Status => true,
        // The unlock dialog is trusted UI, which only the compositor draws
        // (ADR 0009). The owner's TTY may also unlock, for a headless box.
        BeginUnlock | AnswerUnlock => matches!(peer, Compositor | Owner),
        Init => matches!(peer, Compositor | Owner),
        // Screen lock and session end come from the compositor.
        Lock => matches!(peer, Compositor | Owner),
        // Entering, rotating and revoking are human actions (S-08 §2, §6).
        Add | Rotate | Revoke => matches!(peer, Compositor | Owner),
        List => matches!(peer, Compositor | Owner),
        Substitute => matches!(peer, Proxy),
        FieldFill => matches!(peer, Compositor),
        Materialize | Totp => matches!(peer, Agentd),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nobody_but_the_right_peer_releases_a_value() {
        let all = [Peer::Compositor, Peer::Proxy, Peer::Agentd, Peer::Owner];
        for p in all {
            assert_eq!(allowed(p, Op::Substitute), p == Peer::Proxy);
            assert_eq!(allowed(p, Op::FieldFill), p == Peer::Compositor);
            assert_eq!(allowed(p, Op::Materialize), p == Peer::Agentd);
            assert_eq!(allowed(p, Op::Totp), p == Peer::Agentd);
        }
    }

    #[test]
    fn an_agent_facing_peer_cannot_administer() {
        for op in [
            Op::Add,
            Op::Rotate,
            Op::Revoke,
            Op::List,
            Op::Lock,
            Op::BeginUnlock,
            Op::AnswerUnlock,
            Op::Init,
        ] {
            assert!(!allowed(Peer::Proxy, op), "{op:?}");
            assert!(!allowed(Peer::Agentd, op), "{op:?}");
        }
    }
}
