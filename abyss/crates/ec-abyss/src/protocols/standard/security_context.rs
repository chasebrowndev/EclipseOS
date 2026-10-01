// SPDX-License-Identifier: AGPL-3.0-only
//! `wp_security_context_manager_v1` (COMP-06 §1, S-03 §5).
//!
//! A sandbox creates a security context, hands the compositor a listening socket
//! and a close fd, and every client that connects through that socket is tagged
//! with the sandbox engine, app id and instance id. That tag is the identity a
//! nested launch carries; without it a sandboxed client is indistinguishable
//! from anything else on the main socket.
//!
//! The tag is recorded on the client, never trusted as a grant on its own: it
//! names a principal, and the policy layer decides what that principal may do.
//! The socket is closed automatically when the sandbox drops its close fd.

use std::sync::Arc;

use smithay::wayland::security_context::{
    SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
};

use crate::state::{AbyssState, ClientState};

impl SecurityContextHandler for AbyssState {
    fn context_created(&mut self, source: SecurityContextListenerSource, context: SecurityContext) {
        let inserted = self
            .loop_handle
            .insert_source(source, move |client, _, state: &mut AbyssState| {
                let data = Arc::new(ClientState {
                    security_context: Some(context.clone()),
                    ..Default::default()
                });
                if let Err(e) = state.display_handle.insert_client(client, data) {
                    tracing::warn!(?e, "sandboxed client rejected");
                }
            });
        if let Err(e) = inserted {
            tracing::warn!(?e, "security context listener not installed");
        }
    }
}

smithay::delegate_security_context!(AbyssState);
