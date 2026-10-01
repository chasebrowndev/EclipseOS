// SPDX-License-Identifier: Apache-2.0
//! `eclipse_semantic_v1` value types this crate emits (COMP-09 §2, P-01 §1).
//!
//! COMP-09 passes `role` as "uint — enum per P-01 §1.1" without assigning
//! numbers. The codes here are the P-01 §1.1 listing order, starting at 0.
//! This crate is the one place that numbering is written down (ADR 0034:
//! protocol-shaped things live here, not in C).

/// P-01 §1.1 role, wire code = discriminant.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Window = 0,
    Dialog,
    Alert,
    Popup,
    Menu,
    Menubar,
    Menuitem,
    Toolbar,
    Statusbar,
    Panel,
    Group,
    Section,
    List,
    Listitem,
    Tree,
    Treeitem,
    Table,
    Row,
    Cell,
    Columnheader,
    Button,
    Togglebutton,
    Checkbox,
    Radio,
    Switch,
    Link,
    Tab,
    Tablist,
    Textfield,
    Textarea,
    Password,
    Searchbox,
    Combobox,
    Spinbutton,
    Slider,
    Progressbar,
    Label,
    Heading,
    Paragraph,
    Text,
    Image,
    Icon,
    Canvas,
    Video,
    Scrollbar,
    Scrollarea,
    Separator,
    Tooltip,
    Terminal,
    TerminalLine,
    TerminalCell,
    Document,
    Article,
    Region,
    Navigation,
    Form,
    Landmark,
    Unknown,
}

/// COMP-09 §2 requests, one variant per request this publisher emits.
/// Wire code = discriminant; the C header mirrors these as `ECPUB_OP_*`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// `set_root(node)`.
    SetRoot = 1,
    /// `add_node(node, parent, index)`.
    AddNode = 2,
    /// `remove_node(node)` — removes the subtree.
    RemoveNode = 3,
    /// `move_node(node, new_parent, index)`.
    MoveNode = 4,
    /// `set_role(node, role)`.
    SetRole = 5,
    /// `set_value_text(node, text, cursor, sel_start, sel_end)`.
    SetValueText = 6,
    /// `set_ext(node, key, value)`.
    SetExt = 7,
    /// `commit()` — always the last op of a batch.
    Commit = 8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_codes_follow_p01_listing_order() {
        assert_eq!(Role::Window as u32, 0);
        assert_eq!(Role::Group as u32, 10);
        assert_eq!(Role::Terminal as u32, 48);
        assert_eq!(Role::TerminalLine as u32, 49);
        assert_eq!(Role::Unknown as u32, 57);
    }
}
