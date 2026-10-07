// SPDX-License-Identifier: AGPL-3.0-only
//! `brokerd`: the secrets broker (S-08).
//!
//! A separate TCB daemon (S-08 §2): it holds the plaintext of every secret
//! the owner has entered and releases one only at a boundary the agent cannot
//! observe. Three consumers, three injection modes (S-08 §3):
//!
//! * the per-agent egress proxy asks for `proxy_header` substitution;
//! * the compositor asks for `field_fill` after it has validated the target;
//! * the sandbox launcher asks for `materialize`, which needs the separate
//!   `secret.expose` capability and breaks the core property on purpose.
//!
//! The crate is a library plus one binary. Everything that decides is in the
//! library and takes its clock and its peer identity as arguments, so the
//! secrets suite (S-08 §8, `tests/secrets_suite.rs`) drives the same code the
//! daemon runs.
//!
//! Invariants this crate holds, in the order the spec states them:
//!
//! * No value is read from the store before every precondition has passed
//!   (S-08 §8, COMP-08 §4.3). The check order is in [`broker`].
//! * A value, or a hash of one, never reaches an audit record (S-08 §7).
//!   [`audit::SecretRecord`] has no field that could hold one.
//! * Plaintext lives in [`hygiene::LockedBuf`]: `mlock`ed, excluded from core
//!   dumps, zeroed on drop (S-08 §2).
//! * Fail closed: a locked broker, an unknown name, a refused audit write and
//!   an unauthorised peer are all a denial.

#![deny(unsafe_code)]

pub mod aead;
pub mod audit;
pub mod bind;
pub mod broker;
pub mod gate;
pub mod hygiene;
pub mod record;
pub mod sealer;
pub mod store;
pub mod totp;
pub mod wire;
