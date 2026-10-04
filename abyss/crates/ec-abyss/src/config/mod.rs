// SPDX-License-Identifier: AGPL-3.0-only
//! KDL configuration (COMP-13 §1).
//!
//! The schema, parser, editor, approvals and widget hashes live in the
//! `ec-abyss-config` crate (no smithay, so `ec-settings` and `ec-ctl` can link
//! it) and are re-exported here, so `crate::config::Config`,
//! `crate::config::schema::TABLE` and friends keep their paths. What stays in
//! this module is everything that needs `AbyssState`: applying a loaded
//! config, the widget catalog, the file watcher and approval withholding.

pub use ec_abyss_config::*;

pub mod catalog;
pub mod transitions;
pub mod watch;
pub mod withhold;

/// Swap in an already-validated config and retune everything that caches a
/// piece of it.
///
/// Factored out of `watch::reload_now` so the control socket's write path
/// (COMP-13 §1.4) applies a config by exactly the same steps a file edit
/// does. Two apply paths that drift is how a GUI-set value ends up meaning
/// something different from the same value typed into the file.
///
/// Caller's obligation: `next.errors` is empty. Nothing here re-validates.
/// Startup does not come through here: a config that started with dropped
/// nodes (ADR 0064) goes straight into `AbyssState::config`, refusals and all;
/// only a clean reload replaces it.
pub fn apply_loaded(state: &mut crate::state::AbyssState, next: Config) {
    debug_assert!(next.errors.is_empty(), "apply_loaded got an invalid config");
    let mut next = next;
    // ADR 0066/0067: without the hook, widget blocks go; with it, unapproved
    // command widgets are withheld and queued for the owner's prompt.
    withhold::settle(state, &mut next);
    let sources: Vec<String> = next
        .sources
        .iter()
        .map(|s| s.path.display().to_string())
        .collect();
    state.config = next;
    // A clean load: nothing is wrong any more, so nothing is replayed.
    state.config_error = None;
    // Remember what is on disk now, so the inotify event our own write is
    // about to produce can be told from a human's edit by content (A4). Done
    // for every source, not just the one written: the rule is "the live config
    // is exactly these bytes", and a file with no entry would otherwise make
    // every suppression check fail as soon as there are two sources.
    state.config_written = state
        .config
        .sources
        .iter()
        .filter_map(|s| {
            std::fs::read_to_string(&s.path)
                .ok()
                .map(|t| (s.path.clone(), crate::ipc::config_rpc::hash(&t)))
        })
        .collect();
    // Retune the two global bind filters. They hold Allowlist handles rather
    // than snapshots precisely so this line is possible (ADR 0022 amendment).
    state.capture_allow.set(state.config.capture.allow.clone());
    state
        .clipboard_allow
        .set(state.config.clipboard.data_control_allow.clone());
    crate::input::apply_config(state);
    crate::outputs::reapply_settings(state);
    crate::outputs::relayout(state);
    crate::shell::arrange(state);
    crate::backend::damage_all(state);
    crate::trusted_ui::approval::schedule(state);
    tracing::info!(?sources, "config applied");
}

/// ADR 0066: `widget` blocks are inert without the `taskbar-widgets` hook.
/// They are ignored, not refused, so a config written with the taskbar
/// installed still loads after it is removed; their `custom:` ids go with them
/// so no list names a widget that is not there.
pub fn drop_unhooked_widgets(cfg: &mut Config, hooks: crate::addons::HookSet) {
    if hooks.is_on(crate::addons::Hook::TaskbarWidgets) || cfg.bar.custom_widgets.is_empty() {
        return;
    }
    tracing::info!(
        count = cfg.bar.custom_widgets.len(),
        "add-on hook `taskbar-widgets` is off; ignoring widget blocks"
    );
    cfg.bar.custom_widgets.clear();
    let builtin = |id: &String| !id.starts_with(schema::BAR_WIDGET_CUSTOM_PREFIX);
    cfg.bar.widgets.order.retain(builtin);
    cfg.bar.widgets.important.retain(builtin);
}

#[cfg(test)]
pub(crate) mod tests {
    /// Apply `text` as `a.kdl`, returning the config and its refusals.
    pub(crate) fn widgets_cfg(text: &str) -> super::Config {
        super::parse_single_for_tests(text)
    }
}
