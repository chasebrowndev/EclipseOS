// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.Settings`: `org.freedesktop.appearance` from Eclipse
//! tokens (dark, gold) with an optional `~/.config/eclipse/appearance.conf`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ecl_portal_core::settings::{self, parse_appearance, Appearance};
use ecl_portal_core::val::Val;
use zbus::{Connection, OwnedVariant};

use crate::convert::{from_val, owned, wrapped};
use crate::error::PortalError;
use crate::state::{lock, Shared};

pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const CONF: &str = "appearance.conf";

pub struct SettingsSvc(pub Arc<Shared>);

fn read(shared: &Shared, ns: &str, key: &str) -> Result<Val, PortalError> {
    let a = *lock(&shared.appearance);
    a.get(ns, key)
        .ok_or_else(|| PortalError::NotFound(format!("Requested setting not found: {ns} {key}")))
}

#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl SettingsSvc {
    async fn read_all(
        &self,
        namespaces: Vec<String>,
    ) -> HashMap<String, HashMap<String, OwnedVariant>> {
        let a = *lock(&self.0.appearance);
        settings::read_all(&a, &namespaces)
            .into_iter()
            .map(|(ns, kv)| {
                (
                    ns,
                    kv.into_iter()
                        .filter_map(|(k, v)| Some((k, owned(&v)?)))
                        .collect(),
                )
            })
            .collect()
    }

    /// Deprecated upstream: the value is wrapped in a second variant.
    async fn read(&self, namespace: &str, key: &str) -> Result<OwnedVariant, PortalError> {
        wrapped(&read(&self.0, namespace, key)?)
            .ok_or_else(|| PortalError::Failed("unencodable setting".into()))
    }

    async fn read_one(&self, namespace: &str, key: &str) -> Result<OwnedVariant, PortalError> {
        owned(&read(&self.0, namespace, key)?)
            .ok_or_else(|| PortalError::Failed("unencodable setting".into()))
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        2
    }
}

pub fn conf_path(config_dir: &Path) -> std::path::PathBuf {
    config_dir.join(CONF)
}

/// Current appearance from the keys file (defaults when absent/unreadable).
pub fn load(config_dir: &Path) -> Appearance {
    std::fs::read_to_string(conf_path(config_dir))
        .map(|t| parse_appearance(&t))
        .unwrap_or_default()
}

/// Poll the keys file and emit `SettingChanged` for each difference.
pub async fn watch(shared: Arc<Shared>, conn: Connection) {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let new = load(&shared.config_dir);
        let old = std::mem::replace(&mut *lock(&shared.appearance), new);
        for (ns, key, val) in settings::changes(&old, &new) {
            let Some(v) = from_val(&val) else { continue };
            let body = (ns, key, v);
            if let Err(e) = conn
                .emit_signal(
                    None::<&str>,
                    PORTAL_PATH,
                    "org.freedesktop.portal.Settings",
                    "SettingChanged",
                    &body,
                )
                .await
            {
                tracing::warn!("cannot emit SettingChanged: {e}");
            }
        }
    }
}
