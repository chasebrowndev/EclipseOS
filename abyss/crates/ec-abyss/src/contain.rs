// SPDX-License-Identifier: AGPL-3.0-only
//! Panic containment for non-essential features (ADR 0102).
//!
//! A panic anywhere in this process used to end the session. [`contain`] runs
//! one *contained* feature's entry point under `catch_unwind`; if that feature's
//! own code panics, the feature is reset to its "off" state, turned off for the
//! rest of the session, reported once (journal and the `feature-disabled` IPC
//! event), and the loop carries on.
//!
//! What makes that sound is narrow on purpose:
//!
//! - **Only listed features.** [`Feature`] is the whole list. The TCB
//!   (`policy/`, `trusted_ui/`, `audit/`, `render/capture.rs`), the session
//!   lock, input delivery to clients and the per-frame render path are never
//!   behind a boundary, so a panic there unwinds out of the loop as before.
//! - **Only the feature's own code.** The panic hook records where a panic was
//!   raised. A panic inside a boundary is kept only if it was raised in one of
//!   the feature's [own source files](Feature::owns); raised anywhere else
//!   (smithay, `shell/`, the TCB, `std`) it is re-raised untouched. So every
//!   other module was between two of its own calls, invariants held, when the
//!   unwind began; what may be half-done is the feature's own state, and
//!   [`Feature::reset`] throws exactly that away.
//! - **Once.** The off bit is set before the reset runs and is never cleared
//!   (`Contained` has no setter that turns a feature back on), and a boundary
//!   checks it first. A panic during the reset aborts the process.
//! - **Nothing produces pixels inside a boundary.** Contained entry points
//!   change state; the backends draw from it afterwards, outside any boundary.
//!   A contained panic can therefore only leave *less* on screen (a closed
//!   overview, no annotations) or answer an error, never skip a redaction.
//!
//! The hook never logs the panic payload: a `str` slicing panic quotes the
//! string it sliced, which can be a title or typed text. Location and thread
//! only (root invariant on human input).
//!
//! The happy path costs two thread-local writes and the `catch_unwind` landing
//! pad: no allocation, no lock.

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};

use crate::state::AbyssState;

/// A feature whose panic is contained. Adding one is an ADR 0102 amendment:
/// it needs a named reset and a test that injects a panic into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    /// The window switcher, the overview, hot corners and the hover preview
    /// (`overview/`), entered from bindings, the pointer and the corner timer.
    Overview,
    /// COMP-18 annotations, entered from the `annotation_*` socket methods.
    Annotations,
    /// The read-only control-socket queries (`get_*`, `dump_state`).
    Queries,
}

impl Feature {
    pub const ALL: [Feature; 3] = [Feature::Overview, Feature::Annotations, Feature::Queries];

    /// The name on the wire and in the journal.
    pub fn name(self) -> &'static str {
        match self {
            Feature::Overview => "overview",
            Feature::Annotations => "annotations",
            Feature::Queries => "queries",
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }

    /// Source files a panic of this feature may be raised in. Rooted at the
    /// crate directory, matched as a path prefix after any `/`, so it does not
    /// depend on where cargo ran. A miss fails closed: the panic propagates.
    fn sources(self) -> &'static [&'static str] {
        match self {
            Feature::Overview => &[
                "ec-abyss/src/overview/",
                "ec-abyss-render/src/overview.rs",
                "ec-abyss-render/src/thumb.rs",
            ],
            Feature::Annotations => &[
                "ec-abyss/src/ipc/methods.rs",
                "ec-abyss-render/src/annotation.rs",
                "ec-abyss-render/src/text.rs",
            ],
            Feature::Queries => &["ec-abyss/src/ipc/methods.rs", "ec-abyss/src/theme.rs"],
        }
    }

    fn owns(self, file: &str) -> bool {
        self.sources().iter().any(|p| has_path_prefix(file, p))
    }

    /// Put the feature in its "off" state. Touches only the feature's own
    /// state, never the lock or a trusted prompt.
    fn reset(self, state: &mut AbyssState) {
        match self {
            // Everything it owns closed, its textures and timer dropped. The
            // same call the session lock makes as it engages.
            Feature::Overview => crate::overview::close_all(state),
            // A fresh store: whatever the panic left half-written goes with the
            // old one, fading annotations and GPU art included.
            Feature::Annotations => {
                drop(std::mem::take(&mut state.annotations));
                crate::backend::damage_all(state);
            }
            // Read-only: a reply under construction unwound with the stack and
            // was never sent. Nothing to put back.
            Feature::Queries => {}
        }
    }
}

/// One read-only query method. Each has its own off bit: a panic in one turns
/// off only that method (ADR 0102), the other queries keep answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query {
    GetWorkspaces,
    GetDesktopGrid,
    GetOutputs,
    GetWindows,
    GetFocused,
    GetMetrics,
    DumpState,
    GetTheme,
}

impl Query {
    pub const ALL: [Query; 8] = [
        Query::GetWorkspaces,
        Query::GetDesktopGrid,
        Query::GetOutputs,
        Query::GetWindows,
        Query::GetFocused,
        Query::GetMetrics,
        Query::DumpState,
        Query::GetTheme,
    ];

    /// The socket method name.
    pub fn name(self) -> &'static str {
        match self {
            Query::GetWorkspaces => "get_workspaces",
            Query::GetDesktopGrid => "get_desktop_grid",
            Query::GetOutputs => "get_outputs",
            Query::GetWindows => "get_windows",
            Query::GetFocused => "get_focused",
            Query::GetMetrics => "get_metrics",
            Query::DumpState => "dump_state",
            Query::GetTheme => "get_theme",
        }
    }

    const fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// `pat` occurs in `file` at the start or right after a `/`.
fn has_path_prefix(file: &str, pat: &str) -> bool {
    file.match_indices(pat)
        .any(|(i, _)| i == 0 || file.as_bytes()[i - 1] == b'/')
}

/// Which features are off for this session. Lives in `AbyssState`. There is
/// deliberately no way to turn one back on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Contained {
    off: u8,
    queries: u16,
}

impl Contained {
    pub fn is_off(self, f: Feature) -> bool {
        self.off & f.bit() != 0
    }

    pub fn query_is_off(self, q: Query) -> bool {
        self.queries & q.bit() != 0
    }

    /// One `feature-disabled` payload per thing that is off: a feature
    /// (`{"feature"}`) or a query method (`{"feature":"queries","method"}`).
    pub fn payloads(self) -> Vec<serde_json::Value> {
        let mut v: Vec<_> = Feature::ALL
            .into_iter()
            .filter(|f| self.is_off(*f))
            .map(|f| serde_json::json!({ "feature": f.name() }))
            .collect();
        v.extend(
            Query::ALL
                .into_iter()
                .filter(|q| self.query_is_off(*q))
                .map(|q| serde_json::json!({ "feature": "queries", "method": q.name() })),
        );
        v
    }

    fn disable(&mut self, f: Feature, q: Option<Query>) {
        match q {
            Some(q) => self.queries |= q.bit(),
            None => self.off |= f.bit(),
        }
    }
}

thread_local! {
    /// The feature whose boundary is running on this thread.
    static ACTIVE: Cell<Option<Feature>> = const { Cell::new(None) };
    /// Set by the hook when the panic in flight was raised in `ACTIVE`'s own
    /// code: where, as `file:line:col`.
    static CAUGHT: Cell<Option<String>> = const { Cell::new(None) };
    /// A reset is running; a panic now aborts.
    static RESETTING: Cell<bool> = const { Cell::new(false) };
}

/// Run `f`, one entry point of `feature`, with its panics contained. `None`
/// when the feature is off (now or already). Not reentrant across features: a
/// boundary inside another runs as part of the outer one, so a panic there is
/// judged against the outer feature's sources.
pub fn contain<R>(
    state: &mut AbyssState,
    feature: Feature,
    f: impl FnOnce(&mut AbyssState) -> R,
) -> Option<R> {
    contain_as(state, feature, None, f)
}

/// [`contain`] for one query method: only that method's bit is checked and set.
pub fn contain_query<R>(state: &mut AbyssState, q: Query, f: impl FnOnce(&mut AbyssState) -> R) -> Option<R> {
    contain_as(state, Feature::Queries, Some(q), f)
}

fn contain_as<R>(
    state: &mut AbyssState,
    feature: Feature,
    query: Option<Query>,
    f: impl FnOnce(&mut AbyssState) -> R,
) -> Option<R> {
    let off = match query {
        Some(q) => state.contained.query_is_off(q),
        None => state.contained.is_off(feature),
    };
    if off {
        return None;
    }
    if ACTIVE.get().is_some() {
        return Some(f(state));
    }
    ACTIVE.set(Some(feature));
    CAUGHT.set(None);
    let r = panic::catch_unwind(AssertUnwindSafe(|| f(state)));
    ACTIVE.set(None);
    let payload = match r {
        Ok(v) => return Some(v),
        Err(p) => p,
    };
    // Not raised in this feature's own code, or no hook of ours saw it: not
    // ours to keep. Unwind on as if there were no boundary.
    let Some(at) = CAUGHT.take() else {
        panic::resume_unwind(payload)
    };
    drop(payload);
    state.contained.disable(feature, query);
    RESETTING.set(true);
    feature.reset(state);
    RESETTING.set(false);
    tracing::error!(
        feature = feature.name(),
        method = query.map_or("", Query::name),
        at = %at,
        "contained a panic; the feature is off for the rest of the session (ADR 0102)"
    );
    crate::ipc::emit(
        state,
        "feature-disabled",
        query.map_or_else(
            || serde_json::json!({ "feature": feature.name() }),
            |q| serde_json::json!({ "feature": feature.name(), "method": q.name() }),
        ),
    );
    None
}

/// Install the panic hook (once). `main` calls it before the loop starts; a
/// process without it contains nothing, since [`contain`] keeps only panics
/// the hook vouched for.
pub fn install_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !hook(info) && chain_default() {
                prev(info);
            }
        }));
    });
}

/// The hook proper. True when the panic will be contained.
fn hook(info: &PanicHookInfo<'_>) -> bool {
    let loc = info.location();
    let file = loc.map_or("", |l| l.file());
    let at = loc.map_or_else(|| String::from("<unknown>"), ToString::to_string);
    // `try_with`: a panic during thread-local teardown must not panic again
    // in here (that would abort before anything is logged).
    if RESETTING.try_with(Cell::get).unwrap_or(false) {
        tracing::error!(at = %at, "panic while resetting a contained feature; aborting (ADR 0102)");
        std::process::abort();
    }
    let active = ACTIVE.try_with(Cell::get).ok().flatten();
    if active.is_some_and(|f| f.owns(file)) && CAUGHT.try_with(|c| c.set(Some(at.clone()))).is_ok() {
        return true;
    }
    let _ = CAUGHT.try_with(|c| c.set(None));
    let current = std::thread::current();
    let thread = current.name().unwrap_or("<unnamed>");
    tracing::error!(
        at = %at,
        thread,
        within = active.map_or("none", Feature::name),
        "abyss panicked outside any contained feature; not recoverable (ADR 0102)"
    );
    if !cfg!(test) {
        // Frames only: symbol names and source paths, never values.
        let bt = std::backtrace::Backtrace::force_capture();
        tracing::error!(backtrace = %bt, "panic backtrace");
    }
    false
}

/// Whether to also run the previous (std default) hook for an uncontained
/// panic. Never in a release: it prints the payload, which may quote human
/// input. Under test it keeps failing tests readable.
#[cfg(not(test))]
fn chain_default() -> bool {
    false
}

#[cfg(test)]
fn chain_default() -> bool {
    !testing::QUIET.get()
}

/// Test-only fault injection point, placed in a feature's own code. Panics at
/// the *caller's* location once [`testing::arm`] armed `feature`.
#[cfg(test)]
#[track_caller]
pub(crate) fn fault(feature: Feature) {
    if testing::FAULT.get() == Some(feature) {
        testing::FAULT.set(None);
        panic!("injected fault ({})", feature.name());
    }
}

#[cfg(not(test))]
#[inline(always)]
pub(crate) fn fault(_: Feature) {}

#[cfg(test)]
pub(crate) mod testing {
    use super::{Cell, Feature};

    thread_local! {
        pub(super) static FAULT: Cell<Option<Feature>> = const { Cell::new(None) };
        pub(super) static QUIET: Cell<bool> = const { Cell::new(false) };
    }

    /// The next [`super::fault`] for `feature` on this thread panics.
    pub fn arm(feature: Feature) {
        super::install_hook();
        FAULT.set(Some(feature));
    }

    /// Run `f`, which is expected to panic uncontained, without the default
    /// hook printing it.
    pub fn quietly<R>(f: impl FnOnce() -> R) -> R {
        super::install_hook();
        QUIET.set(true);
        let r = f();
        QUIET.set(false);
        r
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};

    use serde_json::json;

    use super::{contain, testing, Feature, Query};
    use crate::{ipc::methods::dispatch, shell::focus::state_tests::harness};

    #[test]
    fn source_prefixes_match_whole_path_segments_only() {
        assert!(Feature::Overview.owns("crates/ec-abyss/src/overview/mod.rs"));
        assert!(Feature::Overview.owns("ec-abyss/src/overview/switcher.rs"));
        assert!(Feature::Overview.owns("/abs/abyss/crates/ec-abyss-render/src/overview.rs"));
        assert!(!Feature::Overview.owns("crates/xec-abyss/src/overview/mod.rs"));
        assert!(!Feature::Overview.owns("crates/ec-abyss/src/policy/scene.rs"));
        assert!(!Feature::Overview.owns("vendor/smithay/src/input/pointer/mod.rs"));
        assert!(!Feature::Queries.owns("crates/ec-abyss/src/trusted_ui/mod.rs"));
        assert!(!Feature::Annotations.owns(""));
    }

    #[test]
    fn a_panic_in_a_query_turns_off_only_that_method() {
        let mut h = harness();
        let (conn, _peer) = crate::ipc::test_conn(&mut h.state);
        let focus = h.state.focus.clone();
        assert!(dispatch(&mut h.state, conn, "get_workspaces", &json!(null)).is_ok());
        let _ = crate::ipc::capture::take();

        testing::arm(Feature::Queries);
        let r = dispatch(&mut h.state, conn, "get_workspaces", &json!(null));
        assert!(r.is_err_and(|e| e.code == crate::ipc::DENIED));
        assert!(h.state.contained.query_is_off(Query::GetWorkspaces));
        assert!(!h.state.contained.is_off(Feature::Queries));
        assert!(!h.state.contained.is_off(Feature::Overview));
        assert!(!h.state.contained.is_off(Feature::Annotations));
        let events = crate::ipc::capture::take();
        assert_eq!(
            events,
            [(
                "feature-disabled".to_owned(),
                json!({"feature": "queries", "method": "get_workspaces"})
            )]
        );

        // That method stays off, quietly (no second event)...
        assert!(dispatch(&mut h.state, conn, "get_workspaces", &json!(null)).is_err());
        assert!(crate::ipc::capture::take().is_empty());
        // ...and every other query still answers.
        for m in ["get_windows", "get_outputs", "get_focused", "dump_state"] {
            assert!(dispatch(&mut h.state, conn, m, &json!(null)).is_ok(), "{m}");
        }
        assert!(dispatch(&mut h.state, conn, "annotation_clear", &json!({})).is_ok());
        assert_eq!(h.state.focus, focus);
        assert!(!h.state.lock.locked);
        assert_eq!(
            h.state.contained.payloads(),
            [json!({"feature": "queries", "method": "get_workspaces"})]
        );
    }

    #[test]
    fn a_panic_in_an_annotation_call_drops_every_annotation_and_leaves_the_lock_alone() {
        let mut h = harness();
        let (conn, _peer) = crate::ipc::test_conn(&mut h.state);
        let one = json!({"text": "one", "x": 10, "y": 10, "w": 100, "h": 40});
        assert!(dispatch(&mut h.state, conn, "annotation_create", &one).is_ok());
        assert!(!h.state.annotations.is_empty());
        // A reset must not disturb the lock (ADR 0102): stand one up.
        h.state.lock.locked = true;
        let _ = crate::ipc::capture::take();

        // The fault fires after the store took the new annotation: the
        // half-done case the reset exists for.
        testing::arm(Feature::Annotations);
        let two = json!({"text": "two"});
        assert!(dispatch(&mut h.state, conn, "annotation_create", &two).is_err());
        assert!(h.state.contained.is_off(Feature::Annotations));
        assert!(h.state.annotations.is_empty(), "the reset drops the store");
        assert!(h.state.lock.locked, "the reset leaves the lock as it was");
        assert!(crate::ipc::capture::take()
            .iter()
            .any(|(k, v)| k == "feature-disabled" && v["feature"] == "annotations"));

        // Second call: refused before anything runs; nothing is stored.
        assert!(dispatch(&mut h.state, conn, "annotation_create", &one).is_err());
        assert!(dispatch(&mut h.state, conn, "annotation_clear", &json!({})).is_err());
        assert!(h.state.annotations.is_empty());
        // Queries are a separate feature and still answer.
        assert!(dispatch(&mut h.state, conn, "get_workspaces", &json!(null)).is_ok());
    }

    #[test]
    fn a_panic_raised_outside_the_features_own_code_is_not_contained() {
        let mut h = harness();
        // Raised here, in `contain.rs`: not an overview source, so it must
        // unwind straight through the boundary (as a TCB or smithay panic would).
        let r = testing::quietly(|| {
            catch_unwind(AssertUnwindSafe(|| {
                let _: Option<()> = contain(&mut h.state, Feature::Overview, |_| panic!("foreign"));
            }))
        });
        assert!(r.is_err(), "propagated");
        assert!(
            !h.state.contained.is_off(Feature::Overview),
            "and nothing was turned off"
        );
        // The boundary is usable again afterwards.
        assert_eq!(contain(&mut h.state, Feature::Overview, |_| 7), Some(7));
    }

    #[test]
    fn feature_code_reached_without_a_boundary_still_propagates() {
        let mut h = harness();
        testing::arm(Feature::Overview);
        // The input path wraps `overview::motion`; called directly, the same
        // code has no boundary around it (as from an unwrapped caller).
        let r = testing::quietly(|| {
            catch_unwind(AssertUnwindSafe(|| {
                crate::overview::motion(&mut h.state, (1.0, 1.0).into());
            }))
        });
        assert!(r.is_err(), "propagated");
        assert!(!h.state.contained.is_off(Feature::Overview));
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn unwinding_is_on() {
        // Containment and the VT restore need unwinding; see Cargo.toml.
        assert!(cfg!(panic = "unwind"), "panic = \"abort\" defeats ADR 0102");
    }

    #[test]
    fn a_panic_in_the_preview_code_reached_by_the_socket_method_is_contained() {
        let mut h = harness();
        let (conn, _peer) = crate::ipc::test_conn(&mut h.state);
        let _ = crate::ipc::capture::take();
        // The fault sits in `overview::preview::hide`, an overview source;
        // `hide_window_preview` reaches it through the Overview boundary.
        testing::arm(Feature::Overview);
        let r = dispatch(&mut h.state, conn, "hide_window_preview", &json!({}));
        assert!(r.is_err_and(|e| e.code == crate::ipc::DENIED));
        assert!(h.state.contained.is_off(Feature::Overview));
        assert!(crate::ipc::capture::take()
            .iter()
            .any(|(k, v)| k == "feature-disabled" && v["feature"] == "overview"));
        assert!(dispatch(&mut h.state, conn, "hide_window_preview", &json!({})).is_err());
    }
}
