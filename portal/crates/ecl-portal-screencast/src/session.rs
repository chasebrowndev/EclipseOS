// SPDX-License-Identifier: AGPL-3.0-only

//! Pure ScreenCast session state machine:
//! `Created -> SourcesSelected -> Started -> Closed`.
//!
//! No I/O. The caller (lane A's `ScreenCastBackend` glue) feeds it parsed
//! option values and, after the chooser pane + `portal.sock` grant have run,
//! the resulting streams. Option values are claims from the requester and are
//! validated against [`Capabilities`] here.

use ecl_portal_proto::Source;

/// `SourceType` bitmask values of `org.freedesktop.portal.ScreenCast`.
pub const SOURCE_MONITOR: u32 = 1;
pub const SOURCE_WINDOW: u32 = 2;
pub const SOURCE_VIRTUAL: u32 = 4;

/// `cursor_mode` bit values.
pub const CURSOR_HIDDEN: u32 = 1;
pub const CURSOR_EMBEDDED: u32 = 2;
pub const CURSOR_METADATA: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceType {
    Monitor,
    Window,
    Virtual,
}

impl SourceType {
    pub fn bit(self) -> u32 {
        match self {
            Self::Monitor => SOURCE_MONITOR,
            Self::Window => SOURCE_WINDOW,
            Self::Virtual => SOURCE_VIRTUAL,
        }
    }
    /// Which source type a chooser result belongs to.
    pub fn of(src: &Source) -> Self {
        match src {
            Source::Output { .. } => Self::Monitor,
            Source::Window { .. } => Self::Window,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMode {
    Hidden,
    Embedded,
    Metadata,
}

impl CursorMode {
    pub fn bit(self) -> u32 {
        match self {
            Self::Hidden => CURSOR_HIDDEN,
            Self::Embedded => CURSOR_EMBEDDED,
            Self::Metadata => CURSOR_METADATA,
        }
    }
    pub fn from_bit(b: u32) -> Option<Self> {
        match b {
            CURSOR_HIDDEN => Some(Self::Hidden),
            CURSOR_EMBEDDED => Some(Self::Embedded),
            CURSOR_METADATA => Some(Self::Metadata),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PersistMode {
    #[default]
    None,
    /// Valid while the requesting app runs.
    Transient,
    /// Until revoked (Settings).
    Persistent,
}

impl PersistMode {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::None),
            1 => Some(Self::Transient),
            2 => Some(Self::Persistent),
            _ => None,
        }
    }
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// What this backend can do right now: `AvailableSourceTypes` and
/// `AvailableCursorModes` properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub source_types: u32,
    pub cursor_modes: u32,
}

impl Capabilities {
    /// What abyss supports today: outputs only (no `ext-foreign-toplevel-list`
    /// yet), cursor hidden or painted into the frame (`overlay_cursor`); the
    /// cursor session is refused so `Metadata` is not offered.
    pub const ABYSS_TODAY: Self = Self {
        source_types: SOURCE_MONITOR,
        cursor_modes: CURSOR_HIDDEN | CURSOR_EMBEDDED,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Created,
    SourcesSelected,
    Started,
    Closed,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("session is {actual:?}, expected {expected:?}")]
    InvalidState { expected: State, actual: State },
    #[error("session closed")]
    Closed,
    #[error("a Start is already in flight")]
    StartInFlight,
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
}

/// Raw `SelectSources` options, straight from the requester (claims).
#[derive(Debug, Clone, Default)]
pub struct SelectOptions {
    pub types: Option<u32>,
    pub multiple: Option<bool>,
    pub cursor_mode: Option<u32>,
    pub persist_mode: Option<u32>,
    pub restore_token: Option<String>,
}

/// Validated selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub types: u32,
    pub multiple: bool,
    pub cursor: CursorMode,
    pub persist: PersistMode,
    /// Opaque claim; lane A validates it against the permission store.
    pub restore_token: Option<String>,
}

/// One stream of a started session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// PipeWire node id (what `Start` returns and `OpenPipeWireRemote` serves).
    pub node_id: u32,
    pub source: Source,
    /// Grant minted by policyd, relayed by abyss (ADR 0085).
    pub grant: u64,
    pub position: Option<(i32, i32)>,
    pub size: (u32, u32),
}

impl Stream {
    pub fn source_type(&self) -> SourceType {
        SourceType::of(&self.source)
    }
}

/// What to persist for `restore_token` (the store itself belongs to lane A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreData {
    pub token: String,
    pub sources: Vec<Source>,
}

/// `Start` results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub streams: Vec<Stream>,
    pub persist: PersistMode,
    pub restore: Option<RestoreData>,
}

#[derive(Debug)]
pub struct Session {
    caps: Capabilities,
    state: State,
    selection: Option<Selection>,
    streams: Vec<Stream>,
    start_in_flight: bool,
}

impl Session {
    /// `CreateSession`.
    pub fn new(caps: Capabilities) -> Self {
        Self {
            caps,
            state: State::Created,
            selection: None,
            streams: Vec::new(),
            start_in_flight: false,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }
    pub fn streams(&self) -> &[Stream] {
        &self.streams
    }

    fn expect(&self, expected: State) -> Result<(), SessionError> {
        if self.state == State::Closed {
            return Err(SessionError::Closed);
        }
        if self.state != expected {
            return Err(SessionError::InvalidState {
                expected,
                actual: self.state,
            });
        }
        Ok(())
    }

    /// `SelectSources`. Valid once, in `Created`. Defaults follow upstream:
    /// types = monitor, cursor = hidden, persist = none.
    pub fn select_sources(&mut self, o: SelectOptions) -> Result<&Selection, SessionError> {
        self.expect(State::Created)?;
        let types = o.types.unwrap_or(SOURCE_MONITOR);
        if types == 0 || types & !self.caps.source_types != 0 {
            return Err(SessionError::InvalidArgument("unsupported source type"));
        }
        let cursor = match o.cursor_mode {
            None => CursorMode::Hidden,
            Some(b) => {
                let m =
                    CursorMode::from_bit(b).ok_or(SessionError::InvalidArgument("cursor_mode"))?;
                if self.caps.cursor_modes & b == 0 {
                    return Err(SessionError::InvalidArgument("cursor_mode unavailable"));
                }
                m
            }
        };
        let persist = match o.persist_mode {
            None => PersistMode::None,
            Some(v) => {
                PersistMode::from_u32(v).ok_or(SessionError::InvalidArgument("persist_mode"))?
            }
        };
        let restore_token = o.restore_token.filter(|t| !t.is_empty());
        self.state = State::SourcesSelected;
        Ok(self.selection.insert(Selection {
            types,
            multiple: o.multiple.unwrap_or(false),
            cursor,
            persist,
            restore_token,
        }))
    }

    /// Mark the beginning of `Start` (chooser + grant are async). Guards
    /// against a second concurrent `Start`.
    pub fn begin_start(&mut self) -> Result<&Selection, SessionError> {
        self.expect(State::SourcesSelected)?;
        if self.start_in_flight {
            return Err(SessionError::StartInFlight);
        }
        self.start_in_flight = true;
        Ok(self.selection.as_ref().expect("selected"))
    }

    /// The chooser was cancelled or the grant denied/timed out. The session
    /// stays `SourcesSelected` (the app may call `Start` again).
    pub fn abort_start(&mut self) {
        self.start_in_flight = false;
    }

    /// Finish `Start`. `mint_token` is only called for a persistent request
    /// that carries no usable token yet.
    pub fn finish_start(
        &mut self,
        streams: Vec<Stream>,
        mint_token: impl FnOnce() -> String,
    ) -> Result<Started, SessionError> {
        self.expect(State::SourcesSelected)?;
        if !self.start_in_flight {
            return Err(SessionError::InvalidArgument("start not begun"));
        }
        let sel = self.selection.clone().expect("selected");
        if streams.is_empty() {
            return Err(SessionError::InvalidArgument("no streams"));
        }
        if !sel.multiple && streams.len() > 1 {
            return Err(SessionError::InvalidArgument(
                "multiple streams not allowed",
            ));
        }
        if streams
            .iter()
            .any(|s| s.source_type().bit() & sel.types == 0)
        {
            return Err(SessionError::InvalidArgument(
                "stream outside selected types",
            ));
        }
        self.start_in_flight = false;
        self.state = State::Started;
        let restore = (sel.persist != PersistMode::None).then(|| RestoreData {
            // Never echo a requester-supplied token: always a fresh one.
            token: mint_token(),
            sources: streams.iter().map(|s| s.source.clone()).collect(),
        });
        self.streams = streams.clone();
        Ok(Started {
            streams,
            persist: sel.persist,
            restore,
        })
    }

    /// `OpenPipeWireRemote`: only once started; yields the node ids the
    /// returned remote may be used with.
    pub fn open_remote_nodes(&self) -> Result<Vec<u32>, SessionError> {
        self.expect(State::Started)?;
        Ok(self.streams.iter().map(|s| s.node_id).collect())
    }

    /// `Session.Close`, peer exit, or revocation. Idempotent. Returns the
    /// streams to tear down.
    pub fn close(&mut self) -> Vec<Stream> {
        self.state = State::Closed;
        self.start_in_flight = false;
        std::mem::take(&mut self.streams)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(n: &str) -> Source {
        Source::Output {
            name: n.into(),
            width: 1920,
            height: 1080,
        }
    }
    fn stream(n: &str, node: u32) -> Stream {
        Stream {
            node_id: node,
            source: out(n),
            grant: 9,
            position: Some((0, 0)),
            size: (1920, 1080),
        }
    }
    fn sess() -> Session {
        Session::new(Capabilities::ABYSS_TODAY)
    }

    #[test]
    fn happy_path() {
        let mut s = sess();
        s.select_sources(SelectOptions {
            cursor_mode: Some(CURSOR_EMBEDDED),
            persist_mode: Some(2),
            restore_token: Some("claimed".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.state(), State::SourcesSelected);
        s.begin_start().unwrap();
        let st = s
            .finish_start(vec![stream("DP-1", 42)], || "fresh".into())
            .unwrap();
        assert_eq!(s.state(), State::Started);
        assert_eq!(st.restore.as_ref().unwrap().token, "fresh");
        assert_eq!(st.persist, PersistMode::Persistent);
        assert_eq!(s.open_remote_nodes().unwrap(), vec![42]);
        assert_eq!(s.close().len(), 1);
        assert_eq!(s.state(), State::Closed);
        assert_eq!(s.open_remote_nodes(), Err(SessionError::Closed));
    }

    #[test]
    fn defaults() {
        let mut s = sess();
        let sel = s.select_sources(SelectOptions::default()).unwrap().clone();
        assert_eq!(sel.types, SOURCE_MONITOR);
        assert_eq!(sel.cursor, CursorMode::Hidden);
        assert_eq!(sel.persist, PersistMode::None);
        assert!(!sel.multiple);
    }

    #[test]
    fn order_enforced() {
        let mut s = sess();
        assert!(matches!(
            s.begin_start(),
            Err(SessionError::InvalidState { .. })
        ));
        assert!(matches!(
            s.open_remote_nodes(),
            Err(SessionError::InvalidState { .. })
        ));
        s.select_sources(SelectOptions::default()).unwrap();
        assert!(matches!(
            s.select_sources(SelectOptions::default()),
            Err(SessionError::InvalidState { .. })
        ));
    }

    #[test]
    fn rejects_unavailable_options() {
        let mut s = sess();
        for o in [
            SelectOptions {
                types: Some(SOURCE_WINDOW),
                ..Default::default()
            },
            SelectOptions {
                types: Some(0),
                ..Default::default()
            },
            SelectOptions {
                cursor_mode: Some(CURSOR_METADATA),
                ..Default::default()
            },
            SelectOptions {
                cursor_mode: Some(3),
                ..Default::default()
            },
            SelectOptions {
                persist_mode: Some(3),
                ..Default::default()
            },
        ] {
            assert!(matches!(
                s.select_sources(o),
                Err(SessionError::InvalidArgument(_))
            ));
            assert_eq!(s.state(), State::Created, "failed select must not advance");
        }
    }

    #[test]
    fn start_guards() {
        let mut s = sess();
        s.select_sources(SelectOptions::default()).unwrap();
        s.begin_start().unwrap();
        assert_eq!(s.begin_start().err(), Some(SessionError::StartInFlight));
        s.abort_start();
        s.begin_start().unwrap();
        // empty, too many, wrong type
        assert!(s.finish_start(vec![], String::new).is_err());
        assert!(s
            .finish_start(vec![stream("A", 1), stream("B", 2)], String::new)
            .is_err());
        let win = Stream {
            source: Source::Window {
                id: 1,
                title: "t".into(),
                app_id: "a".into(),
            },
            ..stream("A", 1)
        };
        assert!(s.finish_start(vec![win], String::new).is_err());
        assert_eq!(s.state(), State::SourcesSelected);
        // no persist -> no restore data
        let st = s
            .finish_start(vec![stream("A", 1)], || panic!("no mint"))
            .unwrap();
        assert!(st.restore.is_none());
    }

    #[test]
    fn close_is_idempotent_and_final() {
        let mut s = sess();
        s.close();
        s.close();
        assert_eq!(
            s.select_sources(SelectOptions::default()).err(),
            Some(SessionError::Closed)
        );
    }
}
