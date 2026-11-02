// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.portal.Account`: user name and avatar from `/etc/passwd`
//! and `~/.face`, shown for the user to confirm or edit in a pane. No
//! AccountsService.

use std::sync::Arc;

use ecl_portal_core::account::lookup_uid;
use ecl_portal_core::filechooser::{path_to_uri, uri_to_path};
use ecl_portal_core::request::{Reply, Results};
use ecl_portal_core::val::Val;
use ecl_portal_proto::{PaneReply, PaneRequest};
use ecl_portal_store::Decision;
use zbus::message::Header;
use zbus::{Connection, OwnedObjectPath};

use crate::convert::{to_options, WireDict};
use crate::error::PortalError;
use crate::pane;
use crate::req::{self, Outcome};
use crate::state::Shared;

pub const TABLE: &str = "account";

pub struct AccountSvc(pub Arc<Shared>);

fn user_info() -> Option<(String, String, Option<String>)> {
    let uid = rustix::process::getuid().as_raw();
    let pw = lookup_uid(&std::fs::read_to_string("/etc/passwd").ok()?, uid)?;
    let face = std::path::Path::new(&pw.home).join(".face");
    let image = face
        .is_file()
        .then(|| face.to_str().map(|p| path_to_uri(p.as_bytes())))
        .flatten();
    let real = if pw.real_name.is_empty() {
        pw.name.clone()
    } else {
        pw.real_name
    };
    Some((pw.name, real, image))
}

fn reply(id: String, name: String, image: Option<String>) -> Reply {
    let mut r = Results::new().with_str("id", id).with_str("name", name);
    if let Some(i) = image {
        r.insert("image", Val::Str(i));
    }
    Reply::ok(r)
}

#[zbus::interface(name = "org.freedesktop.portal.Account")]
impl AccountSvc {
    async fn get_user_information(
        &self,
        _window: &str,
        options: WireDict,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<OwnedObjectPath, PortalError> {
        let opts = to_options(&options);
        let reason = opts
            .str("reason")
            .unwrap_or("")
            .chars()
            .take(256)
            .collect::<String>();
        req::start(&self.0, conn, &hdr, &opts, move |ctx| async move {
            let Some((id, real, image)) = user_info() else {
                return Outcome::Reply(Reply::other());
            };
            let app = ctx.requester.app_id.clone();
            match ctx.shared.store.check(&app, TABLE, "", ctx.shared.now()) {
                Decision::Allow => Outcome::Reply(reply(id, real, image)),
                Decision::Deny => Outcome::Reply(Reply::cancelled()),
                Decision::Ask => {
                    let q = PaneRequest::Account {
                        requester: ctx.requester.clone(),
                        reason,
                    };
                    match pane::ask(&q).await {
                        // The pane may edit what is shared; its values are
                        // what the app gets, never more than the user typed.
                        Ok(PaneReply::Account {
                            name,
                            real_name,
                            image,
                        }) => {
                            let shown = if real_name.is_empty() {
                                name
                            } else {
                                real_name
                            };
                            let shown: String = shown.chars().take(256).collect();
                            // The avatar must be a local file URI.
                            let image = image.filter(|i| uri_to_path(i).is_some());
                            Outcome::Reply(reply(id, shown, image))
                        }
                        Ok(PaneReply::Cancelled) => Outcome::Reply(Reply::cancelled()),
                        _ => Outcome::Reply(Reply::other()),
                    }
                }
            }
        })
        .await
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        1
    }
}
