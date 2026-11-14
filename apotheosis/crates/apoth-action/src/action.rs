// SPDX-License-Identifier: Apache-2.0
//! `enum Action` and the registry generated from it (T-11 §1).
//!
//! One declaration list below produces: the enum (typed, parse-validated arguments), the
//! stable id, the static [`REGISTRY`] (id, title, doc, base classes, parameter specs) that
//! the palette, CLI help, bind editor and reference docs read, the typed parse from a
//! [`Call`], and the format back to a [`Call`]. The per-value capability class is
//! [`Action::classify`].
//!
//! Consent and sheet answers (allow once/always/deny, paste-guard confirm, askpass
//! submit, T-18 §6 sheets) are deliberately absent: they have no id and are not
//! representable as an `Action`.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::arg::{ArgType, Bytes, Dur, ParamKind};
use crate::argv::Argv;
use crate::class::{Class, ClassContext, Classification, NoContext, OverrideClass, Refusal};
use crate::enums::{
    Board, BroadcastMode, CaptureDest, CwdPolicy, Dir, LaunchType, LayoutVerb, Motion, Order,
    Scope, SelectUnit, Signal, Source, SplitDir, Urgency,
};
use crate::error::{ErrorKind, ParseError, suggest};
use crate::key::ChordList;
use crate::text::{Call, format_calls, parse_calls};

/// Longest variable value (T-11 §9: 1 KiB).
pub const MAX_VAR_VALUE_BYTES: usize = 1024;
/// Longest variable name.
pub const MAX_VAR_NAME_BYTES: usize = 128;
/// Longest object name (macro, layout, workspace, key table, bookmark, theme).
pub const MAX_NAME_BYTES: usize = 128;
/// Macro expansion depth cap (macros cannot recurse, so this is a backstop).
pub const MAX_MACRO_DEPTH: u8 = 8;

/// The closed family list (T-11 §1). `prompt` is R2 and has no members yet.
pub const FAMILIES: &[&str] = &[
    "input",
    "clipboard",
    "selection",
    "scroll",
    "search",
    "copy-mode",
    "key-table",
    "cursor",
    "hints",
    "prompt",
    "block",
    "mark",
    "bookmark",
    "palette",
    "interface",
    "pane",
    "tab",
    "window",
    "workspace",
    "layout",
    "broadcast",
    "keyboard",
    "font",
    "theme",
    "config",
    "var",
    "session",
    "scrollback",
    "capture",
    "pipe",
    "launch",
    "open",
    "coprocess",
    "macro",
    "notify",
    "media",
    "graphics",
    "debug",
    "app",
];

/// One parameter of an Action, as the registry exposes it.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    /// Name in the textual and KDL forms.
    pub name: &'static str,
    /// Value kind.
    pub kind: ParamKind,
    /// False when it may be omitted.
    pub required: bool,
    /// True when it may repeat.
    pub repeated: bool,
}

/// One registry row.
#[derive(Debug)]
pub struct ActionInfo {
    /// `family.verb` (or a bare closed id: `capture`, `pipe`, `launch`, `open`, `notify`).
    pub id: &'static str,
    /// Palette title.
    pub title: &'static str,
    /// One-line doc.
    pub doc: &'static str,
    /// Classes always required; [`Action::classify`] adds the argument-dependent ones.
    pub classes: &'static [Class],
    /// Parameters in declaration (positional) order.
    pub params: &'static [ParamSpec],
}

impl ActionInfo {
    /// The family: the part before the first `.`, or the whole id.
    pub fn family(&self) -> &'static str {
        self.id.split('.').next().unwrap_or(self.id)
    }

    /// True when [`Action::classify`] depends on argument values.
    pub fn is_argument_dependent(&self) -> bool {
        matches!(
            self.id,
            "config.set"
                | "config.reset"
                | "config.persist"
                | "macro.run"
                | "layout.apply"
                | "capture"
                | "launch"
                | "pipe"
                | "open"
                | "config.open"
                | "block.pipe-output"
                | "block.open-output"
                | "coprocess.run"
                | "pane.split"
                | "pane.float"
                | "tab.new"
                | "window.new"
        )
    }
}

/// True when `id` is a valid user-defined action id: `User.` then one or more dotted
/// segments of `[A-Za-z0-9_-]` (AUTOMATION-02). Registry ids never start with `User.`;
/// layered override by id is the config layer's job (`apoth-config`).
pub fn is_user_action_id(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("User.") else {
        return false;
    };
    id.len() <= MAX_NAME_BYTES
        && rest.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

/// Look an id up in the registry.
pub fn lookup(id: &str) -> Option<&'static ActionInfo> {
    REGISTRY.iter().find(|i| i.id == id)
}

/// Palette search: every whitespace-separated term must appear (case-insensitive) in the
/// id or title. Ids with the first term as a prefix sort first, then registry order.
pub fn search(query: &str) -> Vec<&'static ActionInfo> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits: Vec<&'static ActionInfo> = REGISTRY
        .iter()
        .filter(|i| {
            let hay = format!("{} {}", i.id, i.title.to_lowercase());
            terms.iter().all(|t| hay.contains(t.as_str()))
        })
        .collect();
    if let Some(first) = terms.first() {
        hits.sort_by_key(|i| !i.id.starts_with(first.as_str()));
    }
    hits
}

/// Typed-by-name argument values of one call, after positional resolution.
#[derive(Debug)]
struct Args {
    named: Vec<(&'static str, Vec<String>)>,
}

impl Args {
    fn get(&self, name: &str) -> &[String] {
        self.named
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }
}

fn field<T: ArgType>(args: &Args, name: &str) -> Result<T, ParseError> {
    let vals = args.get(name);
    T::from_values(vals).map_err(|m| {
        let kind = if vals.is_empty() {
            ErrorKind::MissingParam
        } else {
            ErrorKind::BadValue
        };
        ParseError::new(kind, format!("argument `{name}`: {m}"))
    })
}

fn resolve(info: &'static ActionInfo, call: &Call) -> Result<Args, ParseError> {
    let n = info.params.len();
    let mut vals: Vec<Vec<String>> = vec![Vec::new(); n];
    let mut given = vec![false; n];
    for (k, v) in &call.named {
        let Some(i) = info.params.iter().position(|p| p.name == k.as_str()) else {
            return Err(ParseError::new(
                ErrorKind::UnknownParam,
                format!("`{}` has no parameter `{k}`", info.id),
            )
            .with_suggestion(suggest(k, info.params.iter().map(|p| p.name))));
        };
        let spec = &info.params[i];
        if spec.kind == ParamKind::Argv {
            return Err(ParseError::new(
                ErrorKind::UnknownParam,
                format!("`{k}` is given after `--`, not by name"),
            ));
        }
        if given[i] && !spec.repeated {
            return Err(ParseError::new(
                ErrorKind::UnknownParam,
                format!("parameter `{k}` given more than once"),
            ));
        }
        given[i] = true;
        vals[i].push(v.clone());
    }
    let mut pos = call.positional.iter();
    for (i, spec) in info.params.iter().enumerate() {
        if given[i] || spec.repeated || spec.kind == ParamKind::Argv {
            continue;
        }
        match pos.next() {
            Some(v) => {
                vals[i].push(v.clone());
                given[i] = true;
            }
            None => break,
        }
    }
    if pos.next().is_some() {
        return Err(ParseError::new(
            ErrorKind::UnknownParam,
            format!("too many positional arguments for `{}`", info.id),
        ));
    }
    if let Some(av) = &call.argv {
        match info.params.iter().position(|p| p.kind == ParamKind::Argv) {
            Some(i) => vals[i] = av.clone(),
            None => {
                return Err(ParseError::new(
                    ErrorKind::UnknownParam,
                    format!("`{}` takes no argv", info.id),
                ));
            }
        }
    }
    Ok(Args {
        named: info.params.iter().map(|p| p.name).zip(vals).collect(),
    })
}

macro_rules! pname {
    ($f:ident) => {
        stringify!($f)
    };
    ($f:ident, $pn:literal) => {
        $pn
    };
}

macro_rules! define_actions {
    ($(
        $var:ident = $id:literal [$($cls:ident),*] $title:literal $doc:literal
        { $($f:ident $(($pn:literal))? : $t:ty),* $(,)? } ;
    )*) => {
        /// Every typed way to make something happen. See the module docs.
        #[derive(Debug, Clone, PartialEq)]
        #[allow(clippy::large_enum_variant)]
        pub enum Action {
            $(
                #[doc = $doc]
                $var { $($f: $t),* },
            )*
        }

        /// The registry: one row per Action variant, generated from the enum list.
        pub static REGISTRY: &[ActionInfo] = &[
            $(
                ActionInfo {
                    id: $id,
                    title: $title,
                    doc: $doc,
                    classes: &[$(Class::$cls),*],
                    params: &[
                        $(
                            ParamSpec {
                                name: pname!($f $(, $pn)?),
                                kind: <$t as ArgType>::KIND,
                                required: <$t as ArgType>::REQUIRED,
                                repeated: <$t as ArgType>::REPEATED,
                            }
                        ),*
                    ],
                },
            )*
        ];

        impl Action {
            /// The stable registry id.
            pub fn id(&self) -> &'static str {
                match self {
                    $( Action::$var { .. } => $id, )*
                }
            }

            fn build(info: &'static ActionInfo, args: &Args) -> Result<Action, ParseError> {
                match info.id {
                    $(
                        $id => Ok(Action::$var {
                            $( $f: field::<$t>(args, pname!($f $(, $pn)?))? ),*
                        }),
                    )*
                    _ => Err(ParseError::new(ErrorKind::UnknownAction, "unknown action")),
                }
            }

            fn to_fields(&self) -> Vec<(&'static str, Vec<String>)> {
                match self {
                    $(
                        Action::$var { $($f),* } => vec![
                            $( (pname!($f $(, $pn)?), <$t as ArgType>::to_values($f)) ),*
                        ],
                    )*
                }
            }
        }
    };
}

define_actions! {
    // ---- input.* -------------------------------------------------------------------
    InputSendText = "input.send-text" [Input] "Send text"
        "Send text to the program as typed input."
        { text: String };
    InputSendBytes = "input.send-bytes" [Input] "Send raw bytes"
        "Send raw bytes (hex) to the program, bypassing the key encoder."
        { bytes: Bytes };
    InputSendKeys = "input.send-keys" [Input] "Send keys"
        "Encode key chords through the key encoder, respecting current modes; never fires binds."
        { keys: ChordList };
    InputSendCsi = "input.send-csi" [Input] "Send CSI sequence"
        "Send ESC [ followed by the given body."
        { body: String };
    InputSendEsc = "input.send-esc" [Input] "Send ESC sequence"
        "Send ESC followed by the given body."
        { body: String };
    InputPaste = "input.paste" [Input, Clipboard] "Paste"
        "Paste the given text, or the clipboard, through the paste guard."
        { text: Option<String> };
    InputPasteSelection = "input.paste-selection" [Input, Clipboard] "Paste primary selection"
        "Paste the primary selection through the paste guard."
        { };

    // ---- clipboard.* ---------------------------------------------------------------
    ClipboardCopy = "clipboard.copy" [Clipboard] "Copy text to clipboard"
        "Put the given text on the clipboard."
        { text: String };
    ClipboardPasteTo = "clipboard.paste-to" [Clipboard] "Paste clipboard to pane"
        "Write the clipboard into the target pane (T-11 §1: class `clipboard`)."
        { board: Option<Board> };
    ClipboardClear = "clipboard.clear" [Clipboard] "Clear clipboard"
        "Empty the clipboard."
        { board: Option<Board> };

    // ---- selection.* ---------------------------------------------------------------
    SelectionSelect = "selection.select" [ReadContent] "Select"
        "Select the unit under the cursor."
        { unit: SelectUnit };
    SelectionSelectAll = "selection.select-all" [ReadContent] "Select all"
        "Select the whole screen and scrollback."
        { };
    SelectionExtend = "selection.extend" [ReadContent] "Extend selection"
        "Extend the selection in a direction."
        { dir: Dir, unit: Option<SelectUnit> };
    SelectionClear = "selection.clear" [ReadContent] "Clear selection"
        "Drop the current selection."
        { };
    SelectionCopy = "selection.copy" [ReadContent, Clipboard] "Copy selection"
        "Copy the selection to the clipboard."
        { board: Option<Board> };

    // ---- scroll.* ------------------------------------------------------------------
    ScrollPageUp = "scroll.page-up" [Layout] "Scroll page up" "Scroll back one page." { };
    ScrollPageDown = "scroll.page-down" [Layout] "Scroll page down" "Scroll forward one page." { };
    ScrollHalfPageUp = "scroll.half-page-up" [Layout] "Scroll half page up"
        "Scroll back half a page." { };
    ScrollHalfPageDown = "scroll.half-page-down" [Layout] "Scroll half page down"
        "Scroll forward half a page." { };
    ScrollLineUp = "scroll.line-up" [Layout] "Scroll line up"
        "Scroll back by lines (default 1)."
        { lines: Option<u32> };
    ScrollLineDown = "scroll.line-down" [Layout] "Scroll line down"
        "Scroll forward by lines (default 1)."
        { lines: Option<u32> };
    ScrollTop = "scroll.top" [Layout] "Scroll to top" "Jump to the oldest scrollback line." { };
    ScrollBottom = "scroll.bottom" [Layout] "Scroll to bottom" "Jump to the live screen." { };

    // ---- search.* ------------------------------------------------------------------
    SearchStart = "search.start" [Layout] "Search"
        "Open search, optionally with an initial query."
        { query: Option<String> };
    SearchNext = "search.next" [Layout] "Next match" "Jump to the next search match." { };
    SearchPrevious = "search.previous" [Layout] "Previous match"
        "Jump to the previous search match." { };
    SearchCancel = "search.cancel" [Layout] "Cancel search" "Close search." { };
    SearchSet = "search.set" [Layout] "Set search query" "Replace the search query."
        { query: String };
    SearchToggleCase = "search.toggle-case" [Layout] "Toggle case sensitivity"
        "Toggle case-sensitive search." { };
    SearchToggleRegex = "search.toggle-regex" [Layout] "Toggle regex search"
        "Toggle regular-expression search." { };

    // ---- copy-mode.* ---------------------------------------------------------------
    CopyModeEnter = "copy-mode.enter" [Layout] "Enter copy mode"
        "Start keyboard navigation of the scrollback." { };
    CopyModeExit = "copy-mode.exit" [Layout] "Exit copy mode" "Leave copy mode." { };
    CopyModeToggle = "copy-mode.toggle" [Layout] "Toggle copy mode"
        "Enter or leave copy mode." { };
    CopyModeMove = "copy-mode.move" [Layout] "Copy-mode move"
        "Move the copy-mode cursor."
        { motion: Motion, count: Option<u32> };
    CopyModeToggleSelection = "copy-mode.toggle-selection" [Layout] "Copy-mode toggle selection"
        "Start or end selecting from the copy-mode cursor."
        { unit: Option<SelectUnit> };

    // ---- key-table.* ---------------------------------------------------------------
    KeyTablePush = "key-table.push" [Layout] "Push key table"
        "Activate a named key table, optionally one-shot and with a timeout."
        { name: String, one_shot("one-shot"): bool, timeout: Option<Dur> };
    KeyTablePop = "key-table.pop" [Layout] "Pop key table" "Deactivate the top key table." { };
    KeyTableClear = "key-table.clear" [Layout] "Clear key tables"
        "Deactivate every key table." { };

    // ---- cursor.* ------------------------------------------------------------------
    CursorJumpChar = "cursor.jump-char" [Layout] "Jump to character"
        "Move the copy-mode cursor to a character on the line; with none, the next typed key."
        { ch("char"): Option<char>, backward: bool, till: bool };
    CursorJumpRepeat = "cursor.jump-repeat" [Layout] "Repeat jump"
        "Repeat the last jump-char, optionally reversed."
        { reverse: bool };

    // ---- hints.* -------------------------------------------------------------------
    HintsStart = "hints.start" [Layout] "Start hints"
        "Label matches on screen; the matcher's action is checked under its own class."
        { hint: Option<String> };
    HintsCancel = "hints.cancel" [Layout] "Cancel hints" "Dismiss hint labels." { };

    // ---- block.* -------------------------------------------------------------------
    BlockJump = "block.jump" [Layout] "Jump to command block"
        "Jump to the previous or next command block."
        { direction: Order };
    BlockSelectOutput = "block.select-output" [Layout] "Select command output"
        "Select the output of the current command block." { };
    BlockCopyOutput = "block.copy-output" [ReadContent, Clipboard] "Copy command output"
        "Copy the output of a command block to the clipboard." { };
    BlockCopyCommand = "block.copy-command" [ReadContent, Clipboard] "Copy command"
        "Copy the command line of a command block to the clipboard." { };
    BlockPipeOutput = "block.pipe-output" [Spawn, ReadContent] "Pipe command output"
        "Pipe the output of a command block to a program's stdin."
        { argv: Argv };
    BlockOpenOutput = "block.open-output" [Spawn, ReadContent] "Open command output"
        "Open the output of a command block in a viewer (default viewer if no argv)."
        { argv: Option<Argv> };

    // ---- mark / bookmark / palette / interface -------------------------------------
    MarkSet = "mark.set" [Layout] "Set mark" "Mark the cursor line, optionally named."
        { name: Option<String> };
    MarkClear = "mark.clear" [Layout] "Clear marks" "Remove marks in the pane." { };
    MarkJump = "mark.jump" [Layout] "Jump to mark" "Jump to the previous or next mark."
        { direction: Order };
    BookmarkAdd = "bookmark.add" [Layout] "Add bookmark" "Bookmark the current position."
        { name: String };
    BookmarkRemove = "bookmark.remove" [Layout] "Remove bookmark" "Remove a bookmark."
        { name: String };
    BookmarkJump = "bookmark.jump" [Layout] "Jump to bookmark" "Jump to a bookmark."
        { name: String };
    PaletteToggle = "palette.toggle" [Layout] "Toggle command palette"
        "Open or close the command palette." { };
    PaletteOpen = "palette.open" [Layout] "Open command palette"
        "Open the palette, optionally with a query."
        { query: Option<String> };
    PaletteClose = "palette.close" [Layout] "Close command palette"
        "Close the palette." { };
    InterfaceFocusNext = "interface.focus-next" [Layout] "Focus next interface element"
        "Move interface focus forward." { };
    InterfaceFocusPrev = "interface.focus-prev" [Layout] "Focus previous interface element"
        "Move interface focus back." { };
    InterfaceToggleTabBar = "interface.toggle-tab-bar" [Layout] "Toggle tab bar"
        "Show or hide the tab bar." { };
    InterfaceToggleScrollbar = "interface.toggle-scrollbar" [Layout] "Toggle scrollbar"
        "Show or hide the scrollbar." { };
    InterfaceToggleStatusBar = "interface.toggle-status-bar" [Layout] "Toggle status bar"
        "Show or hide the status bar." { };

    // ---- pane.* --------------------------------------------------------------------
    PaneSplit = "pane.split" [Layout] "Split pane"
        "Split the pane; spawn-class when a command argv is given."
        { direction: SplitDir, argv: Option<Argv>, cwd: Option<String>, profile: Option<String> };
    PaneClose = "pane.close" [Layout] "Close pane"
        "Close the pane; `force` skips the running-process confirmation."
        { force: bool };
    PaneFocus = "pane.focus" [Layout] "Focus pane" "Focus the neighbouring pane."
        { direction: Dir };
    PaneFocusNext = "pane.focus-next" [Layout] "Focus next pane" "Focus the next pane." { };
    PaneFocusPrev = "pane.focus-prev" [Layout] "Focus previous pane"
        "Focus the previous pane." { };
    PaneFocusLast = "pane.focus-last" [Layout] "Focus last pane"
        "Focus the previously focused pane." { };
    PaneMove = "pane.move" [Layout] "Move pane" "Swap the pane with its neighbour."
        { direction: Dir };
    PaneResize = "pane.resize" [Layout] "Resize pane" "Move a pane edge by cells."
        { direction: Dir, amount: Option<u32> };
    PaneToggleZoom = "pane.toggle-zoom" [Layout] "Toggle pane zoom"
        "Zoom the pane to fill the tab, or restore." { };
    PaneEqualize = "pane.equalize" [Layout] "Equalize panes"
        "Make all splits in the tab equal." { };
    PaneBreakOut = "pane.break-out" [Layout] "Break pane out"
        "Move the pane into its own tab." { };
    PaneFloat = "pane.float" [Layout] "New floating pane"
        "Open a floating pane over the tab; spawn-class when a command argv is given."
        { argv: Option<Argv>, cwd: Option<String>, profile: Option<String> };
    PaneToggleFloat = "pane.toggle-float" [Layout] "Float or embed pane"
        "Move the pane between the tiled and the floating layer." { };
    PaneToggleFloats = "pane.toggle-floats" [Layout] "Show or hide floating panes"
        "Show or hide the tab's floating layer." { };
    PaneTogglePin = "pane.toggle-pin" [Layout] "Pin floating pane"
        "Keep the floating pane above unpinned floats." { };
    PaneSetTitle = "pane.set-title" [Layout] "Set pane title"
        "Set or clear (no argument) a pinned pane title."
        { title: Option<String> };
    PaneToggleReadonly = "pane.toggle-readonly" [Layout] "Toggle read-only"
        "Toggle input being discarded for the pane." { };
    PaneReset = "pane.reset" [Layout] "Reset terminal" "Full terminal reset (RIS)." { };
    PaneClearScreen = "pane.clear-screen" [Layout] "Clear screen"
        "Clear the visible screen without touching scrollback." { };
    PaneSignalChild = "pane.signal-child" [Input] "Signal foreground process"
        "Send a signal to the foreground process group (input-class: it acts on the program)."
        { signal: Signal };

    // ---- tab.* ---------------------------------------------------------------------
    TabNew = "tab.new" [Layout] "New tab"
        "Open a tab; spawn-class when a command argv is given."
        { argv: Option<Argv>, cwd: Option<String>, profile: Option<String> };
    TabClose = "tab.close" [Layout] "Close tab" "Close the tab." { };
    TabCloseOthers = "tab.close-others" [Layout] "Close other tabs"
        "Close every tab but this one." { };
    TabNext = "tab.next" [Layout] "Next tab" "Select the next tab." { };
    TabPrev = "tab.prev" [Layout] "Previous tab" "Select the previous tab." { };
    TabLast = "tab.last" [Layout] "Last tab" "Select the previously selected tab." { };
    TabSelect = "tab.select" [Layout] "Select tab" "Select a tab by 1-based index."
        { index: u32 };
    TabMove = "tab.move" [Layout] "Move tab" "Move the tab by a signed offset."
        { delta: i32 };
    TabRename = "tab.rename" [Layout] "Rename tab" "Set or clear (no argument) a pinned tab title."
        { title: Option<String> };
    TabDetach = "tab.detach" [Layout] "Detach tab" "Move the tab to a new window." { };
    TabReopenClosed = "tab.reopen-closed" [Layout] "Reopen closed tab"
        "Restore the most recently closed tab's layout (processes are not revived)." { };

    // ---- window.* ------------------------------------------------------------------
    WindowNew = "window.new" [Layout] "New window"
        "Open a window; spawn-class when a command argv is given."
        { argv: Option<Argv>, cwd: Option<String>, profile: Option<String> };
    WindowClose = "window.close" [Layout] "Close window" "Close the window." { };
    WindowToggleFullscreen = "window.toggle-fullscreen" [Layout] "Toggle fullscreen"
        "Enter or leave fullscreen." { };
    WindowToggleMaximize = "window.toggle-maximize" [Layout] "Toggle maximize"
        "Maximize or restore." { };
    WindowToggleDecorations = "window.toggle-decorations" [Layout] "Toggle decorations"
        "Show or hide window decorations." { };
    WindowMinimize = "window.minimize" [Layout] "Minimize window" "Minimize the window." { };
    WindowSetTitle = "window.set-title" [Layout] "Set window title"
        "Set or clear (no argument) a pinned window title."
        { title: Option<String> };
    WindowToggleQuickTerminal = "window.toggle-quick-terminal" [Layout] "Toggle quick terminal"
        "Show or hide the quick terminal." { };

    // ---- workspace.* ---------------------------------------------------------------
    WorkspaceSwitch = "workspace.switch" [Layout] "Switch workspace"
        "Switch to a workspace by name."
        { name: String };
    WorkspaceNext = "workspace.next" [Layout] "Next workspace" "Switch to the next workspace." { };
    WorkspacePrev = "workspace.prev" [Layout] "Previous workspace"
        "Switch to the previous workspace." { };
    WorkspaceMoveWindow = "workspace.move-window" [Layout] "Move window to workspace"
        "Move the window to a workspace."
        { name: String };

    // ---- layout.* ------------------------------------------------------------------
    LayoutApply = "layout.apply" [Layout] "Apply layout"
        "Apply a named layout; spawn-class when the layout contains any argv."
        { name: String };
    LayoutSave = "layout.save" [Layout] "Save layout" "Save the live tree as a named layout."
        { name: String };
    LayoutNext = "layout.next" [Layout] "Next layout" "Cycle to the next layout engine." { };
    LayoutPrev = "layout.prev" [Layout] "Previous layout"
        "Cycle to the previous layout engine." { };
    LayoutAdjust = "layout.adjust" [Layout] "Adjust layout"
        "Per-layout verb: resize, mirror, rotate, equalize, maximize, bias-cycle."
        { verb: LayoutVerb, amount: Option<i32> };

    // ---- broadcast.* / keyboard.* --------------------------------------------------
    BroadcastSet = "broadcast.set" [Input] "Set input broadcast"
        "Broadcast typed input to the tab, window, all panes, or off."
        { mode: BroadcastMode };
    BroadcastToggle = "broadcast.toggle" [Input] "Toggle input broadcast"
        "Toggle broadcasting to the tab." { };
    KeyboardToggleInhibit = "keyboard.toggle-inhibit" [Input] "Toggle shortcut inhibit"
        "Let the focused program receive every key, binds suspended." { };

    // ---- font.* / theme.* ----------------------------------------------------------
    FontSizeIncrease = "font.size-increase" [Appearance] "Increase font size"
        "Grow the font by points (default 1)."
        { amount: Option<f32> };
    FontSizeDecrease = "font.size-decrease" [Appearance] "Decrease font size"
        "Shrink the font by points (default 1)."
        { amount: Option<f32> };
    FontSizeReset = "font.size-reset" [Appearance] "Reset font size"
        "Restore the configured font size." { };
    FontSizeSet = "font.size-set" [Appearance] "Set font size" "Set the font size in points."
        { size: f32 };
    FontFamilySet = "font.family-set" [Appearance] "Set font family"
        "Set the primary font family."
        { family: String };
    FontLigaturesSet = "font.ligatures-set" [Appearance] "Set ligatures"
        "Enable or disable ligatures; with no argument, toggle. `scope` limits the change."
        { enabled: Option<bool>, scope: Option<Scope> };
    FontFeatureToggle = "font.feature-toggle" [Appearance] "Toggle font feature"
        "Toggle an OpenType feature tag, e.g. ss01."
        { feature: String };
    FontReload = "font.reload" [Appearance] "Reload fonts" "Re-resolve fonts." { };
    FontClearCaches = "font.clear-caches" [Appearance] "Clear font caches"
        "Drop glyph and font caches." { };
    FontBlendingSet = "font.blending-set" [Appearance] "Set text blending"
        "Set the text blending mode."
        { mode: String };
    ThemeSet = "theme.set" [Appearance] "Set theme" "Switch to a named theme."
        { name: String };
    ThemeToggleVariant = "theme.toggle-variant" [Appearance] "Toggle light/dark"
        "Switch between the light and dark variant." { };
    ThemeReload = "theme.reload" [Appearance] "Reload theme" "Re-read the theme files." { };

    // ---- config.* ------------------------------------------------------------------
    ConfigSet = "config.set" [] "Set config key (runtime)"
        "Runtime override of a key; class follows the key's override class."
        { key: String, value: String };
    ConfigReset = "config.reset" [] "Reset config key (runtime)"
        "Remove the runtime override of a key; class follows the key's override class."
        { key: String };
    ConfigReload = "config.reload" [Appearance] "Reload config" "Re-read the configuration." { };
    ConfigOpen = "config.open" [Spawn] "Open config in editor"
        "Open the config file in an editor (argv with {file}; default editor if none)."
        { argv: Option<Argv> };
    ConfigSaveOverride = "config.save-override" [ConfigWrite] "Save overrides to config"
        "Write the runtime overrides of a scope into the config file."
        { scope: Option<Scope> };
    ConfigPersist = "config.persist" [ConfigWrite] "Persist config key"
        "Write a key's value into the config file."
        { key: String, value: String };

    // ---- var.* ---------------------------------------------------------------------
    VarSet = "var.set" [Layout] "Set variable"
        "Set a scoped variable (name up to 128 bytes, value up to 1 KiB)."
        { name: String, value: String, scope: Option<Scope> };
    VarUnset = "var.unset" [Layout] "Unset variable" "Remove a scoped variable."
        { name: String, scope: Option<Scope> };
    VarGet = "var.get" [ReadContent] "Get variable"
        "Read a scoped variable (the reply may carry program-set text)."
        { name: String, scope: Option<Scope> };

    // ---- session.* / scrollback.* --------------------------------------------------
    SessionSave = "session.save" [Layout] "Save session" "Save the session layout."
        { name: Option<String> };
    SessionRestore = "session.restore" [Spawn] "Restore session"
        "Restore a saved session, respawning its commands suspended."
        { name: Option<String> };
    ScrollbackClear = "scrollback.clear" [Layout] "Clear scrollback" "Discard the scrollback." { };
    ScrollbackSave = "scrollback.save" [ReadContent, Spawn] "Save scrollback"
        "Write the scrollback to a file."
        { path: String };

    // ---- bare closed ids -----------------------------------------------------------
    Capture = "capture" [ReadContent] "Capture text"
        "Send pane text to the clipboard, a file, a pager, an editor or a command argv."
        { source: Source, dest: CaptureDest, path: Option<String>, argv: Option<Argv> };
    Pipe = "pipe" [Spawn, ReadContent] "Pipe text to a command"
        "Write pane text to the stdin of an argv command."
        { source: Source, argv: Argv };
    Launch = "launch" [Spawn] "Launch"
        "Create a tab, window, split, overlay, background process or clipboard writer."
        {
            kind("type"): LaunchType,
            cwd: Option<CwdPolicy>,
            workdir: Option<String>,
            env: Vec<String>,
            stdin: Option<Source>,
            hold: bool,
            location: Option<SplitDir>,
            bias: Option<u32>,
            profile: Option<String>,
            title: Option<String>,
            argv: Option<Argv>
        };
    Open = "open" [Spawn] "Open"
        "Open a file or URL with an argv opener (default opener if none)."
        { target: String, argv: Option<Argv> };
    CoprocessRun = "coprocess.run" [Spawn, ReadContent, Input] "Run coprocess"
        "Run a command connected to the pane: its output is typed in, pane text is fed to it."
        { argv: Argv, source: Option<Source> };
    MacroRun = "macro.run" [] "Run macro"
        "Run a named macro; class is the union of its members' classes."
        { name: String };
    Notify = "notify" [Layout] "Notify" "Show a desktop notification."
        { title: String, body: Option<String>, urgency: Option<Urgency> };

    // ---- media.* / graphics.* ------------------------------------------------------
    MediaPlay = "media.play" [Layout] "Play media" "Resume playback." { };
    MediaPause = "media.pause" [Layout] "Pause media" "Pause playback." { };
    MediaTogglePlay = "media.toggle-play" [Layout] "Toggle media playback"
        "Play or pause." { };
    MediaSeek = "media.seek" [Layout] "Seek media" "Seek by seconds, or to a position."
        { delta: f32, absolute: bool };
    MediaRate = "media.rate" [Layout] "Set media rate" "Set the playback rate."
        { rate: f32 };
    MediaVolume = "media.volume" [Layout] "Set media volume" "Set volume from 0 to 1."
        { level: f32 };
    MediaMute = "media.mute" [Layout] "Mute media" "Mute, unmute, or (no argument) toggle."
        { muted: Option<bool> };
    MediaHide = "media.hide" [Layout] "Hide media" "Hide the media surface." { };
    MediaDelete = "media.delete" [Layout] "Delete media" "Remove the media surface." { };
    MediaSave = "media.save" [ReadContent, Spawn] "Save media frame"
        "Write the current media frame or stream to a file."
        { path: String };
    GraphicsHide = "graphics.hide" [Layout] "Hide graphics" "Hide inline images." { };
    GraphicsDelete = "graphics.delete" [Layout] "Delete graphics"
        "Delete one inline image, or all."
        { id: Option<u32> };
    GraphicsSave = "graphics.save" [ReadContent, Spawn] "Save graphic"
        "Write an inline image to a file."
        { path: String, id: Option<u32> };

    // ---- debug.* / app.* -----------------------------------------------------------
    DebugToggleInspector = "debug.toggle-inspector" [ReadContent] "Toggle inspector"
        "Show or hide the terminal inspector." { };
    DebugToggleKeyOverlay = "debug.toggle-key-overlay" [ReadContent] "Toggle key debug overlay"
        "Show or hide the key-event overlay (display only; key events never leave)." { };
    AppQuit = "app.quit" [Layout] "Quit" "Quit; `force` skips the running-process confirmation."
        { force: bool };
    AppNewInstance = "app.new-instance" [Layout] "New instance" "Start a new instance." { };
    AppIgnore = "app.ignore" [Layout] "Ignore key"
        "Consume the key and do nothing (the `ignore` action)." { };
}

fn invalid(msg: impl Into<String>) -> ParseError {
    ParseError::new(ErrorKind::Invalid, msg)
}

fn check_name(what: &str, n: &str) -> Result<(), ParseError> {
    if n.is_empty() || n.len() > MAX_NAME_BYTES || n.chars().any(char::is_control) {
        return Err(invalid(format!(
            "{what} name must be 1..={MAX_NAME_BYTES} bytes without control characters"
        )));
    }
    Ok(())
}

fn check_var_name(n: &str) -> Result<(), ParseError> {
    if n.is_empty()
        || n.len() > MAX_VAR_NAME_BYTES
        || !n
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(invalid(format!(
            "variable name must be 1..={MAX_VAR_NAME_BYTES} bytes of [A-Za-z0-9_.-]"
        )));
    }
    Ok(())
}

fn check_config_key(k: &str) -> Result<(), ParseError> {
    let ok = !k.is_empty()
        && k.len() <= MAX_NAME_BYTES
        && k.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "config key must be dotted kebab-case, e.g. `font.size`",
        ))
    }
}

fn check_sequence_body(body: &str) -> Result<(), ParseError> {
    if body.is_empty() || body.len() > 256 || !body.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(invalid("body must be 1..=256 printable ASCII bytes"));
    }
    Ok(())
}

impl Action {
    /// Registry row of this variant.
    pub fn info(&self) -> &'static ActionInfo {
        lookup(self.id()).unwrap_or(&REGISTRY[0])
    }

    /// Capability requirement of this value, with the argument-dependent parts resolved
    /// through `ctx` (T-11 §1, §4; T-12 §4). Unknown facts classify as strictly as possible.
    pub fn classify(&self, ctx: &dyn ClassContext) -> Classification {
        self.classify_at(ctx, 0, &mut HashMap::new())
    }

    /// [`Action::classify`] with no external knowledge.
    pub fn classify_closed(&self) -> Classification {
        self.classify_at(&NoContext, 0, &mut HashMap::new())
    }

    fn classify_at(
        &self,
        ctx: &dyn ClassContext,
        depth: u8,
        memo: &mut HashMap<(String, u8), Classification>,
    ) -> Classification {
        let mut c = Classification::of(self.info().classes);
        fn argv_classes(c: &mut Classification, argv: &Argv) {
            if argv.placeholders().any(|p| p.carries_content()) {
                c.add(Class::ReadContent);
            }
        }
        fn opt_argv_spawn(c: &mut Classification, argv: &Option<Argv>) {
            if let Some(a) = argv {
                c.add(Class::Spawn);
                argv_classes(c, a);
            }
        }
        match self {
            Action::PaneSplit { argv, .. }
            | Action::PaneFloat { argv, .. }
            | Action::TabNew { argv, .. }
            | Action::WindowNew { argv, .. } => opt_argv_spawn(&mut c, argv),
            Action::LayoutApply { name } => {
                if ctx.layout_has_argv(name) != Some(false) {
                    c.add(Class::Spawn);
                }
            }
            Action::ConfigSet { key, .. } | Action::ConfigReset { key, .. } => {
                match ctx.override_class(key).unwrap_or(OverrideClass::Grant) {
                    OverrideClass::Appearance => c.add(Class::Appearance),
                    OverrideClass::Behaviour => c.add(Class::ConfigWrite),
                    OverrideClass::Security => {
                        c.add(Class::ConfigWrite);
                        c.refuse(Refusal::RuntimeSecurityKey);
                    }
                    OverrideClass::Grant => {
                        c.add(Class::ConfigWrite);
                        c.refuse(Refusal::GrantKey);
                    }
                }
            }
            Action::ConfigPersist { key, .. } => {
                if ctx.override_class(key).unwrap_or(OverrideClass::Grant) == OverrideClass::Grant {
                    c.refuse(Refusal::GrantKey);
                }
            }
            Action::ConfigOpen { argv }
            | Action::Open { argv, .. }
            | Action::BlockOpenOutput { argv } => {
                if let Some(a) = argv {
                    argv_classes(&mut c, a);
                }
            }
            Action::BlockPipeOutput { argv }
            | Action::Pipe { argv, .. }
            | Action::CoprocessRun { argv, .. } => argv_classes(&mut c, argv),
            Action::Capture { dest, argv, .. } => {
                if *dest == CaptureDest::Clipboard {
                    c.add(Class::Clipboard);
                } else {
                    c.add(Class::Spawn);
                }
                if let Some(a) = argv {
                    argv_classes(&mut c, a);
                }
            }
            Action::Launch { stdin, argv, .. } => {
                if stdin.is_some() {
                    c.add(Class::ReadContent);
                }
                if let Some(a) = argv {
                    argv_classes(&mut c, a);
                }
            }
            Action::MacroRun { name } => match ctx.macro_members(name) {
                None => c.refuse(Refusal::UnknownMacro),
                Some(_) if depth >= MAX_MACRO_DEPTH => c.refuse(Refusal::MacroDepth),
                Some(members) => {
                    // Memoised per (name, depth): a DAG of macros is expanded once per
                    // level instead of once per path (no exponential blow-up).
                    let key = (name.clone(), depth);
                    if let Some(hit) = memo.get(&key) {
                        c.merge(hit.clone());
                    } else {
                        let mut sub = Classification::default();
                        for m in members {
                            sub.merge(m.classify_at(ctx, depth + 1, memo));
                        }
                        memo.insert(key, sub.clone());
                        c.merge(sub);
                    }
                }
            },
            _ => {}
        }
        c
    }

    /// Cross-field checks and limits that a type alone cannot express.
    pub fn validate(&self) -> Result<(), ParseError> {
        match self {
            Action::VarSet { name, value, .. } => {
                check_var_name(name)?;
                if value.len() > MAX_VAR_VALUE_BYTES {
                    return Err(invalid(format!(
                        "variable value is {} bytes, limit is {MAX_VAR_VALUE_BYTES}",
                        value.len()
                    )));
                }
                Ok(())
            }
            Action::VarUnset { name, .. } | Action::VarGet { name, .. } => check_var_name(name),
            Action::ConfigSet { key, .. }
            | Action::ConfigReset { key, .. }
            | Action::ConfigPersist { key, .. } => check_config_key(key),
            Action::KeyTablePush { name, .. } => check_name("key table", name),
            Action::MacroRun { name } => check_name("macro", name),
            Action::WorkspaceSwitch { name } | Action::WorkspaceMoveWindow { name } => {
                check_name("workspace", name)
            }
            Action::LayoutApply { name } | Action::LayoutSave { name } => {
                check_name("layout", name)
            }
            Action::BookmarkAdd { name }
            | Action::BookmarkRemove { name }
            | Action::BookmarkJump { name } => check_name("bookmark", name),
            Action::ThemeSet { name } => check_name("theme", name),
            Action::InputSendCsi { body } | Action::InputSendEsc { body } => {
                check_sequence_body(body)
            }
            Action::ScrollLineUp { lines } | Action::ScrollLineDown { lines } => {
                if *lines == Some(0) {
                    return Err(invalid("lines must be at least 1"));
                }
                Ok(())
            }
            Action::CopyModeMove { count, .. } => {
                if *count == Some(0) {
                    return Err(invalid("count must be at least 1"));
                }
                Ok(())
            }
            Action::PaneResize { amount, .. } => {
                if *amount == Some(0) {
                    return Err(invalid("amount must be at least 1"));
                }
                Ok(())
            }
            Action::TabSelect { index } => {
                if *index == 0 {
                    return Err(invalid("tab index is 1-based"));
                }
                Ok(())
            }
            Action::FontSizeSet { size } => {
                if !(*size > 0.0 && *size <= 1000.0) {
                    return Err(invalid("font size must be in (0, 1000]"));
                }
                Ok(())
            }
            Action::MediaVolume { level } => {
                if !(0.0..=1.0).contains(level) {
                    return Err(invalid("volume must be in 0..=1"));
                }
                Ok(())
            }
            Action::MediaRate { rate } => {
                if !(*rate > 0.0 && *rate <= 16.0) {
                    return Err(invalid("rate must be in (0, 16]"));
                }
                Ok(())
            }
            Action::Capture {
                dest, path, argv, ..
            } => match dest {
                CaptureDest::Clipboard if path.is_some() || argv.is_some() => {
                    Err(invalid("capture to clipboard takes no path or argv"))
                }
                CaptureDest::File if path.is_none() || argv.is_some() => {
                    Err(invalid("capture to file needs a path and no argv"))
                }
                CaptureDest::Command if argv.is_none() || path.is_some() => {
                    Err(invalid("capture to command needs an argv and no path"))
                }
                CaptureDest::Pager | CaptureDest::Editor if path.is_some() => {
                    Err(invalid("capture to pager or editor takes no path"))
                }
                _ => Ok(()),
            },
            Action::Launch {
                kind,
                env,
                location,
                bias,
                ..
            } => {
                if *kind != LaunchType::Split && (location.is_some() || bias.is_some()) {
                    return Err(invalid("location and bias apply only to type=split"));
                }
                if let Some(b) = bias
                    && !(1..=99).contains(b)
                {
                    return Err(invalid("bias is a percentage in 1..=99"));
                }
                for e in env {
                    let ok = e.split_once('=').is_some_and(|(k, v)| {
                        !k.is_empty()
                            && !k.starts_with(|c: char| c.is_ascii_digit())
                            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                            && !v.contains('\0')
                    });
                    if !ok {
                        return Err(invalid("env entries are NAME=value"));
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Build a typed, validated Action from an untyped call.
    pub fn from_call(call: &Call) -> Result<Action, ParseError> {
        let Some(info) = lookup(&call.id) else {
            return Err(ParseError::new(
                ErrorKind::UnknownAction,
                format!("unknown action `{}`", call.id),
            )
            .with_suggestion(suggest(&call.id, REGISTRY.iter().map(|i| i.id))));
        };
        let args = resolve(info, call)?;
        let a = Action::build(info, &args)?;
        a.validate()?;
        Ok(a)
    }

    /// The canonical call form: leading required scalars positional, the rest named,
    /// argv after `--`.
    pub fn to_call(&self) -> Call {
        let info = self.info();
        let mut call = Call::new(info.id);
        let mut leading = true;
        for (spec, (_, vals)) in info.params.iter().zip(self.to_fields()) {
            if spec.kind == ParamKind::Argv {
                if !vals.is_empty() {
                    call.argv = Some(vals);
                }
            } else if spec.repeated {
                for v in vals {
                    call.named.push((spec.name.to_string(), v));
                }
            } else if let Some(v) = vals.into_iter().next() {
                if leading && spec.required {
                    call.positional.push(v);
                } else {
                    leading = false;
                    call.named.push((spec.name.to_string(), v));
                }
            } else {
                leading = false;
            }
        }
        call
    }

    /// Parse one action from its textual form.
    pub fn parse(text: &str) -> Result<Action, ParseError> {
        let mut v = Action::parse_script(text)?;
        match v.len() {
            1 => Ok(v.remove(0)),
            0 => Err(ParseError::new(ErrorKind::Syntax, "no action given")),
            _ => Err(ParseError::new(
                ErrorKind::Syntax,
                "expected one action, found a `;` chain (use parse_script)",
            )),
        }
    }

    /// Parse a `;`-chained script (`apoth do`, palette input, `do { }` bodies).
    pub fn parse_script(text: &str) -> Result<Vec<Action>, ParseError> {
        parse_calls(text)?.iter().map(Action::from_call).collect()
    }

    /// Format actions as a script that [`Action::parse_script`] reads back equal.
    pub fn format_script(actions: &[Action]) -> String {
        let calls: Vec<Call> = actions.iter().map(Action::to_call).collect();
        format_calls(&calls)
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_call().format())
    }
}

impl FromStr for Action {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Action::parse(s)
    }
}
