// SPDX-License-Identifier: AGPL-3.0-only

//! Wiring: bus names, exported interfaces, background watchers.

use std::sync::Arc;
use std::time::Duration;

use ecl_portal_core::screencast::StubBackend;
use ecl_portal_store::{data_root, Store};
use zbus::names::BusName;
use zbus::Connection;

use crate::state::{lock, Shared};
use crate::{
    account, apps, email, filechooser, inhibit, notification, openuri, permstore, screen, session,
    settings, trash,
};

pub const FRONTEND_NAME: &str = "org.freedesktop.portal.Desktop";
pub const BACKEND_NAME: &str = "org.freedesktop.impl.portal.desktop.eclipse";

/// Build the shared state from the environment.
pub fn make_shared() -> anyhow::Result<Arc<Shared>> {
    let root = data_root(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .ok_or_else(|| anyhow::anyhow!("neither XDG_DATA_HOME nor HOME is an absolute path"))?;
    let config_dir = apps::config_home()
        .ok_or_else(|| anyhow::anyhow!("no config home"))?
        .join("eclipse");
    let appearance = settings::load(&config_dir);
    Ok(Arc::new(Shared::new(
        Store::new(root),
        StubBackend::shared(),
        apps::load_index(),
        config_dir,
        appearance,
    )))
}

pub async fn connect(shared: &Arc<Shared>) -> zbus::Result<Connection> {
    zbus::connection::Builder::session()
        .name(FRONTEND_NAME)
        .name(BACKEND_NAME)
        .name(permstore::BUS_NAME)
        .serve_at(settings::PORTAL_PATH, settings::SettingsSvc(shared.clone()))
        .serve_at(settings::PORTAL_PATH, openuri::OpenUriSvc(shared.clone()))
        .serve_at(
            settings::PORTAL_PATH,
            notification::NotificationSvc(shared.clone()),
        )
        .serve_at(settings::PORTAL_PATH, inhibit::InhibitSvc(shared.clone()))
        .serve_at(settings::PORTAL_PATH, trash::TrashSvc)
        .serve_at(settings::PORTAL_PATH, email::EmailSvc(shared.clone()))
        .serve_at(settings::PORTAL_PATH, account::AccountSvc(shared.clone()))
        .serve_at(
            settings::PORTAL_PATH,
            filechooser::FileChooserSvc(shared.clone()),
        )
        .serve_at(settings::PORTAL_PATH, filechooser::ImplFileChooser)
        .serve_at(settings::PORTAL_PATH, screen::ScreenshotSvc(shared.clone()))
        .serve_at(settings::PORTAL_PATH, screen::ScreenCastSvc(shared.clone()))
        .serve_at(permstore::PATH, permstore::PermStoreSvc(shared.clone()))
        .build()
        .await
}

/// Cancel everything owned by callers that left the bus. Polled: a request
/// outliving its caller by a few seconds is harmless.
async fn reap(shared: Arc<Shared>, conn: Connection) {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else {
        return;
    };
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    loop {
        tick.tick().await;
        let mut owners = lock(&shared.requests).owners();
        owners.extend(lock(&shared.sessions).owners());
        owners.sort();
        owners.dedup();
        for owner in owners {
            let alive = match BusName::try_from(owner.as_str()) {
                Ok(n) => dbus.name_has_owner(n).await.unwrap_or(true),
                Err(_) => false,
            };
            if alive {
                continue;
            }
            let reqs = lock(&shared.requests).take_owner(&owner);
            for (_, tx) in reqs {
                let _ = tx.send(());
            }
            let sessions: Vec<String> = lock(&shared.sessions)
                .take_owner(&owner)
                .into_iter()
                .map(|(p, _)| p)
                .collect();
            for path in sessions {
                // Re-insert-free teardown: the table entry is already gone, so
                // finish the rest by hand.
                let _ = conn
                    .object_server()
                    .remove::<session::SessionObj, _>(path.as_str())
                    .await;
                shared.backend.close_session(&path).await;
            }
            lock(&shared.notifications).retain(|(s, _), _| s != &owner);
        }
    }
}

pub async fn run() -> anyhow::Result<()> {
    let shared = make_shared()?;
    let conn = connect(&shared).await?;
    tracing::info!("serving {FRONTEND_NAME} and {BACKEND_NAME}");
    tokio::spawn(settings::watch(shared.clone(), conn.clone()));
    tokio::spawn(reap(shared.clone(), conn.clone()));

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down");
    Ok(())
}
