// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_text_input_v3` (COMP-06 §1).
//!
//! Text-input focus follows keyboard focus automatically inside smithay's
//! `KeyboardTarget for WlSurface`, so there is no focus wiring here. Preedit
//! and commit strings are human input and are never logged by content.

use smithay::delegate_text_input_manager;

use crate::state::AbyssState;

delegate_text_input_manager!(AbyssState);
