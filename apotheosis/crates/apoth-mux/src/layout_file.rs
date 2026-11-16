// SPDX-License-Identifier: AGPL-3.0-only
//! Named layouts as KDL (T-13 §6, MUX-22): tabs, tree, cwd, command argv, profile, focus.
//!
//! The spec gives only `layout "dev" { tab { split … } }`; the grammar below is the one
//! this crate pins (reported as a spec gap):
//!
//! ```kdl
//! layout "dev" {
//!     tab "code" {
//!         split "h" ratio=600000 {            // "h" = side by side, "v" = stacked
//!             pane cwd="/src" profile="work" focus=#true { argv "nvim" "." }
//!             stack "v" active=0 {            // "v" / "h" header style
//!                 pane
//!                 pane { argv "htop" }
//!             }
//!         }
//!         float x=4 y=2 w=40 h=10 visible=#true pinned=#false { pane }
//!     }
//! }
//! ```
//!
//! `ratio` is parts per million of the first child (`RATIO_ONE`). A `split` has exactly two
//! children, a `stack` at least two panes, a tab at most one tiled root plus any floats and
//! at least one pane. Unknown properties are ignored; unknown node kinds are an error.
//!
//! The reader is a hand-written KDL v2 subset (nodes, string/integer/`#true`/`#false`
//! values, properties, children, `//` and `/* */` comments) with hard limits, because layout
//! files cross a trust boundary (T-18 §5). No error carries file content.

use std::fmt::Write as _;

use crate::geom::{Orient, Rect};
use crate::session::{MuxError, PaneSpec, Session, WindowId};
use crate::tab::{Float, Tab, TabId};
use crate::tree::{Node, PaneId, RATIO_ONE, Split, Stack, StackStyle};

/// Largest accepted input.
pub const MAX_INPUT_BYTES: usize = 1 << 20;
/// Deepest nesting of `{ }`.
pub const MAX_DEPTH: usize = 64;
/// Most KDL nodes in one file.
pub const MAX_NODES: usize = 8192;
/// Longest string value or name, in bytes.
pub const MAX_STRING_BYTES: usize = 4096;
/// Most panes in one tab of a layout.
pub const MAX_PANES: usize = 1024;

/// A parse or validation failure. Never carries content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutError {
    /// 1-based line, 0 when not tied to one.
    pub line: u32,
    /// What went wrong.
    pub kind: ErrKind,
}

/// Failure kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrKind {
    /// Malformed KDL.
    Syntax(&'static str),
    /// Input larger than [`MAX_INPUT_BYTES`].
    TooLarge,
    /// Nesting deeper than [`MAX_DEPTH`].
    TooDeep,
    /// More than [`MAX_NODES`] nodes.
    TooManyNodes,
    /// A string longer than [`MAX_STRING_BYTES`].
    StringTooLong,
    /// Well-formed KDL that is not a valid layout.
    Schema(&'static str),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            ErrKind::Syntax(m) => write!(f, "line {}: syntax: {m}", self.line),
            ErrKind::TooLarge => f.write_str("layout file too large"),
            ErrKind::TooDeep => write!(f, "line {}: nesting too deep", self.line),
            ErrKind::TooManyNodes => f.write_str("too many nodes"),
            ErrKind::StringTooLong => write!(f, "line {}: string too long", self.line),
            ErrKind::Schema(m) => write!(f, "line {}: {m}", self.line),
        }
    }
}

impl std::error::Error for LayoutError {}

// ---------------------------------------------------------------- spec data

/// One pane of a layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneNode {
    /// cwd, argv, profile.
    pub spec: PaneSpec,
    /// Focus marker.
    pub focus: bool,
}

/// A tiled tree node of a layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeSpec {
    /// A pane.
    Pane(PaneNode),
    /// A stack.
    Stack {
        /// Header style.
        style: StackStyle,
        /// Expanded member.
        active: usize,
        /// Members, at least two.
        panes: Vec<PaneNode>,
    },
    /// A split.
    Split {
        /// Orientation.
        orient: Orient,
        /// First child's share, `1..RATIO_ONE`.
        ratio: u32,
        /// First child.
        a: Box<NodeSpec>,
        /// Second child.
        b: Box<NodeSpec>,
    },
}

/// A floating pane of a layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FloatSpec {
    /// Placement.
    pub rect: Rect,
    /// Shown.
    pub visible: bool,
    /// Pinned.
    pub pinned: bool,
    /// The pane.
    pub pane: PaneNode,
}

/// A tab of a layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabSpec {
    /// Tab name (may be empty).
    pub name: String,
    /// The tiled tree.
    pub root: Option<NodeSpec>,
    /// Floats, back to front.
    pub floats: Vec<FloatSpec>,
}

/// A named layout.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayoutSpec {
    /// Layout name.
    pub name: String,
    /// Tabs in order.
    pub tabs: Vec<TabSpec>,
}

fn node_panes(n: &NodeSpec) -> usize {
    match n {
        NodeSpec::Pane(_) => 1,
        NodeSpec::Stack { panes, .. } => panes.len(),
        NodeSpec::Split { a, b, .. } => node_panes(a) + node_panes(b),
    }
}

fn validate_node(n: &NodeSpec, depth: usize) -> Result<(), &'static str> {
    if depth > MAX_DEPTH {
        return Err("tree too deep");
    }
    match n {
        NodeSpec::Pane(_) => Ok(()),
        NodeSpec::Stack { active, panes, .. } => {
            if panes.len() < 2 {
                Err("stack needs at least two panes")
            } else if *active >= panes.len() {
                Err("stack active index out of range")
            } else {
                Ok(())
            }
        }
        NodeSpec::Split { ratio, a, b, .. } => {
            if *ratio == 0 || *ratio >= RATIO_ONE {
                return Err("ratio out of range");
            }
            validate_node(a, depth + 1)?;
            validate_node(b, depth + 1)
        }
    }
}

impl TabSpec {
    /// Pane count of the tab.
    pub fn pane_count(&self) -> usize {
        self.root.as_ref().map_or(0, node_panes) + self.floats.len()
    }
}

impl LayoutSpec {
    /// True when any pane carries command argv: `layout.apply` is class `spawn` then, else
    /// `layout` (T-11 §1, T-13 §6).
    pub fn has_command(&self) -> bool {
        fn node(n: &NodeSpec) -> bool {
            match n {
                NodeSpec::Pane(p) => !p.spec.argv.is_empty(),
                NodeSpec::Stack { panes, .. } => panes.iter().any(|p| !p.spec.argv.is_empty()),
                NodeSpec::Split { a, b, .. } => node(a) || node(b),
            }
        }
        self.tabs.iter().any(|t| {
            t.root.as_ref().is_some_and(node)
                || t.floats.iter().any(|f| !f.pane.spec.argv.is_empty())
        })
    }

    /// Structural validation: ratios, stacks, depth, at least one pane per tab, limits.
    pub fn validate(&self) -> Result<(), &'static str> {
        for t in &self.tabs {
            if let Some(r) = &t.root {
                validate_node(r, 0)?;
            }
            let n = t.pane_count();
            if n == 0 {
                return Err("tab has no pane");
            }
            if n > MAX_PANES {
                return Err("too many panes");
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------- writer

fn write_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn indent(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push_str("    ");
    }
}

fn write_pane(out: &mut String, p: &PaneNode, ind: usize) {
    indent(out, ind);
    out.push_str("pane");
    if let Some(c) = &p.spec.cwd {
        out.push_str(" cwd=");
        write_str(out, c);
    }
    if let Some(c) = &p.spec.profile {
        out.push_str(" profile=");
        write_str(out, c);
    }
    if p.focus {
        out.push_str(" focus=#true");
    }
    if p.spec.argv.is_empty() {
        out.push('\n');
    } else {
        out.push_str(" {\n");
        indent(out, ind + 1);
        out.push_str("argv");
        for a in &p.spec.argv {
            out.push(' ');
            write_str(out, a);
        }
        out.push('\n');
        indent(out, ind);
        out.push_str("}\n");
    }
}

fn orient_name(o: Orient) -> &'static str {
    match o {
        Orient::Horizontal => "h",
        Orient::Vertical => "v",
    }
}

fn style_name(s: StackStyle) -> &'static str {
    match s {
        StackStyle::Vertical => "v",
        StackStyle::Horizontal => "h",
    }
}

fn write_node(out: &mut String, n: &NodeSpec, ind: usize) {
    match n {
        NodeSpec::Pane(p) => write_pane(out, p, ind),
        NodeSpec::Stack {
            style,
            active,
            panes,
        } => {
            indent(out, ind);
            let _ = writeln!(out, "stack \"{}\" active={active} {{", style_name(*style));
            for p in panes {
                write_pane(out, p, ind + 1);
            }
            indent(out, ind);
            out.push_str("}\n");
        }
        NodeSpec::Split {
            orient,
            ratio,
            a,
            b,
        } => {
            indent(out, ind);
            let _ = writeln!(out, "split \"{}\" ratio={ratio} {{", orient_name(*orient));
            write_node(out, a, ind + 1);
            write_node(out, b, ind + 1);
            indent(out, ind);
            out.push_str("}\n");
        }
    }
}

/// Serialise layouts to KDL. Reading the result back gives the same layouts.
pub fn write_layouts(layouts: &[LayoutSpec]) -> String {
    let mut out = String::new();
    for l in layouts {
        out.push_str("layout ");
        write_str(&mut out, &l.name);
        out.push_str(" {\n");
        for t in &l.tabs {
            indent(&mut out, 1);
            out.push_str("tab ");
            write_str(&mut out, &t.name);
            out.push_str(" {\n");
            if let Some(r) = &t.root {
                write_node(&mut out, r, 2);
            }
            for f in &t.floats {
                indent(&mut out, 2);
                let _ = writeln!(
                    out,
                    "float x={} y={} w={} h={} visible=#{} pinned=#{} {{",
                    f.rect.x, f.rect.y, f.rect.w, f.rect.h, f.visible, f.pinned
                );
                write_pane(&mut out, &f.pane, 3);
                indent(&mut out, 2);
                out.push_str("}\n");
            }
            indent(&mut out, 1);
            out.push_str("}\n");
        }
        out.push_str("}\n");
    }
    out
}

// ------------------------------------------------------------------- reader

#[derive(Clone, Debug, PartialEq)]
enum Val {
    Str(String),
    Int(i64),
    Bool(bool),
}

#[derive(Debug)]
struct KNode {
    name: String,
    args: Vec<Val>,
    props: Vec<(String, Val)>,
    children: Vec<KNode>,
    line: u32,
}

enum Tok {
    Str(String),
    Bare(String),
}

struct Parser {
    c: Vec<char>,
    i: usize,
    line: u32,
    nodes: usize,
}

impl Parser {
    fn err(&self, kind: ErrKind) -> LayoutError {
        LayoutError {
            line: self.line,
            kind,
        }
    }

    fn syn(&self, m: &'static str) -> LayoutError {
        self.err(ErrKind::Syntax(m))
    }

    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.c.get(self.i + 1).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
        }
        Some(c)
    }

    /// Spaces, tabs, comments and line continuations; stops at a newline.
    fn skip_inline(&mut self) -> Result<(), LayoutError> {
        loop {
            match (self.peek(), self.peek2()) {
                (Some(' ' | '\t'), _) => {
                    self.bump();
                }
                (Some('/'), Some('*')) => {
                    self.bump();
                    self.bump();
                    loop {
                        match (self.peek(), self.peek2()) {
                            (Some('*'), Some('/')) => {
                                self.bump();
                                self.bump();
                                break;
                            }
                            (Some(_), _) => {
                                self.bump();
                            }
                            (None, _) => return Err(self.syn("unterminated comment")),
                        }
                    }
                }
                (Some('/'), Some('/')) => {
                    while !matches!(self.peek(), None | Some('\n')) {
                        self.bump();
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn skip_blank(&mut self) -> Result<(), LayoutError> {
        loop {
            self.skip_inline()?;
            if matches!(self.peek(), Some('\n' | '\r' | ';')) {
                self.bump();
            } else {
                return Ok(());
            }
        }
    }

    fn parse_nodes(&mut self, depth: usize, nested: bool) -> Result<Vec<KNode>, LayoutError> {
        if depth > MAX_DEPTH {
            return Err(self.err(ErrKind::TooDeep));
        }
        let mut v = Vec::new();
        loop {
            self.skip_blank()?;
            match self.peek() {
                None if nested => return Err(self.syn("unclosed block")),
                None => return Ok(v),
                Some('}') if nested => {
                    self.bump();
                    return Ok(v);
                }
                Some('}') => return Err(self.syn("unexpected }")),
                Some(_) => v.push(self.parse_node(depth)?),
            }
        }
    }

    fn parse_node(&mut self, depth: usize) -> Result<KNode, LayoutError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(self.err(ErrKind::TooManyNodes));
        }
        let line = self.line;
        let name = match self.parse_token()? {
            Tok::Str(s) | Tok::Bare(s) => s,
        };
        let mut n = KNode {
            name,
            args: vec![],
            props: vec![],
            children: vec![],
            line,
        };
        loop {
            self.skip_inline()?;
            match self.peek() {
                None | Some('\n' | '\r' | ';' | '}') => break,
                Some('{') => {
                    self.bump();
                    n.children = self.parse_nodes(depth + 1, true)?;
                    break;
                }
                Some(_) => {
                    let tok = self.parse_token()?;
                    if self.peek() == Some('=') {
                        self.bump();
                        let key = match tok {
                            Tok::Str(s) | Tok::Bare(s) => s,
                        };
                        let v = self.parse_value()?;
                        n.props.push((key, v));
                    } else {
                        n.args.push(self.tok_to_val(tok)?);
                    }
                }
            }
        }
        Ok(n)
    }

    fn tok_to_val(&self, t: Tok) -> Result<Val, LayoutError> {
        match t {
            Tok::Str(s) => Ok(Val::Str(s)),
            Tok::Bare(s) => match s.as_str() {
                "#true" => Ok(Val::Bool(true)),
                "#false" => Ok(Val::Bool(false)),
                _ => s
                    .parse::<i64>()
                    .map(Val::Int)
                    .map_err(|_| self.syn("unquoted value")),
            },
        }
    }

    fn parse_value(&mut self) -> Result<Val, LayoutError> {
        let t = self.parse_token()?;
        self.tok_to_val(t)
    }

    fn parse_token(&mut self) -> Result<Tok, LayoutError> {
        if self.peek() == Some('"') {
            return self.parse_string().map(Tok::Str);
        }
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_whitespace() || matches!(c, '=' | ';' | '{' | '}' | '"') {
                break;
            }
            s.push(c);
            self.bump();
            if s.len() > MAX_STRING_BYTES {
                return Err(self.err(ErrKind::StringTooLong));
            }
        }
        if s.is_empty() {
            return Err(self.syn("expected a name or value"));
        }
        Ok(Tok::Bare(s))
    }

    fn parse_string(&mut self) -> Result<String, LayoutError> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            let Some(c) = self.bump() else {
                return Err(self.syn("unterminated string"));
            };
            match c {
                '"' => return Ok(s),
                '\\' => {
                    let e = self.bump().ok_or_else(|| self.syn("unterminated string"))?;
                    match e {
                        '"' => s.push('"'),
                        '\\' => s.push('\\'),
                        'n' => s.push('\n'),
                        'r' => s.push('\r'),
                        't' => s.push('\t'),
                        'u' => {
                            if self.bump() != Some('{') {
                                return Err(self.syn("bad \\u escape"));
                            }
                            let mut v: u32 = 0;
                            let mut digits = 0;
                            loop {
                                let d = self.bump().ok_or_else(|| self.syn("bad \\u escape"))?;
                                if d == '}' {
                                    break;
                                }
                                let h = d.to_digit(16).ok_or_else(|| self.syn("bad \\u escape"))?;
                                v = v * 16 + h;
                                digits += 1;
                                if digits > 6 {
                                    return Err(self.syn("bad \\u escape"));
                                }
                            }
                            if digits == 0 {
                                return Err(self.syn("bad \\u escape"));
                            }
                            s.push(char::from_u32(v).ok_or_else(|| self.syn("bad \\u escape"))?);
                        }
                        _ => return Err(self.syn("unknown escape")),
                    }
                }
                c => s.push(c),
            }
            if s.len() > MAX_STRING_BYTES {
                return Err(self.err(ErrKind::StringTooLong));
            }
        }
    }
}

fn schema(n: &KNode, m: &'static str) -> LayoutError {
    LayoutError {
        line: n.line,
        kind: ErrKind::Schema(m),
    }
}

fn prop<'a>(n: &'a KNode, k: &str) -> Option<&'a Val> {
    n.props.iter().find(|(key, _)| key == k).map(|(_, v)| v)
}

fn prop_str(n: &KNode, k: &str) -> Result<Option<String>, LayoutError> {
    match prop(n, k) {
        None => Ok(None),
        Some(Val::Str(s)) => Ok(Some(s.clone())),
        Some(_) => Err(schema(n, "property must be a string")),
    }
}

fn prop_bool(n: &KNode, k: &str, default: bool) -> Result<bool, LayoutError> {
    match prop(n, k) {
        None => Ok(default),
        Some(Val::Bool(b)) => Ok(*b),
        Some(_) => Err(schema(n, "property must be #true or #false")),
    }
}

fn prop_u32(n: &KNode, k: &str, default: Option<u32>) -> Result<u32, LayoutError> {
    match prop(n, k) {
        None => default.ok_or_else(|| schema(n, "missing property")),
        Some(Val::Int(i)) => u32::try_from(*i).map_err(|_| schema(n, "integer out of range")),
        Some(_) => Err(schema(n, "property must be an integer")),
    }
}

fn arg_str(n: &KNode) -> Result<&str, LayoutError> {
    match n.args.first() {
        Some(Val::Str(s)) => Ok(s),
        _ => Err(schema(n, "expected a string argument")),
    }
}

fn pane_from(n: &KNode) -> Result<PaneNode, LayoutError> {
    let mut argv = Vec::new();
    for c in &n.children {
        if c.name == "argv" {
            for a in &c.args {
                match a {
                    Val::Str(s) => argv.push(s.clone()),
                    _ => return Err(schema(c, "argv entries must be strings")),
                }
            }
        }
    }
    Ok(PaneNode {
        spec: PaneSpec {
            cwd: prop_str(n, "cwd")?,
            argv,
            profile: prop_str(n, "profile")?,
        },
        focus: prop_bool(n, "focus", false)?,
    })
}

fn node_from(n: &KNode) -> Result<NodeSpec, LayoutError> {
    match n.name.as_str() {
        "pane" => Ok(NodeSpec::Pane(pane_from(n)?)),
        "stack" => {
            let style = match arg_str(n)? {
                "v" => StackStyle::Vertical,
                "h" => StackStyle::Horizontal,
                _ => return Err(schema(n, "stack style must be \"v\" or \"h\"")),
            };
            let active = prop_u32(n, "active", Some(0))? as usize;
            let mut panes = Vec::new();
            for c in &n.children {
                if c.name != "pane" {
                    return Err(schema(c, "stack children must be panes"));
                }
                panes.push(pane_from(c)?);
            }
            Ok(NodeSpec::Stack {
                style,
                active,
                panes,
            })
        }
        "split" => {
            let orient = match arg_str(n)? {
                "h" => Orient::Horizontal,
                "v" => Orient::Vertical,
                _ => return Err(schema(n, "split orientation must be \"h\" or \"v\"")),
            };
            let ratio = prop_u32(n, "ratio", Some(RATIO_ONE / 2))?;
            if n.children.len() != 2 {
                return Err(schema(n, "split needs exactly two children"));
            }
            Ok(NodeSpec::Split {
                orient,
                ratio,
                a: Box::new(node_from(&n.children[0])?),
                b: Box::new(node_from(&n.children[1])?),
            })
        }
        _ => Err(schema(n, "unknown node kind")),
    }
}

fn tab_from(n: &KNode) -> Result<TabSpec, LayoutError> {
    let name = match n.args.first() {
        None => String::new(),
        Some(Val::Str(s)) => s.clone(),
        Some(_) => return Err(schema(n, "tab name must be a string")),
    };
    let mut t = TabSpec {
        name,
        root: None,
        floats: vec![],
    };
    for c in &n.children {
        if c.name == "float" {
            if c.children.len() != 1 || c.children[0].name != "pane" {
                return Err(schema(c, "float needs exactly one pane"));
            }
            t.floats.push(FloatSpec {
                rect: Rect::new(
                    prop_u32(c, "x", Some(0))?,
                    prop_u32(c, "y", Some(0))?,
                    prop_u32(c, "w", None)?,
                    prop_u32(c, "h", None)?,
                ),
                visible: prop_bool(c, "visible", true)?,
                pinned: prop_bool(c, "pinned", false)?,
                pane: pane_from(&c.children[0])?,
            });
        } else if t.root.is_some() {
            return Err(schema(c, "tab has more than one tiled root"));
        } else {
            t.root = Some(node_from(c)?);
        }
    }
    Ok(t)
}

/// Parse a layout file (any number of `layout` nodes).
pub fn parse_layouts(src: &str) -> Result<Vec<LayoutSpec>, LayoutError> {
    if src.len() > MAX_INPUT_BYTES {
        return Err(LayoutError {
            line: 0,
            kind: ErrKind::TooLarge,
        });
    }
    let mut p = Parser {
        c: src.chars().collect(),
        i: 0,
        line: 1,
        nodes: 0,
    };
    let nodes = p.parse_nodes(0, false)?;
    let mut out = Vec::new();
    for n in &nodes {
        if n.name != "layout" {
            return Err(schema(n, "expected a layout node"));
        }
        let mut l = LayoutSpec {
            name: arg_str(n)?.to_owned(),
            tabs: vec![],
        };
        for c in &n.children {
            if c.name != "tab" {
                return Err(schema(c, "layout children must be tabs"));
            }
            l.tabs.push(tab_from(c)?);
        }
        l.validate().map_err(|m| schema(n, m))?;
        out.push(l);
    }
    Ok(out)
}

// ------------------------------------------------- Session <-> layout (apply/save)

impl<P> Session<P> {
    fn new_layout_pane(
        &mut self,
        pn: &PaneNode,
        make: &mut dyn FnMut(PaneId, &PaneSpec) -> P,
        focus: &mut Option<PaneId>,
    ) -> PaneId {
        let id = self.alloc();
        let payload = make(id, &pn.spec);
        self.panes.insert(
            id,
            crate::session::PaneMeta {
                spec: pn.spec.clone(),
                payload,
            },
        );
        if pn.focus && focus.is_none() {
            *focus = Some(id);
        }
        id
    }

    fn build_node(
        &mut self,
        n: &NodeSpec,
        make: &mut dyn FnMut(PaneId, &PaneSpec) -> P,
        focus: &mut Option<PaneId>,
    ) -> Node {
        match n {
            NodeSpec::Pane(p) => Node::Pane(self.new_layout_pane(p, make, focus)),
            NodeSpec::Stack {
                style,
                active,
                panes,
            } => {
                let ids = panes
                    .iter()
                    .map(|p| self.new_layout_pane(p, make, focus))
                    .collect();
                Node::Stack(Stack {
                    panes: ids,
                    active: *active,
                    style: *style,
                })
            }
            NodeSpec::Split {
                orient,
                ratio,
                a,
                b,
            } => {
                let a = self.build_node(a, make, focus);
                let b = self.build_node(b, make, focus);
                Node::Split(Box::new(Split {
                    orient: *orient,
                    ratio: *ratio,
                    a,
                    b,
                }))
            }
        }
    }

    /// `layout.apply`: create the layout's tabs in `window` and show the first. `make`
    /// builds each pane's payload (the caller spawns the process from the spec). Returns
    /// the new tab ids. Nothing is created when validation fails.
    pub fn apply_layout(
        &mut self,
        window: WindowId,
        layout: &LayoutSpec,
        mut make: impl FnMut(PaneId, &PaneSpec) -> P,
    ) -> Result<Vec<TabId>, MuxError> {
        layout.validate().map_err(|_| MuxError::BadLayout)?;
        let size = self
            .windows
            .get(&window)
            .ok_or(MuxError::NoSuchWindow)?
            .size;
        let mut created = Vec::new();
        for ts in &layout.tabs {
            let mut focus = None;
            let root = ts
                .root
                .as_ref()
                .map(|r| self.build_node(r, &mut make, &mut focus));
            let mut floats = Vec::new();
            for f in &ts.floats {
                let pane = self.new_layout_pane(&f.pane, &mut make, &mut focus);
                floats.push(Float {
                    pane,
                    rect: f.rect,
                    visible: f.visible,
                    pinned: f.pinned,
                });
            }
            let tid = self.alloc();
            let mut tab = Tab::new(tid, ts.name.clone(), size, 0);
            tab.root = root;
            tab.floats = floats;
            let mut all = tab.panes();
            let f = focus
                .or_else(|| all.first().copied())
                .ok_or(MuxError::BadLayout)?;
            all.retain(|&p| p != f);
            all.insert(0, f);
            tab.focused = Some(f);
            tab.mru = all;
            if let Some(r) = &mut tab.root {
                r.activate(f);
            }
            self.tabs.insert(tid, tab);
            created.push(tid);
        }
        let w = self
            .windows
            .get_mut(&window)
            .ok_or(MuxError::NoSuchWindow)?;
        w.tabs.extend(&created);
        // later tabs go behind the first in MRU so `tab.focus last` is the old tab
        if let Some(&first) = created.first() {
            w.tab_mru.extend(created.iter().skip(1));
            w.tab_mru.insert(0, first);
            w.active = Some(first);
        }
        Ok(created)
    }

    fn node_spec(&self, n: &Node, focused: Option<PaneId>) -> NodeSpec {
        let pn = |id: PaneId| PaneNode {
            spec: self
                .panes
                .get(&id)
                .map(|m| m.spec.clone())
                .unwrap_or_default(),
            focus: focused == Some(id),
        };
        match n {
            Node::Pane(p) => NodeSpec::Pane(pn(*p)),
            Node::Stack(s) => NodeSpec::Stack {
                style: s.style,
                active: s.active,
                panes: s.panes.iter().map(|&p| pn(p)).collect(),
            },
            Node::Split(sp) => NodeSpec::Split {
                orient: sp.orient,
                ratio: sp.ratio,
                a: Box::new(self.node_spec(&sp.a, focused)),
                b: Box::new(self.node_spec(&sp.b, focused)),
            },
        }
    }

    /// `layout.save`: capture the tabs of `window` (tree, floats, cwd, argv, profile, focus).
    pub fn capture_layout(&self, window: WindowId, name: &str) -> Result<LayoutSpec, MuxError> {
        let w = self.windows.get(&window).ok_or(MuxError::NoSuchWindow)?;
        let mut tabs = Vec::new();
        for t in &w.tabs {
            let tab = self.tabs.get(t).ok_or(MuxError::NoSuchTab)?;
            let focused = tab.focused;
            tabs.push(TabSpec {
                name: tab.name.clone(),
                root: tab.root.as_ref().map(|r| self.node_spec(r, focused)),
                floats: tab
                    .floats
                    .iter()
                    .map(|f| FloatSpec {
                        rect: f.rect,
                        visible: f.visible,
                        pinned: f.pinned,
                        pane: PaneNode {
                            spec: self
                                .panes
                                .get(&f.pane)
                                .map(|m| m.spec.clone())
                                .unwrap_or_default(),
                            focus: focused == Some(f.pane),
                        },
                    })
                    .collect(),
            });
        }
        Ok(LayoutSpec {
            name: name.to_owned(),
            tabs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Dir, Size};
    use crate::tab::SplitSize;

    fn pane(cwd: Option<&str>, argv: &[&str], focus: bool) -> PaneNode {
        PaneNode {
            spec: PaneSpec {
                cwd: cwd.map(str::to_owned),
                argv: argv.iter().map(|s| (*s).to_owned()).collect(),
                profile: None,
            },
            focus,
        }
    }

    fn sample() -> LayoutSpec {
        LayoutSpec {
            name: "dev".into(),
            tabs: vec![TabSpec {
                name: "code \"x\"".into(),
                root: Some(NodeSpec::Split {
                    orient: Orient::Horizontal,
                    ratio: 600_000,
                    a: Box::new(NodeSpec::Pane(pane(Some("/src"), &["nvim", "a b"], true))),
                    b: Box::new(NodeSpec::Stack {
                        style: StackStyle::Horizontal,
                        active: 1,
                        panes: vec![pane(None, &[], false), pane(Some("/tmp"), &["htop"], false)],
                    }),
                }),
                floats: vec![FloatSpec {
                    rect: Rect::new(4, 2, 40, 10),
                    visible: false,
                    pinned: true,
                    pane: pane(None, &[], false),
                }],
            }],
        }
    }

    #[test]
    fn kdl_round_trip() {
        let l = sample();
        let text = write_layouts(std::slice::from_ref(&l));
        let back = parse_layouts(&text).unwrap();
        assert_eq!(back, vec![l]);
    }

    #[test]
    fn control_chars_and_unicode_round_trip() {
        let mut l = sample();
        l.name = "a\u{1}\u{7f}\n\t\r\\\"é☃".into();
        let text = write_layouts(std::slice::from_ref(&l));
        assert_eq!(parse_layouts(&text).unwrap(), vec![l]);
    }

    #[test]
    fn reads_the_documented_form_with_comments() {
        let src = r#"
// a comment
layout "dev" { /* block */
    tab {
        split "v" {
            pane cwd="/x"
            pane profile="p" focus=#true { argv "ls" }
        }
    }
}
layout "two" { tab { pane } }
"#;
        let v = parse_layouts(src).unwrap();
        assert_eq!(v.len(), 2);
        let NodeSpec::Split { ratio, b, .. } = v[0].tabs[0].root.as_ref().unwrap() else {
            panic!()
        };
        assert_eq!(*ratio, RATIO_ONE / 2);
        let NodeSpec::Pane(p) = &**b else { panic!() };
        assert!(p.focus && p.spec.argv == ["ls"] && p.spec.profile.as_deref() == Some("p"));
        assert!(v[0].has_command());
        assert!(!v[1].has_command());
    }

    #[test]
    fn rejects_bad_input_without_panicking() {
        for src in [
            "layout",
            "layout \"a\" {",
            "}",
            "layout \"a\" { tab { split \"h\" { pane } } }",
            "layout \"a\" { tab { stack \"v\" { pane } } }",
            "layout \"a\" { tab { stack \"v\" active=5 { pane pane } } }",
            "layout \"a\" { tab { split \"h\" ratio=0 { pane pane } } }",
            "layout \"a\" { tab { split \"h\" ratio=1000000 { pane pane } } }",
            "layout \"a\" { tab { split \"x\" { pane pane } } }",
            "layout \"a\" { tab { } }",
            "layout \"a\" { tab { pane pane } }",
            "layout \"a\" { tab { frob } }",
            "layout \"a\" { tab { pane cwd=5 } }",
            "layout \"a\" { tab { float w=1 h=1 { pane pane } } }",
            "layout \"a\" { tab { float x=-1 w=1 h=1 { pane } } }",
            "layout \"a\" { tab { pane } } junk",
            "layout \"a\\q\" { tab { pane } }",
            "layout \"a\\u{110000}\" { tab { pane } }",
            "layout \"a { tab { pane } }",
            "layout bare { tab { pane } }",
            "layout \"a\" { /* x",
        ] {
            assert!(parse_layouts(src).is_err(), "should reject: {src}");
        }
    }

    #[test]
    fn limits_hold() {
        let big = "a".repeat(MAX_STRING_BYTES + 1);
        let e = parse_layouts(&format!("layout \"{big}\" {{ tab {{ pane }} }}")).unwrap_err();
        assert_eq!(e.kind, ErrKind::StringTooLong);
        let e = parse_layouts(&" ".repeat(MAX_INPUT_BYTES + 1)).unwrap_err();
        assert_eq!(e.kind, ErrKind::TooLarge);
        let deep = format!(
            "{}{}",
            "a {".repeat(MAX_DEPTH + 2),
            "}".repeat(MAX_DEPTH + 2)
        );
        assert_eq!(parse_layouts(&deep).unwrap_err().kind, ErrKind::TooDeep);
        let many = "n\n".repeat(MAX_NODES + 1);
        assert_eq!(
            parse_layouts(&many).unwrap_err().kind,
            ErrKind::TooManyNodes
        );
    }

    #[test]
    fn apply_then_capture_round_trips() {
        let mut s: Session<PaneSpec> = Session::new("main");
        let (w, t0, _) = s.new_window(
            Size {
                cols: 120,
                rows: 40,
            },
            None,
            PaneSpec::default(),
            PaneSpec::default(),
        );
        let l = sample();
        let tabs = s.apply_layout(w, &l, |_, spec| spec.clone()).unwrap();
        assert_eq!(tabs.len(), 1);
        s.check_invariants().unwrap();
        let win = s.window(w).unwrap();
        assert_eq!(win.tabs, [t0, tabs[0]]);
        assert_eq!(win.active, Some(tabs[0]));
        let tab = s.tab(tabs[0]).unwrap();
        let f = tab.focused().unwrap();
        assert_eq!(s.pane(f).unwrap().spec.argv, ["nvim", "a b"]);
        assert_eq!(s.pane(f).unwrap().payload.argv, ["nvim", "a b"]);
        // the stack's expanded member is the active index of the layout
        let cap = s.capture_layout(w, "dev").unwrap();
        assert_eq!(cap.tabs[1], l.tabs[0]);
    }

    #[test]
    fn apply_rejects_invalid_and_creates_nothing() {
        let mut s: Session<()> = Session::new("main");
        let (w, ..) = s.new_window(Size { cols: 80, rows: 24 }, None, PaneSpec::default(), ());
        let before = s.pane_count();
        let bad = LayoutSpec {
            name: "x".into(),
            tabs: vec![TabSpec::default()],
        };
        assert_eq!(s.apply_layout(w, &bad, |_, _| ()), Err(MuxError::BadLayout));
        assert_eq!(s.pane_count(), before);
        assert_eq!(
            s.apply_layout(999, &sample(), |_, _| ()),
            Err(MuxError::NoSuchWindow)
        );
    }

    #[test]
    fn capture_reflects_live_edits() {
        let mut s: Session<()> = Session::new("main");
        let spec = PaneSpec {
            cwd: Some("/a".into()),
            ..PaneSpec::default()
        };
        let (w, t, p) = s.new_window(Size { cols: 80, rows: 24 }, None, spec, ());
        let q = s
            .split_pane(
                p,
                Dir::Right,
                SplitSize::Fraction(250_000),
                PaneSpec::default(),
                (),
            )
            .unwrap();
        s.tab_mut(t).unwrap().focus(q).unwrap();
        let cap = s.capture_layout(w, "n").unwrap();
        let NodeSpec::Split { ratio, a, b, .. } = cap.tabs[0].root.as_ref().unwrap() else {
            panic!()
        };
        assert_eq!(*ratio, 750_000);
        let (NodeSpec::Pane(a), NodeSpec::Pane(b)) = (&**a, &**b) else {
            panic!()
        };
        assert_eq!(a.spec.cwd.as_deref(), Some("/a"));
        assert!(b.focus && !a.focus);
        let text = write_layouts(std::slice::from_ref(&cap));
        assert_eq!(parse_layouts(&text).unwrap(), vec![cap]);
    }
}
