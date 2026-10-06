// SPDX-License-Identifier: AGPL-3.0-only
//! Shared KDL value helpers and chord/bind/gesture parsers used by the
//! `Config::apply_*` methods.

use super::*;

/// Collect the string arguments of a list node, warning when the same node
/// appears twice in one block: the second occurrence replaces the first rather
/// than adding to it, and silently dropping names from an allowlist is the
/// failure direction that matters in a fail-closed path.
pub(crate) fn names(n: &KdlNode, seen: &mut bool) -> Vec<String> {
    *seen = true;
    args(n)
        .into_iter()
        .filter_map(|v| v.as_string().map(str::to_string))
        .collect()
}

pub(crate) fn arg(node: &KdlNode) -> Option<&KdlValue> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .map(|e| e.value())
}

pub(crate) fn args(node: &KdlNode) -> Vec<&KdlValue> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| e.value())
        .collect()
}

/// `false` when the node carries no integer, so the caller can reject it.
#[must_use]
pub(crate) fn set_i32(slot: &mut i32, node: &KdlNode) -> bool {
    match arg(node).and_then(KdlValue::as_integer) {
        Some(v) => {
            *slot = v.clamp(0, 512) as i32;
            true
        }
        None => false,
    }
}

/// As [`set_i32`], for a slot that defaults to mirroring another key rather
/// than to a literal (`gaps-in-vertical`, `gaps-out-vertical`).
#[must_use]
pub(crate) fn set_opt_i32(slot: &mut Option<i32>, node: &KdlNode) -> bool {
    match arg(node).and_then(KdlValue::as_integer) {
        Some(v) => {
            *slot = Some(v.clamp(0, 512) as i32);
            true
        }
        None => false,
    }
}

/// `#rrggbb`, `#rrggbbaa`, `0xaarrggbb` (Hyprland's form) or `rrggbb`.
pub(crate) fn parse_color(s: &str) -> Option<[f32; 4]> {
    let t = s.trim();
    let (hex, argb) = if let Some(h) = t.strip_prefix('#') {
        (h, false)
    } else if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        (h, h.len() == 8)
    } else {
        (t, false)
    };
    let v = u32::from_str_radix(hex, 16).ok()?;
    let f = |b: u32| (b & 0xff) as f32 / 255.0;
    Some(match (hex.len(), argb) {
        (6, _) => [f(v >> 16), f(v >> 8), f(v), 1.0],
        (8, true) => [f(v >> 16), f(v >> 8), f(v), f(v >> 24)],
        (8, false) => [f(v >> 24), f(v >> 16), f(v >> 8), f(v)],
        _ => return None,
    })
}

pub(crate) fn parse_mods(s: &str) -> Result<Mods, String> {
    let mut mods = Mods::default();
    for part in s.split(['+', ' ', ',']).filter(|p| !p.is_empty()) {
        match part.to_ascii_uppercase().as_str() {
            "SUPER" | "MOD" | "LOGO" | "MOD4" => mods.logo = true,
            "SHIFT" => mods.shift = true,
            "CTRL" | "CONTROL" => mods.ctrl = true,
            "ALT" | "MOD1" => mods.alt = true,
            "NONE" | "" => {}
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    Ok(mods)
}

pub(crate) fn parse_keysym(s: &str) -> Result<Keysym, String> {
    let k = xkb::keysym_from_name(s, xkb::KEYSYM_NO_FLAGS);
    let k = if k == Keysym::NoSymbol {
        xkb::keysym_from_name(s, xkb::KEYSYM_CASE_INSENSITIVE)
    } else {
        k
    };
    if k == Keysym::NoSymbol {
        Err(format!("unknown key '{s}'"))
    } else {
        Ok(k)
    }
}

/// A launcher chord as one string (`launcher.bind.*`): `"Super+R"`,
/// `"Super+Shift+Return"`, or `"none"` (or empty) for no chord. The last
/// `+`- or space-separated part is the xkb keysym and the rest are modifiers
/// as `bind` spells them. A single letter is case-folded, so `"Super+R"` is
/// the chord `bind "SUPER" "r"` makes. The reserved chords are refused as
/// `parse_bind` refuses them.
pub(crate) fn parse_chord(text: &str) -> Result<Option<(Mods, Keysym)>, String> {
    let text = text.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let (mods, key) = match text.rfind(['+', ' ']) {
        Some(i) => (&text[..i], &text[i + 1..]),
        None => ("", text),
    };
    if key.is_empty() {
        return Err(format!("{text:?} names no key"));
    }
    let mods = parse_mods(mods)?;
    let key = if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() {
        parse_keysym(&key.to_ascii_lowercase())?
    } else {
        parse_keysym(key)?
    };
    if mods == m(true, false, false, false) && key == Keysym::Escape {
        return Err("Super+Escape is reserved (COMP-04 §6) and cannot be bound".into());
    }
    if mods == m(true, true, false, false) && key == Keysym::Escape {
        return Err("Super+Shift+Escape is reserved (COMP-04 §6) and cannot be bound".into());
    }
    if mods == m(true, false, false, false) && key == Keysym::space {
        return Err("Super+space is reserved (COMP-13 §1.1) and cannot be bound".into());
    }
    Ok(Some((mods, key)))
}

/// `bind "SUPER" "Return" { spawn "foot"; }`
pub(crate) fn parse_bind(node: &KdlNode) -> Result<Bind, String> {
    let a = args(node);
    let (mods, key) = match a.len() {
        1 => (Mods::default(), a[0].as_string().ok_or("key must be a string")?),
        2 => (
            parse_mods(a[0].as_string().ok_or("modifiers must be a string")?)?,
            a[1].as_string().ok_or("key must be a string")?,
        ),
        _ => return Err("bind takes [modifiers] key { action }".into()),
    };
    let key = parse_keysym(key)?;
    let children = node.children().ok_or("bind needs an action block")?;
    let action_node = children.nodes().first().ok_or("bind action block is empty")?;
    let action = parse_action(action_node)?;
    if mods == m(true, false, false, false) && key == Keysym::Escape {
        return Err("Super+Escape is reserved (COMP-04 §6) and cannot be bound".into());
    }
    if mods == m(true, true, false, false) && key == Keysym::Escape {
        return Err("Super+Shift+Escape is reserved (COMP-04 §6) and cannot be bound".into());
    }
    if mods == m(true, false, false, false) && key == Keysym::space {
        return Err("Super+space is reserved (COMP-13 §1.1) and cannot be bound".into());
    }
    Ok(Bind { mods, key, action })
}

/// A parsed `gesture` node: a swipe binding, or a drag for a finger count
/// with its modifiers (`None` switches that finger count's drag off).
pub(crate) enum ParsedGesture {
    Swipe(GestureBind),
    Drag(u32, Option<Mods>),
}

/// `gesture "swipe" 3 "left" { workspace-next; }` or a drag (see
/// [`parse_drag_gesture`]).
pub(crate) fn parse_gesture(node: &KdlNode) -> Result<ParsedGesture, String> {
    let a = args(node);
    if a.first().and_then(|k| k.as_string()) == Some("drag") {
        return parse_drag_gesture(node);
    }
    let [kind, fingers, direction] = a[..] else {
        return Err("gesture takes \"swipe\" fingers direction { action }".into());
    };
    if kind.as_string() != Some("swipe") {
        return Err("only \"swipe\" and \"drag\" gestures can be bound".into());
    }
    let fingers = match fingers.as_integer() {
        Some(n @ (3 | 4)) => n as u32,
        _ => return Err("gesture fingers must be 3 or 4".into()),
    };
    let direction = match direction.as_string() {
        Some("left") => Direction::Left,
        Some("right") => Direction::Right,
        Some("up") => Direction::Up,
        Some("down") => Direction::Down,
        _ => return Err("gesture direction must be left, right, up or down".into()),
    };
    let children = node.children().ok_or("gesture needs an action block")?;
    let action_node = children.nodes().first().ok_or("gesture action block is empty")?;
    let action = parse_action(action_node)?;
    Ok(ParsedGesture::Swipe(GestureBind {
        fingers,
        direction,
        action,
    }))
}

/// `gesture "drag" 2 "Super" { move-window; }`, or `gesture "drag" 2 { none; }`
/// (modifiers optional) to switch that finger count's drag off.
pub(crate) fn parse_drag_gesture(node: &KdlNode) -> Result<ParsedGesture, String> {
    let a = args(node);
    let (fingers, mods) = match a[..] {
        [_, fingers] => (fingers, None),
        [_, fingers, mods] => (fingers, Some(mods)),
        _ => return Err("gesture takes \"drag\" fingers \"modifiers\" { move-window; }".into()),
    };
    let fingers = match fingers.as_integer() {
        Some(n @ 2..=4) => n as u32,
        _ => return Err("drag gesture fingers must be 2, 3 or 4".into()),
    };
    let mods = match mods {
        Some(m) => parse_mods(m.as_string().ok_or("modifiers must be a string")?)?,
        None => Mods::default(),
    };
    let children = node.children().ok_or("gesture needs an action block")?;
    let action_node = children.nodes().first().ok_or("gesture action block is empty")?;
    match action_node.name().value() {
        "none" => Ok(ParsedGesture::Drag(fingers, None)),
        // As with mousebind: a bare drag would take every scroll from the app.
        "move-window" if mods == Mods::default() => Err("drag gesture needs at least one modifier".into()),
        "move-window" => Ok(ParsedGesture::Drag(fingers, Some(mods))),
        other => Err(format!("unknown drag gesture action '{other}'")),
    }
}

/// `mousebind "Alt" "left" { move-window; }`
pub(crate) fn parse_mousebind(node: &KdlNode) -> Result<MouseBind, String> {
    let a = args(node);
    let [mods, button] = a[..] else {
        return Err("mousebind takes \"modifiers\" \"button\" { action }".into());
    };
    let mods = parse_mods(mods.as_string().ok_or("modifiers must be a string")?)?;
    let button = match button.as_string() {
        Some("left") => MouseButton::Left,
        Some("right") => MouseButton::Right,
        Some("middle") => MouseButton::Middle,
        _ => return Err("mousebind button must be left, right or middle".into()),
    };
    let children = node.children().ok_or("mousebind needs an action block")?;
    let action_node = children
        .nodes()
        .first()
        .ok_or("mousebind action block is empty")?;
    let action = match action_node.name().value() {
        "move-window" => MouseAction::MoveWindow,
        "resize-window" => MouseAction::ResizeWindow,
        other => return Err(format!("unknown mousebind action '{other}'")),
    };
    // A bare click is not a binding: it would take every press away from the
    // client under it.
    if mods == Mods::default() {
        return Err("mousebind needs at least one modifier".into());
    }
    Ok(MouseBind { mods, button, action })
}

pub(crate) fn parse_action(node: &KdlNode) -> Result<Action, String> {
    let a = args(node);
    let text = || a.first().and_then(|v| v.as_string()).map(str::to_owned);
    let num = || a.first().and_then(|v| v.as_integer());
    let name = node.name().value();
    if schema::find(schema::BIND_ACTIONS, name).is_none() {
        return Err(format!("unknown action '{name}'"));
    }
    Ok(match name {
        "spawn" | "exec" => Action::Spawn(text().ok_or("spawn needs a command string")?),
        "close-window" | "killactive" => Action::Close,
        "toggle-floating" => Action::ToggleFloating,
        "minimize" => Action::Minimize,
        "unminimize" | "restore" => Action::Unminimize,
        "toggle-layout" => Action::ToggleLayout,
        "focus-left" => Action::Focus(Direction::Left),
        "focus-right" => Action::Focus(Direction::Right),
        "focus-up" => Action::Focus(Direction::Up),
        "focus-down" => Action::Focus(Direction::Down),
        "move-left" => Action::Move(Direction::Left),
        "move-right" => Action::Move(Direction::Right),
        "move-up" => Action::Move(Direction::Up),
        "move-down" => Action::Move(Direction::Down),
        "priority-up" => Action::Priority(1),
        "priority-down" => Action::Priority(-1),
        "workspace" => Action::SwitchWorkspace(workspace_arg(num())?),
        "workspace-next" => Action::WorkspaceNext,
        "workspace-prev" => Action::WorkspacePrev,
        "move-to-workspace" => Action::MoveToWorkspace(workspace_arg(num())?),
        "move-to-output" => Action::MoveToOutputWorkspace(output_number_arg(num())?),
        "agent-override" => Action::AgentOverride,
        "agent-terminate" => Action::AgentTerminate,
        "agent-attention" => Action::AgentAttention,
        "annotation-select" => Action::AnnotationSelect,
        "annotation-dismiss" => Action::AnnotationDismiss,
        "annotation-expand" => Action::AnnotationExpand,
        "annotation-auto-toggle" => Action::AnnotationAutoToggle,
        "quit" | "exit" => Action::Quit,
        other => return Err(format!("unknown action '{other}'")),
    })
}

pub(crate) fn workspace_arg(n: Option<i128>) -> Result<usize, String> {
    match n {
        Some(v) if (1..=10).contains(&v) => Ok(v as usize),
        _ => Err("workspace number must be 1..=10".into()),
    }
}

/// Display number for `move-to-output` (ADR 0049). Wider range than
/// `workspace_arg`'s 1..=10: this matches an output's configured/assigned
/// `number` (also 1..=255, see `OutputRule::number`), not a workspace index —
/// the default binds only ever go up to 10, but a config override is free to
/// name a higher display number.
pub(crate) fn output_number_arg(n: Option<i128>) -> Result<u8, String> {
    match n {
        Some(v) if (1..=255).contains(&v) => Ok(v as u8),
        _ => Err("output number must be 1..=255".into()),
    }
}

/// A duration written either as a bare integer of milliseconds or as a string
/// with a unit: `"150ms"`, `"2s"`, `"1m"`. KDL 2.0 has no duration literal, so
/// the unit form has to be quoted.
pub(crate) fn parse_duration_ms(v: &KdlValue) -> Option<u32> {
    if let Some(i) = v.as_integer() {
        return u32::try_from(i).ok();
    }
    let s = v.as_string()?.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else {
        (s, 1)
    };
    num.trim().parse::<u32>().ok()?.checked_mul(mult)
}

pub(crate) fn as_f64(v: &KdlValue) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// One pointer key of `input` or a `device` block, validated. `Err(None)` is
/// a name this does not know; `Err(Some)` a known name with a bad value.
pub(crate) fn pointer_key(n: &KdlNode) -> Result<PointerKey, Option<String>> {
    let s = arg(n).and_then(KdlValue::as_string);
    match n.name().value() {
        "accel-profile" => match s {
            Some(v @ ("flat" | "adaptive")) => Ok(PointerKey::AccelProfile(v.to_owned())),
            other => Err(Some(format!("unknown accel-profile {other:?}"))),
        },
        "accel-speed" => match arg(n).and_then(as_f64) {
            Some(v) if (-1.0..=1.0).contains(&v) => Ok(PointerKey::AccelSpeed(v)),
            _ => Err(Some("accel-speed must be a number -1.0..=1.0".to_owned())),
        },
        "scroll-method" => match s {
            Some(v @ ("default" | "none" | "on-button-down")) => Ok(PointerKey::ScrollMethod(v.to_owned())),
            other => Err(Some(format!(
                "unknown scroll-method {other:?}; expected default, none or on-button-down"
            ))),
        },
        "calibration" => {
            let vals = args(n);
            let nums: Vec<f32> = vals
                .iter()
                .filter_map(|v| as_f64(v))
                .filter(|v| v.is_finite())
                .map(|v| v as f32)
                .collect();
            match <[f32; 6]>::try_from(nums) {
                Ok(m) if vals.len() == 6 => Ok(PointerKey::Calibration(m)),
                _ => Err(Some(
                    "calibration needs exactly six numbers: a b c d e f".to_owned(),
                )),
            }
        }
        _ => Err(None),
    }
}

/// One `touchpad` key, validated; errors as in [`pointer_key`].
pub(crate) fn touchpad_key(n: &KdlNode) -> Result<TouchpadKey, Option<String>> {
    let name = n.name().value();
    let s = arg(n).and_then(KdlValue::as_string);
    match name {
        "click-method" => {
            return match s {
                Some(v @ ("clickfinger" | "button-areas")) => Ok(TouchpadKey::ClickMethod(v.to_owned())),
                other => Err(Some(format!("unknown click-method {other:?}"))),
            }
        }
        "scroll-method" => {
            return match s {
                Some(v @ ("two-finger" | "edge" | "none")) => Ok(TouchpadKey::ScrollMethod(v.to_owned())),
                other => Err(Some(format!(
                    "unknown touchpad scroll-method {other:?}; expected two-finger, edge or none"
                ))),
            }
        }
        _ => {}
    }
    let Some(b) = arg(n).and_then(KdlValue::as_bool) else {
        return Err(Some(format!("touchpad key needs a boolean: {name:?}")));
    };
    match name {
        "natural-scroll" => Ok(TouchpadKey::NaturalScroll(b)),
        "tap-to-click" => Ok(TouchpadKey::TapToClick(b)),
        "tap-and-drag" => Ok(TouchpadKey::TapAndDrag(b)),
        "dwt" => Ok(TouchpadKey::Dwt(b)),
        _ => Err(None),
    }
}

/// `*` matches any run of characters; everything else is literal. Anchored.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return pattern == text;
    };
    if !text.starts_with(first) {
        return false;
    }
    if !pattern.contains('*') {
        return text.len() == first.len();
    }
    let mut rest = &text[first.len()..];
    let parts: Vec<&str> = parts.collect();
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() {
            continue;
        }
        if i + 1 == parts.len() && !pattern.ends_with('*') {
            return rest.ends_with(p) && rest.len() >= p.len();
        }
        match rest.find(p) {
            Some(at) => rest = &rest[at + p.len()..],
            None => return false,
        }
    }
    true
}
