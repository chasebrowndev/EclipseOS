// SPDX-License-Identifier: AGPL-3.0-only
//! `zwlr_output_management_unstable_v1` v4 (COMP-06 §1, COMP-03 §4).
//!
//! Smithay 0.7 has no module for this, so the five dispatches are written out
//! against the wlr bindings it re-exports, following `output_power.rs`.
//!
//! The compositor stays the authority. A configuration is only ever a request:
//! it is validated whole, then applied head by head through
//! [`crate::outputs::apply_change`] — the same path the human IPC uses — so the
//! "never disable the last output" refusal cannot be routed around. A
//! configuration built against a stale serial is `cancelled`, never applied.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use smithay::{
    output::Mode as OutputMode,
    reexports::{
        wayland_protocols_wlr::output_management::v1::server::{
            zwlr_output_configuration_head_v1::{
                Error as HeadConfigError, Request as ConfHeadRequest, ZwlrOutputConfigurationHeadV1,
            },
            zwlr_output_configuration_v1::{
                Error as ConfigError, Request as ConfRequest, ZwlrOutputConfigurationV1,
            },
            zwlr_output_head_v1::{AdaptiveSyncState, Request as HeadRequest, ZwlrOutputHeadV1},
            zwlr_output_manager_v1::{Request as ManagerRequest, ZwlrOutputManagerV1},
            zwlr_output_mode_v1::{Request as ModeRequest, ZwlrOutputModeV1},
        },
        wayland_server::{
            backend::{ClientId, GlobalId},
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource, WEnum,
        },
    },
    utils::Transform,
};

use crate::{outputs::OutputChange, state::AbyssState};

const VERSION: u32 = 4;

/// User data on a `zwlr_output_head_v1`: the output it describes.
pub struct HeadData {
    id: u64,
}

/// User data on a `zwlr_output_mode_v1`.
pub struct ModeData {
    mode: OutputMode,
}

/// One head inside a pending configuration.
struct PendingHead {
    id: u64,
    enabled: bool,
    change: Arc<Mutex<OutputChange>>,
    /// Something was asked for that this compositor cannot do (a custom mode,
    /// say). The configuration `failed`s rather than being half-honoured.
    unsupported: Arc<AtomicBool>,
}

/// User data on a `zwlr_output_configuration_v1`.
pub struct ConfigurationData {
    serial: u32,
    inner: Mutex<PendingConfig>,
}

#[derive(Default)]
struct PendingConfig {
    /// `apply`/`test` already consumed this object.
    used: bool,
    heads: Vec<PendingHead>,
}

/// User data on a `zwlr_output_configuration_head_v1`.
pub struct ConfigurationHeadData {
    change: Arc<Mutex<OutputChange>>,
    unsupported: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct OutputManagementState {
    #[allow(dead_code)] // holds the global alive
    global: Option<GlobalId>,
    managers: Vec<ZwlrOutputManagerV1>,
    /// Live head objects: (manager, head, output id).
    heads: Vec<(ZwlrOutputManagerV1, ZwlrOutputHeadV1, u64)>,
    /// Live mode objects, keyed by the head they belong to.
    modes: Vec<(ZwlrOutputHeadV1, ZwlrOutputModeV1)>,
    serial: u32,
}

impl OutputManagementState {
    pub fn new(display: &DisplayHandle) -> Self {
        let global = display.create_global::<AbyssState, ZwlrOutputManagerV1, _>(VERSION, ());
        Self {
            global: Some(global),
            ..Default::default()
        }
    }
}

fn adaptive_sync(on: bool) -> AdaptiveSyncState {
    if on {
        AdaptiveSyncState::Enabled
    } else {
        AdaptiveSyncState::Disabled
    }
}

/// Describe every output to one manager, from scratch.
///
/// Heads are cheap and the set changes only on hotplug or an applied
/// configuration, so a full re-send is simpler — and less error-prone — than
/// diffing each field against what the client was last told.
fn send_heads(state: &mut AbyssState, manager: &ZwlrOutputManagerV1) {
    let Some(client) = manager.client() else {
        return;
    };
    let dh = state.display_handle.clone();
    let version = manager.version();
    let entries: Vec<_> = state
        .outputs
        .iter()
        .map(|e| (e.id, e.connector.clone(), e.output.clone(), e.enabled))
        .collect();

    for (id, connector, output, enabled) in entries {
        let Ok(head) =
            client.create_resource::<ZwlrOutputHeadV1, _, AbyssState>(&dh, version, HeadData { id })
        else {
            continue;
        };
        manager.head(&head);

        let props = output.physical_properties();
        head.name(connector.clone());
        head.description(if props.model.is_empty() {
            connector.clone()
        } else {
            format!("{} {}", props.make, props.model)
        });
        if props.size.w > 0 && props.size.h > 0 {
            head.physical_size(props.size.w, props.size.h);
        }

        let current = output.current_mode();
        for mode in output.modes() {
            let Ok(res) =
                client.create_resource::<ZwlrOutputModeV1, _, AbyssState>(&dh, version, ModeData { mode })
            else {
                continue;
            };
            head.mode(&res);
            res.size(mode.size.w, mode.size.h);
            res.refresh(mode.refresh);
            if output.preferred_mode() == Some(mode) {
                res.preferred();
            }
            if current == Some(mode) {
                head.current_mode(&res);
            }
            state.output_management.modes.push((head.clone(), res));
        }

        head.enabled(enabled as i32);
        if enabled {
            if let Some(geo) = state.space.output_geometry(&output) {
                head.position(geo.loc.x, geo.loc.y);
            }
            head.transform(output.current_transform().into());
            head.scale(output.current_scale().fractional_scale());
        }
        if version >= 2 {
            head.make(props.make.clone());
            head.model(props.model.clone());
            head.serial_number(
                state
                    .outputs
                    .get(id)
                    .map(|e| e.identity.clone())
                    .unwrap_or_default(),
            );
        }
        if version >= 4 {
            head.adaptive_sync(adaptive_sync(crate::backend::output_vrr(state, id)));
        }

        state.output_management.heads.push((manager.clone(), head, id));
    }

    let serial = state.output_management.serial;
    manager.done(serial);
}

/// Outputs changed: tear the advertised set down and build it again, bumping
/// the serial so configurations in flight against the old one are cancelled.
pub fn refresh(state: &mut AbyssState) {
    for (_, mode) in std::mem::take(&mut state.output_management.modes) {
        mode.finished();
    }
    for (_, head, _) in std::mem::take(&mut state.output_management.heads) {
        head.finished();
    }
    state.output_management.serial = state.output_management.serial.wrapping_add(1);
    for manager in state.output_management.managers.clone() {
        send_heads(state, &manager);
    }
}

impl GlobalDispatch<ZwlrOutputManagerV1, ()> for AbyssState {
    fn bind(
        state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        manager: New<ZwlrOutputManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let manager = data_init.init(manager, ());
        state.output_management.managers.push(manager.clone());
        send_heads(state, &manager);
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _manager: &ZwlrOutputManagerV1,
        request: ManagerRequest,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ManagerRequest::CreateConfiguration { id, serial } => {
                data_init.init(
                    id,
                    ConfigurationData {
                        serial,
                        inner: Mutex::new(PendingConfig::default()),
                    },
                );
                let _ = state;
            }
            ManagerRequest::Stop => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, manager: &ZwlrOutputManagerV1, _data: &()) {
        state.output_management.managers.retain(|m| m != manager);
        state.output_management.heads.retain(|(m, _, _)| m != manager);
    }
}

impl Dispatch<ZwlrOutputHeadV1, HeadData> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _head: &ZwlrOutputHeadV1,
        request: HeadRequest,
        _data: &HeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            HeadRequest::Release => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, head: &ZwlrOutputHeadV1, _data: &HeadData) {
        state.output_management.heads.retain(|(_, h, _)| h != head);
        state.output_management.modes.retain(|(h, _)| h != head);
    }
}

impl Dispatch<ZwlrOutputModeV1, ModeData> for AbyssState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _mode: &ZwlrOutputModeV1,
        request: ModeRequest,
        _data: &ModeData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ModeRequest::Release => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, mode: &ZwlrOutputModeV1, _data: &ModeData) {
        state.output_management.modes.retain(|(_, m)| m != mode);
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ConfigurationData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        config: &ZwlrOutputConfigurationV1,
        request: ConfRequest,
        data: &ConfigurationData,
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            ConfRequest::EnableHead { id, head } => {
                let Some(out_id) = head.data::<HeadData>().map(|d| d.id) else {
                    return;
                };
                let mut inner = data.inner.lock().unwrap();
                if inner.heads.iter().any(|h| h.id == out_id) {
                    drop(inner);
                    config.post_error(
                        ConfigError::AlreadyConfiguredHead,
                        "head configured twice in one configuration",
                    );
                    return;
                }
                let change = Arc::new(Mutex::new(OutputChange {
                    enabled: Some(true),
                    ..Default::default()
                }));
                let unsupported = Arc::new(AtomicBool::new(false));
                inner.heads.push(PendingHead {
                    id: out_id,
                    enabled: true,
                    change: change.clone(),
                    unsupported: unsupported.clone(),
                });
                drop(inner);
                data_init.init(id, ConfigurationHeadData { change, unsupported });
            }
            ConfRequest::DisableHead { head } => {
                let Some(out_id) = head.data::<HeadData>().map(|d| d.id) else {
                    return;
                };
                let mut inner = data.inner.lock().unwrap();
                if inner.heads.iter().any(|h| h.id == out_id) {
                    drop(inner);
                    config.post_error(
                        ConfigError::AlreadyConfiguredHead,
                        "head configured twice in one configuration",
                    );
                    return;
                }
                inner.heads.push(PendingHead {
                    id: out_id,
                    enabled: false,
                    change: Arc::new(Mutex::new(OutputChange {
                        enabled: Some(false),
                        ..Default::default()
                    })),
                    unsupported: Arc::new(AtomicBool::new(false)),
                });
            }
            ConfRequest::Apply => finish(state, config, data, true),
            ConfRequest::Test => finish(state, config, data, false),
            ConfRequest::Destroy => {}
            _ => unreachable!(),
        }
    }
}

/// Shared tail of `apply` and `test`: validate the whole configuration, then
/// either apply it head by head or just report that it would have worked.
fn finish(state: &mut AbyssState, config: &ZwlrOutputConfigurationV1, data: &ConfigurationData, apply: bool) {
    {
        let mut inner = data.inner.lock().unwrap();
        if inner.used {
            drop(inner);
            config.post_error(ConfigError::AlreadyUsed, "configuration already used");
            return;
        }
        inner.used = true;
    }
    if data.serial != state.output_management.serial {
        config.cancelled();
        return;
    }

    let inner = data.inner.lock().unwrap();
    // Every advertised head must be accounted for, or the client is working
    // from a picture of the outputs we never sent it.
    let known: Vec<u64> = state.outputs.iter().map(|e| e.id).collect();
    if known.iter().any(|id| !inner.heads.iter().any(|h| h.id == *id)) {
        drop(inner);
        config.post_error(
            ConfigError::UnconfiguredHead,
            "configuration left a head unconfigured",
        );
        return;
    }
    if inner.heads.iter().any(|h| h.unsupported.load(Ordering::Relaxed)) {
        drop(inner);
        config.failed();
        return;
    }
    if !inner.heads.iter().any(|h| h.enabled) {
        drop(inner);
        config.failed();
        return;
    }

    let planned: Vec<(u64, OutputChange)> = inner
        .heads
        .iter()
        .map(|h| (h.id, h.change.lock().unwrap().clone()))
        .collect();
    drop(inner);

    if !apply {
        config.succeeded();
        return;
    }

    // Enables first: disabling before the replacement output is up would trip
    // the last-enabled-output refusal on a valid swap.
    let mut ok = true;
    for (id, change) in planned.iter().filter(|(_, c)| c.enabled != Some(false)) {
        if let Err(err) = crate::outputs::apply_change(state, *id, change) {
            tracing::warn!(id, err, "output configuration rejected");
            ok = false;
        }
    }
    for (id, change) in planned.iter().filter(|(_, c)| c.enabled == Some(false)) {
        if let Err(err) = crate::outputs::apply_change(state, *id, change) {
            tracing::warn!(id, err, "output configuration rejected");
            ok = false;
        }
    }

    if ok {
        config.succeeded();
    } else {
        config.failed();
    }
    refresh(state);
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ConfigurationHeadData> for AbyssState {
    fn request(
        state: &mut Self,
        _client: &Client,
        head: &ZwlrOutputConfigurationHeadV1,
        request: ConfHeadRequest,
        data: &ConfigurationHeadData,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let mut change = data.change.lock().unwrap();
        match request {
            ConfHeadRequest::SetMode { mode } => {
                if change.mode.is_some() {
                    drop(change);
                    head.post_error(HeadConfigError::AlreadySet, "mode already set");
                    return;
                }
                match mode.data::<ModeData>() {
                    Some(m) => change.mode = Some(m.mode),
                    None => {
                        drop(change);
                        head.post_error(HeadConfigError::InvalidMode, "unknown mode object");
                    }
                }
            }
            ConfHeadRequest::SetCustomMode {
                width,
                height,
                refresh,
            } => {
                if change.mode.is_some() {
                    drop(change);
                    head.post_error(HeadConfigError::AlreadySet, "mode already set");
                    return;
                }
                if width <= 0 || height <= 0 || refresh < 0 {
                    drop(change);
                    head.post_error(
                        HeadConfigError::InvalidCustomMode,
                        "custom mode has non-positive dimensions",
                    );
                    return;
                }
                // No modesetting outside the connector's own list: the
                // configuration fails rather than silently landing on a
                // neighbouring mode.
                data.unsupported.store(true, Ordering::Relaxed);
                let _ = state;
            }
            ConfHeadRequest::SetPosition { x, y } => {
                if change.position.is_some() {
                    drop(change);
                    head.post_error(HeadConfigError::AlreadySet, "position already set");
                    return;
                }
                change.position = Some((x, y));
            }
            ConfHeadRequest::SetTransform { transform } => {
                if change.transform.is_some() {
                    drop(change);
                    head.post_error(HeadConfigError::AlreadySet, "transform already set");
                    return;
                }
                match transform {
                    WEnum::Value(t) => change.transform = Some(Transform::from(t)),
                    other => {
                        drop(change);
                        head.post_error(
                            HeadConfigError::InvalidTransform,
                            format!("invalid transform {other:?}"),
                        );
                    }
                }
            }
            ConfHeadRequest::SetScale { scale } => {
                if change.scale.is_some() {
                    drop(change);
                    head.post_error(HeadConfigError::AlreadySet, "scale already set");
                    return;
                }
                if !(scale > 0.0 && scale <= 10.0) {
                    drop(change);
                    head.post_error(
                        HeadConfigError::InvalidScale,
                        format!("scale {scale} out of range"),
                    );
                    return;
                }
                change.scale = Some(crate::outputs::scale_from(scale));
            }
            ConfHeadRequest::SetAdaptiveSync { state: want } => {
                if change.vrr.is_some() {
                    drop(change);
                    head.post_error(HeadConfigError::AlreadySet, "adaptive sync already set");
                    return;
                }
                match want {
                    WEnum::Value(AdaptiveSyncState::Enabled) => change.vrr = Some(true),
                    WEnum::Value(AdaptiveSyncState::Disabled) => change.vrr = Some(false),
                    other => {
                        drop(change);
                        head.post_error(
                            HeadConfigError::InvalidAdaptiveSyncState,
                            format!("invalid adaptive sync state {other:?}"),
                        );
                    }
                }
            }
            _ => unreachable!(),
        }
    }
}
