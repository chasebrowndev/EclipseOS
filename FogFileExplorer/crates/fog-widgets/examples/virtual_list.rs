// SPDX-License-Identifier: AGPL-3.0-only

//! 100 000-row [`fog_widgets::VirtualList`] demo.
//!
//! Run from `FogFileExplorer/`:
//!
//! ```sh
//! cargo run -p fog-widgets --example virtual_list            # 100 000 rows
//! cargo run -p fog-widgets --example virtual_list -- 1000000 # any count
//! ```
//!
//! Keys: `j`/`k` or arrows move the selection (kept in view with
//! `scroll_into_view`), `d`/`u` or PageDown/PageUp move a page, `g`/`G` jump
//! to the first/last row. The wheel and the scrollbar scroll freely.
//!
//! `--tour` drives it without input, for screenshots: from its first tick,
//! every 16 ms it scrolls down 1000 px (about 42 rows, more than a screen plus
//! overscan, so every frame rebuilds) via `scroll_by`; after 10 s it jumps
//! the selection to the last row. A blank band in any frame would mean rows
//! were materialized too late.
//!
//! ```sh
//! cargo run -p fog-widgets --example virtual_list -- --tour
//! ```
//!
//! Colours are local constants: this is a crate demo, not a themed Fog pane
//! (theming is M2).

use std::time::{Duration, Instant};

use iced::keyboard::{self, key::Named, Key};
use iced::widget::operation::{scroll_by, AbsoluteOffset};
use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, text, Id};
use iced::{Background, Color, Element, Font, Length, Subscription, Task, Theme};

use fog_widgets::{scroll_into_view, virtual_list};

const DEFAULT_ROWS: usize = 100_000;
const ROW_HEIGHT: f32 = 24.0;
const PAGE_ROWS: usize = 20;
const TEXT_SIZE: f32 = 13.0;
const PAD_X: f32 = 12.0;
const HEADER_PAD: f32 = 10.0;
const INDEX_WIDTH: f32 = 80.0;
const SIZE_WIDTH: f32 = 96.0;
const TOUR_TICK: Duration = Duration::from_millis(16);
const TOUR_FLING: f32 = 1000.0;
const TOUR_FLING_FOR: Duration = Duration::from_secs(10);

const BG: Color = Color::from_rgb8(0x0b, 0x0b, 0x0d);
const BG_ALT: Color = Color::from_rgb8(0x10, 0x10, 0x13);
const RULE: Color = Color::from_rgb8(0x22, 0x22, 0x26);
const FG: Color = Color::from_rgb8(0xd8, 0xd4, 0xc8);
const DIM: Color = Color::from_rgb8(0x6a, 0x66, 0x5c);
const GOLD: Color = Color::from_rgb8(0xe0, 0xb0, 0x40);

fn list_id() -> Id {
    Id::new("virtual-list-demo")
}

struct Demo {
    len: usize,
    selected: usize,
    tour: bool,
    tour_start: Option<Instant>,
}

#[derive(Debug, Clone)]
enum Message {
    Key(keyboard::Event),
    Tour(Instant),
}

fn boot() -> Demo {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let len = args
        .iter()
        .find_map(|a| a.parse().ok())
        .unwrap_or(DEFAULT_ROWS);
    let tour = args.iter().any(|a| a == "--tour");
    Demo {
        len,
        selected: 0,
        tour,
        tour_start: None,
    }
}

fn update(demo: &mut Demo, message: Message) -> Task<Message> {
    let key = match message {
        Message::Key(keyboard::Event::KeyPressed { key, .. }) => key,
        Message::Key(_) => return Task::none(),
        Message::Tour(now) => {
            if !demo.tour {
                return Task::none();
            }
            let start = *demo.tour_start.get_or_insert(now);
            if now.duration_since(start) < TOUR_FLING_FOR {
                return scroll_by(
                    list_id(),
                    AbsoluteOffset {
                        x: 0.0,
                        y: TOUR_FLING,
                    },
                );
            }
            demo.tour = false;
            demo.selected = demo.len.saturating_sub(1);
            return scroll_into_view(list_id(), demo.selected, ROW_HEIGHT);
        }
    };
    let last = demo.len.saturating_sub(1);
    let s = demo.selected;
    let next = match key.as_ref() {
        Key::Character("j") | Key::Named(Named::ArrowDown) => (s + 1).min(last),
        Key::Character("k") | Key::Named(Named::ArrowUp) => s.saturating_sub(1),
        Key::Character("d") | Key::Named(Named::PageDown) => (s + PAGE_ROWS).min(last),
        Key::Character("u") | Key::Named(Named::PageUp) => s.saturating_sub(PAGE_ROWS),
        Key::Character("g") | Key::Named(Named::Home) => 0,
        Key::Character("G") | Key::Named(Named::End) => last,
        _ => return Task::none(),
    };
    demo.selected = next;
    scroll_into_view(list_id(), next, ROW_HEIGHT)
}

fn view(demo: &Demo) -> Element<'_, Message> {
    // The live count never wraps; the title yields and clips at narrow widths.
    let header = container(row![
        container(
            text("fog-widgets / virtual_list")
                .size(TEXT_SIZE)
                .wrapping(Wrapping::None)
                .color(DIM),
        )
        .width(Length::Fill)
        .clip(true),
        text(format!("row {} / {}", demo.selected + 1, demo.len))
            .size(TEXT_SIZE)
            .wrapping(Wrapping::None)
            .color(GOLD),
    ])
    .padding([HEADER_PAD, PAD_X])
    .width(Length::Fill)
    .style(|_| container::Style {
        background: Some(Background::Color(BG)),
        border: iced::Border {
            color: RULE,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    });

    // The builder runs during layout, only for materialized rows.
    let selected = demo.selected;
    let list =
        virtual_list(demo.len, ROW_HEIGHT, move |i| demo_row(i, i == selected)).id(list_id());

    container(column![header, list])
        .style(|_| container::Style {
            background: Some(Background::Color(BG)),
            text_color: Some(FG),
            ..Default::default()
        })
        .into()
}

fn demo_row(i: usize, selected: bool) -> Element<'static, Message> {
    let (fg, dim) = if selected { (BG, BG) } else { (FG, DIM) };
    let bg = if selected {
        GOLD
    } else if i % 2 == 0 {
        BG
    } else {
        BG_ALT
    };
    // Deterministic fake sizes so rows differ without any I/O.
    let size = (i as u64).wrapping_mul(2_654_435_761) % 10_000_000;

    container(
        row![
            text(format!("{i:>7}"))
                .size(TEXT_SIZE)
                .color(dim)
                .width(INDEX_WIDTH),
            container(
                text(format!("file-{i:06}.txt"))
                    .size(TEXT_SIZE)
                    .wrapping(Wrapping::None)
                    .color(fg),
            )
            .width(Length::Fill)
            .clip(true),
            text(format!("{size:>10}"))
                .size(TEXT_SIZE)
                .color(dim)
                .width(SIZE_WIDTH)
                .align_x(iced::alignment::Horizontal::Right),
        ]
        .align_y(iced::alignment::Vertical::Center)
        .height(Length::Fill),
    )
    .padding([0.0, PAD_X])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(bg)),
        ..Default::default()
    })
    .into()
}

fn subscription(demo: &Demo) -> Subscription<Message> {
    let keys = keyboard::listen().map(Message::Key);
    if demo.tour {
        Subscription::batch([keys, iced::time::every(TOUR_TICK).map(Message::Tour)])
    } else {
        keys
    }
}

fn main() -> iced::Result {
    iced::application(boot, update, view)
        .title("fog virtual list")
        .subscription(subscription)
        .theme(|_: &Demo| Theme::Dark)
        .default_font(Font::MONOSPACE)
        .window_size((720.0, 540.0))
        .run()
}
