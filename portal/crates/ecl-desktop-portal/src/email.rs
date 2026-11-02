// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.Email`: confirm in an Eclipse pane, then hand a
//! `mailto:` URI to the default mail handler.

use std::sync::Arc;

use ecl_portal_core::email::{addresses, mailto};
use ecl_portal_core::openuri::scheme_mime;
use ecl_portal_core::request::Reply;
use ecl_portal_proto::{PaneReply, PaneRequest};
use ecl_portal_store::Decision;
use zbus::message::Header;
use zbus::{Connection, OwnedObjectPath};

use crate::apps;
use crate::convert::{to_options, WireDict};
use crate::error::PortalError;
use crate::launch;
use crate::pane;
use crate::req::{self, Outcome};
use crate::state::Shared;

pub const TABLE: &str = "email";

pub struct EmailSvc(pub Arc<Shared>);

#[zbus::interface(name = "org.freedesktop.portal.Email")]
impl EmailSvc {
    async fn compose_email(
        &self,
        _parent_window: &str,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let opts = to_options(&options);
        let uri = mailto(&opts);
        let to = addresses(&opts);
        let n = self.0.next_id();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| async move {
            let app = ctx.requester.app_id.clone();
            let decision = ctx.shared.store.check(&app, TABLE, "", ctx.shared.now());
            let send = match decision {
                Decision::Allow => true,
                Decision::Deny => false,
                Decision::Ask => {
                    let q = PaneRequest::Email {
                        requester: ctx.requester.clone(),
                        addresses: to,
                    };
                    matches!(pane::ask(&q).await, Ok(PaneReply::Email { send: true }))
                }
            };
            if !send {
                return Outcome::Reply(Reply::cancelled());
            }
            let cands = apps::mime_apps().candidates(&scheme_mime("mailto"));
            Outcome::Reply(if launch::open(&cands, &uri, None, n).await {
                Reply::ok(Default::default())
            } else {
                Reply::other()
            })
        })
        .await
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        4
    }
}
