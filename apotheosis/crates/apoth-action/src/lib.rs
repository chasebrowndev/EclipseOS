// SPDX-License-Identifier: Apache-2.0
//! The one Action/Event vocabulary, its registry, the match language and the template language.
//!
//! Governing spec: T-11 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! M4 scope (T-11 §1): [`Action`] with typed arguments, the generated [`REGISTRY`], argv
//! arrays with typed placeholders ([`argv`]), the textual action syntax ([`text`]), key and
//! mouse chords with bind options ([`key`]), per-value capability classes ([`class`]),
//! macros ([`macros`]) and the rule event vocabulary ([`event`]). The match language
//! (T-11 §7) and the template language (T-11 §8) are later milestones and not here.
//!
//! Pure: no I/O, no dependencies.
#![forbid(unsafe_code)]

pub mod action;
pub mod arg;
pub mod argv;
pub mod class;
pub mod enums;
pub mod error;
pub mod event;
pub mod key;
pub mod macros;
pub mod text;

pub use action::{
    Action, ActionInfo, FAMILIES, MAX_MACRO_DEPTH, ParamSpec, REGISTRY, is_user_action_id, lookup,
    search,
};
pub use arg::{ArgType, Bytes, Dur, ParamKind};
pub use argv::{Argv, ArgvElem, ExpandError, Placeholder, PlaceholderValues};
pub use class::{
    Class, ClassContext, ClassSet, Classification, ConsentError, NoContext, OverrideClass, Refusal,
    rule_consent,
};
pub use error::{ErrorKind, ParseError};
pub use event::{Cause, Event, EventKind, FieldValue};
pub use key::{Bind, BindOptions, KeyChord, ModeCond, Mods, MouseBind};
pub use macros::{MacroDef, MacroError, MacroSet, WithMacros};
pub use text::Call;
