// SPDX-License-Identifier: AGPL-3.0-only
//! Compositor-drawn glass panes (`eclipse_glass_v1`, COMP-02 section 9).
//!
//! A cell's ground (the blurred, bent backdrop, the lit hairline, the seam)
//! is drawn by the compositor, under the client's surface, in step with its
//! commits. The client draws only the cell's content. This module is the
//! client half: it binds the protocol, collects the panes a frame draws and
//! sends them just before the frame is presented.
//!
//! Fail-soft by construction. With no `eclipse_glass_manager_v1` global (a
//! non-abyss host, a client the compositor did not allowlist, a blur mode
//! other than glass) [`available`] is `false`, [`push`] does nothing and the
//! view draws its own ground exactly as it always has.
//!
//! Wiring, once per process, before the application runs:
//!
//! ```ignore
//! let conn = ec_ui::glass::bootstrap();            // Option<wayland Connection>
//! iced_layershell::set_surface_hook(ec_ui::glass::surface_created);
//! // ... Settings { with_connection: conn.map(Into::into), .. }
//! ```
//!
//! In a view, wrap the window's root once and have every cell push its pane
//! while it draws:
//!
//! ```ignore
//! ec_ui::glass::root(id, content)                  // root widget, per window id
//! if ec_ui::glass::available() { ec_ui::glass::push(Pane::new(bounds, r, tone).status(s)); }
//! ```
//!
//! `bounds` are in surface-local logical pixels, which is what a widget's
//! `layout.bounds()` is for a root-level cell (no scroll offset).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use ec_protocols::glass::client::eclipse_glass_manager_v1::{self, EclipseGlassManagerV1};
use ec_protocols::glass::client::eclipse_glass_surface_v1::{self, EclipseGlassSurfaceV1};
use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::widget::button;
use iced::{mouse, window, Element, Event, Length, Rectangle, Size, Theme, Vector};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle};

use crate::theme::CellTone;

/// Bytes in one pane record: nine native-endian 32-bit fields.
pub const RECORD_BYTES: usize = 36;
/// Most panes one `set_panes` may carry.
pub const MAX_PANES: usize = 64;

/// State bit: the pointer is over the pane.
pub const HOVERED: u32 = 1;
/// State bit: the pane is being pressed.
pub const PRESSED: u32 = 2;

/// One glass pane: a rounded box in surface-local logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pane {
    pub rect: Rectangle,
    /// Corner radius, logical px.
    pub radius: f32,
    /// Bezel width, logical px; 0 is the compositor's default.
    pub bezel: f32,
    pub tone: CellTone,
    /// [`HOVERED`] | [`PRESSED`].
    pub state: u32,
    /// Ground opacity, 0..=255.
    pub alpha: u8,
}

impl Pane {
    /// A pane at rest with the default bezel and a fully opaque ground.
    pub fn new(rect: Rectangle, radius: f32, tone: CellTone) -> Self {
        Self {
            rect,
            radius,
            bezel: 0.0,
            tone,
            state: 0,
            alpha: 255,
        }
    }

    /// The state bits of a button's status.
    pub fn status(mut self, status: button::Status) -> Self {
        self.state = match status {
            button::Status::Hovered => HOVERED,
            button::Status::Pressed => HOVERED | PRESSED,
            _ => 0,
        };
        self
    }

    pub fn bezel(mut self, bezel: f32) -> Self {
        self.bezel = bezel;
        self
    }

    pub fn alpha(mut self, alpha: u8) -> Self {
        self.alpha = alpha;
        self
    }
}

fn tone_code(tone: CellTone) -> u32 {
    match tone {
        CellTone::Plain => 0,
        CellTone::Focused => 1,
        CellTone::Away => 2,
        CellTone::Latent => 3,
    }
}

/// The panes of one frame, already packed for `set_panes`.
#[derive(Debug, Default)]
pub struct GlassPanes {
    bytes: Vec<u8>,
}

impl GlassPanes {
    pub fn clear(&mut self) {
        self.bytes.clear();
    }

    pub fn len(&self) -> usize {
        self.bytes.len() / RECORD_BYTES
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The packed records.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Append `pane`. An empty or non-finite box, or the 65th pane, is
    /// dropped; the compositor would reject it as a protocol error.
    pub fn push(&mut self, pane: &Pane) {
        if self.len() >= MAX_PANES {
            return;
        }
        let r = pane.rect;
        if ![r.x, r.y, r.width, r.height, pane.radius, pane.bezel]
            .iter()
            .all(|v| v.is_finite())
        {
            return;
        }
        let (w, h) = (r.width.round() as i32, r.height.round() as i32);
        if w <= 0 || h <= 0 {
            return;
        }
        let fields: [[u8; 4]; 9] = [
            (r.x.round() as i32).to_ne_bytes(),
            (r.y.round() as i32).to_ne_bytes(),
            w.to_ne_bytes(),
            h.to_ne_bytes(),
            (pane.radius.round().max(0.0) as u32).to_ne_bytes(),
            (pane.bezel.round().max(0.0) as u32).to_ne_bytes(),
            tone_code(pane.tone).to_ne_bytes(),
            (pane.state & (HOVERED | PRESSED)).to_ne_bytes(),
            u32::from(pane.alpha).to_ne_bytes(),
        ];
        for f in fields {
            self.bytes.extend_from_slice(&f);
        }
    }
}

// ---- protocol state ------------------------------------------------------

static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether the compositor is drawing glass panes right now. Draw code reads
/// this to decide whether to paint its own ground.
pub fn available() -> bool {
    AVAILABLE.load(Ordering::Relaxed)
}

struct Dispatcher;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Dispatcher {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<EclipseGlassManagerV1, ()> for Dispatcher {
    fn event(
        _: &mut Self,
        _: &EclipseGlassManagerV1,
        event: eclipse_glass_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let eclipse_glass_manager_v1::Event::Capability { available } = event {
            AVAILABLE.store(available != 0, Ordering::Relaxed);
        }
    }
}

impl Dispatch<EclipseGlassSurfaceV1, ()> for Dispatcher {
    fn event(
        _: &mut Self,
        _: &EclipseGlassSurfaceV1,
        _: eclipse_glass_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

struct Entry {
    glass: EclipseGlassSurfaceV1,
    /// What was last sent, so an unchanged frame sends nothing.
    last: Vec<u8>,
}

struct Shared {
    queue: EventQueue<Dispatcher>,
    manager: Option<EclipseGlassManagerV1>,
    surfaces: HashMap<window::Id, Entry>,
}

thread_local! {
    // The event loop, the hook and every draw run on one thread.
    static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) };
    static FRAME: RefCell<GlassPanes> = RefCell::new(GlassPanes::default());
}

/// Connect to the compositor, bind `eclipse_glass_manager_v1` if it is
/// offered to this client, and return the connection for the layer-shell
/// runtime to share (`Settings::with_connection`). `None` means no
/// connection could be made; the caller then runs with its own, unchanged.
///
/// Call on the thread that runs the event loop, before it starts.
pub fn bootstrap() -> Option<Connection> {
    let conn = Connection::connect_to_env().ok()?;
    let (globals, mut queue) = registry_queue_init::<Dispatcher>(&conn).ok()?;
    let qh = queue.handle();
    let manager = globals.bind::<EclipseGlassManagerV1, _, _>(&qh, 1..=1, ()).ok();
    if manager.is_some() {
        // The capability event rides the bind: take it before the first frame.
        let _ = queue.roundtrip(&mut Dispatcher);
    }
    SHARED.with(|s| {
        *s.borrow_mut() = Some(Shared {
            queue,
            manager,
            surfaces: HashMap::new(),
        });
    });
    Some(conn)
}

/// The `iced_layershell::set_surface_hook` callback: a window's layer surface
/// now exists, so it gets its glass object.
pub fn surface_created(id: window::Id, surface: &WlSurface) {
    SHARED.with(|s| {
        let mut s = s.borrow_mut();
        let Some(sh) = s.as_mut() else { return };
        let Some(manager) = sh.manager.as_ref() else {
            return;
        };
        let glass = manager.get_glass(surface, &sh.queue.handle(), ());
        sh.surfaces.insert(
            id,
            Entry {
                glass,
                last: Vec::new(),
            },
        );
    });
}

/// Add a pane to the frame being drawn. A no-op while glass is unavailable.
pub fn push(pane: Pane) {
    if !available() {
        return;
    }
    FRAME.with(|f| f.borrow_mut().push(&pane));
}

fn begin() {
    FRAME.with(|f| f.borrow_mut().clear());
}

/// Send the frame's panes for `id` if they changed. Runs in draw, ahead of
/// the present that commits the surface, so the compositor latches panes and
/// buffer together.
fn finish(id: window::Id) {
    SHARED.with(|s| {
        let mut s = s.borrow_mut();
        let Some(sh) = s.as_mut() else { return };
        // Pick up capability changes the shared connection delivered.
        let _ = sh.queue.dispatch_pending(&mut Dispatcher);
        let Some(entry) = sh.surfaces.get_mut(&id) else {
            return;
        };
        FRAME.with(|f| {
            let f = f.borrow();
            if entry.last == f.as_bytes() {
                return;
            }
            entry.last.clear();
            entry.last.extend_from_slice(f.as_bytes());
            entry.glass.set_panes(f.as_bytes().to_vec());
        });
    });
}

// ---- the root widget -----------------------------------------------------

/// Wrap a window's root element: clears the frame before the content draws
/// and sends what the content pushed once it has.
pub fn root<'a, Message>(
    id: window::Id,
    content: impl Into<Element<'a, Message, Theme>>,
) -> Root<'a, Message> {
    Root {
        id,
        content: content.into(),
    }
}

/// See [`root`].
pub struct Root<'a, Message> {
    id: window::Id,
    content: Element<'a, Message, Theme>,
}

impl<Message> Widget<Message, Theme, iced::Renderer> for Root<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, clipboard, shell, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        begin();
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
        finish(self.id);
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &iced::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, iced::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

impl<'a, Message: 'a> From<Root<'a, Message>> for Element<'a, Message, Theme> {
    fn from(root: Root<'a, Message>) -> Self {
        Element::new(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_ne_bytes(b[i * 4..i * 4 + 4].try_into().unwrap())
    }

    #[test]
    fn a_record_is_nine_fields_in_protocol_order() {
        let mut p = GlassPanes::default();
        let pane = Pane::new(
            Rectangle::new(iced::Point::new(-3.0, 4.4), Size::new(100.4, 28.0)),
            14.0,
            CellTone::Away,
        )
        .bezel(2.0)
        .alpha(200)
        .status(button::Status::Pressed);
        p.push(&pane);
        let b = p.as_bytes();
        assert_eq!(b.len(), RECORD_BYTES);
        assert_eq!(i32::from_ne_bytes(b[0..4].try_into().unwrap()), -3);
        assert_eq!(u32_at(b, 1), 4);
        assert_eq!(u32_at(b, 2), 100);
        assert_eq!(u32_at(b, 3), 28);
        assert_eq!(u32_at(b, 4), 14);
        assert_eq!(u32_at(b, 5), 2);
        assert_eq!(u32_at(b, 6), 2);
        assert_eq!(u32_at(b, 7), HOVERED | PRESSED);
        assert_eq!(u32_at(b, 8), 200);
    }

    #[test]
    fn tones_follow_the_protocol_enum() {
        assert_eq!(
            [
                CellTone::Plain,
                CellTone::Focused,
                CellTone::Away,
                CellTone::Latent
            ]
            .map(tone_code),
            [0, 1, 2, 3]
        );
    }

    #[test]
    fn status_sets_state_bits() {
        let r = Rectangle::new(iced::Point::ORIGIN, Size::new(1.0, 1.0));
        let s = |st| Pane::new(r, 0.0, CellTone::Plain).status(st).state;
        assert_eq!(s(button::Status::Active), 0);
        assert_eq!(s(button::Status::Hovered), HOVERED);
        assert_eq!(s(button::Status::Pressed), HOVERED | PRESSED);
    }

    #[test]
    fn the_collector_caps_at_sixty_four_and_drops_empty_boxes() {
        let mut p = GlassPanes::default();
        let ok = Pane::new(
            Rectangle::new(iced::Point::ORIGIN, Size::new(10.0, 10.0)),
            4.0,
            CellTone::Plain,
        );
        for _ in 0..80 {
            p.push(&ok);
        }
        assert_eq!(p.len(), MAX_PANES);
        assert_eq!(p.as_bytes().len(), MAX_PANES * RECORD_BYTES);
        let mut q = GlassPanes::default();
        q.push(&Pane {
            rect: Rectangle::new(iced::Point::ORIGIN, Size::new(0.0, 10.0)),
            ..ok
        });
        q.push(&Pane {
            rect: Rectangle::new(iced::Point::ORIGIN, Size::new(-5.0, 10.0)),
            ..ok
        });
        q.push(&Pane {
            radius: f32::NAN,
            ..ok
        });
        assert!(q.is_empty());
    }

    #[test]
    fn clear_keeps_the_buffer() {
        let mut p = GlassPanes::default();
        let ok = Pane::new(
            Rectangle::new(iced::Point::ORIGIN, Size::new(10.0, 10.0)),
            4.0,
            CellTone::Plain,
        );
        p.push(&ok);
        let cap = p.bytes.capacity();
        p.clear();
        assert!(p.is_empty());
        assert_eq!(p.bytes.capacity(), cap);
    }
}
