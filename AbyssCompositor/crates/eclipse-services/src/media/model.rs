// SPDX-License-Identifier: AGPL-3.0-only
//! The media feed's bus-free half: MPRIS property mapping, which player the
//! widget shows, the art cache and the dedupe. Everything here is a pure
//! function of what the bus said, so it is tested without a bus.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;

use zbus::zvariant::{OwnedValue, Value};

use super::{Art, NowPlaying, Playback, Update};

/// Every MPRIS player owns a name under this prefix.
pub(super) const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

/// Cover art larger than this is not read. Real covers are well under it; the
/// cap is there because the path comes from the player, not from us.
pub(super) const ART_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// How many covers stay resident. A few tracks' worth, so skipping back and
/// forth never re-reads a file, without growing for the length of a session.
const ART_CACHE_ENTRIES: usize = 8;

/// The track, as the player describes it in `Metadata`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Meta {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub art_url: Option<String>,
}

/// Map an MPRIS `Metadata` dict (`a{sv}`). Unknown keys are ignored; a value
/// of the wrong type is treated as absent rather than trusted.
pub(super) fn metadata_from(value: &Value<'_>) -> Meta {
    let mut meta = Meta::default();
    let Value::Dict(dict) = unwrap(value) else {
        return meta;
    };
    for (key, value) in dict.iter() {
        let Value::Str(key) = unwrap(key) else {
            continue;
        };
        match key.as_str() {
            "xesam:title" => meta.title = text(value),
            "xesam:artist" => meta.artist = texts(value),
            "xesam:album" => meta.album = text(value),
            "mpris:artUrl" => meta.art_url = text(value),
            _ => {}
        }
    }
    meta
}

/// `PlaybackStatus`. Anything not in the spec reads as stopped: a player that
/// cannot say it is playing does not get the widget.
pub(super) fn playback_from(value: &Value<'_>) -> Playback {
    match unwrap(value) {
        Value::Str(s) if s.as_str() == "Playing" => Playback::Playing,
        Value::Str(s) if s.as_str() == "Paused" => Playback::Paused,
        _ => Playback::Stopped,
    }
}

/// Peel `v` wrappers: a variant inside a variant is legal and some players
/// send one.
fn unwrap<'a>(mut value: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(inner) = value {
        value = inner;
    }
    value
}

fn text(value: &Value<'_>) -> Option<String> {
    match unwrap(value) {
        Value::Str(s) if !s.is_empty() => Some(s.as_str().to_owned()),
        _ => None,
    }
}

/// `xesam:artist` is `as` by the spec, a plain string from some players.
fn texts(value: &Value<'_>) -> Option<String> {
    match unwrap(value) {
        Value::Array(items) => {
            let joined = items.iter().filter_map(text).collect::<Vec<_>>().join(", ");
            (!joined.is_empty()).then_some(joined)
        }
        other => text(other),
    }
}

fn flag(value: &Value<'_>) -> Option<bool> {
    match unwrap(value) {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// One player, as last heard.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Player {
    /// The unique name (`:1.42`) behind the well-known one: property signals
    /// come from it.
    pub owner: String,
    pub status: Playback,
    pub meta: Meta,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_pause: bool,
    /// Clock value when it last started playing; 0 if it never has.
    last_played: u64,
    /// Clock value when it appeared, to break ties among the never-played.
    seen: u64,
}

/// Every player on the bus, keyed by well-known name, and which one the
/// widget is showing.
#[derive(Debug, Default)]
pub(super) struct Players {
    map: BTreeMap<String, Player>,
    active: Option<String>,
    clock: u64,
}

impl Players {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// A player appeared or changed hands. A new owner is a new process, so
    /// what the old one said no longer holds.
    pub fn upsert(&mut self, name: &str, owner: &str) {
        if self.map.get(name).is_some_and(|p| p.owner == owner) {
            return;
        }
        let seen = self.tick();
        self.map.insert(
            name.to_owned(),
            Player {
                owner: owner.to_owned(),
                status: Playback::Stopped,
                meta: Meta::default(),
                can_prev: false,
                can_next: false,
                can_pause: false,
                last_played: 0,
                seen,
            },
        );
    }

    pub fn remove(&mut self, name: &str) {
        self.map.remove(name);
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.active = None;
    }

    pub fn names_owned_by(&self, owner: &str) -> Vec<String> {
        self.map
            .iter()
            .filter(|(_, p)| p.owner == owner)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Fold `org.mpris.MediaPlayer2.Player` properties (a `GetAll` reply or
    /// the changed half of `PropertiesChanged`) into one player.
    pub fn apply(&mut self, name: &str, props: &HashMap<String, OwnedValue>) {
        let now = self.clock + 1;
        let Some(player) = self.map.get_mut(name) else {
            return;
        };
        let mut started = false;
        for (key, value) in props {
            match key.as_str() {
                "PlaybackStatus" => {
                    let status = playback_from(value);
                    started |= status == Playback::Playing && player.status != Playback::Playing;
                    player.status = status;
                }
                "Metadata" => player.meta = metadata_from(value),
                "CanGoPrevious" => player.can_prev = flag(value).unwrap_or(false),
                "CanGoNext" => player.can_next = flag(value).unwrap_or(false),
                "CanPause" => player.can_pause = flag(value).unwrap_or(false),
                _ => {}
            }
        }
        if started {
            player.last_played = now;
            self.clock = now;
        }
    }

    /// Pick the player to show, and remember it. The most recent to start
    /// playing wins; failing that, the one already shown stays while paused;
    /// failing that, the most recently used paused player. All stopped (or
    /// none at all) is nothing.
    pub fn choose(&mut self) -> Option<(&str, &Player)> {
        let recency = |p: &Player| (p.last_played, p.seen);
        let playing = self
            .map
            .iter()
            .filter(|(_, p)| p.status == Playback::Playing)
            .max_by_key(|(_, p)| recency(p));
        let kept = || {
            self.active
                .as_deref()
                .and_then(|name| self.map.get_key_value(name))
                .filter(|(_, p)| p.status == Playback::Paused)
        };
        let paused = || {
            self.map
                .iter()
                .filter(|(_, p)| p.status == Playback::Paused)
                .max_by_key(|(_, p)| recency(p))
        };
        let chosen = playing.or_else(kept).or_else(paused).map(|(n, _)| n.clone());
        self.active = chosen;
        let name = self.active.as_deref()?;
        self.map.get_key_value(name).map(|(n, p)| (n.as_str(), p))
    }

    pub fn active(&self) -> Option<&str> {
        self.active.as_deref()
    }
}

/// What the widget draws for `player`, with `art` already resolved.
pub(super) fn now_playing(name: &str, player: &Player, art: Option<Art>) -> NowPlaying {
    NowPlaying {
        player: name.strip_prefix(MPRIS_PREFIX).unwrap_or(name).to_owned(),
        title: player.meta.title.clone().unwrap_or_default(),
        artist: player.meta.artist.clone(),
        album: player.meta.album.clone(),
        art,
        status: player.status,
        can_prev: player.can_prev,
        can_next: player.can_next,
        can_pause: player.can_pause,
    }
}

/// Suppresses an update identical to the last one sent. Starts as if
/// `Player(None)` had been sent: the GUI starts with nothing too.
#[derive(Debug, Default)]
pub(super) struct Dedupe {
    last: Option<NowPlaying>,
}

impl Dedupe {
    pub fn admit(&mut self, next: Option<NowPlaying>) -> Option<Update> {
        if self.last == next {
            return None;
        }
        self.last.clone_from(&next);
        Some(Update::Player(next))
    }
}

/// What to do about an `mpris:artUrl`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ArtLookup {
    /// No art to show, and nothing to read.
    None,
    Ready(Art),
    /// Not read yet; the caller should read it now.
    Read,
    /// A read is already in flight.
    Pending,
}

/// Covers read so far, keyed by URL. A failed read is cached as `None` so a
/// missing file is not retried on every signal.
#[derive(Debug, Default)]
pub(super) struct ArtCache {
    entries: VecDeque<(String, Option<Arc<[u8]>>)>,
    pending: HashSet<String>,
}

impl ArtCache {
    pub fn lookup(&mut self, url: &str) -> ArtLookup {
        if !url.starts_with("file://") {
            return ArtLookup::None;
        }
        if let Some((key, bytes)) = self.entries.iter().find(|(k, _)| k == url) {
            return match bytes {
                Some(bytes) => ArtLookup::Ready(Art {
                    key: key.clone(),
                    bytes: bytes.clone(),
                }),
                None => ArtLookup::None,
            };
        }
        if self.pending.insert(url.to_owned()) {
            ArtLookup::Read
        } else {
            ArtLookup::Pending
        }
    }

    pub fn insert(&mut self, url: String, bytes: Option<Arc<[u8]>>) {
        self.pending.remove(&url);
        self.entries.retain(|(k, _)| *k != url);
        if self.entries.len() == ART_CACHE_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back((url, bytes));
    }
}

/// Read a `file://` cover. `None` for anything else, a missing or non-regular
/// file, or one over [`ART_MAX_BYTES`]. Blocking: call it off the service
/// thread.
///
/// `http(s)` art is never fetched. The services ship no HTTP client, and a
/// player's say-so is not a reason to make network requests on the human's
/// behalf; such players simply show no cover.
pub(super) fn read_art(url: &str) -> Option<Arc<[u8]>> {
    let rest = url.strip_prefix("file://")?;
    // `file://localhost/x` and `file:///x` both name `/x`.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let decoded = percent_decode(rest)?;
    let path = Path::new(OsStr::from_bytes(&decoded));
    // Not a FIFO or a device: those would block or never end.
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > ART_MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(path)
        .ok()?
        .take(ART_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.is_empty() || bytes.len() as u64 > ART_MAX_BYTES {
        return None;
    }
    Some(bytes.into())
}

/// `%XX` escapes in a URL path. A malformed escape rejects the URL.
fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(value: Value<'_>) -> OwnedValue {
        OwnedValue::try_from(value).expect("owned")
    }

    fn metadata(entries: Vec<(&str, Value<'static>)>) -> Value<'static> {
        let map: HashMap<String, Value<'static>> =
            entries.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        Value::from(map)
    }

    fn props(entries: Vec<(&str, Value<'static>)>) -> HashMap<String, OwnedValue> {
        entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), owned(v)))
            .collect()
    }

    fn status(s: &'static str) -> HashMap<String, OwnedValue> {
        props(vec![("PlaybackStatus", Value::from(s))])
    }

    #[test]
    fn metadata_maps_the_xesam_and_mpris_keys() {
        let value = metadata(vec![
            ("xesam:title", Value::from("Song")),
            ("xesam:artist", Value::from(vec!["A", "B"])),
            ("xesam:album", Value::from("Record")),
            ("mpris:artUrl", Value::from("file:///tmp/cover.png")),
            ("mpris:length", Value::from(123_i64)),
        ]);
        assert_eq!(
            metadata_from(&value),
            Meta {
                title: Some("Song".into()),
                artist: Some("A, B".into()),
                album: Some("Record".into()),
                art_url: Some("file:///tmp/cover.png".into()),
            }
        );
    }

    #[test]
    fn metadata_tolerates_wrapped_single_and_mistyped_values() {
        let value = Value::new(metadata(vec![
            ("xesam:title", Value::new(Value::from("Wrapped"))),
            ("xesam:artist", Value::from("Solo")),
            ("xesam:album", Value::from(7_u32)),
            ("mpris:artUrl", Value::from("")),
        ]));
        assert_eq!(
            metadata_from(&value),
            Meta {
                title: Some("Wrapped".into()),
                artist: Some("Solo".into()),
                album: None,
                art_url: None,
            }
        );
        assert_eq!(metadata_from(&Value::from(1_u8)), Meta::default());
    }

    #[test]
    fn playback_status_maps_and_unknown_is_stopped() {
        assert_eq!(playback_from(&Value::from("Playing")), Playback::Playing);
        assert_eq!(playback_from(&Value::from("Paused")), Playback::Paused);
        assert_eq!(playback_from(&Value::from("Stopped")), Playback::Stopped);
        assert_eq!(playback_from(&Value::from("Buffering")), Playback::Stopped);
        assert_eq!(playback_from(&Value::from(true)), Playback::Stopped);
    }

    #[test]
    fn apply_folds_a_getall_reply() {
        let mut players = Players::default();
        players.upsert("org.mpris.MediaPlayer2.mpv", ":1.5");
        players.apply(
            "org.mpris.MediaPlayer2.mpv",
            &props(vec![
                ("PlaybackStatus", Value::from("Paused")),
                ("Metadata", metadata(vec![("xesam:title", Value::from("T"))])),
                ("CanGoPrevious", Value::from(true)),
                ("CanGoNext", Value::from(false)),
                ("CanPause", Value::from(true)),
                ("Volume", Value::from(0.5_f64)),
            ]),
        );
        let (name, player) = players.choose().expect("paused player shown");
        let np = now_playing(name, player, None);
        assert_eq!(np.player, "mpv");
        assert_eq!(np.title, "T");
        assert_eq!(np.status, Playback::Paused);
        assert!(np.can_prev && !np.can_next && np.can_pause);
    }

    fn chosen(players: &mut Players) -> Option<String> {
        players.choose().map(|(n, _)| n.to_owned())
    }

    #[test]
    fn nothing_or_all_stopped_shows_nothing() {
        let mut players = Players::default();
        assert_eq!(chosen(&mut players), None);
        players.upsert("a", ":1");
        players.upsert("b", ":2");
        players.apply("a", &status("Stopped"));
        assert_eq!(chosen(&mut players), None);
    }

    #[test]
    fn most_recently_playing_wins() {
        let mut players = Players::default();
        players.upsert("a", ":1");
        players.upsert("b", ":2");
        players.apply("a", &status("Playing"));
        assert_eq!(chosen(&mut players).as_deref(), Some("a"));
        players.apply("b", &status("Playing"));
        assert_eq!(chosen(&mut players).as_deref(), Some("b"));
        // Re-announcing Playing is not a new start.
        players.apply("a", &status("Playing"));
        assert_eq!(chosen(&mut players).as_deref(), Some("b"));
        // b stops: a is still playing and takes over.
        players.apply("b", &status("Stopped"));
        assert_eq!(chosen(&mut players).as_deref(), Some("a"));
    }

    #[test]
    fn a_paused_active_player_stays_over_other_paused_ones() {
        let mut players = Players::default();
        players.upsert("a", ":1");
        players.upsert("b", ":2");
        players.apply("b", &status("Paused"));
        players.apply("a", &status("Playing"));
        assert_eq!(chosen(&mut players).as_deref(), Some("a"));
        players.apply("a", &status("Paused"));
        // b appeared paused and never played; a keeps the widget.
        assert_eq!(chosen(&mut players).as_deref(), Some("a"));
        // a stops: the other paused player is still something to show.
        players.apply("a", &status("Stopped"));
        assert_eq!(chosen(&mut players).as_deref(), Some("b"));
        players.remove("b");
        assert_eq!(chosen(&mut players), None);
    }

    #[test]
    fn a_new_owner_resets_what_the_old_one_said() {
        let mut players = Players::default();
        players.upsert("a", ":1");
        players.apply("a", &status("Playing"));
        players.upsert("a", ":1");
        assert_eq!(chosen(&mut players).as_deref(), Some("a"));
        players.upsert("a", ":9");
        assert_eq!(chosen(&mut players), None);
        assert_eq!(players.names_owned_by(":9"), vec!["a".to_owned()]);
        assert!(players.names_owned_by(":1").is_empty());
    }

    #[test]
    fn dedupe_drops_repeats_and_starts_at_none() {
        let mut dedupe = Dedupe::default();
        assert_eq!(dedupe.admit(None), None);
        let mut players = Players::default();
        players.upsert("org.mpris.MediaPlayer2.x", ":1");
        players.apply("org.mpris.MediaPlayer2.x", &status("Playing"));
        let (n, p) = players.choose().unwrap();
        let np = now_playing(n, p, None);
        assert_eq!(
            dedupe.admit(Some(np.clone())),
            Some(Update::Player(Some(np.clone())))
        );
        assert_eq!(dedupe.admit(Some(np.clone())), None);
        assert_eq!(dedupe.admit(None), Some(Update::Player(None)));
        assert_eq!(dedupe.admit(None), None);
    }

    #[test]
    fn art_cache_reads_file_urls_once_and_skips_http() {
        let mut cache = ArtCache::default();
        assert_eq!(cache.lookup("https://example.com/a.png"), ArtLookup::None);
        let url = "file:///x.png";
        assert_eq!(cache.lookup(url), ArtLookup::Read);
        assert_eq!(cache.lookup(url), ArtLookup::Pending);
        let bytes: Arc<[u8]> = Arc::from(&b"png"[..]);
        cache.insert(url.to_owned(), Some(bytes.clone()));
        assert_eq!(
            cache.lookup(url),
            ArtLookup::Ready(Art {
                key: url.to_owned(),
                bytes
            })
        );
        cache.insert("file:///missing".to_owned(), None);
        assert_eq!(cache.lookup("file:///missing"), ArtLookup::None);
        for i in 0..ART_CACHE_ENTRIES {
            cache.insert(format!("file:///{i}"), None);
        }
        assert_eq!(cache.lookup(url), ArtLookup::Read, "evicted, oldest first");
    }

    #[test]
    fn read_art_decodes_paths_and_enforces_the_cap() {
        let dir = std::env::temp_dir().join(format!("eclipse-media-art-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cover art.png");
        std::fs::write(&file, b"\x89PNG").unwrap();
        let url = format!("file://{}", file.display()).replace(' ', "%20");
        assert_eq!(read_art(&url).as_deref(), Some(&b"\x89PNG"[..]));
        let localhost = url.replacen("file://", "file://localhost", 1);
        assert!(read_art(&localhost).is_some());

        let big = dir.join("big.png");
        std::fs::write(&big, vec![0u8; ART_MAX_BYTES as usize + 1]).unwrap();
        assert_eq!(read_art(&format!("file://{}", big.display())), None);

        assert_eq!(read_art(&format!("file://{}", dir.display())), None, "dir");
        assert_eq!(read_art("file:///dev/zero"), None, "device");
        assert_eq!(read_art("file://%zz"), None);
        assert_eq!(read_art("http://example.com/a.png"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
