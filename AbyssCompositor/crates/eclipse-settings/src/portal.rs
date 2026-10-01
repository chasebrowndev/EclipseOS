// SPDX-License-Identifier: AGPL-3.0-only
//! The XDG file-chooser portal, called directly over D-Bus: Browse for the
//! wallpaper image. No toolkit dialog crate; the portal backend draws it.

use std::collections::HashMap;
use std::path::PathBuf;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";

/// Ask the portal for one image. `Ok(None)` is the user cancelling; a missing
/// portal or a failed call is an `Err` the caller shows.
pub async fn pick_image() -> Result<Option<PathBuf>, String> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    // The call blocks until the dialog closes, so it gets its own thread
    // rather than an executor worker.
    std::thread::Builder::new()
        .name("settings-portal".into())
        .spawn(move || {
            let _ = tx.send(pick_blocking());
        })
        .map_err(|e| format!("file chooser: {e}"))?;
    rx.await
        .map_err(|_| "file chooser: portal thread died".to_string())?
}

/// The Request object path the portal will use for `token` from the caller
/// with unique bus name `unique` (`:1.42` becomes `1_42`).
pub fn request_path(unique: &str, token: &str) -> String {
    let sender = unique.trim_start_matches(':').replace('.', "_");
    format!("{DESKTOP_PATH}/request/{sender}/{token}")
}

/// A `file://` URI as a path, percent-decoded. Other schemes are `None`.
pub fn decode_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file://host/path`: the host is empty or `localhost` in practice.
    let path = &rest[rest.find('/')?..];
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

fn pick_blocking() -> Result<Option<PathBuf>, String> {
    let err = |what: &str, e: zbus::Error| format!("file chooser portal: {what}: {e}");
    let conn = Connection::session().map_err(|e| err("no session bus", e))?;
    let unique = conn.unique_name().map(|n| n.to_string()).unwrap_or_default();
    let token = format!("eclipse_settings_{}", std::process::id());
    let req_path = request_path(&unique, &token);

    // Subscribe before the call, or a fast Response is lost.
    let request = Proxy::new(&conn, DEST, req_path.as_str(), "org.freedesktop.portal.Request")
        .map_err(|e| err("request", e))?;
    let mut responses = request
        .receive_signal("Response")
        .map_err(|e| err("subscribe", e))?;

    let chooser = Proxy::new(&conn, DEST, DESKTOP_PATH, "org.freedesktop.portal.FileChooser")
        .map_err(|e| err("unavailable", e))?;
    let filter: (&str, Vec<(u32, &str)>) = (
        "Images",
        vec![(1, "image/png"), (1, "image/jpeg"), (1, "image/webp")],
    );
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("filters", Value::from(vec![filter]));
    let _handle: zbus::zvariant::OwnedObjectPath = chooser
        .call("OpenFile", &("", "Choose wallpaper", options))
        .map_err(|e| err("OpenFile", e))?;

    let msg = responses
        .next()
        .ok_or_else(|| "file chooser portal: closed without a response".to_string())?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(|e| format!("file chooser portal: bad response: {e}"))?;
    match code {
        0 => {}
        1 => return Ok(None),
        _ => return Err("file chooser portal: dialog failed".into()),
    }
    let uris: Vec<String> = results
        .get("uris")
        .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default();
    match uris.first() {
        None => Ok(None),
        Some(u) => decode_uri(u)
            .map(Some)
            .ok_or_else(|| format!("file chooser portal: not a file URI: {u}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_path_mangles_sender() {
        assert_eq!(
            request_path(":1.42", "tok"),
            "/org/freedesktop/portal/desktop/request/1_42/tok"
        );
    }

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(
            decode_uri("file:///home/a/My%20Pics/%C3%A9.png"),
            Some(PathBuf::from("/home/a/My Pics/é.png"))
        );
        assert_eq!(
            decode_uri("file://localhost/x.png"),
            Some(PathBuf::from("/x.png"))
        );
        assert_eq!(decode_uri("https://x/y.png"), None);
        assert_eq!(decode_uri("file:///bad%2"), None);
    }
}
