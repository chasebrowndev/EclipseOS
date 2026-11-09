// SPDX-License-Identifier: AGPL-3.0-only
//! The Widgets page's hero: the bar as it will draw (the canvas), the status
//! line that says what the last move did, and the shelf of widgets that are
//! not on it. Every widget, on the bar, on the shelf and in hand, is drawn as
//! the bar draws it: what is picked up is exactly what lands.

use super::*;
use ec_ui::widget::{pager_tile, shelf_tile, ShellFrame};
use iced::widget::{column, row};

/// A still reading for a widget's core, drawn from the bar's primitives.
pub(super) fn core_of<'a>(app: &App, id: &str) -> Element<'a, Message, Theme> {
    const LEVELS: [f32; bar::VIZ_BANDS] = [
        0.35, 0.6, 0.8, 0.55, 0.7, 0.9, 0.5, 0.65, 0.4, 0.75, 0.55, 0.3, 0.45, 0.6, 0.35, 0.25,
    ];
    let k = knobs(app);
    let g = &app.bar.glyphs;
    let icon = |p: &Option<std::path::PathBuf>| mark(p.clone(), bar::MARK, color::text());
    match id {
        "now-playing" => {
            let mut r = Row::new().spacing(bar::GAP).align_y(Alignment::Center);
            if k.art {
                r = r.push(art_thumb(None, false));
            }
            r = r.push(track_label("Midnight City", "M83", bar::MEDIA_TEXT_W));
            if k.visualizer {
                r = r.push(viz_bars(&LEVELS, false));
            }
            r.into()
        }
        "system-usage" => mini_meter("CPU", 0.42, false),
        "volume" => icon(&g.volume),
        "network" => icon(&g.network),
        "bluetooth" => icon(&g.bluetooth),
        bp::HISTORY => icon(&g.bell),
        "battery" => row![
            battery_gauge(84, color::text_secondary()),
            tag(bp::BATTERY_READING, theme::text_primary)
        ]
        .spacing(bar::GAP)
        .align_y(Alignment::Center)
        .into(),
        "tray" => {
            // The first two pinned apps' own icons, as the bar shows them.
            let t = app.tray();
            let pinned = t.taskbar();
            let slot = |i: usize| {
                let art = pinned
                    .get(i)
                    .and_then(|id| app.tray_art.iter().find(|(x, _)| x == id))
                    .map_or(Art::None, |(_, a)| a.clone());
                let tint = if matches!(art, Art::None) {
                    color::text_tertiary()
                } else {
                    color::text_secondary()
                };
                container(art_mark(&art, bar::MARK, tint))
                    .width(Length::Fixed(bar::TRAY_MARK_W))
                    .align_x(Alignment::Center)
            };
            row![slot(0), slot(1)].align_y(Alignment::Center).into()
        }
        "clock" => container(tag(
            if flag(app, "bar.clock.hour-12", true) {
                "9:41 PM"
            } else {
                "21:41"
            },
            theme::text_primary,
        ))
        .width(Length::Fixed(bar::CLOCK_W))
        .align_x(Alignment::Center)
        .into(),
        custom => row![
            mark(None, bar::MARK, color::text_secondary()),
            tag(&elide(&bp::title(custom), bp::CUSTOM_CHARS), theme::text_primary),
        ]
        .spacing(bar::GAP)
        .align_y(Alignment::Center)
        .into(),
    }
}

/// A widget whole and open, as a shelf shows it: its own shell, grip and
/// all, at its core width.
fn cell_picture<'a>(app: &App, id: &str) -> Element<'a, Message, Theme> {
    let span = bp::span(id, &knobs(app));
    widget_shell_rounded(
        drag_bar::<Message>().state(Grip::Rest),
        core_of(app, id),
        None,
        ec_ui::widget::ShellSpan {
            core: span.core,
            revealed: 0.0,
        },
        ShellFrame {
            open: 1.0,
            reveal: 0.0,
            presence: 1.0,
        },
        widget_radius(app),
    )
}

fn window_chip<'a>(i: usize, w: f32) -> Element<'a, Message, Theme> {
    let (app_name, title) = bp::WINDOWS[i % bp::WINDOWS.len()];
    let glyph = || mark(None, bar::MARK, color::text_secondary());
    let label = |t: &str| {
        text(t.to_owned())
            .font(font::UI)
            .size(size::BODY_SMALL)
            .style(theme::text_primary)
            .wrapping(text::Wrapping::None)
    };
    let body: Element<'a, Message, Theme> = match bp::Rung::of(w) {
        bp::Rung::Full => row![glyph(), label(title)]
            .spacing(bar::GAP * 2.0)
            .align_y(Alignment::Center)
            .into(),
        bp::Rung::Name => row![glyph(), label(app_name)]
            .spacing(bar::GAP * 2.0)
            .align_y(Alignment::Center)
            .into(),
        bp::Rung::Icon => container(glyph())
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .into(),
        bp::Rung::Bare => Space::new().into(),
    };
    bar_cell_frame(body, w, false)
}

/// The pager the mode draws: a row of workspace tiles, or the desktop grid.
fn pager<'a>(mode: Mode) -> Element<'a, Message, Theme> {
    let p = mode.pager();
    let mut col = Column::new().spacing(p.gap);
    for r in 0..p.rows {
        let mut line = Row::new().spacing(p.gap);
        for c in 0..p.columns {
            let i = r * p.columns + c;
            line = line.push(pager_tile(i + 1, p.tile.0, p.tile.1, i == canvas::PREVIEW_ACTIVE));
        }
        col = col.push(line);
    }
    col.into()
}

/// The bar, drawn. Every cell is a layer at an animated x.
fn picture(app: &App) -> Element<'_, Message, Theme> {
    let b = &app.bar;
    let mode = mode(app);
    let sel = selected(app);
    let lifted_id = b.lift.as_ref().map(|l| l.id.clone());
    let marked = lifted_id.clone().or(sel.clone());
    let radius = widget_radius(app);
    let away = |id: &str| b.away(&Payload::Cell(id.to_owned()));
    let mark_cell = b
        .cells
        .iter()
        .find(|c| marked.as_deref() == Some(c.id.as_str()) && !away(&c.id) && c.presence.value() > 0.0);

    // The selection's ground, under every cell: the focused cell's brighter
    // glass. Always the first layer, zero-wide when nothing is selected, so
    // a selection coming and going never moves a cell's place in the tree
    // (iced keeps a press there).
    let (gx, gw) = mark_cell.map_or((0.0, 0.0), |c| (c.x.value(), cell_w(c)));
    let mut layers: Vec<Element<'_, Message, Theme>> = vec![placed(
        gx,
        ec_ui::widget::quad(
            Length::Fixed(gw),
            Length::Fixed(bar::WIDGET_H),
            color::cell_focus(),
            radius,
        ),
    )];
    layers.push(placed(
        0.0,
        bar_cell_frame(
            container(ring(bar::MARK, bar::RING, color::text_secondary()))
                .width(Length::Fill)
                .align_x(Alignment::Center),
            bar::TASK_MIN,
            false,
        ),
    ));
    layers.push(placed(bp::CHIPS_X, pager(mode)));
    let start = mode.chips_x();
    let mut x = start;
    for (i, c) in b.chips.iter().enumerate() {
        let w = c.w.value();
        if w >= 1.0 {
            layers.push(placed(x, window_chip(i, w)));
            x += w + bar::GAP;
        }
    }
    let plus_w = b.plus_w.value();
    if plus_w >= 1.0 {
        layers.push(placed(
            x,
            bar_cell_frame(
                container(tag(&format!("+{}", b.plus_count), theme::text_secondary))
                    .width(Length::Fill)
                    .align_x(Alignment::Center),
                plus_w,
                false,
            ),
        ));
    }
    for c in &b.cells {
        let presence = c.presence.value().clamp(0.0, 1.0);
        let x = c.x.value();
        // The cell in hand (or flying home) leaves its place empty: an
        // outline where it was, so the drop reads as moving it out of that
        // hole. The draggable stays in the same layer, so iced keeps the
        // press it is holding.
        let body: Element<'_, Message, Theme> = if away(&c.id) {
            outline(
                cell_w(c),
                bar::WIDGET_H,
                bar::HAIRLINE,
                color::border_strong(),
                radius,
            )
        } else if c.cell.grip() {
            widget_shell_rounded(
                drag_bar::<Message>().state(Grip::Rest),
                core_of(app, &c.id),
                None,
                drawn_span(c),
                c.cell.frame(c.extent.value(), presence),
                radius,
            )
        } else {
            container(
                container(core_of(app, &c.id))
                    .width(Length::Fixed(c.cell.span.core))
                    .align_y(Alignment::Center),
            )
            .padding([0.0, bar::WIDGET_X])
            .width(Length::Fixed(cell_w(c)))
            .height(Length::Fixed(bar::WIDGET_H))
            .align_y(Alignment::Center)
            .clip(true)
            .into()
        };
        // The history bell is the bar's, not the owner's: drawn, never
        // picked up.
        if c.id == bp::HISTORY {
            layers.push(placed(x, body));
            continue;
        }
        let what = Payload::Cell(c.id.clone());
        let (lifted, homing) = b.ghost_of(&what);
        layers.push(placed(
            x,
            draggable(body)
                .ghost(drag_ghost(core_of(app, &c.id), radius))
                .lifted(lifted)
                .homing(homing)
                .on_move(move |p| bar_msg(Msg::Move(what.clone(), p)))
                .on_release(|p| bar_msg(Msg::Release(p)))
                .on_cancel(bar_msg(Msg::Cancel))
                .on_click(bar_msg(Msg::Select(c.id.clone()))),
        ));
    }
    // The selection's crisp rim, and the drop mark, above every cell and
    // after them in the stack.
    if let Some(c) = mark_cell {
        layers.push(placed(
            c.x.value(),
            outline(
                cell_w(c),
                bar::WIDGET_H,
                bar::HAIRLINE,
                color::cell_focus_rim(),
                radius,
            ),
        ));
    }
    if let Some((_, Target::Bar(slot))) = aimed(app) {
        let spans = b.spans(&order(app));
        let gap = |i: usize| match (i.checked_sub(1).and_then(|j| spans.get(j)), spans.get(i)) {
            (_, Some((x, _))) if i == 0 => x - bar::GAP / 2.0,
            (Some((x, w)), _) => x + w + bar::GAP / 2.0,
            (None, _) => 0.0,
        };
        layers.push(placed(
            gap(slot) - canvas::DROP_MARK_W / 2.0,
            edge_quad(
                Length::Fixed(canvas::DROP_MARK_W),
                Length::Fixed(bar::WIDGET_H),
                color::text(),
            ),
        ));
    }
    // The sheet is the bar's width, never less than its narrowest: a pane
    // narrower than that scrolls the picture sideways instead of drawing
    // the bar overlapping itself. The sensor measures the room, not the
    // sheet, so the picture follows the pane both ways.
    let sheet = bar_sheet(
        Stack::with_children(layers)
            .width(Length::Fill)
            .height(Length::Fill),
        Length::Fixed(b.inner() + 2.0 * bar::EDGE),
    );
    let sheet = drop_zone(sheet, |r| bar_msg(Msg::Zone(Zone::Sheet, r)));
    sensor(
        iced::widget::scrollable(sheet)
            .direction(iced::widget::scrollable::Direction::Horizontal(
                iced::widget::scrollable::Scrollbar::new()
                    .width(space::SCROLLBAR_W)
                    .scroller_width(space::SCROLLBAR_W),
            ))
            .style(theme::eclipse_scrollable)
            .width(Length::Fill),
    )
    .on_show(|s| bar_msg(Msg::Measured(s)))
    .on_resize(|s| bar_msg(Msg::Measured(s)))
    .into()
}

/// What the picture is showing, in words.
fn readout(app: &App) -> String {
    let b = &app.bar;
    let Some(s) = &b.solved else {
        return String::new();
    };
    let mode = mode(app);
    let mut parts = vec![format!("{} mode", mode.as_str())];
    if mode.shows_chips() {
        parts.push(match b.windows {
            1 => "1 window".to_owned(),
            n => format!("{n} windows"),
        });
    }
    parts.push(match s.compressed {
        0 => "every widget open".to_owned(),
        1 => "1 folded to its grip".to_owned(),
        n => format!("{n} folded to their grips"),
    });
    if s.overflow > 0 {
        parts.push(format!("+{} in overflow", s.overflow));
    }
    parts.join(" \u{b7} ")
}

/// A widget off the bar, on the shelf: its real cell, to pick up or click.
fn shelf_item<'a>(app: &'a App, id: String, name: &str) -> Element<'a, Message, Theme> {
    let what = Payload::Pick(id.clone());
    let (lifted, homing) = app.bar.ghost_of(&what);
    let tile = shelf_tile(cell_picture(app, &id), name, false, bar_msg(Msg::Add(id.clone())));
    // Its place stays, veiled, while it is in hand: the shelf does not
    // reflow under the pointer.
    let tile = ec_ui::widget::dimmed(tile, app.bar.away(&what));
    draggable(tile)
        .ghost(drag_ghost(core_of(app, &id), widget_radius(app)))
        .lifted(lifted)
        .homing(homing)
        .on_move(move |p| bar_msg(Msg::Move(what.clone(), p)))
        .on_release(|p| bar_msg(Msg::Release(p)))
        .on_cancel(bar_msg(Msg::Cancel))
        .on_click(bar_msg(Msg::Add(id)))
        .into()
}

/// The widgets not on the bar, and the place a bar cell is dropped to take
/// it off. Its tiles wrap, so it fits any width.
fn shelf(app: &App) -> Element<'_, Message, Theme> {
    let o = order(app);
    let mut tiles: Vec<Element<'_, Message, Theme>> = Vec::new();
    for id in bp::BUILTINS.iter().filter(|id| !o.iter().any(|x| x == *id)) {
        tiles.push(shelf_item(app, (*id).to_owned(), &bp::title(id)));
    }
    let on = crate::addons::taskbar_widgets_on(app);
    let customs = app.bar.customs.iter().map(|w| w.name.as_str());
    let withheld = app
        .bar
        .statuses
        .iter()
        .filter(|s| s.approval == Approval::Pending)
        .map(|s| s.name.as_str());
    for name in customs.chain(withheld).filter(|_| on) {
        let id = format!("{}{}", bp::CUSTOM, name);
        if !o.contains(&id) {
            tiles.push(shelf_item(app, id, name));
        }
    }
    let held = match &app.bar.drag {
        Some(Drag {
            what: Payload::Cell(id),
            ..
        }) => Some(id.as_str()),
        _ => None,
    };
    let state = match (held, aimed(app)) {
        (Some(_), Some((_, Target::Picker))) => Well::Hot,
        (Some(_), _) => Well::Armed,
        (None, _) => Well::Idle,
    };
    let hint = match held {
        Some(id) => format!("drop here to take {} off the bar", bp::title(id)),
        None if tiles.is_empty() => {
            "every widget is on the bar \u{b7} drag one here to take it off".to_owned()
        }
        None => "drag onto the bar, or click to add at the end".to_owned(),
    };
    let mut head = Row::new()
        .push(micro_label("available"))
        .push(Space::new().width(Length::Fill))
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    if on {
        head = head.push(pill("New custom widget", false, bar_msg(Msg::New)));
    }
    let body = column![
        column![head, caption_prose(&hint)].spacing(space::LINE_GAP),
        Row::with_children(tiles)
            .spacing(canvas::SHELF_GAP)
            .wrap()
            .vertical_spacing(canvas::SHELF_GAP)
    ]
    .spacing(space::ROW_Y);
    drop_zone(drop_well(body, state), |r| bar_msg(Msg::Zone(Zone::Picker, r))).into()
}

/// The status line: what the last move did, or how to make one.
fn status_line(app: &App) -> String {
    if let Some(s) = &app.bar.said {
        return s.clone();
    }
    if app.key_hints_on() {
        "drag along the bar to move \u{b7} onto the shelf to remove \u{b7} Tab selects, Space lifts"
            .to_owned()
    } else {
        "drag along the bar to move \u{b7} onto the shelf to remove".to_owned()
    }
}

/// The hero: the bar as it will draw, the line that says what happened,
/// and the shelf it is built from.
pub(super) fn hero(app: &App) -> Element<'_, Message, Theme> {
    let b = &app.bar;
    let mut status = Row::new()
        .push(
            text(status_line(app))
                .font(font::UI)
                .size(size::BODY_SMALL)
                .style(if b.said.is_some() {
                    theme::text_secondary
                } else {
                    theme::text_tertiary
                }),
        )
        .push(Space::new().width(Length::Fill))
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    if mode(app).shows_chips() {
        let shown = b.windows_text.clone().unwrap_or_else(|| b.windows.to_string());
        let invalid = b
            .windows_text
            .as_ref()
            .is_some_and(|t| t.trim().parse::<usize>().is_err());
        let windows = NumericSlider::new(
            0.0..=canvas::WINDOWS_MAX as f64,
            b.windows as f64,
            shown,
            |v| bar_msg(Msg::Windows(v)),
            |t| bar_msg(Msg::WindowsTyped(t)),
        )
        .step(1.0)
        .on_commit(bar_msg(Msg::WindowsCommitted))
        .invalid(invalid);
        status = status.push(
            Row::new()
                .push(prose("Preview with windows"))
                .push(windows)
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center)
                .wrap()
                .vertical_spacing(space::PILL_GAP),
        );
    }

    let col = column![
        Row::new()
            .push(micro_label("on the bar"))
            .push(
                text(readout(app))
                    .font(font::DATA)
                    .size(size::MICRO)
                    .style(theme::text_tertiary),
            )
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(space::LINE_GAP),
        picture(app),
        status.wrap().vertical_spacing(space::PILL_GAP),
        shelf(app),
    ]
    .spacing(space::ROW_Y);
    panel(app.glass_radius, app.hairline_cards, col)
}
