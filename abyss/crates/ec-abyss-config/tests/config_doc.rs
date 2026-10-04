// SPDX-License-Identifier: AGPL-3.0-only
//! `docs/CONFIG.md` is generated from the schema, not written by hand.
//!
//! F-07 §7 makes `docs/` source of truth once code starts, and a
//! hand-maintained key reference is a promise to let the two drift. So the
//! reference is a function of `config::schema::TABLE`, and this test fails if
//! the checked-in file is not what that function produces. Regenerate with:
//!
//! ```text
//! UPDATE_CONFIG_DOC=1 cargo test -p ec-abyss-config --test config_doc
//! ```

use std::fmt::Write as _;

use ec_abyss_config::schema::{
    rule_owner, Collection, Dv, Form, Key, Owner, Reload, Ty, ANIMATION_CURVES, ANIMATION_DEFAULT_CURVE,
    ANIMATION_DEFAULT_MS, ANIMATION_MAX_MS, BIND_ACTIONS, COLLECTIONS, EASING_CURVES, LEGACY_ANIMATIONS,
    LID_CLOSE, OUTPUT_KEYS, OUTPUT_TRANSFORMS, REFUSED_MATCHERS, RULE_ACTION_FORMS, RULE_MATCHERS, TABLE,
    WIDGET_KEYS, WIDGET_SOURCES,
};

fn ty(t: &Ty) -> String {
    match t {
        Ty::Bool => "bool".into(),
        Ty::Int { min, max } => format!("int {min}..{max}"),
        Ty::Float { min, max } => format!("float {min}..{max}"),
        Ty::Str => "string".into(),
        Ty::Enum(vs) => vs.join(" \\| "),
        Ty::Color => "colour `#rrggbb[aa]`".into(),
        Ty::StrList => "list of strings".into(),
    }
}

fn default(d: &Dv) -> String {
    match d {
        Dv::Null => "_unset_".into(),
        Dv::Bool(b) => format!("`#{b}`"),
        Dv::Int(i) => format!("`{i}`"),
        Dv::Float(f) => format!("`{f}`"),
        Dv::Str(s) => format!("`\"{s}\"`"),
        Dv::EmptyList => "_empty_".into(),
        Dv::List(l) => format!(
            "`{}`",
            l.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(" ")
        ),
        Dv::Color([r, g, b, a]) => format!(
            "`#{:02x}{:02x}{:02x}{:02x}`",
            (r * 255.0) as u8,
            (g * 255.0) as u8,
            (b * 255.0) as u8,
            (a * 255.0) as u8
        ),
    }
}

fn file(o: Owner) -> &'static str {
    match o {
        Owner::Abyss => "abyss.kdl",
        Owner::Policy => "policy.kdl",
    }
}

fn section(out: &mut String, keys: &[&Key]) {
    out.push_str("| setting | type | default | reload | what it does |\n");
    out.push_str("| --- | --- | --- | --- | --- |\n");
    for k in keys {
        let reload = match k.reload {
            Reload::Live => "live",
            Reload::NeedsRestart => "restart",
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {reload} | {} |",
            k.path,
            ty(&k.ty),
            default(&k.default),
            k.doc
        );
    }
    out.push('\n');
}

fn ticks(vs: &[&str]) -> String {
    vs.iter().map(|v| format!("`{v}`")).collect::<Vec<_>>().join(", ")
}

/// `name args` for each name, with `|` escaped for a table cell.
fn syntax(f: &Form) -> String {
    f.names
        .iter()
        .map(|n| {
            let s = if f.args.is_empty() {
                n.to_string()
            } else {
                format!("{n} {}", f.args)
            };
            format!("`{}`", s.replace('|', "\\|"))
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

fn examples(f: &Form) -> String {
    f.examples
        .iter()
        .map(|e| format!("`{}`", e.replace('|', "\\|")))
        .collect::<Vec<_>>()
        .join("<br>")
}

fn forms(out: &mut String, head: &str, fs: &[Form]) {
    let _ = writeln!(out, "| {head} | example | what it does |\n| --- | --- | --- |");
    for f in fs {
        let _ = writeln!(out, "| {} | {} | {} |", syntax(f), examples(f), f.doc);
    }
    out.push('\n');
}

fn animations(out: &mut String) {
    use ec_abyss_config::animations::{Animations, Event, Preset};
    let _ = writeln!(
        out,
        "Every event resolves the same way: the preset's row, then any field its block sets \
         (`<event> {{ style …; duration-ms …; curve …; }}`), then `duration-ms / speed` rounded, \
         then `reduce-motion`. A field left out keeps the preset's value, and a block may sit in \
         any config file: fields merge key by key down the search path. A bad child drops its \
         whole block. `style \"none\"` or `duration-ms 0` is no animation. `duration-ms` is at \
         most {}s. `curve` is one of {}. A style is one of the event's own (below) or an add-on \
         style `pack:style`; one this build does not know falls back to the event's first style. \
         `reduce-motion` turns movement (`window-move`, `workspace-switch`, \
         `window-to-workspace`) off, makes every event that offers `fade` a fade of at most \
         100ms, `ease-out`, and turns the rest off. Animations are drawn only: a window is \
         always mapped, focused and clickable at its final place, so input never lands on an \
         in-between position (COMP-02 §9).\n",
        ANIMATION_MAX_MS / 1000,
        ticks(ANIMATION_CURVES),
    );
    let _ = write!(out, "| event | styles |");
    for p in Preset::ALL {
        let _ = write!(out, " `{}` |", p.key());
    }
    let _ = write!(out, "\n| --- | --- |");
    for _ in Preset::ALL {
        out.push_str(" --- |");
    }
    out.push('\n');
    for ev in Event::ALL {
        let _ = write!(out, "| `{}` | {} |", ev.key(), ticks(ev.styles()));
        for p in Preset::ALL {
            let r = Animations {
                preset: p,
                ..Default::default()
            }
            .resolve(ev);
            if r.off() {
                out.push_str(" none |");
            } else {
                let _ = write!(out, " {} {}ms {} |", r.style, r.duration_ms, r.curve.key());
            }
        }
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "\nLegacy forms still load, and `ec-ctl config migrate` rewrites them: `enabled #false` \
         is `preset \"off\"` (unless the block names a preset); with `enabled #true`, each \
         `animation \"<name>\" duration=… curve=…` is a block for its event with the style \
         below, `duration` as milliseconds (an integer or `\"150ms\"`, `\"2s\"`, default \
         `{ANIMATION_DEFAULT_MS}`) and `curve` one of {} (default `{ANIMATION_DEFAULT_CURVE}`). \
         Without `enabled #true` they do nothing. Writing `animations.preset` removes them. \
         `migrate` also adds a `bar-layout` block for a written `bar.motion`, which stays.\n",
        ticks(EASING_CURVES),
    );
    let _ = writeln!(out, "| name | example | what it does |\n| --- | --- | --- |");
    for f in LEGACY_ANIMATIONS {
        let _ = writeln!(out, "| `{}` | {} | {} |", f.names[0], examples(f), f.doc);
    }
    out.push('\n');
}

fn collections(out: &mut String, cs: &[&Collection]) {
    for c in cs {
        let _ = writeln!(out, "### `{}`\n\n{}\n", c.node, c.doc);
        match c.node {
            "bind" => forms(out, "action", BIND_ACTIONS),
            "output" => {
                forms(out, "key", OUTPUT_KEYS);
                let _ = writeln!(
                    out,
                    "`<transform>` is one of {}; `0` is an alias of `normal`. `<lid-close>` is one of {}.\n",
                    ticks(OUTPUT_TRANSFORMS),
                    ticks(LID_CLOSE),
                );
            }
            "windowrule" => {
                out.push_str("Matchers, all of which must match. A rule with none is refused.\n\n");
                forms(out, "matcher", RULE_MATCHERS);
                for (m, why) in REFUSED_MATCHERS {
                    let _ = writeln!(out, "`{m}` is refused and drops its rule: {why}.\n");
                }
                out.push_str(
                    "Actions. The action and its argument are one string: \
                     `windowrule \"size 800x600\" { app-id \"mpv\"; }`.\n\n",
                );
                out.push_str("| action | file | example | what it does |\n| --- | --- | --- | --- |\n");
                for f in RULE_ACTION_FORMS {
                    let owner = rule_owner(f.names[0]).expect("every rule action form has an owner");
                    let _ = writeln!(
                        out,
                        "| {} | `{}` | {} | {} |",
                        syntax(f),
                        file(owner),
                        examples(f),
                        f.doc
                    );
                }
                out.push('\n');
            }
            "widget" => {
                forms(out, "key", WIDGET_KEYS);
                let _ = writeln!(out, "`<source>` is one of {}.\n", ticks(WIDGET_SOURCES));
            }
            _ => {}
        }
    }
}

fn generate() -> String {
    let mut out = String::new();
    out.push_str(
        "<!-- Generated by `cargo test -p ec-abyss-config --test config_doc`. Do not edit by hand:\n     \
         the source is `abyss/crates/ec-abyss-config/src/schema.rs`. -->\n\n\
         # Configuration reference\n\n\
         Every setting abyss understands, with the file it lives in (COMP-13 §1.3).\n\n\
         Configuration is two files, not one. `abyss.kdl` is everything cosmetic and\n\
         behavioural; `policy.kdl` is the security surface. The control socket — and so\n\
         every GUI, since a GUI writes only through the COMP-13 §1.4 API — can read both\n\
         and write only the first. Changing a policy setting is a decision a human makes\n\
         in a text editor, on purpose.\n\n\
         Both files are searched in `/etc/eclipse/` then `$XDG_CONFIG_HOME/eclipse/`, and\n\
         both are hot-reloaded. `ec-ctl config list` prints this same table with the\n\
         values you actually have; `ec-ctl config migrate` splits a legacy\n\
         single-file `abyss.kdl` into the two.\n\n\
         KDL v2: booleans are `#true` and `#false`, never bare `true`.\n\n\
         Add-ons are not configured here (ADR 0066). An installed add-on package's\n\
         manifest in `/usr/share/eclipse/addons/` turns on hooks; no key in either file\n\
         can. `get_config` reports them on every reply as `addons: [{\"id\", \"name\",\n\
         \"hooks\": [..], \"capture_requested\": bool}]` and `hooks_on: [..]`, and\n\
         `ec-ctl addons` prints the same.\n\n\
         With the `taskbar-widgets` hook on, the premade widget catalog in\n\
         `/usr/share/eclipse/widgets/*.kdl` is read first, below `/etc/eclipse/abyss.kdl`;\n\
         a catalog file may hold only `bar { widget … }`. A command widget that is not\n\
         an unedited premade runs only after you approve it in a compositor-drawn\n\
         prompt (ADR 0067); see `widget` below and the `review_widget` method.\n\n",
    );

    for owner in [Owner::Abyss, Owner::Policy] {
        let _ = writeln!(out, "## `{}`\n", file(owner));
        if owner == Owner::Policy {
            out.push_str("Read-only over the socket. A GUI shows these; it cannot change them.\n\n");
        }
        let mut nodes: Vec<&str> = Vec::new();
        for k in TABLE.iter().filter(|k| k.owner == owner) {
            let node = k.path.split('.').next().unwrap_or(k.path);
            if !nodes.contains(&node) {
                nodes.push(node);
            }
        }
        for node in nodes {
            let keys: Vec<&Key> = TABLE
                .iter()
                .filter(|k| k.owner == owner && k.path.split('.').next() == Some(node))
                .collect();
            let _ = writeln!(out, "### `{node}`\n");
            section(&mut out, &keys);
            if node == "animations" {
                animations(&mut out);
            }
        }
        let cs: Vec<&Collection> = COLLECTIONS.iter().filter(|c| c.owner == owner).collect();
        if !cs.is_empty() {
            collections(&mut out, &cs);
        }
    }
    out
}

#[test]
fn config_doc_is_current() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../docs/CONFIG.md");
    let want = generate();
    if std::env::var_os("UPDATE_CONFIG_DOC").is_some() {
        std::fs::write(path, &want).expect("write docs/CONFIG.md");
        return;
    }
    let have = std::fs::read_to_string(path).unwrap_or_default();
    assert!(
        have == want,
        "docs/CONFIG.md is stale; regenerate with \
         `UPDATE_CONFIG_DOC=1 cargo test -p ec-abyss-config --test config_doc`"
    );
}
