// SPDX-License-Identifier: Apache-2.0
//! Closed enums used as Action arguments.

crate::string_enum! {
    /// A direction.
    Dir { Left = "left", Right = "right", Up = "up", Down = "down" }
}

crate::string_enum! {
    /// Where a new split goes.
    SplitDir { Right = "right", Down = "down", Left = "left", Up = "up" }
}

crate::string_enum! {
    /// Which text a pipe, capture or stdin reads (AUTOMATION-08).
    Source {
        Visible = "visible",
        Scrollback = "scrollback",
        Selection = "selection",
        LastCommandOutput = "last-command-output",
    }
}

crate::string_enum! {
    /// What `launch` creates (AUTOMATION-09).
    LaunchType {
        Tab = "tab",
        Window = "window",
        Split = "split",
        Overlay = "overlay",
        Background = "background",
        Clipboard = "clipboard",
    }
}

crate::string_enum! {
    /// Where a launched process starts.
    CwdPolicy { Current = "current", Home = "home", LastReported = "last-reported" }
}

crate::string_enum! {
    /// Destination of a `capture`.
    CaptureDest {
        Clipboard = "clipboard",
        File = "file",
        Pager = "pager",
        Editor = "editor",
        Command = "command",
    }
}

crate::string_enum! {
    /// Which clipboard.
    Board { Clipboard = "clipboard", Primary = "primary" }
}

crate::string_enum! {
    /// Selection granularity.
    SelectUnit { Cell = "cell", Word = "word", Line = "line", Block = "block" }
}

crate::string_enum! {
    /// Copy-mode cursor motions.
    Motion {
        Left = "left",
        Right = "right",
        Up = "up",
        Down = "down",
        WordForward = "word-forward",
        WordBack = "word-back",
        LineStart = "line-start",
        LineEnd = "line-end",
        Top = "top",
        Bottom = "bottom",
        PageUp = "page-up",
        PageDown = "page-down",
    }
}

crate::string_enum! {
    /// Previous or next.
    Order { Prev = "prev", Next = "next" }
}

crate::string_enum! {
    /// Input broadcast target (T-13 §5).
    BroadcastMode { Off = "off", Tab = "tab", Window = "window", All = "all" }
}

crate::string_enum! {
    /// Variable and override scope.
    Scope { Pane = "pane", Tab = "tab", Window = "window", Global = "global" }
}

crate::string_enum! {
    /// Per-layout verb of `layout.adjust` (AUTOMATION-10).
    LayoutVerb {
        Resize = "resize",
        Mirror = "mirror",
        Rotate = "rotate",
        Equalize = "equalize",
        Maximize = "maximize",
        BiasCycle = "bias-cycle",
    }
}

crate::string_enum! {
    /// Signals `pane.signal-child` may send.
    Signal {
        Hup = "hup",
        Int = "int",
        Quit = "quit",
        Term = "term",
        Kill = "kill",
        Usr1 = "usr1",
        Usr2 = "usr2",
        Winch = "winch",
        Cont = "cont",
        Stop = "stop",
    }
}

crate::string_enum! {
    /// Notification urgency.
    Urgency { Low = "low", Normal = "normal", Critical = "critical" }
}
