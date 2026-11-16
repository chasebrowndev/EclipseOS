// SPDX-License-Identifier: AGPL-3.0-only
//! The AT-SPI transport: `accesskit_unix` with its async-io executor (no tokio). Its
//! handlers run on the adapter's own thread; they only forward to the caller's channel, and
//! the main loop answers with [`Adapter::update`].
//!
//! Activation (T-20 §1): `accesskit_unix` asks for the first tree once an assistive
//! technology is present; until then nothing is built. Its signal is `IsEnabled` alone, not
//! the spec's `IsEnabled && ScreenReaderEnabled || first request` (reported gap).

use accesskit::{ActionRequest, TreeUpdate};

/// What the adapter thread asks of the main loop.
#[derive(Debug)]
pub enum Request {
    /// An AT is present: build and send a full tree.
    Activate,
    /// No AT any more: drop the tree and snapshots.
    Deactivate,
    /// An AT asked for an action on a node.
    Action(ActionRequest),
}

/// A sink for [`Request`]s (a channel sender that also wakes the main loop).
pub trait Notify: Send + Sync + 'static {
    /// Deliver one request; must not block.
    fn notify(&self, r: Request);
}

struct Forward<N: Notify>(std::sync::Arc<N>);

impl<N: Notify> accesskit::ActivationHandler for Forward<N> {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.0.notify(Request::Activate);
        None
    }
}

impl<N: Notify> accesskit::ActionHandler for Forward<N> {
    fn do_action(&mut self, request: ActionRequest) {
        self.0.notify(Request::Action(request));
    }
}

impl<N: Notify> accesskit::DeactivationHandler for Forward<N> {
    fn deactivate_accessibility(&mut self) {
        self.0.notify(Request::Deactivate);
    }
}

/// One window's adapter.
pub struct Adapter {
    inner: accesskit_unix::Adapter,
}

impl std::fmt::Debug for Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Adapter")
    }
}

impl Adapter {
    /// Register with the accessibility bus (lazily, on the adapter thread).
    pub fn new<N: Notify>(notify: N) -> Adapter {
        let n = std::sync::Arc::new(notify);
        Adapter {
            inner: accesskit_unix::Adapter::new(Forward(n.clone()), Forward(n.clone()), Forward(n)),
        }
    }

    /// Push an update (the first after [`Request::Activate`] must be full). A no-op while
    /// no AT is active.
    pub fn update(&mut self, u: TreeUpdate) {
        self.inner.update_if_active(|| u);
    }

    /// The window gained or lost keyboard focus.
    pub fn window_focus(&mut self, focused: bool) {
        self.inner.update_window_focus_state(focused);
    }
}
