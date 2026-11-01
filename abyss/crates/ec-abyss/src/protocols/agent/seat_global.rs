// SPDX-License-Identifier: AGPL-3.0-only
//! Who may see an agent's `wl_seat` (KNOWNBUGS SEAT-01). Not TCB: the
//! decision is `policy::scene::visible`, asked once per toplevel and seat
//! capability; this module only turns its answers into globals.
//!
//! # The rule
//!
//! A client on the human display sees and can bind `agent-<id>` only while
//! it owns a toplevel that one of the agent's seat capabilities could act on
//! (`policy::SEAT_CAPS`, each through its own scope view). That is exactly
//! the set of clients the seat can deliver to: `policy::enforce` resolves
//! every acting request's target through the same view and the same filter
//! (COMP-04 §7, "scoped to what the agent may see"; COMP-08 §10 step 4). A
//! client outside that set learns neither the seat's name nor anything it
//! sends (F-07: invisible is nonexistent).
//!
//! # Why globals are made by hand
//!
//! smithay 0.7's `new_wl_seat` makes an unfiltered global, and
//! `GlobalDispatch::can_view` sees neither the state nor which seat a
//! `SeatGlobalData` belongs to. So the agent's seat is made on a hidden
//! display no client ever connects to (`seat::HIDDEN`), its global handler is
//! kept, and the human display only ever carries [`SeatGlobal`]s that forward
//! `bind` to it and filter `can_view` on a fixed client list.
//!
//! A registry announces a global once, when the global or the registry is
//! made. A client that comes into scope later (it maps a window, the window
//! is retitled into scope) gets a new [`SeatGlobal`] listing just it.
//!
//! # Clients connected before a global (decoys)
//!
//! wayland-backend 0.3.17's libwayland backend does not filter the
//! announcement made while a global is being created: the global is not yet
//! in its own list, so its filter answers "not mine, show it". Every client
//! already connected is told a `wl_seat` appeared, and toolkits bind every
//! seat they are told of. Refusing that bind would kill the client with a
//! protocol error, so a client that predates the global and is not admitted
//! binds a decoy instead: a second smithay seat, nameless, with no devices,
//! that nothing ever focuses or sends to. It is a real smithay seat because
//! every protocol taking a `wl_seat` (text input, input method, virtual
//! keyboard, ...) assumes one. The client learns that some seat appeared,
//! never whose or what it carries. A client that connects after the global
//! is not told of it and cannot bind it.
//!
//! # Scope changes after bind (fail closed)
//!
//! A client that leaves the set keeps its `wl_seat` object, but it is told
//! the seat has no capabilities, and the act path refuses its windows
//! anyway. Coming back into scope restores them. A client that binds while
//! out of reach (it raced the change) is stripped at bind.

use std::sync::Arc;

use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, GlobalHandler, GlobalId, Handle, ObjectData, ObjectId,
};
use smithay::reexports::wayland_server::{protocol::wl_seat::WlSeat, DisplayHandle, Resource};

use crate::policy::scene;
use crate::state::{AbyssState, ClientState};

/// smithay's `wl_seat` version.
pub(super) const VERSION: u32 = 9;

/// One filtered `wl_seat` global on the human display.
pub(super) struct SeatGlobal {
    agent: u64,
    /// smithay's own handler for the agent's seat.
    inner: Arc<dyn GlobalHandler<AbyssState>>,
    /// smithay's handler for the agent's decoy seat.
    decoy: Arc<dyn GlobalHandler<AbyssState>>,
    /// The clients this global was announced to. Fixed at creation.
    admit: Vec<ClientId>,
    /// `ClientState::seq` of the first client made after this global; every
    /// client below it was told of it on creation (see the module docs).
    born: u64,
}

impl SeatGlobal {
    pub(super) fn new(
        agent: u64,
        inner: Arc<dyn GlobalHandler<AbyssState>>,
        decoy: Arc<dyn GlobalHandler<AbyssState>>,
        admit: Vec<ClientId>,
    ) -> Self {
        Self {
            agent,
            inner,
            decoy,
            admit,
            born: crate::state::client_seq(),
        }
    }

    pub(super) fn admits(&self, client: &ClientId) -> bool {
        self.admit.contains(client)
    }

    /// Every client it admits has disconnected.
    pub(super) fn all_gone(&self, dh: &DisplayHandle) -> bool {
        let handle = dh.backend_handle();
        self.admit
            .iter()
            .all(|c| handle.get_client_data(c.clone()).is_err())
    }

    /// `client` was connected when this global was made.
    fn predates(&self, data: &Arc<dyn ClientData>) -> bool {
        data.downcast_ref::<ClientState>()
            .is_some_and(|c| c.seq < self.born)
    }
}

impl GlobalHandler<AbyssState> for SeatGlobal {
    /// wayland-backend applies this to `bind` as well as to the listing.
    fn can_view(&self, client: ClientId, data: &Arc<dyn ClientData>, _global: GlobalId) -> bool {
        self.admits(&client) || self.predates(data)
    }

    fn bind(
        self: Arc<Self>,
        handle: &Handle,
        state: &mut AbyssState,
        client: ClientId,
        global: GlobalId,
        object: ObjectId,
    ) -> Arc<dyn ObjectData<AbyssState>> {
        if !self.admits(&client) {
            return Arc::clone(&self.decoy).bind(handle, state, client, global, object);
        }
        let data = Arc::clone(&self.inner).bind(handle, state, client.clone(), global, object.clone());
        if let Ok(seat) = WlSeat::from_id(&DisplayHandle::from(handle.clone()), object) {
            super::seat::bound(state, self.agent, client, seat);
        }
        data
    }
}

/// The clients agent `aid`'s seat may reach right now, into `out`: every
/// client owning a toplevel some held seat capability could act on. Empty
/// for an agent that is gone or holds no seat capability.
pub(super) fn reachable(state: &mut AbyssState, aid: u64, out: &mut Vec<ClientId>) {
    out.clear();
    let Some(mut agent) = state.agents.take_agent(aid) else {
        return;
    };
    let now = super::now_ms();
    for cap in crate::policy::SEAT_CAPS {
        let Some(view) = agent.act_view(now, cap) else {
            continue;
        };
        for entry in state.outputs.iter() {
            for ws in &entry.workspaces {
                for w in ws.all_windows() {
                    let Some(client) = crate::shell::window_surface(&w)
                        .and_then(|s| s.client())
                        .map(|c| c.id())
                    else {
                        continue;
                    };
                    if !out.contains(&client) && scene::visible(state, view, &w) {
                        out.push(client);
                    }
                }
            }
        }
    }
    state.agents.put_agent(aid, agent);
}
