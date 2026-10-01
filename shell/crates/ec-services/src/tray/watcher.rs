// SPDX-License-Identifier: AGPL-3.0-only
//! `org.kde.StatusNotifierWatcher`: the registry apps announce their tray
//! items to.
//!
//! Every host reads the list from here, including ours, so the watcher keeps
//! nothing but names: which items exist, and which hosts are listening. An
//! entry is `bus/path`, the form every host already knows how to split.
//! Entries go away when their owner leaves the bus, and the host thread
//! reports that to us (see [`Watcher::forget`]), because a watcher that
//! remembered dead items would give the bar icons that do nothing when
//! clicked.

use zbus::message::Header;
use zbus::names::BusName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::ObjectPath;
use zbus::{fdo, interface};

pub(super) const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
pub(super) const WATCHER_PATH: &str = "/StatusNotifierWatcher";
pub(super) const WATCHER_INTERFACE: &str = "org.kde.StatusNotifierWatcher";

/// Where an item lives when it registers by bus name alone.
pub(super) const DEFAULT_ITEM_PATH: &str = "/StatusNotifierItem";

/// Past this, a client is flooding the registry rather than showing icons.
const MAX_ITEMS: usize = 128;
const MAX_HOSTS: usize = 16;

struct Registered {
    /// `bus/path` for an item, the bus name for a host.
    entry: String,
    /// The unique name that registered it. When that leaves the bus, so does
    /// everything it registered.
    owner: String,
}

impl Registered {
    /// Whether `name` leaving the bus takes this entry with it: either the
    /// connection that registered it, or the bus name it lives at (an app
    /// hides its icon by releasing that name).
    fn gone_with(&self, name: &str) -> bool {
        self.owner == name || bus_of(&self.entry) == name
    }
}

#[derive(Default)]
pub(super) struct Watcher {
    items: Vec<Registered>,
    hosts: Vec<Registered>,
}

/// What [`Watcher::forget`] removed, for the signals the spec asks for.
#[derive(Default)]
pub(super) struct Forgotten {
    pub(super) items: Vec<String>,
    pub(super) last_host: bool,
}

impl Watcher {
    /// `name` left the bus. Drop everything that went with it.
    pub(super) fn forget(&mut self, name: &str) -> Forgotten {
        let mut forgotten = Forgotten::default();
        self.items.retain(|item| {
            let keep = !item.gone_with(name);
            if !keep {
                forgotten.items.push(item.entry.clone());
            }
            keep
        });
        let had_hosts = !self.hosts.is_empty();
        self.hosts.retain(|host| !host.gone_with(name));
        forgotten.last_host = had_hosts && self.hosts.is_empty();
        forgotten
    }
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    /// `service` is a bus name (the item is at `/StatusNotifierItem` on it)
    /// or an object path (the item is at that path on the caller). Both forms
    /// are in the wild: KDE sends a name, libappindicator sends a path.
    async fn register_status_notifier_item(
        &mut self,
        service: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let owner = sender(&header)?;
        let entry = item_entry(&service, &owner)
            .ok_or_else(|| fdo::Error::InvalidArgs("not a bus name or object path".to_owned()))?;
        if self.items.iter().any(|item| item.entry == entry) {
            return Ok(());
        }
        if self.items.len() >= MAX_ITEMS {
            return Err(fdo::Error::LimitsExceeded("too many tray items".to_owned()));
        }
        self.items.push(Registered {
            entry: entry.clone(),
            owner,
        });
        Self::status_notifier_item_registered(&emitter, &entry).await?;
        self.registered_status_notifier_items_changed(&emitter).await?;
        Ok(())
    }

    async fn register_status_notifier_host(
        &mut self,
        service: String,
        #[zbus(header)] header: Header<'_>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let owner = sender(&header)?;
        BusName::try_from(service.as_str())
            .map_err(|_| fdo::Error::InvalidArgs("not a bus name".to_owned()))?;
        if self.hosts.iter().any(|host| host.entry == service) {
            return Ok(());
        }
        if self.hosts.len() >= MAX_HOSTS {
            return Err(fdo::Error::LimitsExceeded("too many tray hosts".to_owned()));
        }
        let first = self.hosts.is_empty();
        self.hosts.push(Registered {
            entry: service,
            owner,
        });
        if first {
            Self::status_notifier_host_registered(&emitter).await?;
            self.is_status_notifier_host_registered_changed(&emitter).await?;
        }
        Ok(())
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.iter().map(|item| item.entry.clone()).collect()
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        !self.hosts.is_empty()
    }

    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }

    #[zbus(signal)]
    pub(super) async fn status_notifier_item_registered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(super) async fn status_notifier_item_unregistered(
        emitter: &SignalEmitter<'_>,
        service: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_notifier_host_registered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(super) async fn status_notifier_host_unregistered(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

fn sender(header: &Header<'_>) -> fdo::Result<String> {
    header
        .sender()
        .map(ToString::to_string)
        .ok_or_else(|| fdo::Error::InvalidArgs("no sender".to_owned()))
}

/// The registry entry for what an item sent: `bus/path`.
fn item_entry(service: &str, sender: &str) -> Option<String> {
    if service.starts_with('/') {
        ObjectPath::try_from(service).ok()?;
        Some(format!("{sender}{service}"))
    } else {
        BusName::try_from(service).ok()?;
        Some(format!("{service}{DEFAULT_ITEM_PATH}"))
    }
}

/// The bus half of a `bus/path` entry. Bus names never contain `/`.
pub(super) fn bus_of(entry: &str) -> &str {
    entry.split_once('/').map_or(entry, |(bus, _)| bus)
}

/// Split an entry into bus and path. A bare bus name means the default path.
pub(super) fn split_entry(entry: &str) -> (&str, &str) {
    match entry.find('/') {
        Some(at) => (&entry[..at], &entry[at..]),
        None => (entry, DEFAULT_ITEM_PATH),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_registration_forms_become_bus_and_path() {
        assert_eq!(
            item_entry("/org/ayatana/NotificationItem/discord", ":1.42").as_deref(),
            Some(":1.42/org/ayatana/NotificationItem/discord")
        );
        assert_eq!(
            item_entry("org.kde.StatusNotifierItem-4242-1", ":1.42").as_deref(),
            Some("org.kde.StatusNotifierItem-4242-1/StatusNotifierItem")
        );
        assert_eq!(item_entry("not a name", ":1.42"), None);
        assert_eq!(item_entry("/bad//path", ":1.42"), None);
        assert_eq!(
            split_entry(":1.42/org/ayatana/NotificationItem/discord"),
            (":1.42", "/org/ayatana/NotificationItem/discord")
        );
        assert_eq!(split_entry("org.x.Y"), ("org.x.Y", DEFAULT_ITEM_PATH));
    }

    #[test]
    fn an_item_leaves_with_its_owner_or_its_name() {
        let mut watcher = Watcher::default();
        watcher.items.push(Registered {
            entry: ":1.7/org/ayatana/NotificationItem/a".to_owned(),
            owner: ":1.7".to_owned(),
        });
        watcher.items.push(Registered {
            entry: "org.kde.StatusNotifierItem-9-1/StatusNotifierItem".to_owned(),
            owner: ":1.9".to_owned(),
        });
        watcher.hosts.push(Registered {
            entry: "org.kde.StatusNotifierHost-3".to_owned(),
            owner: ":1.3".to_owned(),
        });

        // The app released its item name but is still on the bus.
        let gone = watcher.forget("org.kde.StatusNotifierItem-9-1");
        assert_eq!(gone.items, ["org.kde.StatusNotifierItem-9-1/StatusNotifierItem"]);
        assert!(!gone.last_host);

        let gone = watcher.forget(":1.7");
        assert_eq!(gone.items, [":1.7/org/ayatana/NotificationItem/a"]);
        assert!(watcher.registered_status_notifier_items().is_empty());

        let gone = watcher.forget(":1.3");
        assert!(gone.last_host);
        assert!(!watcher.is_status_notifier_host_registered());
    }
}
