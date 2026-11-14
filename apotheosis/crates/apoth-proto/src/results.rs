// SPDX-License-Identifier: Apache-2.0
//! Typed reply results for the methods whose shape the spec names. Content-bearing
//! fields are `Option` and serialise as `null` when withheld (T-12 §4 `observe`).
//! Result structs are lenient about unknown fields so a newer server can add fields; the
//! request side is strict (T-12 §1).

use serde::{Deserialize, Serialize};

use crate::caps::Cap;

/// Reply to `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloReply {
    /// Protocol version in force.
    pub proto: u32,
    /// Server version string.
    pub version: String,
    /// Capability classes granted now (a subset of `want`, plus defaults).
    pub granted: Vec<Cap>,
    /// Feature names the server offers.
    pub features: Vec<String>,
}

/// Reply to `version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionReply {
    /// Current wire protocol version.
    pub proto: u32,
    /// Oldest wire protocol version still accepted.
    pub oldest: u32,
    /// Server version string.
    pub version: String,
}

/// Reply to `subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeReply {
    /// Id carried by this subscription's events.
    pub subscription: u64,
}

/// Reply to `launch`; `exit` is set only when `wait` was requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchReply {
    /// New pane id.
    pub pane: u64,
    /// Window id.
    pub window: u64,
    /// The child's exit code (negative for death by signal `-n`), with `wait`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
}

/// Reply to `pane.text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneTextReply {
    /// The text.
    pub text: String,
    /// True when the server cut it to stay within the line limit.
    #[serde(default)]
    pub truncated: bool,
}

/// Pane state in `tree`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PaneState {
    /// Child running.
    Running,
    /// Child exited (held open or pending close).
    Exited,
    /// The pane is in the credential state (T-17 §5).
    Credential,
}

/// The foreground process of a pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundProc {
    /// Process id.
    pub pid: u32,
    /// argv; `null` when withheld.
    pub argv: Option<Vec<String>>,
}

/// One pane in `tree` / `pane.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneNode {
    /// Opaque, stable, never reused within an instance.
    pub id: u64,
    /// Title; `null` when withheld.
    pub title: Option<String>,
    /// Working directory; `null` when withheld.
    pub cwd: Option<String>,
    /// Foreground process.
    pub foreground: Option<ForegroundProc>,
    /// Columns.
    pub cols: u32,
    /// Rows.
    pub rows: u32,
    /// State.
    pub state: PaneState,
}

/// One tab in `tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabNode {
    /// Id.
    pub id: u64,
    /// Title; `null` when withheld.
    pub title: Option<String>,
    /// Panes.
    pub panes: Vec<PaneNode>,
}

/// One window in `tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowNode {
    /// Id.
    pub id: u64,
    /// Tabs.
    pub tabs: Vec<TabNode>,
}

/// Reply to `tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeReply {
    /// Windows.
    pub windows: Vec<WindowNode>,
}

/// One process in `pane.processes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    /// Process id.
    pub pid: u32,
    /// Parent process id.
    pub ppid: u32,
    /// True for the foreground process group leader.
    pub foreground: bool,
    /// argv; `null` when withheld.
    pub argv: Option<Vec<String>>,
}

/// Reply to `pane.processes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessesReply {
    /// Processes in the pane's session.
    pub processes: Vec<ProcessInfo>,
}
