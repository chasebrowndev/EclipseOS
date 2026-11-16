// SPDX-License-Identifier: AGPL-3.0-only
//! Named cursor shapes (cursor-shape-v1) and the OSC 22 push/pop stack (INTEGRATION-29).

use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

/// A cursor shape by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum CursorShape {
    Default,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    EResize,
    NResize,
    NeResize,
    NwResize,
    SResize,
    SeResize,
    SwResize,
    WResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ColResize,
    RowResize,
    AllScroll,
    ZoomIn,
    ZoomOut,
    DndAsk,
    AllResize,
}

impl CursorShape {
    /// Resolve an OSC 22 / CSS cursor name. Case, `_` and `-` are not significant; a few
    /// X11 names common in terminals are accepted. Unknown names give `None`.
    pub fn from_name(name: &str) -> Option<CursorShape> {
        use CursorShape::*;
        let n: String = name
            .trim()
            .chars()
            .map(|c| {
                if c == '_' {
                    '-'
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect();
        Some(match n.as_str() {
            "default" | "left-ptr" | "arrow" => Default,
            "context-menu" => ContextMenu,
            "help" | "question-arrow" => Help,
            "pointer" | "hand" | "hand1" | "hand2" => Pointer,
            "progress" | "left-ptr-watch" => Progress,
            "wait" | "watch" => Wait,
            "cell" | "plus" => Cell,
            "crosshair" | "cross" => Crosshair,
            "text" | "xterm" | "ibeam" => Text,
            "vertical-text" => VerticalText,
            "alias" => Alias,
            "copy" => Copy,
            "move" | "fleur" => Move,
            "no-drop" => NoDrop,
            "not-allowed" | "crossed-circle" => NotAllowed,
            "grab" | "openhand" => Grab,
            "grabbing" | "closedhand" => Grabbing,
            "e-resize" => EResize,
            "n-resize" => NResize,
            "ne-resize" => NeResize,
            "nw-resize" => NwResize,
            "s-resize" => SResize,
            "se-resize" => SeResize,
            "sw-resize" => SwResize,
            "w-resize" => WResize,
            "ew-resize" | "sb-h-double-arrow" => EwResize,
            "ns-resize" | "sb-v-double-arrow" => NsResize,
            "nesw-resize" => NeswResize,
            "nwse-resize" => NwseResize,
            "col-resize" => ColResize,
            "row-resize" => RowResize,
            "all-scroll" => AllScroll,
            "zoom-in" => ZoomIn,
            "zoom-out" => ZoomOut,
            "dnd-ask" => DndAsk,
            "all-resize" => AllResize,
            _ => return None,
        })
    }

    /// The protocol enum value.
    pub fn to_wire(self) -> Shape {
        use CursorShape::*;
        match self {
            Default => Shape::Default,
            ContextMenu => Shape::ContextMenu,
            Help => Shape::Help,
            Pointer => Shape::Pointer,
            Progress => Shape::Progress,
            Wait => Shape::Wait,
            Cell => Shape::Cell,
            Crosshair => Shape::Crosshair,
            Text => Shape::Text,
            VerticalText => Shape::VerticalText,
            Alias => Shape::Alias,
            Copy => Shape::Copy,
            Move => Shape::Move,
            NoDrop => Shape::NoDrop,
            NotAllowed => Shape::NotAllowed,
            Grab => Shape::Grab,
            Grabbing => Shape::Grabbing,
            EResize => Shape::EResize,
            NResize => Shape::NResize,
            NeResize => Shape::NeResize,
            NwResize => Shape::NwResize,
            SResize => Shape::SResize,
            SeResize => Shape::SeResize,
            SwResize => Shape::SwResize,
            WResize => Shape::WResize,
            EwResize => Shape::EwResize,
            NsResize => Shape::NsResize,
            NeswResize => Shape::NeswResize,
            NwseResize => Shape::NwseResize,
            ColResize => Shape::ColResize,
            RowResize => Shape::RowResize,
            AllScroll => Shape::AllScroll,
            ZoomIn => Shape::ZoomIn,
            ZoomOut => Shape::ZoomOut,
            DndAsk => Shape::DndAsk,
            AllResize => Shape::AllResize,
        }
    }
}

/// Deepest OSC 22 push stack; further pushes are dropped.
pub const MAX_STACK: usize = 16;

/// The OSC 22 shape stack. The base shape is what the UI wants when nothing is pushed.
#[derive(Clone, Debug)]
pub struct CursorStack {
    base: CursorShape,
    pushed: Vec<CursorShape>,
}

impl CursorStack {
    /// New with a base shape.
    pub fn new(base: CursorShape) -> CursorStack {
        CursorStack {
            base,
            pushed: Vec::new(),
        }
    }

    /// Set the base (UI-driven: text over cells, pointer over links, ...).
    pub fn set_base(&mut self, base: CursorShape) {
        self.base = base;
    }

    /// Push a program-requested shape. Returns false when the stack is full.
    pub fn push(&mut self, shape: CursorShape) -> bool {
        if self.pushed.len() >= MAX_STACK {
            return false;
        }
        self.pushed.push(shape);
        true
    }

    /// Replace the top (OSC 22 `=` form); pushes when empty.
    pub fn set_top(&mut self, shape: CursorShape) {
        match self.pushed.last_mut() {
            Some(top) => *top = shape,
            None => {
                self.pushed.push(shape);
            }
        }
    }

    /// Pop the top. Returns false when nothing was pushed.
    pub fn pop(&mut self) -> bool {
        self.pushed.pop().is_some()
    }

    /// Drop every program-requested shape (pane reset).
    pub fn clear(&mut self) {
        self.pushed.clear();
    }

    /// The shape to show.
    pub fn current(&self) -> CursorShape {
        self.pushed.last().copied().unwrap_or(self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_loosely() {
        assert_eq!(CursorShape::from_name("text"), Some(CursorShape::Text));
        assert_eq!(
            CursorShape::from_name("  Not_Allowed "),
            Some(CursorShape::NotAllowed)
        );
        assert_eq!(CursorShape::from_name("xterm"), Some(CursorShape::Text));
        assert_eq!(CursorShape::from_name("hand2"), Some(CursorShape::Pointer));
        assert_eq!(
            CursorShape::from_name("ew-resize"),
            Some(CursorShape::EwResize)
        );
        assert_eq!(CursorShape::from_name("nonsense"), None);
        assert_eq!(CursorShape::from_name(""), None);
    }

    #[test]
    fn wire_mapping_spot_checks() {
        assert_eq!(CursorShape::Default.to_wire(), Shape::Default);
        assert_eq!(CursorShape::NeswResize.to_wire(), Shape::NeswResize);
        assert_eq!(CursorShape::AllResize.to_wire(), Shape::AllResize);
    }

    #[test]
    fn stack_push_pop_and_base() {
        let mut s = CursorStack::new(CursorShape::Text);
        assert_eq!(s.current(), CursorShape::Text);
        assert!(s.push(CursorShape::Wait));
        assert!(s.push(CursorShape::Pointer));
        assert_eq!(s.current(), CursorShape::Pointer);
        assert!(s.pop());
        assert_eq!(s.current(), CursorShape::Wait);
        s.set_base(CursorShape::Default);
        assert_eq!(s.current(), CursorShape::Wait);
        assert!(s.pop());
        assert_eq!(s.current(), CursorShape::Default);
        assert!(!s.pop());
    }

    #[test]
    fn stack_is_bounded_and_set_top_works() {
        let mut s = CursorStack::new(CursorShape::Default);
        for _ in 0..MAX_STACK {
            assert!(s.push(CursorShape::Grab));
        }
        assert!(!s.push(CursorShape::Grab));
        s.set_top(CursorShape::Help);
        assert_eq!(s.current(), CursorShape::Help);
        s.clear();
        assert_eq!(s.current(), CursorShape::Default);
        s.set_top(CursorShape::Cell);
        assert_eq!(s.current(), CursorShape::Cell);
    }
}
