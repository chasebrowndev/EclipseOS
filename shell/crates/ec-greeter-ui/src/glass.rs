// SPDX-License-Identifier: AGPL-3.0-only
//! Compositor-drawn glass panes for the greeter's plain xdg window
//! (`eclipse_glass_v1`, COMP-02 section 9).
//!
//! The compositor draws a pane's ground (blur, bent backdrop, lit rim, shadow)
//! under this window's surface; the client draws only the content. This is the
//! minimum of `ec-ui`'s glass client a plain window needs, copied because the
//! greeter's crate may not depend on the desktop widget crate (ADR 0126). The
//! wire is `ec_protocols`'s; keep the two copies in step.
//!
//! Fail-soft: without the manager global (a host that is not abyss, a client
//! not allow-listed, a blur mode other than glass) [`available`] is false,
//! [`push`] does nothing, and the view paints its own opaque ground.
//!
//! Wiring: subscribe to [`xdg_window`] and answer its id with
//! [`attach_window`]; wrap the root in [`root_sole`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};

use ec_protocols::glass::client::eclipse_glass_manager_v1::{self, EclipseGlassManagerV1};
use ec_protocols::glass::client::eclipse_glass_surface_v1::{self, EclipseGlassSurfaceV1};
use iced::advanced::layout::{self, Layout};
use iced::advanced::overlay;
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Operation, Tree, Widget};
use iced::advanced::{Clipboard, Shell};
use iced::{mouse, window, Element, Event, Length, Rectangle, Size, Subscription, Task, Theme, Vector};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};

/// Bytes in one pane record: nine native-endian 32-bit fields.
pub const RECORD_BYTES: usize = 36;
/// Most panes one `set_panes` may carry.
pub const MAX_PANES: usize = 64;

/// State bit: the pointer is over the pane.
pub const HOVERED: u32 = 1;
/// State bit: the pane is being pressed.
pub const PRESSED: u32 = 2;

/// A pane's look, as the compositor knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// A card or a cell at rest.
    Plain,
    /// The focused cell: a brighter fill and a crisp rim, never a glow.
    Focused,
}

impl Tone {
    fn code(self) -> u32 {
        match self {
            Tone::Plain => 0,
            Tone::Focused => 1,
        }
    }
}

/// One glass pane: a rounded box in surface-local logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pane {
    pub rect: Rectangle,
    /// Corner radius, logical px.
    pub radius: f32,
    pub tone: Tone,
    /// [`HOVERED`] | [`PRESSED`].
    pub state: u32,
    /// Ground opacity, 0..=255.
    pub alpha: u8,
}

impl Pane {
    pub fn new(rect: Rectangle, radius: f32, tone: Tone) -> Self {
        Self {
            rect,
            radius,
            tone,
            state: 0,
            alpha: 255,
        }
    }
}

/// The panes of one frame, packed for `set_panes`.
#[derive(Debug, Default)]
struct GlassPanes {
    bytes: Vec<u8>,
    /// Panes refused for want of room this frame.
    dropped: usize,
}

impl GlassPanes {
    fn clear(&mut self) {
        self.bytes.clear();
        self.dropped = 0;
    }

    fn len(&self) -> usize {
        self.bytes.len() / RECORD_BYTES
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Append `pane`. An empty or non-finite box, or the 65th pane, is dropped;
    /// the compositor would reject it as a protocol error.
    fn push(&mut self, pane: &Pane) {
        if self.len() >= MAX_PANES {
            self.dropped += 1;
            return;
        }
        let r = pane.rect;
        if ![r.x, r.y, r.width, r.height, pane.radius]
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
            0u32.to_ne_bytes(),
            pane.tone.code().to_ne_bytes(),
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
static XDG_DONE: AtomicBool = AtomicBool::new(false);
static SENT: AtomicBool = AtomicBool::new(false);

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
    /// Keeps a foreign (winit) display's wrapper alive for the queue below.
    #[allow(dead_code)]
    conn: Option<Connection>,
    queue: EventQueue<Dispatcher>,
    surfaces: HashMap<window::Id, Entry>,
}

thread_local! {
    // The event loop, the draw and every push run on one thread. Never dropped:
    // the process is going away and the compositor reaps the objects with the
    // socket, so no proxy is destroyed after winit has closed its display.
    static SHARED: RefCell<Option<ManuallyDrop<Shared>>> = const { RefCell::new(None) };
    static FRAME: RefCell<GlassPanes> = RefCell::new(GlassPanes::default());
    // The fade of the subtree being drawn ([`with_fade`]): ground opacity and a
    // vertical offset, applied to every pane pushed beneath it.
    static FADE: Cell<(f32, f32)> = const { Cell::new((1.0, 0.0)) };
}

/// Add a pane to the frame being drawn. A no-op while glass is unavailable.
pub fn push(mut pane: Pane) {
    if !available() {
        return;
    }
    let (alpha, dy) = FADE.with(|f| f.get());
    if alpha <= 0.0 {
        return;
    }
    pane.alpha = (f32::from(pane.alpha) * alpha.min(1.0)).round() as u8;
    pane.rect.y += dy;
    FRAME.with(|f| f.borrow_mut().push(&pane));
}

/// Run `draw` with every pane it pushes faded to `alpha` (`0.0..=1.0`) and
/// moved down by `dy`, nested fades multiplying. The compositor's half of a
/// subtree that fades in.
pub fn with_fade<R>(alpha: f32, dy: f32, draw: impl FnOnce() -> R) -> R {
    let prev = FADE.with(|f| f.get());
    FADE.with(|f| f.set((prev.0 * alpha.clamp(0.0, 1.0), prev.1 + dy)));
    let out = draw();
    FADE.with(|f| f.set(prev));
    out
}

fn begin() {
    FRAME.with(|f| f.borrow_mut().clear());
}

/// Send the frame's panes if they changed. Runs in draw, ahead of the present
/// that commits the surface, so the compositor latches panes and buffer together.
fn finish() {
    SHARED.with(|s| {
        let mut s = s.borrow_mut();
        let Some(sh) = s.as_mut() else { return };
        // Pick up capability changes the shared connection delivered.
        let _ = sh.queue.dispatch_pending(&mut Dispatcher);
        // A single-window app: the one surface is the one meant.
        let Some(entry) = sh.surfaces.values_mut().next() else {
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
            if !SENT.swap(true, Ordering::Relaxed) {
                eprintln!("ec-greeter-ui: first set_panes sent, {} panes", f.len());
            }
        });
        if let Some(c) = sh.conn.as_ref() {
            let _ = c.flush();
        }
    });
}

// ---- xdg windows (plain iced, no layer shell) ----------------------------

/// A subscription that yields the window id until [`attach_window`] has run
/// once. Map it to a message and answer that with `attach_window(id)`.
pub fn xdg_window() -> Subscription<window::Id> {
    iced::event::listen_with(|_event, _status, id| (!XDG_DONE.load(Ordering::Relaxed)).then_some(id))
}

/// Bind the glass manager for window `id` through its raw `wl_display` and
/// `wl_surface`, on a private queue of winit's own connection. Fail-soft: any
/// failure leaves [`available`] false. The first frame is drawn before this
/// runs, so panes land one frame late; the task's answer makes iced redraw.
pub fn attach_window(id: window::Id) -> Task<bool> {
    if XDG_DONE.swap(true, Ordering::Relaxed) {
        return Task::none();
    }
    window::run(id, move |w| attach_raw(id, w))
}

fn attach_raw(id: window::Id, w: &dyn window::Window) -> bool {
    use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
    let (Ok(d), Ok(s)) = (w.display_handle(), w.window_handle()) else {
        return false;
    };
    let (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(s)) = (d.as_raw(), s.as_raw()) else {
        return false;
    };
    // SAFETY: both pointers come from winit through raw-window-handle for this
    // process's own window, and are live while `window::run`'s closure executes;
    // the display outlives the window. `from_ptr` checks the interface and
    // rejects a pointer that is not a wl_surface.
    let (conn, surface) = unsafe {
        let backend = wayland_backend::client::Backend::from_foreign_display(d.display.as_ptr().cast());
        let conn = Connection::from_backend(backend);
        let Ok(oid) =
            wayland_backend::client::ObjectId::from_ptr(WlSurface::interface(), s.surface.as_ptr().cast())
        else {
            return false;
        };
        let Ok(surface) = WlSurface::from_id(&conn, oid) else {
            return false;
        };
        (conn, surface)
    };
    let Ok((globals, mut queue)) = registry_queue_init::<Dispatcher>(&conn) else {
        return false;
    };
    let qh = queue.handle();
    let Ok(manager) = globals.bind::<EclipseGlassManagerV1, _, _>(&qh, 1..=1, ()) else {
        return false;
    };
    let _ = queue.roundtrip(&mut Dispatcher);
    let glass = manager.get_glass(&surface, &qh, ());
    let mut surfaces = HashMap::new();
    surfaces.insert(
        id,
        Entry {
            glass,
            last: Vec::new(),
        },
    );
    SHARED.with(|sh| {
        *sh.borrow_mut() = Some(ManuallyDrop::new(Shared {
            conn: Some(conn),
            queue,
            surfaces,
        }));
    });
    let on = available();
    eprintln!("ec-greeter-ui: eclipse_glass_v1 bound, compositor capability: {on}");
    on
}

// ---- the root widget -----------------------------------------------------

/// Wrap the window's root element: clears the frame before the content draws
/// and sends what the content pushed once it has. `animating` keeps the window
/// redrawing frame after frame, so the springs advance; iced does not redraw
/// on its own between events.
pub fn root_sole<'a, Message>(
    content: impl Into<Element<'a, Message, Theme>>,
    animating: bool,
) -> Root<'a, Message> {
    Root {
        content: content.into(),
        animating,
    }
}

/// See [`root_sole`].
pub struct Root<'a, Message> {
    content: Element<'a, Message, Theme>,
    animating: bool,
}

impl<'a, Message: 'a> From<Root<'a, Message>> for Element<'a, Message, Theme> {
    fn from(root: Root<'a, Message>) -> Self {
        Element::new(root)
    }
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
        if self.animating && matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            shell.request_redraw();
        }
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
        finish();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_ne_bytes(b[i * 4..i * 4 + 4].try_into().unwrap())
    }

    #[test]
    fn a_record_is_nine_fields_in_protocol_order() {
        let mut p = GlassPanes::default();
        let mut pane = Pane::new(
            Rectangle::new(iced::Point::new(-3.0, 4.4), Size::new(100.4, 28.0)),
            14.0,
            Tone::Focused,
        );
        pane.state = HOVERED | PRESSED;
        pane.alpha = 200;
        p.push(&pane);
        let b = p.as_bytes();
        assert_eq!(b.len(), RECORD_BYTES);
        assert_eq!(i32::from_ne_bytes(b[0..4].try_into().unwrap()), -3);
        assert_eq!(u32_at(b, 1), 4);
        assert_eq!(u32_at(b, 2), 100);
        assert_eq!(u32_at(b, 3), 28);
        assert_eq!(u32_at(b, 4), 14);
        assert_eq!(u32_at(b, 5), 0);
        assert_eq!(u32_at(b, 6), 1);
        assert_eq!(u32_at(b, 7), HOVERED | PRESSED);
        assert_eq!(u32_at(b, 8), 200);
    }

    #[test]
    fn the_collector_caps_and_drops_empty_boxes() {
        let mut p = GlassPanes::default();
        let ok = Pane::new(
            Rectangle::new(iced::Point::ORIGIN, Size::new(10.0, 10.0)),
            4.0,
            Tone::Plain,
        );
        for _ in 0..80 {
            p.push(&ok);
        }
        assert_eq!(p.len(), MAX_PANES);
        assert_eq!(p.dropped, 80 - MAX_PANES);
        let mut q = GlassPanes::default();
        q.push(&Pane {
            rect: Rectangle::new(iced::Point::ORIGIN, Size::new(0.0, 10.0)),
            ..ok
        });
        q.push(&Pane {
            radius: f32::NAN,
            ..ok
        });
        assert_eq!(q.len(), 0);
    }
}
