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
    let old_mode = state.config.mode;
    state.config = next;
    // The wallpaper file may have changed under the same name: re-read it.
    state.borders.wallpaper.invalidate();
    // `mode` moved: float or re-tile the windows before the arrange below.
    crate::shell::de::mode_changed(state, old_mode, state.config.mode);
    // The desktop icons run only under "de": start or stop their unit.
    crate::session::desktop_sync(Some(old_mode), state.config.mode);
    // The theme follows the new config (ADR 0084); emits `theme` if it moved.
    crate::theme::refresh(state);
    // ext_background_effect_v1: `blur` is advertised only while blur is on.
    crate::protocols::standard::background_effect::update_capabilities(state);
    // eclipse_glass_v1: panes are drawn, and advertised, only in `glass` mode.
    crate::protocols::standard::glass::update_capabilities(state);
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
    // Retune the admission filters. They hold ExeAllowlist handles rather
    // than snapshots precisely so this is possible (ADR 0022 amendment); each
    // `set` re-resolves symlink entries (ADR 0094).
    state.capture_allow.set(state.config.capture.allow.clone());
    state
        .clipboard_allow
        .set(state.config.clipboard.data_control_allow.clone());
    state
        .input_method_allow
        .set(state.config.input_method.allow.clone());
    state
        .virtual_pointer
        .allow
        .set(state.config.virtual_pointer.allow.clone());
    warn_missing_admissions(&[
        ("capture.allow", &state.capture_allow),
        ("clipboard.data-control-allow", &state.clipboard_allow),
        ("input-method.allow", &state.input_method_allow),
        ("virtual-pointer.allow", &state.virtual_pointer.allow),
    ]);
    crate::input::apply_config(state);
    crate::outputs::reapply_settings(state);
    crate::outputs::relayout(state);
    crate::shell::arrange(state);
    // `capture.redact-app-id` may have grown: withdraw what is now `secret`.
    crate::protocols::standard::foreign_toplevel::refresh_all(state);
    crate::backend::damage_all(state);
    crate::trusted_ui::approval::schedule(state);
    tracing::info!(?sources, "config applied");
}

/// ADR 0094: an admission entry that names no file admits nothing. Almost
/// always a bare name for a binary outside `/usr/bin` (a portal backend in
/// `/usr/lib`, a dev build behind `/usr/local/bin`), so say how to fix it.
/// Startup and every reload; executable names only, never input.
pub fn warn_missing_admissions(lists: &[(&'static str, &ExeAllowlist)]) {
    for (key, list) in lists {
        for entry in list.missing() {
            tracing::warn!(
                key,
                entry = %entry,
                "admission entry names no existing file and admits nothing; a bare name means /usr/bin/<name>, anything else needs its absolute path (ADR 0094)"
            );
        }
    }
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
