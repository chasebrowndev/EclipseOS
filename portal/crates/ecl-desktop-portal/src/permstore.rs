// SPDX-License-Identifier: AGPL-3.0-only

//! `org.freedesktop.impl.portal.PermissionStore` on its own bus name, backed
//! by `ecl-portal-store`. Like the upstream service this is an unauthenticated
//! per-user key-value store; the portal's own allow/deny decisions use the
//! separate grant records, never these tables.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ecl_portal_store::{Data, Entry};
use zbus::{Connection, OwnedVariant};

use crate::convert::{from_val, to_val};
use crate::error::PortalError;
use crate::state::Shared;
use ecl_portal_core::val::Val;

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.PermissionStore";
pub const PATH: &str = "/org/freedesktop/impl/portal/PermissionStore";
const IFACE: &str = "org.freedesktop.impl.portal.PermissionStore";

pub struct PermStoreSvc(pub Arc<Shared>);

type WirePerms = HashMap<String, Vec<String>>;

fn data_from(v: &OwnedVariant) -> Result<Data, PortalError> {
    Ok(match to_val(v) {
        Val::Bool(b) => Data::Bool(b),
        Val::U32(x) => Data::U32(x),
        Val::I32(x) => Data::I64(i64::from(x)),
        Val::I64(x) => Data::I64(x),
        Val::F64(x) => Data::F64(x),
        Val::Str(s) => Data::Str(s),
        v @ Val::List(_) => match (v.as_strs(), v.as_bytes()) {
            (Some(s), _) if !s.is_empty() => Data::Strs(s),
            (_, Some(b)) => Data::Bytes(b),
            (Some(_), _) => Data::Strs(Vec::new()),
            _ => return Err(PortalError::NotSupported("unsupported data type".into())),
        },
        _ => return Err(PortalError::NotSupported("unsupported data type".into())),
    })
}

fn data_to(d: &Data) -> OwnedVariant {
    let v = match d {
        Data::None => Val::Bool(false),
        Data::Bool(b) => Val::Bool(*b),
        Data::U32(x) => Val::U32(*x),
        Data::I64(x) => Val::I64(*x),
        Data::F64(x) => Val::F64(*x),
        Data::Str(s) => Val::Str(s.clone()),
        Data::Bytes(b) => Val::Bytes(b.clone()),
        Data::Strs(s) => Val::Strs(s.clone()),
    };
    from_val(&v)
        .and_then(|v| OwnedVariant::try_from(v).ok())
        .unwrap_or_else(|| OwnedVariant::from(false))
}

fn perms_in(p: WirePerms) -> BTreeMap<String, Vec<String>> {
    p.into_iter().collect()
}

fn perms_out(e: &Entry) -> WirePerms {
    e.permissions.clone().into_iter().collect()
}

async fn changed(conn: &Connection, table: &str, id: &str, deleted: bool, entry: &Entry) {
    let body = (
        table.to_owned(),
        id.to_owned(),
        deleted,
        data_to(&entry.data),
        perms_out(entry),
    );
    if let Err(e) = conn
        .emit_signal(None::<&str>, PATH, IFACE, "Changed", &body)
        .await
    {
        tracing::warn!("cannot emit Changed: {e}");
    }
}

#[zbus::interface(name = "org.freedesktop.impl.portal.PermissionStore")]
impl PermStoreSvc {
    async fn lookup(
        &self,
        table: &str,
        id: &str,
    ) -> Result<(WirePerms, OwnedVariant), PortalError> {
        let e = self
            .0
            .store
            .lookup(table, id)?
            .ok_or_else(|| PortalError::NotFound(format!("No entry for {id}")))?;
        Ok((perms_out(&e), data_to(&e.data)))
    }

    async fn set(
        &self,
        table: &str,
        create: bool,
        id: &str,
        app_permissions: WirePerms,
        data: OwnedVariant,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PortalError> {
        let d = data_from(&data)?;
        self.0
            .store
            .set(table, create, id, perms_in(app_permissions), d)?;
        if let Some(e) = self.0.store.lookup(table, id)? {
            changed(conn, table, id, false, &e).await;
        }
        Ok(())
    }

    async fn set_value(
        &self,
        table: &str,
        create: bool,
        id: &str,
        data: OwnedVariant,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PortalError> {
        let d = data_from(&data)?;
        self.0.store.set_value(table, create, id, d)?;
        if let Some(e) = self.0.store.lookup(table, id)? {
            changed(conn, table, id, false, &e).await;
        }
        Ok(())
    }

    async fn set_permission(
        &self,
        table: &str,
        create: bool,
        id: &str,
        app: &str,
        permissions: Vec<String>,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PortalError> {
        self.0
            .store
            .set_permission(table, create, id, app, permissions)?;
        if let Some(e) = self.0.store.lookup(table, id)? {
            changed(conn, table, id, false, &e).await;
        }
        Ok(())
    }

    async fn delete(
        &self,
        table: &str,
        id: &str,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PortalError> {
        let before = self.0.store.lookup(table, id)?;
        self.0.store.delete(table, id)?;
        if let Some(e) = before {
            changed(conn, table, id, true, &e).await;
        }
        Ok(())
    }

    async fn delete_permission(
        &self,
        table: &str,
        id: &str,
        app: &str,
        #[zbus(connection)] conn: &Connection,
    ) -> Result<(), PortalError> {
        self.0.store.delete_permission(table, id, app)?;
        if let Some(e) = self.0.store.lookup(table, id)? {
            changed(conn, table, id, false, &e).await;
        }
        Ok(())
    }

    async fn get_permission(
        &self,
        table: &str,
        id: &str,
        app: &str,
    ) -> Result<Vec<String>, PortalError> {
        Ok(self.0.store.get_permission(table, id, app)?)
    }

    async fn list(&self, table: &str) -> Result<Vec<String>, PortalError> {
        Ok(self.0.store.list(table)?)
    }

    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        2
    }
}
