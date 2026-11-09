// SPDX-License-Identifier: AGPL-3.0-only
//! The Tray page: three lanes — on the bar, in the drawer, hidden — of the
//! running apps' own status icons. Each is dragged within its lane or into
//! another; a click selects it for the keyboard.

use super::*;
use ec_ui::widget::icon_chip;
use iced::widget::{column, row};

fn art_for(app: &App, id: &str) -> Art {
    app.tray_art
        .iter()
        .find(|(x, _)| x == id)
        .map_or(Art::None, |(_, a)| a.clone())
}

/// The page's one block: the lanes, flat inside an inset.
pub(super) fn tray(app: &App) -> Element<'_, Message, Theme> {
    let t = app.tray();
    let sel = app.tray_sel.as_deref();
    let aim = aimed(app);
    let held = match &app.bar.drag {
        Some(Drag {
            what: Payload::Tray(id),
            ..
        }) => Some(id.clone()),
        _ => None,
    };
    let entry = |id: &str| -> Element<'_, Message, Theme> {
        let what = Payload::Tray(id.to_owned());
        let (lifted, homing) = app.bar.ghost_of(&what);
        let art = art_for(app, id);
        let icon = || art_mark(&art, bar::MARK, color::text_secondary());
        let at_rest = ec_ui::widget::dimmed(
            icon_chip(
                icon(),
                id,
                sel == Some(id) && held.as_deref() != Some(id),
                Message::TraySelect(id.to_owned()),
            ),
            app.bar.away(&what),
        );
        drop_zone(
            draggable(at_rest)
                .ghost(drag_ghost(
                    row![icon(), tag(id, theme::text_primary)]
                        .spacing(space::CHIP_ORDINAL_GAP)
                        .align_y(Alignment::Center),
                    ec_ui::tokens::radius::CHIP,
                ))
                .lifted(lifted)
                .homing(homing)
                .on_move(move |p| bar_msg(Msg::Move(what.clone(), p)))
                .on_release(|p| bar_msg(Msg::Release(p)))
                .on_cancel(bar_msg(Msg::Cancel))
                .on_click(Message::TraySelect(id.to_owned())),
            {
                let id = id.to_owned();
                move |r| bar_msg(Msg::Zone(Zone::Chip(id.clone()), r))
            },
        )
        .into()
    };
    let chips = |lane: Lane, ids: &[String]| -> Element<'_, Message, Theme> {
        let mark = match aim {
            Some((_, Target::Tray(l, at))) if l == lane => Some(at),
            _ => None,
        };
        // Only the bar lane keeps an order, so only it shows where in the
        // order a drop lands. Every gap has its mark's room, lit or not: a
        // mark that came and went between chips would move the chips after
        // it in the tree, and the press iced keeps on the one in hand with
        // them.
        let gap = |k: usize| -> Element<'_, Message, Theme> {
            let lit = lane == Lane::Taskbar && mark == Some(k);
            let ink = if lit {
                color::text()
            } else {
                iced::Color::TRANSPARENT
            };
            container(edge_quad(
                Length::Fixed(canvas::DROP_MARK_W),
                Length::Fixed(space::CHIP_H),
                ink,
            ))
            .width(Length::Fixed(space::CHIP_GAP))
            .align_x(Alignment::Center)
            .into()
        };
        let mut r: Vec<Element<'_, Message, Theme>> = Vec::new();
        for (i, id) in ids.iter().enumerate() {
            r.push(gap(i));
            r.push(entry(id));
        }
        r.push(gap(ids.len()));
        if ids.is_empty() && mark.is_none() {
            r.push(caption("none".into()));
        }
        Row::with_children(r)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(space::CHIP_GAP)
            .into()
    };
    let live = match &app.tray_live {
        None => "reading the tray".to_owned(),
        Some(None) => "tray unreachable".to_owned(),
        Some(Some(l)) if l.len() == 1 => "1 app running".to_owned(),
        Some(Some(l)) => format!("{} apps running", l.len()),
    };
    let lane = |name: &str, lane: Lane, ids: Vec<String>| {
        let state = match (&held, aim) {
            (Some(_), Some((_, Target::Tray(l, _)))) if l == lane => Well::Hot,
            (Some(_), _) => Well::Armed,
            _ => Well::Idle,
        };
        drop_zone(
            drop_well(
                column![micro_label(name), chips(lane, &ids)].spacing(space::PILL_GAP),
                state,
            ),
            move |r| bar_msg(Msg::Zone(Zone::Lane(lane), r)),
        )
    };
    // The status line: what the last move did, else the selection, else
    // how to start.
    let line = match (&app.bar.said, sel) {
        (Some(s), _) => s.clone(),
        (None, Some(id)) => format!("{id} selected"),
        (None, None) => "drag an icon between lanes \u{b7} Tab selects one for the keyboard".to_owned(),
    };
    let head = Row::new()
        .push(micro_label("status icons"))
        .push(caption(live))
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    // The hints show whether or not an icon is selected, so selecting one
    // moves no lane, and they wrap rather than clip in a narrow pane.
    let mut top = Column::new().push(head).spacing(space::PILL_GAP);
    if app.key_hints_on() {
        top = top.push(wrapped_hints(&[
            ("tab", "select"),
            ("\u{2190} \u{2192}", "move"),
            ("\u{2191} \u{2193}", "lane"),
            ("delete", "hides"),
            ("ctrl z", "undo"),
        ]));
    }
    inset(
        column![
            padded(column![
                top,
                text(line)
                    .font(font::UI)
                    .size(size::BODY_SMALL)
                    .style(theme::text_secondary),
            ]),
            container(
                column![
                    lane("on the bar", Lane::Taskbar, t.taskbar()),
                    lane("drawer", Lane::Overflow, t.in_lane(Lane::Overflow)),
                    lane("hidden", Lane::Hidden, t.in_lane(Lane::Hidden)),
                ]
                .spacing(space::HAIRLINE),
            )
            .padding([space::ROW_Y, 0.0])
            .width(Length::Fill),
        ]
        .width(Length::Fill),
    )
}
