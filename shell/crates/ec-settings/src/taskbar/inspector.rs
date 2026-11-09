// SPDX-License-Identifier: AGPL-3.0-only
//! The Widgets page's inspector: one sheet under the shelf for whatever is
//! selected — where it sits, the controls that move it without a drag, and
//! its few settings. A custom widget's block opens behind Edit, and the
//! approvals and the shared corner radius sit on the sheet shown when
//! nothing is selected. Progressive disclosure: the common thing is in
//! view, the rest one press away.

use super::*;
use iced::widget::{column, row};

/// The ask a draft with unsaved changes gets before it is left.
fn leaving_row<'a>(app: &App) -> Option<Element<'a, Message, Theme>> {
    app.bar.leaving.as_ref()?;
    let name = app
        .bar
        .editor
        .as_ref()
        .map(|e| e.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "the new widget".to_owned());
    Some(padded(
        Row::new()
            .push(prose(&format!("Unsaved changes to \u{201c}{name}\u{201d}.")))
            .push(Space::new().width(Length::Fill))
            .push(pill("Keep editing", false, bar_msg(Msg::Stay)))
            .push(
                button(text("Discard changes").font(font::UI).size(size::BODY_SMALL))
                    .padding([space::CHIP_Y, space::CHIP_X])
                    .style(theme::danger)
                    .on_press(bar_msg(Msg::Discard)),
            )
            .spacing(space::PILL_GAP)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(space::PILL_GAP),
    ))
}

/// The sheet under the shelf.
pub(super) fn inspector(app: &App) -> Element<'_, Message, Theme> {
    let mut col = Column::new();
    if let Some(r) = leaving_row(app) {
        col = col.push(r).push(hairline());
    }
    if let (true, Some(ed)) = (app.bar.editing, app.bar.editor.as_ref()) {
        return inset(col.push(editor_sheet(app, ed)));
    }
    let body = match selected(app) {
        Some(id) => selected_sheet(app, id),
        None => everything(app),
    };
    inset(col.push(body))
}

/// The editor, under a head that names what is edited and closes it.
fn editor_sheet<'a>(app: &App, ed: &'a Editor) -> Element<'a, Message, Theme> {
    let title = if ed.is_new() {
        "New custom widget".to_owned()
    } else {
        format!(
            "Edit \u{201c}{}\u{201d}",
            ed.original.as_deref().unwrap_or_default()
        )
    };
    column![
        padded(
            row![
                text(title)
                    .font(font::UI)
                    .size(size::CARD_TITLE)
                    .style(theme::text_primary),
                Space::new().width(Length::Fill),
                pill(
                    if ed.is_new() { "Cancel" } else { "Done" },
                    false,
                    bar_msg(Msg::Close)
                ),
            ]
            .align_y(Alignment::Center)
        ),
        hairline(),
        editor_view(app, ed),
    ]
    .into()
}

/// Nothing selected: what belongs to every widget at once.
fn everything(app: &App) -> Element<'_, Message, Theme> {
    let mut col = Column::new().push(padded(
        Row::new()
            .push(micro_label("all widgets"))
            .push(caption("select one on the bar to change it".into()))
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center)
            .wrap()
            .vertical_spacing(space::PILL_GAP),
    ));
    if let Some(r) = key_row(app, WIDGET_ROUNDING) {
        col = col.push(r);
    }
    if crate::addons::taskbar_widgets_on(app) && !app.bar.statuses.is_empty() {
        col = col.push(hairline()).push(approvals(app));
    }
    col.into()
}

/// One selected widget.
fn selected_sheet(app: &App, id: String) -> Element<'_, Message, Theme> {
    let o = order(app);
    let at = o.iter().position(|x| *x == id);
    let imp = important(app).contains(&id);
    let custom = id.strip_prefix(bp::CUSTOM).map(str::to_owned);

    // The name is the page's one yellow: the selection, said once.
    let mut name = Row::new()
        .push(
            text(bp::title(&id))
                .font(font::UI)
                .size(size::CARD_TITLE)
                .color(color::accent_text()),
        )
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center);
    if custom.as_deref().is_some_and(|n| premade(app, n)) {
        name = name.push(badge("Premade"));
    }
    name = name.push(caption(match at {
        Some(i) => format!("{} of {} on the bar", i + 1, o.len()),
        None => "not on the bar".to_owned(),
    }));
    let mut moves = Row::new().spacing(space::PILL_GAP).align_y(Alignment::Center);
    match at {
        Some(i) => {
            let earlier = pill("Move left", false, bar_msg(Msg::Step(false)));
            let later = pill("Move right", false, bar_msg(Msg::Step(true)));
            moves = moves
                .push(ec_ui::widget::dimmed(earlier, i == 0))
                .push(ec_ui::widget::dimmed(later, i + 1 == o.len()))
                .push(pill("Remove", false, bar_msg(Msg::Remove)));
        }
        None => moves = moves.push(pill("Add to bar", false, bar_msg(Msg::Add(id.clone())))),
    }
    let head = Row::new()
        .push(name)
        .push(moves)
        .spacing(space::CONTROL_GAP)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(space::PILL_GAP);
    let mut col = Column::new().push(padded(head));
    if at.is_some() && app.key_hints_on() {
        col = col.push(padded(wrapped_hints(&[
            ("tab", "next"),
            ("\u{2190} \u{2192}", "move"),
            ("space", "lift"),
            ("delete", "remove"),
            ("ctrl z", "undo"),
        ])));
    }
    col = col.push(hairline());
    if at.is_some() {
        col = col.push(padded(
            row![
                pin(color::text_secondary()),
                column![
                    text("Always full size")
                        .font(font::UI)
                        .size(size::BODY)
                        .style(theme::text_primary),
                    text("keeps its whole width while the bar is crowded; the others fold to a grip")
                        .font(font::DATA)
                        .size(size::MICRO)
                        .style(theme::text_tertiary),
                ]
                .spacing(space::HAIRLINE)
                .width(Length::Fill),
                Toggle::new(imp, |on| bar_msg(Msg::Important(on))),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        ));
    }
    for path in widget_keys(&id) {
        if let Some(r) = key_row(app, path) {
            col = col.push(r);
        }
    }
    if id == "tray" {
        col = col.push(hairline()).push(padded(
            Row::new()
                .push(prose(
                    "Which apps' icons sit on the bar, in the drawer, or nowhere",
                ))
                .push(Space::new().width(Length::Fill))
                .push(pill("Arrange the tray", false, Message::Open(Page::BarTray)))
                .spacing(space::CONTROL_GAP)
                .align_y(Alignment::Center)
                .wrap()
                .vertical_spacing(space::PILL_GAP),
        ));
    }
    if let Some(name) = custom {
        col = col.push(hairline());
        let st = status_of(app, &name).filter(|s| s.approval == Approval::Pending);
        match (&app.bar.editor, st) {
            _ if !crate::addons::taskbar_widgets_on(app) => {
                col = col.push(padded(prose(
                    "The taskbar add-on is off: the compositor ignores widget blocks.",
                )))
            }
            (_, Some(st)) => col = col.push(approval_row(app, st, false)),
            (Some(ed), None) => col = col.push(summary(ed)),
            (None, None) => {
                col = col.push(padded(prose(&format!(
                    "No widget block named \u{201c}{name}\u{201d} in abyss.kdl: the bar draws nothing here."
                ))))
            }
        }
    } else if !note(&id).is_empty() {
        col = col.push(padded(caption_prose(note(&id))));
    }
    if let Some(name) = &app.bar.offer {
        col = col.push(hairline()).push(padded(
            Row::new()
                .push(prose(&format!("Put \u{201c}{name}\u{201d} on the bar?")))
                .push(Space::new().width(Length::Fill))
                .push(pill("Add to bar", false, bar_msg(Msg::Offer(true))))
                .push(pill("Not now", false, bar_msg(Msg::Offer(false))))
                .spacing(space::PILL_GAP)
                .align_y(Alignment::Center)
                .wrap()
                .vertical_spacing(space::PILL_GAP),
        ));
    }
    col.into()
}

/// A custom widget at a glance: what it runs, and the way into its block.
fn summary<'a>(ed: &Editor) -> Element<'a, Message, Theme> {
    let what = match ed.kind {
        Kind::Exec | Kind::Stream => {
            let argv = ed.argv(Slot::Command).args.join(" ");
            elide(&argv, canvas::ARG_MAX_CHARS)
        }
        Kind::Source => ed.source.clone(),
    };
    padded(
        Row::new()
            .push(
                column![
                    text(kind_label(ed.kind))
                        .font(font::UI)
                        .size(size::BODY)
                        .style(theme::text_primary),
                    caption(what),
                ]
                .spacing(space::HAIRLINE),
            )
            .push(Space::new().width(Length::Fill))
            .push(pill("Edit widget", false, bar_msg(Msg::Edit)))
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
    )
}

/// A kind as a person reads it.
fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Exec => "Command",
        Kind::Stream => "Live command",
        Kind::Source => "System reading",
    }
}

/// One command widget's approval, as abyss reports it: the state line and,
/// for a withheld one, the Review control. Review only brings the
/// compositor's prompt back; Settings never answers it.
/// `named` is false inside the widget's own sheet, whose head already says it.
fn approval_row<'a>(app: &App, st: &WidgetStatus, named: bool) -> Element<'a, Message, Theme> {
    let (state, style): (&str, fn(&Theme) -> text::Style) = match st.approval {
        Approval::Approved => ("live", theme::text_secondary),
        Approval::Pending if st.altered => ("Waiting for approval \u{b7} altered", theme::text_danger),
        Approval::Pending => ("Waiting for approval", theme::text_secondary),
    };
    let mut r = Row::new().spacing(space::CONTROL_GAP).align_y(Alignment::Center);
    if named {
        r = r.push(
            text(st.name.clone())
                .font(font::UI)
                .size(size::BODY)
                .style(theme::text_primary),
        );
        if st.premade {
            r = r.push(badge("Premade"));
        }
        r = r.push(Space::new().width(Length::Fill)).push(tag(state, style));
    } else {
        r = r.push(tag(state, style)).push(Space::new().width(Length::Fill));
    }
    let answer = app
        .bar
        .reviewed
        .iter()
        .find(|(n, _)| *n == st.name)
        .map(|(_, a)| a)
        .filter(|_| st.approval == Approval::Pending);
    if st.approval == Approval::Pending {
        match answer {
            Some(Ok(true)) => r = r.push(caption("prompt queued".into())),
            Some(Ok(false)) => r = r.push(caption("prompt already open".into())),
            Some(Err(_)) => r = r.push(tag("prompt not shown", theme::text_danger)),
            None => {}
        }
        r = r.push(pill("Review", false, bar_msg(Msg::Review(st.name.clone()))));
    }
    let mut c = Column::new()
        .push(r.wrap().vertical_spacing(space::PILL_GAP))
        .spacing(space::LINE_GAP);
    if st.approval == Approval::Pending {
        c = c.push(caption_prose(if st.altered {
            ALTERED
        } else {
            "It does not run until you approve it in the compositor's prompt."
        }));
    }
    if let Some(Err(p)) = answer {
        c = c.push(config_error(&p.headline(), p.detail(), "", 0, 0));
    }
    padded(c)
}

/// Every command widget and whether it runs. Withheld ones are here even
/// when they are not on the bar, since abyss serves nothing else for them.
fn approvals(app: &App) -> Element<'_, Message, Theme> {
    let waiting = app
        .bar
        .statuses
        .iter()
        .filter(|s| s.approval == Approval::Pending)
        .count();
    let mut col = Column::new().push(padded(
        Row::new()
            .push(micro_label("custom widgets"))
            .push(Space::new().width(Length::Fill))
            .push(caption(match waiting {
                0 => "every command approved".to_owned(),
                n => format!("{n} waiting \u{b7} approve in the compositor's prompt"),
            }))
            .align_y(Alignment::Center)
            .wrap(),
    ));
    for st in &app.bar.statuses {
        col = col.push(hairline()).push(approval_row(app, st, true));
    }
    col.into()
}

fn errors_for<'a>(ed: &'a Editor, f: Field) -> Column<'a, Message, Theme> {
    Column::with_children(
        ed.errors_for(f)
            .map(|e| config_error(&e.position, &e.message, &e.snippet, e.col, e.span)),
    )
}

fn field<'a>(
    placeholder: &str,
    value: &str,
    bad: bool,
    on_input: impl Fn(String) -> Msg + 'a,
) -> text_input::TextInput<'a, Message, Theme> {
    text_input(placeholder, value)
        .on_input(move |t| bar_msg(on_input(t)))
        .font(font::DATA)
        .size(size::MONO)
        .width(Length::Fixed(space::FIELD_W))
        .style(if bad {
            theme::eclipse_input_invalid
        } else {
            theme::eclipse_input
        })
}

fn argv_row(ed: &Editor, slot: Slot) -> Element<'_, Message, Theme> {
    let a = ed.argv(slot);
    let bad = ed.errors_for(slot.field()).next().is_some();
    let mut r = Row::with_children(
        a.args
            .iter()
            .enumerate()
            .map(|(i, s)| arg_chip(s, bar_msg(Msg::ArgRemove(slot, i)))),
    );
    let hint = if a.args.is_empty() {
        "program, then Enter"
    } else {
        "argument, then Enter"
    };
    r = r.push(
        field(hint, &a.draft, bad, move |t| Msg::ArgDraft(slot, t))
            .id(slot.input_id())
            .on_submit(bar_msg(Msg::ArgPush(slot))),
    );
    column![
        list_row(
            slot.label(),
            r.spacing(canvas::ARG_GAP).align_y(Alignment::Center).wrap()
        ),
        errors_for(ed, slot.field()),
    ]
    .into()
}

/// Whether the less-used fields hold anything: then they are never folded
/// away, so nothing set is hidden.
fn extras(ed: &Editor) -> bool {
    !ed.icon.is_empty()
        || Slot::ACTIONS
            .iter()
            .any(|s| !ed.argv(*s).args.is_empty() || !ed.argv(*s).draft.is_empty())
        || ed.errors_for(Field::Icon).next().is_some()
        || Slot::ACTIONS
            .iter()
            .any(|s| ed.errors_for(s.field()).next().is_some())
}

/// One custom widget, as the fields its kind has.
fn editor_view<'a>(app: &App, ed: &'a Editor) -> Element<'a, Message, Theme> {
    let bad = |f| ed.errors_for(f).next().is_some();
    let mut kinds = Row::new().spacing(space::PILL_GAP);
    for k in Kind::ALL {
        kinds = kinds.push(pill(kind_label(k), ed.kind == k, bar_msg(Msg::Kind(k))));
    }
    // Premade means the block on file is the catalog's, unedited; a renamed
    // draft is a new block and is not.
    let is_premade = ed
        .original
        .as_deref()
        .is_some_and(|n| !ed.renamed() && premade(app, n));
    let mut col = Column::new();
    match ed.kind {
        Kind::Exec | Kind::Stream if !is_premade => {
            col = col.push(edge_note(DISCLAIMER, RISK, color::danger()));
        }
        Kind::Exec | Kind::Stream => {
            col = col.push(padded(caption_prose(
                "Premade: shipped with EclipseOS and unedited. Saving a change makes it altered, \
                 and an altered widget does not run until you approve it in the compositor's prompt.",
            )));
        }
        Kind::Source => {}
    }
    let mut col = col.push(column![
        list_row("Name", field("weather", &ed.name, bad(Field::Name), Msg::Name)),
        errors_for(ed, Field::Name),
        list_row("Shows", kinds.wrap()),
        padded(caption_prose(ed.kind.gist())),
    ]);
    match ed.kind {
        Kind::Exec | Kind::Stream => {
            col = col
                .push(padded(caption_prose(
                    "Runs as you, without a shell: each chip is one argument, passed exactly as written.",
                )))
                .push(argv_row(ed, Slot::Command));
            if ed.kind == Kind::Exec {
                col = col
                    .push(list_row(
                        "Refresh every, ms",
                        field("5000", &ed.interval, bad(Field::Interval), Msg::Interval),
                    ))
                    .push(errors_for(ed, Field::Interval));
            }
        }
        Kind::Source => {
            let current = SOURCES.iter().copied().find(|s| *s == ed.source);
            col = col
                .push(list_row(
                    "Reading",
                    pick_list(&SOURCES[..], current, |s: &str| {
                        bar_msg(Msg::Source(s.to_owned()))
                    }),
                ))
                .push(errors_for(ed, Field::Source))
                .push(list_row(
                    "Shown as",
                    row![
                        caption(format!("\u{2192} {}", ed.format_preview())),
                        field("{}", &ed.format, bad(Field::Format), Msg::Format),
                    ]
                    .spacing(space::CONTROL_GAP)
                    .align_y(Alignment::Center)
                    .wrap(),
                ))
                .push(errors_for(ed, Field::Format));
        }
    }
    // The icon and what a click or the wheel does: folded until wanted, and
    // never folded while they hold something.
    let forced = extras(ed);
    let open = app.bar.more || forced;
    col = col.push(hairline());
    if !forced {
        col = col.push(padded(
            row![
                micro_label("more options"),
                caption("icon, and what a click or the wheel runs".into()),
                Space::new().width(Length::Fill),
                pill(
                    if open { "Hide" } else { "Show" },
                    false,
                    bar_msg(Msg::More(!open))
                ),
            ]
            .spacing(space::CONTROL_GAP)
            .align_y(Alignment::Center),
        ));
    }
    if open {
        col = col
            .push(list_row(
                "Icon",
                field("icon name or path", &ed.icon, bad(Field::Icon), Msg::Icon),
            ))
            .push(errors_for(ed, Field::Icon));
        for slot in Slot::ACTIONS {
            col = col.push(argv_row(ed, slot));
        }
    }
    col = col.push(errors_for(ed, Field::Block));

    let verdict = if let Some(r) = &ed.refused {
        r.clone()
    } else if ed.check_at.is_some() {
        "checking\u{2026}".to_owned()
    } else if !ed.checked {
        "unchanged".to_owned()
    } else if ed.errors.is_empty() {
        "the compositor would load this".to_owned()
    } else {
        match ed.errors.len() {
            1 => "1 problem".to_owned(),
            n => format!("{n} problems"),
        }
    };
    let mut foot = Row::new()
        .push(caption(verdict))
        .push(Space::new().width(Length::Fill))
        .spacing(space::PILL_GAP)
        .align_y(Alignment::Center);
    let delete = || {
        button(text("Delete").font(font::UI).size(size::BODY_SMALL))
            .padding([space::CHIP_Y, space::CHIP_X])
            .style(theme::danger)
            .on_press(bar_msg(Msg::Delete))
    };
    if ed.confirm_delete {
        // The ask replaces the foot rather than floating over the pane: the
        // question sits where the button was, answered in place.
        let name = ed.original.as_deref().unwrap_or_default();
        let ask = Row::new()
            .push(
                text(format!(
                    "Delete \u{201c}{name}\u{201d}? Its block leaves abyss.kdl and the bar."
                ))
                .font(font::UI)
                .size(size::BODY_SMALL),
            )
            .push(Space::new().width(Length::Fill))
            .push(pill("Keep", false, bar_msg(Msg::Keep)))
            .push(delete())
            .spacing(space::PILL_GAP)
            .align_y(Alignment::Center)
            .wrap();
        return col.push(hairline()).push(padded(ask)).into();
    }
    if !ed.is_new() {
        foot = foot.push(delete());
    }
    // A command widget is the one write here that waits for a press: saving
    // it puts a command on the approval prompt (ADR 0067), so it is never
    // written on a keystroke.
    foot = foot.push(pill(
        if ed.is_new() { "Create" } else { "Save" },
        false,
        bar_msg(Msg::Save),
    ));
    col.push(hairline()).push(padded(foot)).into()
}
