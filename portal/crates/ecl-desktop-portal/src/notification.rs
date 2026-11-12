// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.Notification`, forwarded to the session's
//! `org.freedesktop.Notifications` server (ec-services). Notification text is
//! the app's own and is passed through verbatim; it is never logged here.

use std::collections::HashMap;
use std::sync::Arc;

use ecl_portal_core::notification::to_notify;
use zbus::message::Header;
use zbus::{Connection, OwnedVariant, Variant};

use crate::convert::{to_options, WireDict};
use crate::error::PortalError;
use crate::ident;
use crate::req::sender_of;
use crate::state::{lock, Shared};

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

pub struct NotificationSvc(pub Arc<Shared>);

#[zbus::interface(name = "org.freedesktop.portal.Notification")]
impl NotificationSvc {
    async fn add_notification(
        &self,
        id: &str,
        notification: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PortalError> {
        if id.is_empty() || id.len() > 255 {
            return Err(PortalError::InvalidArgument("bad notification id".into()));
        }
        let sender = sender_of(&hdr)?;
        let who = ident::requester(&self.0, conn, &sender).await;
        let name = who
            .display_name
            .clone()
            .unwrap_or_else(|| who.app_id.clone());
        let n = to_notify(&name, &to_options(&notification));
        let key = (sender, id.to_owned());
        let replaces = lock(&self.0.notifications).get(&key).copied().unwrap_or(0);

        let mut hints: HashMap<String, OwnedVariant> = HashMap::new();
        hints.insert("urgency".into(), OwnedVariant::from(n.urgency));
        if let Ok(v) = OwnedVariant::try_from(Variant::from(who.app_id.clone())) {
            hints.insert("desktop-entry".into(), v);
        }
        let reply = conn
            .call_method(
                Some(DEST),
                PATH,
                Some(DEST),
                "Notify",
                &(
                    n.app_name,
                    replaces,
                    n.icon,
                    n.summary,
                    n.body,
                    n.actions,
                    hints,
                    n.expire_timeout,
                ),
            )
            .await
            .map_err(|e| PortalError::Failed(format!("notification server: {e}")))?;
        let server_id: u32 = reply
            .body()
            .deserialize::<u32>()
            .map_err(|e| PortalError::Failed(format!("notification server reply: {e}")))?;
        lock(&self.0.notifications).insert(key, server_id);
        Ok(())
    }

    async fn remove_notification(
        &self,
        id: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PortalError> {
        let key = (sender_of(&hdr)?, id.to_owned());
        let Some(server_id) = lock(&self.0.notifications).remove(&key) else {
            return Ok(());
        };
        conn.call_method(
            Some(DEST),
            PATH,
            Some(DEST),
            "CloseNotification",
            &(server_id,),
        )
        .await
        .map_err(|e| PortalError::Failed(format!("notification server: {e}")))?;
        Ok(())
    }

    #[zbus(property, name = "SupportedOptions")]
    async fn supported_options(&self) -> WireDict {
        HashMap::new()
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        1
    }
}
