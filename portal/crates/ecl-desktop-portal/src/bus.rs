// SPDX-License-Identifier: AGPL-3.0-only

//! Wiring: bus names, exported interfaces, background watchers.

use std::sync::Arc;
use std::time::Duration;

use ecl_portal_store::{data_root, Store};
use tokio::sync::mpsc::UnboundedReceiver;
use zbus::names::BusName;
use zbus::Connection;

use crate::state::{lock, Shared};
use crate::{
    account, apps, email, filechooser, impl_cast, inhibit, notification, openuri, permstore,
    screen, session, settings, trash,
};

pub const FRONTEND_NAME: &str = "org.freedesktop.portal.Desktop";
pub const BACKEND_NAME: &str = "org.freedesktop.impl.portal.desktop.eclipse";

/// Build the shared state from the environment.
pub fn make_shared() -> anyhow::Result<(Arc<Shared>, Option<UnboundedReceiver<String>>)> {
    let root = data_root(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .ok_or_else(|| anyhow::anyhow!("neither XDG_DATA_HOME nor HOME is an absolute path"))?;
    let config_dir = apps::config_home()
        .ok_or_else(|| anyhow::anyhow!("no config home"))?
        .join("eclipse");
    let appearance = settings::load(&config_dir);
    let backend = crate::cast::CastBackend::real();
    let revoked = backend.take_revocations();
    Ok((
        Arc::new(Shared::new(
            Store::new(root),
            backend,
            apps::load_index(),
            config_dir,
            appearance,
        )),
        revoked,
    ))
}

/// Set to `1` to serve the frontend names too: only for a system where
/// ecl-desktop-portal replaces upstream `xdg-desktop-portal` (D-08 §7).
pub const FRONTEND_ENV: &str = "ECL_PORTAL_FRONTEND";

/// Claim the frontend names, when asked to and nobody owns them. As shipped,
/// upstream `xdg-desktop-portal` owns `org.freedesktop.portal.Desktop` and
/// PermissionStore and routes ScreenCast/Screenshot to our backend names
/// (`impl_cast`), so we serve the backend only. It bus-activates us before it
/// requests its own name, so claiming "if free" would steal the frontend and
/// leave every other interface unserved.
async fn claim_frontend(conn: &Connection) {
    if std::env::var(FRONTEND_ENV).as_deref() != Ok("1") {
        tracing::info!("serving the backend interfaces only");
        return;
    }
    for name in [FRONTEND_NAME, permstore::BUS_NAME] {
        let got = conn
            .request_name_with_flags(name, zbus::fdo::RequestNameFlags::DO_NOT_QUEUE)
            .await;
        match got {
            Ok(
                zbus::fdo::RequestNameReply::PrimaryOwner
                | zbus::fdo::RequestNameReply::AlreadyOwner,
            ) => {
                tracing::info!("serving {name}");
            }
            _ => tracing::info!("{name} is owned elsewhere; serving the backend interfaces only"),
        }
    }
}

pub async fn connect(shared: &Arc<Shared>) -> zbus::Result<Connection> {
    let conn = build(shared).await?;
    claim_frontend(&conn).await;
    Ok(conn)
}

async fn build(shared: &Arc<Shared>) -> zbus::Result<Connection> {
    zbus::connection::Builder::session()
        .name(BACKEND_NAME)
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
        .serve_at(
            settings::PORTAL_PATH,
            impl_cast::ImplScreenCast(shared.clone()),
        )
        .serve_at(
            settings::PORTAL_PATH,
            impl_cast::ImplScreenshot(shared.clone()),
        )
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

/// abyss closed a grant's portal.sock connection (it revokes every grant on
/// session lock): close the session it belonged to and tell the app.
async fn revocations(shared: Arc<Shared>, conn: Connection, mut rx: UnboundedReceiver<String>) {
    while let Some(path) = rx.recv().await {
        tracing::info!("capture grant revoked by abyss; closing {path}");
        if lock(&shared.sessions).owner(&path).is_some() {
            session::close(&shared, &conn, &path).await;
        } else {
            impl_cast::revoked(&shared, &conn, &path).await;
        }
    }
}

pub async fn run() -> anyhow::Result<()> {
    let (shared, revoked) = make_shared()?;
    let conn = connect(&shared).await?;
    if let Some(rx) = revoked {
        tokio::spawn(revocations(shared.clone(), conn.clone(), rx));
    }
    tracing::info!("serving {BACKEND_NAME}");
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
