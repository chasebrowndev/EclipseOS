// SPDX-License-Identifier: AGPL-3.0-only
//! Config parser, validator and default-table tests.

use super::*;
// The held-modifier set the compositor passes in; same shape as the
// smithay `ModifiersState` these tests used to build.
use crate::input::Mods as ModifiersState;

fn abyss_src(p: &str) -> Source {
    Source {
        path: PathBuf::from(p),
        owner: schema::Owner::Abyss,
    }
}

fn policy_src(p: &str) -> Source {
    Source {
        path: PathBuf::from(p),
        owner: schema::Owner::Policy,
    }
}

/// COMP-13 §1.3: the security surface lives in `policy.kdl` and nowhere
/// else. A policy key written into `abyss.kdl` is refused rather than
/// honoured, because `abyss.kdl` is what the config GUI may write.
#[test]
fn ownership_is_refused_in_both_directions() {
    fn err(src: Source, text: &str) -> String {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config {
            cur: Some((src, text.to_owned())),
            ..Config::default()
        };
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        cfg.errors[0].message.clone()
    }

    // Whole node, policy-owned, in the wrong file.
    let m = err(abyss_src("a.kdl"), "capture {\n    allow #true\n}\n");
    assert!(m.contains("policy.kdl"), "{m}");

    // Whole node, abyss-owned, in policy.kdl.
    let m = err(policy_src("p.kdl"), "general {\n    gaps-in 4\n}\n");
    assert!(m.contains("abyss.kdl"), "{m}");

    // `misc` is mixed: the policy key is refused, the abyss key beside it
    // is not.
    let m = err(
        abyss_src("a.kdl"),
        "misc {\n    render-device \"/dev/dri/card0\"\n    scripted-input #true\n}\n",
    );
    assert!(
        m.contains("misc.scripted-input") && m.contains("policy.kdl"),
        "{m}"
    );

    // `windowrule` is decided per action, not per node.
    let m = err(abyss_src("a.kdl"), "windowrule \"no-agent\" app-id=\"x\"\n");
    assert!(m.contains("no-agent") && m.contains("policy.kdl"), "{m}");
    let m = err(policy_src("p.kdl"), "windowrule \"float\" app-id=\"x\"\n");
    assert!(m.contains("float") && m.contains("abyss.kdl"), "{m}");
}

/// Each directory contributes its policy file after its abyss file, so a
/// later abyss.d drop-in can never shadow policy.
#[test]
fn the_search_path_pairs_each_directory() {
    let path = search_path();
    let names: Vec<String> = path
        .iter()
        .map(|s| {
            format!(
                "{}:{}",
                s.path.display(),
                if s.owner == schema::Owner::Policy {
                    "p"
                } else {
                    "a"
                }
            )
        })
        .collect();
    assert!(names[0].ends_with("/etc/eclipse/abyss.kdl:a"), "{names:?}");
    assert!(names[1].ends_with("/etc/eclipse/policy.kdl:p"), "{names:?}");
    let policies: Vec<_> = path.iter().filter(|s| s.owner == schema::Owner::Policy).collect();
    assert!(policies
        .iter()
        .all(|s| s.path.file_name().unwrap() == "policy.kdl"));
}

/// COMP-13 §1.2: validation is total — an unknown key is an error, not a
/// warning, and it carries the position of the offending token.
#[test]
fn unknown_keys_are_errors_with_a_position() {
    let text = "general {\n    gaps-in 4\n    gaps-inn 4\n}\n";
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((abyss_src("/etc/eclipse/abyss.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.errors.len(), 1);
    let e = &cfg.errors[0];
    assert_eq!((e.line, e.col), (3, 5));
    assert!(e.message.contains("gaps-inn"), "{}", e.message);
    assert!(e.to_string().starts_with("/etc/eclipse/abyss.kdl:3:5: "));
    // The caret run underlines the offending token, on the right line.
    assert_eq!(
        e.to_string().lines().skip(1).collect::<Vec<_>>(),
        ["  |", "3 |     gaps-inn 4", "  |     ^^^^^^^^"]
    );
}

/// COMP-17 §2/§2.2, ADR 0062: `mode` and `components` parse, default to
/// Standard's values, and refuse an unknown id naming the key.
#[test]
fn mode_and_components_parse_default_and_refuse() {
    let d = Config::default();
    assert_eq!(d.mode, Mode::Hybrid);
    assert_eq!(
        (
            d.components.bar.as_str(),
            d.components.launcher.as_str(),
            d.components.notifications.as_str(),
            d.components.control_center.as_str()
        ),
        ("ec-hyperion-bar", "ec-launcher", "ec-toasts", "ec-center")
    );
    for (m, want) in [("wm", Mode::Wm), ("hybrid", Mode::Hybrid), ("de", Mode::De)] {
        let doc: KdlDocument = format!("mode \"{m}\"\n").parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.mode, want);
    }
    let doc: KdlDocument = "components {\n    bar \"waybar\"\n    launcher \"fuzzel\"\n    notifications \"mako\"\n    control-center \"none\"\n}\n"
            .parse()
            .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.components.bar, "waybar");
    assert_eq!(cfg.components.launcher, "fuzzel");
    assert_eq!(cfg.components.notifications, "mako");
    assert_eq!(cfg.components.control_center, "none");
    assert_eq!(
        schema::get(&cfg, "components.bar"),
        Some(schema::Value::Str("waybar".into()))
    );

    // Pre-`ec-` ids load as their new names (ADR 0070).
    let doc: KdlDocument = "components {\n    bar \"hyperion\"\n    launcher \"eclipse-launcher\"\n    notifications \"eclipse-toasts\"\n    control-center \"eclipse-center\"\n}\n"
            .parse()
            .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(
        (
            cfg.components.bar.as_str(),
            cfg.components.launcher.as_str(),
            cfg.components.notifications.as_str(),
            cfg.components.control_center.as_str()
        ),
        ("ec-hyperion-bar", "ec-launcher", "ec-toasts", "ec-center")
    );

    let text = "mode \"tiling\"\ncomponents {\n    bar \"polybar\"\n    launcher \"ec-toasts\"\n    control-center \"mako\"\n    notifications 3\n    dock \"x\"\n}\n";
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((abyss_src("/etc/eclipse/abyss.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
    assert!(
        cfg.errors[0].message.contains("mode"),
        "{}",
        cfg.errors[0].message
    );
    assert!(cfg.errors[1].message.contains("components.bar"));
    assert!(cfg.errors[2].message.contains("components.launcher"));
    assert!(cfg.errors[3].message.contains("components.control-center"));
    assert!(cfg.errors[4].message.contains("components.notifications"));
    assert_eq!(cfg.mode, Mode::Hybrid, "a refused value must not land");
    assert_eq!(cfg.components.bar, "ec-hyperion-bar");

    // abyss.kdl only.
    let text = "mode \"wm\"\n";
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((policy_src("/etc/eclipse/policy.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.errors.len(), 1);
    assert_eq!(cfg.mode, Mode::Hybrid);
}

/// Both keys hot-reload (COMP-17 §2, §2.2): `reload` is `Live`.
#[test]
fn mode_and_components_are_live() {
    for p in [
        "mode",
        "components.bar",
        "components.launcher",
        "components.notifications",
        "components.control-center",
    ] {
        let k = schema::get_key(p).expect(p);
        assert_eq!(k.reload, schema::Reload::Live, "{p}");
        assert_eq!(k.owner, schema::Owner::Abyss, "{p}");
    }
}

fn wallpaper_cfg(text: &str) -> Config {
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((abyss_src("/etc/eclipse/abyss.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    cfg
}

/// No `wallpaper` node: no image, `fill`, `#0b0906`, no overrides.
#[test]
fn wallpaper_defaults_when_absent() {
    let cfg = wallpaper_cfg("general { gaps-in 4; }\n");
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.wallpaper.path, None);
    assert_eq!(cfg.wallpaper.mode, "fill");
    assert_eq!(cfg.wallpaper.color, parse_color("#0b0906").unwrap());
    assert!(cfg.wallpaper.outputs.is_empty());
    assert_eq!(schema::get(&cfg, "wallpaper.path"), Some(schema::Value::Null));
}

#[test]
fn wallpaper_parses_each_mode_and_keys() {
    for m in ["fill", "fit", "center"] {
        let cfg = wallpaper_cfg(&format!(
            "wallpaper {{\n    path \"~/Pictures/x.png\"\n    mode \"{m}\"\n    color \"#102030\"\n}}\n"
        ));
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.wallpaper.mode, m);
        // Not expanded and not checked for existence: the daemon's job.
        assert_eq!(cfg.wallpaper.path.as_deref(), Some("~/Pictures/x.png"));
        assert_eq!(cfg.wallpaper.color, parse_color("#102030").unwrap());
        assert_eq!(
            schema::get(&cfg, "wallpaper.mode"),
            Some(schema::Value::Str(m.into()))
        );
    }
}

#[test]
fn wallpaper_refuses_bad_mode_and_color() {
    let cfg =
        wallpaper_cfg("wallpaper {\n    mode \"stretch\"\n    color \"#0b09\"\n    path 3\n    size 2\n}\n");
    assert_eq!(cfg.errors.len(), 4, "{:?}", cfg.errors);
    let m = &cfg.errors[0].message;
    assert!(
        m.contains("wallpaper.mode") && m.contains("fill, fit, center"),
        "{m}"
    );
    assert_eq!((cfg.errors[0].line, cfg.errors[0].col), (2, 5));
    let m = &cfg.errors[1].message;
    assert!(m.contains("wallpaper.color") && m.contains("#rrggbb"), "{m}");
    assert!(cfg.errors[2].message.contains("wallpaper.path"));
    assert!(cfg.errors[3].message.contains("size"));
    // A refused value never lands.
    assert_eq!(cfg.wallpaper.mode, "fill");
    assert_eq!(cfg.wallpaper.color, schema::WALLPAPER_DEFAULT_COLOR);
    assert_eq!(cfg.wallpaper.path, None);

    let cfg = wallpaper_cfg("wallpaper {\n    output \"DP-1\" { mode \"tile\"; }\n}\n");
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    assert!(cfg.errors[0].message.contains("wallpaper.output.mode"));
    assert_eq!(cfg.wallpaper.outputs[0].mode, None);
}

#[test]
fn wallpaper_per_output_override() {
    let cfg = wallpaper_cfg(
            "wallpaper {\n    path \"/a.png\"\n    output \"DP-1\" { mode \"fit\"; }\n    output \"HDMI-A-1\" { path \"/b.png\"; color \"#ffffff\"; }\n    output \"DP-1\" { color \"#000000\"; }\n}\n",
        );
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.wallpaper.path.as_deref(), Some("/a.png"));
    assert_eq!(cfg.wallpaper.mode, "fill");
    // One entry per name, file order; the later DP-1 block adds a key
    // without dropping the earlier one's.
    assert_eq!(
        cfg.wallpaper.outputs,
        [
            WallpaperOutput {
                name: "DP-1".into(),
                path: None,
                mode: Some("fit".into()),
                color: parse_color("#000000"),
            },
            WallpaperOutput {
                name: "HDMI-A-1".into(),
                path: Some("/b.png".into()),
                mode: None,
                color: parse_color("#ffffff"),
            },
        ]
    );
    let cfg = wallpaper_cfg("wallpaper {\n    output { mode \"fit\"; }\n}\n");
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    assert!(cfg.wallpaper.outputs.is_empty());
}

/// D-07 §4: `setup.*` parses, lives in `abyss.kdl` only, and an unknown or
/// ill-typed key is refused with its position like any other.
#[test]
fn setup_keys_parse_and_refuse_junk() {
    let doc: KdlDocument =
        "setup {\n    profile \"agentic\"\n    complete #true\n    pending-preset #true\n}\n"
            .parse()
            .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.setup.profile, "agentic");
    assert!(cfg.setup.complete && cfg.setup.pending_preset);

    let text =
        "setup {\n    profile \"standard\"\n    mode \"wm\"\n    profile \"nope\"\n    complete 1\n}\n";
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((abyss_src("/etc/eclipse/abyss.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.errors.len(), 3, "{:?}", cfg.errors);
    assert!(cfg.errors[0]
        .to_string()
        .starts_with("/etc/eclipse/abyss.kdl:3:5: "));
    assert!(
        cfg.errors[0].message.contains("\"mode\""),
        "{}",
        cfg.errors[0].message
    );
    assert_eq!(cfg.errors[1].line, 4);
    assert_eq!(cfg.errors[2].line, 5);
    assert_eq!(cfg.setup.profile, "standard", "a refused value must not land");
    assert!(!cfg.setup.complete);

    // The whole node is abyss-owned: policy.kdl may not carry it.
    let text = "setup {\n    complete #true\n}\n";
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((policy_src("/etc/eclipse/policy.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.errors.len(), 1);
    assert!(
        cfg.errors[0].message.contains("abyss.kdl"),
        "{}",
        cfg.errors[0].message
    );
    assert!(!cfg.setup.complete);
}

/// Anti-drift side (b): the parser's reject path consults the schema, so a
/// near-miss is named and a key the schema claims but the parser does not
/// handle is reported as an abyss bug rather than as the human's mistake.
#[test]
fn unknown_keys_suggest_the_schema_path_they_nearly_are() {
    fn err(text: &str) -> String {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.errors.len(), 1);
        cfg.errors[0].message.clone()
    }

    let m = err("general {\n    gaps-inn 4\n}\n");
    assert!(m.contains("did you mean \"general.gaps-in\""), "{m}");

    // Nothing close enough: no suggestion rather than a misleading one.
    let m = err("general {\n    quux 4\n}\n");
    assert!(!m.contains("did you mean"), "{m}");
}

/// HW-04 rule: every shipped bind spawns a binary in `eclipseos-meta`'s
/// dependency closure, or it reads to the user as a dead keybind. The
/// closure is read from `packaging/pkg/eclipseos/PKGBUILD`, so it cannot drift.
#[test]
fn default_bind_spawns_name_shipped_binaries() {
    let pkgbuild = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../packaging/pkg/eclipseos/PKGBUILD"
    ))
    .expect("PKGBUILD readable");
    // `eclipseos-meta`'s `depends=( … )`. Its `optdepends` (the hyperion
    // add-on, ADR 0066) are not the closure: a bind cannot rely on them.
    let meta = &pkgbuild[pkgbuild.find("package_eclipseos-meta()").expect("meta package")..];
    let depends = &meta[meta.find("depends=(").expect("meta depends") + 9..];
    let depends: Vec<&str> = depends[..depends.find(')').unwrap()].split_whitespace().collect();
    // Binaries the split packages in that closure install: `_bin NAME`
    // and `for b in A B; do _bin "$b"; done`, under `package_NAME()`.
    let mut ours: Vec<&str> = Vec::new();
    let mut in_closure = false;
    for line in pkgbuild.lines().map(str::trim) {
        if let Some(pkg) = line.strip_prefix("package_") {
            let pkg = pkg.split("()").next().unwrap_or_default();
            in_closure = depends.contains(&pkg);
        } else if !in_closure {
            continue;
        } else if let Some(names) = line.strip_prefix("for b in ") {
            ours.extend(names.split(';').next().unwrap_or_default().split_whitespace());
        } else if let Some(name) = line.strip_prefix("_bin ") {
            ours.push(name.split_whitespace().next().unwrap_or_default());
        }
    }
    assert!(ours.contains(&"ec-launcher"), "PKGBUILD parse: {ours:?}");
    assert!(
        !ours.contains(&"ec-hyperion-bar"),
        "hyperion is an add-on, not the closure"
    );
    // Third-party binaries: (argv0, providing package). `base` is the
    // Arch base every image has.
    const EXTERNAL: &[(&str, &str)] = &[
        ("foot", "foot"),
        ("wpctl", "wireplumber"),
        ("brightnessctl", "brightnessctl"),
        ("playerctl", "playerctl"),
        ("loginctl", "base"),
    ];
    for bind in default_binds() {
        let Action::Spawn(cmd) = &bind.action else {
            continue;
        };
        let argv0 = cmd.split_whitespace().next().unwrap_or_default();
        let shipped = ours.contains(&argv0)
            || EXTERNAL
                .iter()
                .any(|(bin, pkg)| *bin == argv0 && (*pkg == "base" || depends.contains(pkg)));
        assert!(
            shipped,
            "default bind {:?}+{:?} spawns {argv0:?}, which no EclipseOS package installs",
            bind.mods, bind.key
        );
    }
}

/// CFG-01: `misc { xwayland … }` was accepted and ignored; it is an error
/// that points at the real node (COMP-13 §1.1, amended C-05).
#[test]
fn misc_xwayland_is_rejected_with_a_hint() {
    let doc: KdlDocument = "misc {\n    xwayland #false\n}\n".parse().unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    let m = &cfg.errors[0].message;
    assert!(m.contains("xwayland { enable"), "{m}");
    assert!(cfg.xwayland.enable, "the misc key must not disable X11");
}

/// `bar.clock.*` and `bar.popup-anchor`: defaults, parse, and a bad
/// anchor rejected without moving off the default.
#[test]
fn bar_clock_and_popup_anchor_parse() {
    fn cfg(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        cfg
    }
    let d = Config::default();
    assert_eq!(
        d.bar.clock,
        BarClock {
            hour_12: true,
            date_mdy: true
        }
    );
    assert_eq!(d.bar.popup_anchor, BarPopupAnchor::Cell);

    let c = cfg("bar {\n    clock { hour-12 #false; date-mdy #false }\n    popup-anchor \"pointer\"\n}\n");
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    assert_eq!(
        c.bar.clock,
        BarClock {
            hour_12: false,
            date_mdy: false
        }
    );
    assert_eq!(c.bar.popup_anchor, BarPopupAnchor::Pointer);

    let c = cfg("bar { popup-anchor \"corner\" }\n");
    assert_eq!(c.errors.len(), 1);
    assert!(
        c.errors[0].message.contains("\"cell\" or \"pointer\""),
        "{}",
        c.errors[0].message
    );
    assert_eq!(c.bar.popup_anchor, BarPopupAnchor::Cell);

    let c = cfg("bar { clock { hour-24 #true } }\n");
    assert_eq!(c.errors.len(), 1, "{:?}", c.errors);
}

/// `bar.launcher-style` is the deprecated alias of `launcher.style`:
/// centred by default, `menu` parses, and a bad style is rejected
/// without moving off the default.
#[test]
fn bar_launcher_style_parses() {
    fn cfg(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        cfg
    }
    assert_eq!(Config::default().launcher.style, LauncherStyle::Centered);

    let c = cfg("bar { launcher-style \"menu\" }\n");
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    assert_eq!(c.launcher.style, LauncherStyle::Menu);
    assert_eq!(c.launcher.style.name(), "menu");

    let c = cfg("bar { launcher-style \"sideways\" }\n");
    assert_eq!(c.errors.len(), 1);
    assert!(
        c.errors[0].message.contains("\"centered\" or \"menu\""),
        "{}",
        c.errors[0].message
    );
    assert_eq!(c.launcher.style, LauncherStyle::Centered);
}

/// `launcher.style` wins over the deprecated alias whichever comes first.
#[test]
fn launcher_style_beats_the_bar_alias() {
    fn cfg(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        cfg
    }
    let c = cfg("bar { launcher-style \"menu\" }\nlauncher { style \"centered\" }\n");
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    assert_eq!(c.launcher.style, LauncherStyle::Centered);
    let c = cfg("launcher { style \"centered\" }\nbar { launcher-style \"menu\" }\n");
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    assert_eq!(c.launcher.style, LauncherStyle::Centered);
    let c = cfg("launcher { style \"menu\" }\n");
    assert_eq!(c.launcher.style, LauncherStyle::Menu);
}

/// The `launcher` block: sizes, anchor and search flags parse, and out of
/// range values keep the default.
#[test]
fn launcher_block_parses() {
    fn cfg(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        cfg
    }
    let d = Config::default();
    assert_eq!(d.launcher.centered.width, 540);
    assert_eq!(d.launcher.centered.max_rows, 6);
    assert_eq!(d.launcher.centered.anchor, LauncherAnchor::Center);
    assert_eq!(d.launcher.menu.max_rows, 8);
    assert!(!d.launcher.search.path_binaries);
    assert!(d.launcher.search.terminal_apps);
    assert!(!d.launcher.search.match_descriptions);
    assert!(d.launcher.search.frecency);
    assert!(d.ui.show_key_hints);
    assert!(d.settings.search_frecency);

    let c = cfg(concat!(
        "launcher {\n",
        "  centered { width 720; max-rows 12; anchor \"top\"; }\n",
        "  menu { max-rows 5; }\n",
        "  search { path-binaries; terminal-apps #false; match-descriptions #true; }\n",
        "}\n",
        "ui { show-key-hints #false; }\n",
    ));
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    assert_eq!(c.launcher.centered.width, 720);
    assert_eq!(c.launcher.centered.max_rows, 12);
    assert_eq!(c.launcher.centered.anchor, LauncherAnchor::Top);
    assert_eq!(c.launcher.menu.max_rows, 5);
    assert!(c.launcher.search.path_binaries);
    assert!(!c.launcher.search.terminal_apps);
    assert!(c.launcher.search.match_descriptions);
    assert!(!c.ui.show_key_hints);

    let c = cfg("settings { search { frecency #false; } }\n");
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    assert!(!c.settings.search_frecency);

    let c = cfg("launcher { centered { width 20; anchor \"left\"; }; menu { max-rows 99; } }\n");
    assert_eq!(c.errors.len(), 3, "{:?}", c.errors);
    assert_eq!(c.launcher.centered.width, 540);
    assert_eq!(c.launcher.centered.anchor, LauncherAnchor::Center);
    assert_eq!(c.launcher.menu.max_rows, 8);
}

/// Launcher chords: `Super+R` is the chord `bind "SUPER" "r"` makes,
/// `none` is no chord, and the reserved chords are refused.
#[test]
fn launcher_chords_parse() {
    let sup = m(true, false, false, false);
    assert_eq!(parse_chord("Super+R"), Ok(Some((sup, Keysym::r))));
    assert_eq!(parse_chord("SUPER r"), Ok(Some((sup, Keysym::r))));
    assert_eq!(
        parse_chord("Super+Shift+Return"),
        Ok(Some((m(true, true, false, false), Keysym::Return)))
    );
    assert_eq!(parse_chord("F2"), Ok(Some((Mods::default(), Keysym::F2))));
    assert_eq!(parse_chord("none"), Ok(None));
    assert_eq!(parse_chord(""), Ok(None));
    assert!(parse_chord("Super+Escape").is_err());
    assert!(parse_chord("Super+space").is_err());
    assert!(parse_chord("Super+").is_err());
    assert!(parse_chord("Hyper+r").is_err());
}

/// `launcher.bind.*` replaces the shipped Super+E / Super+R launcher
/// binds; a `bind` block on the same chord still wins.
#[test]
fn launcher_binds_replace_the_defaults() {
    let sup = m(true, false, false, false);
    let spawn = Action::Spawn("ec-launcher".into());
    let launcher_chords = |binds: &[Bind]| {
        let mut v: Vec<_> = binds
            .iter()
            .filter(|b| b.action == spawn)
            .map(|b| (b.mods, b.key))
            .collect();
        v.sort_by_key(|(_, k)| k.raw());
        v
    };

    let stock = defaults_with_launcher(&Launcher::default());
    assert_eq!(launcher_chords(&stock), vec![(sup, Keysym::e), (sup, Keysym::r)]);

    let doc: KdlDocument = "launcher { bind { open \"Super+o\"; run \"none\"; } }\n"
        .parse()
        .unwrap();
    let mut c = Config::default();
    c.apply(&doc, &mut Vec::new());
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    let binds = defaults_with_launcher(&c.launcher);
    assert_eq!(launcher_chords(&binds), vec![(sup, Keysym::o)]);

    let doc: KdlDocument = "launcher { bind { open \"Super+Escape\"; } }\n".parse().unwrap();
    let mut c = Config::default();
    c.apply(&doc, &mut Vec::new());
    assert_eq!(c.errors.len(), 1, "{:?}", c.errors);
    assert_eq!(c.launcher.bind.open.text, "Super+E");
}

/// `bar.eye`: on by default, a bare node is on, `#false` turns it off.
#[test]
fn bar_eye_parses() {
    fn cfg(text: &str) -> Config {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        cfg
    }
    assert!(Config::default().bar.eye);
    let c = cfg("bar { eye #false }\n");
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    assert!(!c.bar.eye);
    let c = cfg("bar { eye }\n");
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    assert!(c.bar.eye);
}

/// `bar.tray`: an absent `pinned` is "the bar decides", a bare one pins
/// nothing, and order is kept exactly as written.
#[test]
fn bar_tray_lists_keep_order_and_unset_differs_from_empty() {
    fn tray(text: &str) -> BarTray {
        let doc: KdlDocument = text.parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        cfg.bar.tray
    }
    assert_eq!(tray("bar { position top }\n"), BarTray::default());
    assert_eq!(tray("bar { tray { pinned } }\n").pinned, Some(vec![]));
    let t = tray("bar { tray { pinned volume \"org.kde.x\" network; hidden battery } }\n");
    assert_eq!(
        t.pinned.as_deref(),
        Some(&["volume".to_string(), "org.kde.x".into(), "network".into()][..])
    );
    assert_eq!(t.hidden, ["battery"]);
}

/// Apply `text` as `a.kdl`, returning the config and its refusals.
pub(crate) fn widgets_cfg(text: &str) -> Config {
    parse_single_for_tests(text)
}

/// `bar.widgets` and `bar.motion` (ADR 0065): defaults, a full block, and
/// a `custom:` id resolved by a `widget` block written after it.
#[test]
fn bar_widgets_parse() {
    let d = Config::default().bar;
    assert_eq!(d.widgets.order, schema::BAR_WIDGET_DEFAULT_ORDER);
    assert_eq!(d.widgets.important, ["clock", "battery"]);
    assert_eq!(d.motion.curve, "spring");
    let cfg = widgets_cfg(
            "bar {\n    widgets {\n        order \"clock\" \"custom:cpu\" \"tray\"\n        important \"custom:cpu\"\n        \
             now-playing { art #false; visualizer; remote-art #false; }\n        system-usage { interval-ms \"2s\"; gpu #false; disk-path \"/home\"; }\n        \
             volume { step 10; scroll #false; max-percent 150; }\n    }\n    motion { enabled #false; duration-ms 300; curve \"ease-out\"; }\n    \
             widget \"cpu\" { source \"usage.cpu\"; format \"{}%\"; }\n}\n",
        );
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    let w = &cfg.bar.widgets;
    assert_eq!(w.order, ["clock", "custom:cpu", "tray"]);
    assert_eq!(w.important, ["custom:cpu"]);
    assert!(!w.now_playing.art && w.now_playing.visualizer && !w.now_playing.remote_art);
    assert!(Config::default().bar.widgets.now_playing.remote_art);
    assert_eq!(w.system_usage.interval_ms, 2000);
    assert!(!w.system_usage.gpu && w.system_usage.disk);
    assert_eq!(w.system_usage.disk_path, "/home");
    assert_eq!(
        (w.volume.step, w.volume.scroll, w.volume.max_percent),
        (10, false, 150)
    );
    let m = &cfg.bar.motion;
    assert_eq!(
        (m.enabled, m.duration_ms, m.curve.as_str()),
        (false, 300, "ease-out")
    );
    assert_eq!(
        cfg.bar.custom_widgets[0].kind,
        CustomWidgetKind::Source {
            source: "usage.cpu".into(),
            format: "{}%".into()
        }
    );
    // An empty order draws nothing and is not an error.
    let cfg = widgets_cfg("bar { widgets { order; } }\n");
    assert!(cfg.errors.is_empty() && cfg.bar.widgets.order.is_empty());
}

/// An unknown id is refused at its own position with a did-you-mean, and
/// only that id is dropped.
#[test]
fn an_unknown_widget_id_is_refused_where_it_stands() {
    let cfg = widgets_cfg("bar {\n    widgets { order \"clock\" \"batery\"; }\n}\n");
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    let e = &cfg.errors[0];
    assert!(e.message.contains("did you mean \"battery\""), "{}", e.message);
    assert_eq!((e.line, e.col, e.span_len), (2, 29, 8));
    assert_eq!(cfg.bar.widgets.order, ["clock"]);

    let cfg = widgets_cfg("bar { widgets { important \"clock\" \"clock\" \"zzzzzz\"; } }\n");
    assert_eq!(cfg.errors.len(), 2, "{:?}", cfg.errors);
    assert!(cfg.errors[0].message.contains("twice"));
    assert!(cfg.errors[1].message.contains("built-in widgets are"));
}

/// `custom:<name>` must name a `widget` block, wherever in the file it is.
#[test]
fn a_custom_id_must_name_a_widget_block() {
    let cfg = widgets_cfg(
        "bar {\n    widgets { order \"custom:wether\"; }\n    widget \"weather\" { exec \"curl\"; }\n}\n",
    );
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    let e = &cfg.errors[0];
    assert!(e.message.contains("did you mean"), "{}", e.message);
    assert_eq!((e.line, e.col), (2, 21));
    let cfg = widgets_cfg("bar { widgets { order \"custom:\"; } }\n");
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
}

/// One bad field in a `widget` block is one error, at the field: the
/// refused block is not also reported missing by a `custom:` id naming it.
#[test]
fn an_invalid_widget_block_is_not_also_missing() {
    let cfg = widgets_cfg(
        "bar {\n    widgets { order \"custom:a\"; }\n    widget \"a\" { exec \"x\"; interval-ms 10; }\n}\n",
    );
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    let e = &cfg.errors[0];
    assert!(!e.message.contains("names no widget block"), "{}", e.message);
    assert_eq!(e.line, 3);
    assert!(cfg.bar.custom_widgets.is_empty());
}

/// An order entry whose block is truly absent is reported missing.
#[test]
fn an_absent_widget_block_is_reported_missing() {
    let cfg = widgets_cfg("bar {\n    widgets { order \"custom:a\"; }\n}\n");
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    let e = &cfg.errors[0];
    assert!(
        e.message.contains("custom:a names no widget block"),
        "{}",
        e.message
    );
    assert_eq!((e.line, e.col), (2, 21));
}

/// A `widget` block is all or nothing, and a later one replaces an
/// earlier one of the same name in place.
#[test]
fn widget_blocks_are_validated_whole() {
    for bad in [
        "widget \"a\" { }",
        "widget \"a\" { exec \"x\"; source \"usage.cpu\"; }",
        "widget \"a\" { exec \"x\"; stream; interval-ms 1000; }",
        "widget \"a\" { exec \"x\"; format \"{}\"; }",
        "widget \"a\" { source \"usage.cpu\"; interval-ms 1000; }",
        "widget \"a\" { source \"usage.cpux\"; }",
        "widget \"a\" { exec; }",
        "widget \"a\" { exec \"x\" 1; }",
        "widget \"a\" { exec \"x\"; interval-ms 10; }",
        "widget \"a\" { exec \"x\"; colour \"red\"; }",
        "widget \"a\" { exec \"x\"; exec \"y\"; }",
        "widget { exec \"x\"; }",
    ] {
        let cfg = widgets_cfg(&format!("bar {{ {bad} }}\n"));
        assert!(!cfg.errors.is_empty(), "{bad}");
        assert!(cfg.bar.custom_widgets.is_empty(), "{bad}");
    }
    let cfg = widgets_cfg(
            "bar { widget \"a\" { exec \"x\"; }; widget \"b\" { exec \"y\"; stream; }; widget \"a\" { exec \"z\"; interval-ms \"1m\"; } }\n",
        );
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    let names: Vec<_> = cfg.bar.custom_widgets.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(
        cfg.bar.custom_widgets[0].kind,
        CustomWidgetKind::Exec {
            argv: vec!["z".into()],
            interval_ms: 60_000
        }
    );
    assert_eq!(
        cfg.bar.custom_widgets[1].kind,
        CustomWidgetKind::Stream {
            argv: vec!["y".into()]
        }
    );
}

/// Every premade in `packaging/widgets/` (ADR 0067) loads through the same
/// path as a user `widget` block: one block, no refusals, and a command
/// widget (exec or stream), never a declarative `source`.
#[test]
fn premade_widgets_are_valid_command_widgets() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../packaging/widgets");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "kdl") {
            continue;
        }
        n += 1;
        let text = std::fs::read_to_string(&path).unwrap();
        let cfg = widgets_cfg(&text);
        assert!(cfg.errors.is_empty(), "{}: {:?}", path.display(), cfg.errors);
        let [w] = &cfg.bar.custom_widgets[..] else {
            panic!("{}: expected exactly one widget block", path.display());
        };
        assert_eq!(Some(w.name.as_str()), path.file_stem().and_then(|s| s.to_str()));
        let argv = match &w.kind {
            CustomWidgetKind::Exec { argv, .. } | CustomWidgetKind::Stream { argv } => argv,
            CustomWidgetKind::Source { .. } => {
                panic!("{}: premade must be a command widget", path.display())
            }
        };
        // Argv only: nothing handed to a shell or an interpreter.
        for a in [
            Some(argv),
            w.on_click.as_ref(),
            w.on_scroll_up.as_ref(),
            w.on_scroll_down.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            let argv0 = a[0].rsplit('/').next().unwrap();
            assert!(
                !["sh", "bash", "zsh", "dash", "fish", "env", "python", "python3", "perl"].contains(&argv0),
                "{}: {argv0} is a shell or interpreter",
                path.display()
            );
            assert!(!a.iter().any(|s| s == "-c"), "{}: -c", path.display());
        }
    }
    assert!(n > 0, "no premade widgets in {}", dir.display());
}

/// Out-of-range and mistyped widget settings are refused, keeping the
/// default; a bad motion curve too, and the legacy `animation` node still
/// refuses a new-style event name.
#[test]
fn widget_settings_out_of_range_keep_the_default() {
    let cfg = widgets_cfg(
            "bar { widgets { volume { step 0; max-percent 200; }; system-usage { interval-ms 100; disk-path \"home\"; }; }; motion { curve \"wobble\"; duration-ms 5000; } }\n",
        );
    assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
    assert_eq!(cfg.bar.widgets, BarWidgets::default());
    assert_eq!(cfg.bar.motion, BarMotion::default());
    let cfg = widgets_cfg("animations { enabled #true; animation \"window-open\" curve=\"spring\"; }\n");
    assert!(!cfg.errors.is_empty());
}

/// Built-in applet ids in `bar.tray` still load: deprecated, not refused.
#[test]
fn legacy_tray_ids_still_load() {
    let cfg = widgets_cfg("bar { tray { pinned \"volume\" \"org.kde.x\"; hidden \"battery\"; } }\n");
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.bar.tray.hidden, ["battery"]);
}

/// A tab-indented line keeps its tabs in the caret gutter so the run still
/// lands under the token whatever tab width the terminal uses.
#[test]
fn the_caret_gutter_preserves_tabs() {
    let text = "general {\n\tgaps-inn 4\n}\n";
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((abyss_src("a.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(
        cfg.errors[0].to_string().lines().last().unwrap(),
        "  | \t^^^^^^^^"
    );
}

/// A valid config records no refusals, so hot-reload applies it.
#[test]
fn a_valid_config_has_no_errors() {
    let doc: KdlDocument = "general { gaps-in 4; layout \"master\" }\n".parse().unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
}

/// An unparseable file is a refusal, not a silent fall back to defaults —
/// which used to wipe the live config on hot-reload.
#[test]
fn an_unparseable_file_is_refused_not_defaulted() {
    let dir = std::env::temp_dir().join(format!("abyss-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("abyss.kdl");
    std::fs::write(&f, "general { gaps-in \"unterminated\n").unwrap();
    let cfg = Config::load(Some(&f));
    assert!(!cfg.errors.is_empty());
    assert_eq!(cfg.errors[0].file, f);
    std::fs::remove_dir_all(&dir).ok();
}

/// A config that declares binds extends the defaults; it does not replace
/// the table. The live regression this covers: a 3-bind user config left a
/// 3-bind table, dropping the Super+Escape override chord.
#[test]
fn config_binds_extend_defaults() {
    let dir = std::env::temp_dir().join(format!("abyss-cfg-merge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("abyss.kdl");
    std::fs::write(&f, "bind \"SUPER\" \"F1\" { quit; }\n").unwrap();
    let cfg = Config::load(Some(&f));
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.binds.len(), default_binds().len() + 1);
    assert!(cfg.binds.iter().any(|b| b.key == Keysym::F1));

    // The two reserved chords survive any config (COMP-04 §6, COMP-13 §1.1).
    let sup = m(true, false, false, false);
    assert!(cfg
        .binds
        .iter()
        .any(|b| b.mods == sup && b.key == Keysym::Escape && matches!(b.action, Action::AgentOverride)));
    assert!(cfg
        .binds
        .iter()
        .any(|b| b.mods == sup && b.key == Keysym::space && matches!(b.action, Action::AgentAttention)));
    std::fs::remove_dir_all(&dir).ok();
}

/// A config bind on a chord the defaults already use wins the lookup.
#[test]
fn config_bind_overrides_default_chord() {
    let dir = std::env::temp_dir().join(format!("abyss-cfg-override-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("abyss.kdl");
    std::fs::write(&f, "bind \"SUPER\" \"q\" { spawn \"alacritty\"; }\n").unwrap();
    let cfg = Config::load(Some(&f));
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.binds.len(), default_binds().len(), "override, not addition");
    let mods = ModifiersState {
        logo: true,
        ..Default::default()
    };
    match cfg.action_for(&mods, Keysym::q) {
        Some(Action::Spawn(cmd)) => assert_eq!(cmd, "alacritty"),
        other => panic!("expected the config spawn, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A later source replaces an earlier one for the same chord.
#[test]
fn later_config_bind_wins() {
    let binds = merge_binds(
        default_binds(),
        vec![
            Bind {
                mods: m(true, false, false, false),
                key: Keysym::F1,
                action: Action::Spawn("first".into()),
            },
            Bind {
                mods: m(true, false, false, false),
                key: Keysym::F1,
                action: Action::Spawn("second".into()),
            },
        ],
    );
    assert_eq!(binds.len(), default_binds().len() + 1);
    let f1 = binds.iter().find(|b| b.key == Keysym::F1).unwrap();
    assert!(matches!(&f1.action, Action::Spawn(c) if c == "second"));
}

#[test]
fn parses_input() {
    let doc: KdlDocument = r#"
            input {
                kb-layout "de"
                kb-options "compose:ralt"
                repeat-rate 25
                repeat-delay 400
                accel-profile "flat"
                touchpad { natural-scroll #true; tap-to-click #true; dwt #false; click-method "button-areas" }
            }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    let mut binds = Vec::new();
    cfg.apply(&doc, &mut binds);
    assert_eq!(cfg.input.kb_layout, "de");
    assert_eq!(cfg.input.kb_options.as_deref(), Some("compose:ralt"));
    assert_eq!((cfg.input.repeat_rate, cfg.input.repeat_delay), (25, 400));
    assert_eq!(cfg.input.accel_profile, "flat");
    assert!(cfg.input.touchpad.natural_scroll && cfg.input.touchpad.tap_to_click);
    assert!(!cfg.input.touchpad.dwt);
    assert_eq!(cfg.input.touchpad.click_method, "button-areas");
    assert_eq!(Config::default().input.touchpad.click_method, "clickfinger");
    let d = Config::default().input;
    assert_eq!((d.accel_speed, d.scroll_method.as_str()), (0.0, "default"));
    assert!(d.touchpad.tap_and_drag && d.devices.is_empty());
    assert_eq!(d.touchpad.scroll_method, "two-finger");
    // The override chord is built in, never taken from the config.
    assert!(binds.is_empty());
    assert!(default_binds()
        .iter()
        .any(|b| b.key == Keysym::Escape && matches!(b.action, crate::input::Action::AgentOverride)));
    assert!(default_binds()
        .iter()
        .any(|b| b.key == Keysym::space && matches!(b.action, crate::input::Action::AgentAttention)));
}

/// Repeat delay takes the schema's full 0..=5000 ms, unclamped, and
/// refuses anything past it.
#[test]
fn repeat_delay_range() {
    for (text, want, errs) in [("600", 600, 0), ("5000", 5000, 0), ("5001", 300, 1)] {
        let doc: KdlDocument = format!("input {{ repeat-delay {text} }}").parse().unwrap();
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.input.repeat_delay, want, "repeat-delay {text}");
        assert_eq!(cfg.errors.len(), errs, "repeat-delay {text}");
    }
}

#[test]
fn parses_decoration_and_animations() {
    let doc: KdlDocument = r#"
            decoration {
                rounding 8
                active-opacity 1.0
                inactive-opacity 0.95
                dim-inactive 0.2
                blur { enabled #false; size 12; passes 3 }
                shadow { enabled #true; range 20 }
                glow { enabled #true; inactive #false; strength 80 }
            }
            animations {
                enabled #true
                animation "windows" duration="150ms" curve="ease-out"
                animation "workspaces" duration=200 curve="linear"
            }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.decoration.rounding, 8);
    assert_eq!(cfg.decoration.active_opacity, 1.0);
    assert_eq!(cfg.decoration.inactive_opacity, 0.95);
    assert_eq!(cfg.decoration.dim_inactive, 0.2);
    assert_eq!(cfg.decoration.blur.mode, BlurMode::Off, "legacy `enabled #false`");
    assert_eq!((cfg.decoration.blur.size, cfg.decoration.blur.passes), (12, 3));
    assert!(cfg.decoration.shadow.enabled && cfg.decoration.shadow.range == 20);
    let g = &cfg.decoration.glow;
    assert_eq!(
        (g.enabled, g.active, g.inactive, g.strength),
        (true, true, false, 80)
    );
    assert!(cfg.decoration.any_window_effect());
    // Legacy nodes beside `enabled #true` are full overrides of their
    // event, under the default preset.
    use animations::{Curve, Event, Override};
    assert_eq!(cfg.animations.preset, animations::Preset::Smooth);
    assert_eq!(
        cfg.animations.overrides.get(&Event::WindowMove),
        Some(&Override {
            style: Some("glide".into()),
            duration_ms: Some(150),
            curve: Some(Curve::EaseOut),
        })
    );
    let ws = cfg.animations.resolve(Event::WorkspaceSwitch);
    assert_eq!(
        (ws.style.as_str(), ws.duration_ms, ws.curve),
        ("slide", 200, Curve::Linear)
    );
    assert_eq!(cfg.animations.resolve(Event::WindowOpen).style, "pop");
}

#[test]
fn decoration_defaults_round_but_are_otherwise_no_effect() {
    let cfg = Config::default();
    // Rounding (9px) and the shadow ship on (C-16), so any_window_effect()
    // is already true out of the box. Content never fades: both
    // opacities ship at 1.0, and blur follows the surface's opaque region
    // instead; dim is untouched.
    assert!(cfg.decoration.any_window_effect());
    assert!(cfg.decoration.shadow.enabled);
    assert_eq!(cfg.decoration.shadow.range, 16);
    assert!(!cfg.decoration.glow.on());
    assert_eq!(cfg.decoration.rounding, 9);
    assert_eq!(cfg.decoration.active_opacity, 1.0);
    assert_eq!(cfg.decoration.inactive_opacity, 1.0);
    assert_eq!(cfg.decoration.dim_inactive, 0.0);
    // Animations ship on (the Smooth preset), with nothing overridden.
    assert_eq!(cfg.animations.preset, animations::Preset::Smooth);
    assert!(cfg.animations.any() && !cfg.animations.custom());
}

/// Blur ships on, in glass mode (C-16): translucent surfaces get the
/// Liquid Glass pass without being asked. It costs a render pass, so it
/// is called out here rather than folded into the no-effect test above
/// --- if this flips, the schema default column and `docs/CONFIG.md` flip
/// with it.
#[test]
fn blur_ships_enabled() {
    assert_eq!(Config::default().decoration.blur.mode, BlurMode::Glass);
}

fn blur_cfg(text: &str) -> Config {
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    cfg
}

#[test]
fn blur_mode_parses_every_mode() {
    for (name, want) in [
        ("off", BlurMode::Off),
        ("blur", BlurMode::Blur),
        ("frost", BlurMode::Frost),
        ("glass", BlurMode::Glass),
    ] {
        let cfg = blur_cfg(&format!("decoration {{ blur {{ mode {name:?} }} }}\n"));
        assert!(cfg.errors.is_empty(), "{name}: {:?}", cfg.errors);
        assert_eq!(cfg.decoration.blur.mode, want);
        assert_eq!(want.name(), name);
    }
    assert_eq!(BlurMode::NAMES, ["off", "blur", "frost", "glass"]);
    let cfg = blur_cfg("decoration { blur { mode \"liquid\" } }\n");
    assert_eq!(cfg.errors.len(), 1);
    assert_eq!(
        cfg.decoration.blur.mode,
        BlurMode::Glass,
        "a bad mode keeps the default"
    );
}

/// The pre-`mode` bool still loads: false is `off`, true is plain `blur`.
#[test]
fn legacy_blur_enabled_maps_onto_mode() {
    let cfg = blur_cfg("decoration { blur { enabled #false } }\n");
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.decoration.blur.mode, BlurMode::Off);
    let cfg = blur_cfg("decoration { blur { enabled #true } }\n");
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.decoration.blur.mode, BlurMode::Blur);
}

/// An explicit `mode` wins over a legacy `enabled` in either order.
#[test]
fn blur_mode_wins_over_enabled_regardless_of_order() {
    for text in [
        "decoration { blur { mode \"glass\"; enabled #false } }\n",
        "decoration { blur { enabled #false; mode \"glass\" } }\n",
    ] {
        let cfg = blur_cfg(text);
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.decoration.blur.mode, BlurMode::Glass, "{text}");
    }
    let cfg = blur_cfg("decoration { blur { mode \"off\"; enabled #true } }\n");
    assert_eq!(cfg.decoration.blur.mode, BlurMode::Off);
}

#[test]
fn glass_and_frost_tunables_parse_and_bad_ones_keep_defaults() {
    let cfg = blur_cfg(
            "decoration { blur {\n  glass { refraction 20; bevel 8; dispersion 0.5; rim 1.0 }\n  frost { tint \"#10203040\" }\n} }\n",
        );
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    let g = &cfg.decoration.blur.glass;
    assert_eq!((g.refraction, g.bevel, g.dispersion, g.rim), (20, 8, 0.5, 1.0));
    assert_eq!(cfg.decoration.blur.frost.tint[3], 0x40 as f32 / 255.0);
    let cfg = blur_cfg(
            "decoration { blur {\n  glass { refraction 99; bevel 0; dispersion 2.0; rim -1.0; nope 1 }\n  frost { tint \"gold\" }\n} }\n",
        );
    assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
    assert_eq!(cfg.decoration.blur.glass, GlassBlur::default());
    assert_eq!(cfg.decoration.blur.frost, FrostBlur::default());
}

#[test]
fn annotation_colours_parse_and_bad_ones_keep_defaults() {
    let cfg = parse_single_for_tests(
        "annotations {\n  accent \"#112233\"\n  danger \"#ff000080\"\n  selection-dim \"#00000099\"\n}\n",
    );
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    let a = &cfg.annotations;
    assert_eq!(a.accent, rgba8(0x11, 0x22, 0x33, 0xff));
    assert_eq!(a.danger, rgba8(0xff, 0, 0, 0x80));
    assert_eq!(a.selection_dim, rgba8(0, 0, 0, 0x99));
    assert_eq!(a.text, AnnotationColors::DEFAULT.text);

    let cfg = parse_single_for_tests("annotations {\n  accent \"gold\"\n  accnet \"#112233\"\n  text 7\n}\n");
    assert_eq!(cfg.errors.len(), 3, "{:?}", cfg.errors);
    assert!(cfg.errors[1].to_string().contains("accnet"), "{:?}", cfg.errors);
    assert_eq!(cfg.annotations, AnnotationColors::DEFAULT);
}

#[test]
fn annotation_panel_tint_alpha_is_raised_to_the_floor() {
    let cfg = parse_single_for_tests("annotations { panel-tint \"#ffffff10\"; }\n");
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    let t = cfg.annotations.panel_tint;
    assert_eq!(t[..3], [1.0, 1.0, 1.0]);
    assert_eq!(t[3], PANEL_TINT_MIN_ALPHA);
    let cfg = parse_single_for_tests("annotations { panel-tint \"#000000e0\"; }\n");
    assert_eq!(cfg.annotations.panel_tint[3], 0xe0 as f32 / 255.0);
    const { assert!(AnnotationColors::DEFAULT.panel_tint[3] >= PANEL_TINT_MIN_ALPHA) };
}

#[test]
fn oracle_eyes_parses_and_bad_values_keep_defaults() {
    let cfg = parse_single_for_tests(
        "oracle-eyes {\n  model-command \"claude\" \"--model\" \"haiku\"\n  timeout-ms 45000\n  \
             auto-interval-ms 5000\n  hold-ms 6000\n  debug #true\n}\n",
    );
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(
        cfg.oracle_eyes,
        OracleEyes {
            model_command: Some(vec!["claude".into(), "--model".into(), "haiku".into()]),
            timeout_ms: 45_000,
            auto_interval_ms: 5000,
            hold_ms: 6000,
            debug: true,
            bind: OracleEyesBinds::default(),
        }
    );

    let cfg = parse_single_for_tests(
        "oracle-eyes {\n  model-command\n  timeout-ms 10\n  auto-interval-ms \"3s\"\n  hold-ms 999999\n  \
             debug \"yes\"\n  colour \"red\"\n}\n",
    );
    assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
    assert_eq!(cfg.oracle_eyes, OracleEyes::default());

    for bad in [
        "oracle-eyes { model-command \"\"; }",
        "oracle-eyes { model-command \"claude\" 3; }",
        "oracle-eyes { model-command \"claude\" flag=\"x\"; }",
    ] {
        let cfg = parse_single_for_tests(bad);
        assert_eq!(cfg.errors.len(), 1, "{bad}: {:?}", cfg.errors);
        assert_eq!(cfg.oracle_eyes, OracleEyes::default(), "{bad}");
    }
}

/// Settings writes these through `edit` and reads them back through
/// `schema::get`: both ends name the same thing, and the file stays one
/// block per section.
#[test]
fn oracle_eyes_binds_become_default_binds() {
    let c = Config::default();
    assert!(
        oracle_eyes_binds(&c.oracle_eyes).is_empty(),
        "nothing bound by default"
    );
    let c = parse_single_for_tests(
        "oracle-eyes { bind { select \"Super+Shift+A\"; dismiss \"Super+Shift+D\"; \
             expand \"none\"; auto-toggle \"Super+Shift+T\"; } }",
    );
    assert!(c.errors.is_empty(), "{:?}", c.errors);
    let binds = oracle_eyes_binds(&c.oracle_eyes);
    assert_eq!(binds.len(), 3);
    assert_eq!(binds[0].action, Action::AnnotationSelect);
    assert_eq!(binds[0].key, Keysym::a);
    assert_eq!(binds[2].action, Action::AnnotationAutoToggle);
    assert_eq!(c.oracle_eyes.bind.expand.text, "none");

    let c =
        parse_single_for_tests("oracle-eyes { bind { select \"Super+Escape\"; frob \"x\"; dismiss 3; } }");
    assert_eq!(c.errors.len(), 3, "{:?}", c.errors);
    assert_eq!(c.oracle_eyes.bind, OracleEyesBinds::default());
}

#[test]
fn annotation_and_oracle_eyes_edits_round_trip() {
    let text = edit::set_value("", "annotations.leader", &KdlValue::String("#12345678".into())).unwrap();
    let text = edit::set_value(
        &text,
        "annotations.panel-tint",
        &KdlValue::String("#00000020".into()),
    )
    .unwrap();
    let text = edit::set_value(&text, "oracle-eyes.hold-ms", &KdlValue::Integer(7000)).unwrap();
    let text = edit::set_value(&text, "oracle-eyes.debug", &KdlValue::Bool(true)).unwrap();
    let argv = [KdlValue::String("ollama".into()), KdlValue::String("run".into())];
    let text = edit::set_list(&text, "oracle-eyes.model-command", &argv).unwrap();
    let text = edit::set_value(&text, "annotations.accent", &KdlValue::String("#abcdef".into())).unwrap();
    assert_eq!(text.matches("annotations").count(), 1, "{text}");
    assert_eq!(text.matches("oracle-eyes").count(), 1, "{text}");

    let cfg = parse_single_for_tests(&text);
    assert!(cfg.errors.is_empty(), "{text}\n{:?}", cfg.errors);
    use schema::Value as V;
    assert_eq!(
        schema::get(&cfg, "annotations.leader"),
        Some(V::Color(rgba8(0x12, 0x34, 0x56, 0x78)))
    );
    assert_eq!(
        schema::get(&cfg, "annotations.accent"),
        Some(V::Color(rgba8(0xab, 0xcd, 0xef, 0xff)))
    );
    assert_eq!(
        schema::get(&cfg, "annotations.panel-tint"),
        Some(V::Color([0.0, 0.0, 0.0, PANEL_TINT_MIN_ALPHA]))
    );
    assert_eq!(schema::get(&cfg, "oracle-eyes.hold-ms"), Some(V::Int(7000)));
    assert_eq!(schema::get(&cfg, "oracle-eyes.debug"), Some(V::Bool(true)));
    assert_eq!(
        schema::get(&cfg, "oracle-eyes.model-command"),
        Some(V::List(vec!["ollama".into(), "run".into()]))
    );

    // A withheld command reads as unset, never as its argv.
    let mut cfg = cfg;
    cfg.oracle_eyes.model_command = None;
    assert_eq!(schema::get(&cfg, "oracle-eyes.model-command"), Some(V::Null));
}

#[test]
fn windowrule_blur_takes_a_mode_or_a_bool() {
    for (value, want) in [
        ("true", BlurRule::On),
        ("false", BlurRule::Mode(BlurMode::Off)),
        ("off", BlurRule::Mode(BlurMode::Off)),
        ("blur", BlurRule::Mode(BlurMode::Blur)),
        ("frost", BlurRule::Mode(BlurMode::Frost)),
        ("glass", BlurRule::Mode(BlurMode::Glass)),
    ] {
        let cfg = blur_cfg(&format!("windowrule \"blur {value}\" {{ app-id \"a\"; }}\n"));
        assert!(cfg.errors.is_empty(), "{value}: {:?}", cfg.errors);
        assert!(
            matches!(cfg.window_rules[0].action, RuleAction::Blur(got) if got == want),
            "{value}: {:?}",
            cfg.window_rules[0].action
        );
    }
    let cfg = blur_cfg("windowrule \"blur maybe\" { app-id \"a\"; }\n");
    assert!(cfg.window_rules.is_empty());
    // `true` follows the global mode, falling back to plain blur when that is off.
    assert_eq!(BlurRule::On.resolve(BlurMode::Glass), BlurMode::Glass);
    assert_eq!(BlurRule::On.resolve(BlurMode::Off), BlurMode::Blur);
    assert_eq!(
        BlurRule::Mode(BlurMode::Off).resolve(BlurMode::Glass),
        BlurMode::Off
    );
    assert_eq!(
        BlurRule::Mode(BlurMode::Frost).resolve(BlurMode::Off),
        BlurMode::Frost
    );
}

#[test]
fn rejects_bad_decoration_and_animation_values() {
    let doc: KdlDocument = r#"
            decoration {
                rounding 999
                active-opacity 4.0
                inactive-opacity "half"
                nonsense 1
            }
            animations {
                enabled #true
                animation "windows" curve="bounce"
                animation "nope" duration="10ms"
                animation "fade" duration="2h"
            }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    // Every bad value keeps its default rather than half-applying.
    assert_eq!(cfg.decoration.rounding, 9);
    assert_eq!(cfg.decoration.active_opacity, 1.0);
    assert_eq!(cfg.decoration.inactive_opacity, 1.0);
    assert!(cfg.animations.overrides.is_empty());
}

#[test]
fn parses_geometry_and_classification_rules() {
    let doc: KdlDocument = r#"
            windowrule "size 800x600"        { app-id "mpv" }
            windowrule "position 40,-20"     { app-id "mpv" }
            windowrule "output DP-*"         { app-id "mpv" }
            windowrule "app-trust trusted"   { cgroup "app-remnant" }
            windowrule "seat-compat multi"   { app-id "mpv" }
            windowrule "idle-inhibit"        { app-id "mpv" }
            windowrule "size 800"            { app-id "mpv" }
            windowrule "position left"       { app-id "mpv" }
            windowrule "app-trust root"      { app-id "mpv" }
            windowrule "seat-compat none"    { app-id "mpv" }
            windowrule "cgroup-typo"         { cgroup "(" }
            windowrule "irreversible-capable true"  { app-id "foot" }
            windowrule "irreversible-capable false" { app-id "mpv" }
            windowrule "irreversible-capable maybe" { app-id "mpv" }
            windowrule "irreversible-capable"       { app-id "mpv" }
            windowrule "irreversable-capable true"  { app-id "mpv" }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    // Every malformed rule above is a config error, not a warning that
    // lets it through: the five bad shapes plus the three bad
    // `irreversible-capable` forms (milestone 9e).
    assert_eq!(cfg.errors.len(), 8, "{:?}", cfg.errors);
    assert!(cfg
        .errors
        .iter()
        .any(|e| e.message.contains("irreversable-capable")));
    let actions: Vec<&RuleAction> = cfg.window_rules.iter().map(|r| &r.action).collect();
    assert_eq!(
        actions,
        vec![
            &RuleAction::Size(800, 600),
            &RuleAction::Position(40, -20),
            &RuleAction::Output("DP-*".to_owned()),
            &RuleAction::Trust(AppTrust::Trusted),
            &RuleAction::Seat(SeatCompat::Multi),
            &RuleAction::IdleInhibit,
            &RuleAction::IrreversibleCapable(true),
            &RuleAction::IrreversibleCapable(false),
        ]
    );
    assert!(cfg.window_rules[3].matchers.cgroup.is_some());
}

#[test]
fn pairs() {
    assert_eq!(parse_pair("800x600", 'x'), Some((800, 600)));
    assert_eq!(parse_pair(" 40 , -20 ", ','), Some((40, -20)));
    assert_eq!(parse_pair("800x600x1", 'x'), None);
    assert_eq!(parse_pair("800", 'x'), None);
}

#[test]
fn durations() {
    assert_eq!(parse_duration_ms(&KdlValue::Integer(150)), Some(150));
    assert_eq!(parse_duration_ms(&KdlValue::String("150ms".into())), Some(150));
    assert_eq!(parse_duration_ms(&KdlValue::String("2s".into())), Some(2000));
    assert_eq!(parse_duration_ms(&KdlValue::String("1m".into())), Some(60_000));
    assert_eq!(parse_duration_ms(&KdlValue::String("2h".into())), None);
    assert_eq!(parse_duration_ms(&KdlValue::Integer(-1)), None);
}

#[test]
fn parses_windowrules() {
    let doc: KdlDocument = r#"
            windowrule "float" { app-id "pavucontrol|org.gnome.Calculator"; }
            windowrule "workspace 3" { title "^Meet"; output "DP-*"; }
            windowrule "opacity 0.85" { app-id "kitty"; xwayland #false; }
            windowrule "sensitivity secret" { app-id "keepassxc"; }
            windowrule "no-agent" { pid 42; }
            windowrule "fullscreen" { app-id "mpv"; }
        "#
    .parse()
    .unwrap();
    let mut c = Config::default();
    c.apply(&doc, &mut Vec::new());
    assert_eq!(c.window_rules.len(), 6);
    assert_eq!(c.window_rules[1].action, RuleAction::Workspace(3));
    assert_eq!(c.window_rules[2].action, RuleAction::Opacity(0.85));
    assert_eq!(c.window_rules[4].action, RuleAction::NoAgent);
    assert_eq!(c.window_rules[5].action, RuleAction::Fullscreen);
    let m = &c.window_rules[0].matchers;
    let app = m.app_id.as_ref().expect("app-id");
    assert!(app.matches("pavucontrol"));
    assert!(app.matches("org.gnome.Calculator"));
    assert!(!app.matches("firefox"));
    assert_eq!(c.window_rules[2].matchers.xwayland, Some(false));
    assert_eq!(c.window_rules[4].matchers.pid, Some(42));
}

#[test]
fn rejects_unhonourable_windowrules() {
    let doc: KdlDocument = r#"
            windowrule "float" { }
            windowrule "float"
            windowrule "size 800 600" { app-id "a"; }
            windowrule "sensitivity public" { app-id "a"; }
            windowrule "workspace 99" { app-id "a"; }
            windowrule "opacity 2.0" { app-id "a"; }
            windowrule "float" { launching-principal "x"; }
            windowrule "float" { title "^(a|b"; }
            windowrule "float" { pid 0; }
        "#
    .parse()
    .unwrap();
    let mut c = Config::default();
    c.apply(&doc, &mut Vec::new());
    assert!(c.window_rules.is_empty(), "{:?}", c.window_rules);
}

#[test]
fn patterns_are_regexes() {
    let p = Pattern::parse("Meet").expect("literal");
    assert!(p.matches("Google Meet — call"));
    let p = Pattern::parse("^Meet").expect("anchored");
    assert!(p.matches("Meet — call"));
    assert!(!p.matches("Google Meet"));
    let p = Pattern::parse("^kitty$").expect("exact");
    assert!(p.matches("kitty"));
    assert!(!p.matches("kitty-dev"));
    let p = Pattern::parse("^org\\.gnome\\.").expect("escaped");
    assert!(p.matches("org.gnome.Calculator"));
    assert!(!p.matches("org-gnome-Calculator"));
    // The full syntax, not the old glob subset.
    assert!(Pattern::parse("a[bc]d").expect("class").matches("abd"));
    assert!(Pattern::parse("^(chromium|firefox)$")
        .expect("group")
        .matches("firefox"));
    assert!(Pattern::parse("x+y").expect("repeat").matches("xxy"));
    // A malformed regex is refused, never approximated.
    assert!(Pattern::parse("a[bc").is_none());
    assert!(Pattern::parse("").is_none());
}

#[test]
fn parses_render_device() {
    let doc: KdlDocument = r#"
            misc { render-device "pci:0000:01:00.0" }
        "#
    .parse()
    .unwrap();
    let mut c = Config::default();
    c.apply(&doc, &mut Vec::new());
    assert_eq!(c.misc.render_device.as_deref(), Some("pci:0000:01:00.0"));

    // "auto" is the explicit spelling of the default.
    let doc: KdlDocument = r#"misc { render-device "auto" }"#.parse().unwrap();
    let mut c = Config::default();
    c.misc.render_device = Some("/dev/dri/card9".into());
    c.apply(&doc, &mut Vec::new());
    assert_eq!(c.misc.render_device, None);
}

#[test]
fn colors() {
    assert_eq!(parse_color("#ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(parse_color("0x80ff0000"), Some([1.0, 0.0, 0.0, 0.5019608]));
    assert_eq!(parse_color("nope"), None);
}

#[test]
fn parses_idle_and_lid() {
    let doc: KdlDocument = r#"
            idle { dpms-timeout-seconds 300; lock-timeout-seconds 600; lock-command "hyprlock" }
            output "eDP-1" { lid-close "ignore" }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    let mut binds = Vec::new();
    cfg.apply(&doc, &mut binds);
    assert_eq!(cfg.idle.dpms_timeout, Some(300));
    assert_eq!(cfg.idle.lock_timeout, Some(600));
    assert_eq!(cfg.idle.lock_command.as_deref(), Some("hyprlock"));
    assert_eq!(
        cfg.output_rule("eDP-1", "eDP-1").lid_close.as_deref(),
        Some("ignore")
    );
}

#[test]
fn parses_general_and_binds() {
    let doc: KdlDocument = r#"
            general { gaps-in 3; layout "master"; border-size 4 }
            bind "SUPER SHIFT" "Return" { spawn "foot"; }
            workspace 2 { layout "dwindle" }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    let mut binds = Vec::new();
    cfg.apply(&doc, &mut binds);
    assert_eq!(cfg.general.gaps_in, 3);
    assert_eq!(cfg.general.border_size, 4);
    assert_eq!(cfg.general.layout, LayoutKind::Master);
    assert_eq!(cfg.layout_for(2), LayoutKind::Dwindle);
    assert_eq!(binds.len(), 1);
    assert!(matches!(binds[0].action, Action::Spawn(ref c) if c == "foot"));
    assert!(binds[0].mods.shift && binds[0].mods.logo);
}

/// The vertical gaps ship set (C-16), independent of the horizontal
/// ones. Unset, `gaps-in-vertical`/`gaps-out-vertical` still mirror their
/// horizontal counterpart, including a horizontal value already
/// customised away from the default — the resolver, not a hardcoded
/// literal, is what makes that true.
#[test]
fn vertical_gaps_mirror_horizontal_until_set() {
    let d = General::default();
    assert_eq!((d.gaps_in_y(), d.gaps_out_y()), (3, 7));

    let doc: KdlDocument = "general { gaps-in 3; gaps-out 12 }\n".parse().unwrap();
    let mut cfg = Config::default();
    cfg.general.gaps_in_vertical = None;
    cfg.general.gaps_out_vertical = None;
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.general.gaps_in_vertical, None);
    assert_eq!(cfg.general.gaps_out_vertical, None);
    assert_eq!(
        cfg.general.gaps_in_y(),
        3,
        "mirrors the customised horizontal value"
    );
    assert_eq!(
        cfg.general.gaps_out_y(),
        12,
        "mirrors the customised horizontal value"
    );

    let doc: KdlDocument = "general { gaps-in 3; gaps-in-vertical 8; gaps-out 12; gaps-out-vertical 1 }\n"
        .parse()
        .unwrap();
    cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.general.gaps_in_vertical, Some(8));
    assert_eq!(cfg.general.gaps_in_y(), 8, "an explicit vertical value wins");
    assert_eq!(cfg.general.gaps_out_vertical, Some(1));
    assert_eq!(cfg.general.gaps_out_y(), 1, "an explicit vertical value wins");
}

/// Both follow behaviours are on out of the box and both can be turned
/// off: a move you cannot see reads as a move that did not happen, but
/// someone who wants the cursor to stay put must be able to say so.
#[test]
fn the_follow_behaviours_default_on_and_parse_off() {
    let d = General::default();
    assert!(d.cursor_follows_moved_window);
    assert!(d.follow_window_to_workspace);

    let doc: KdlDocument = r#"
            general { cursor-follows-moved-window #false; follow-window-to-workspace #false }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert!(!cfg.general.cursor_follows_moved_window);
    assert!(!cfg.general.follow_window_to_workspace);
}

#[test]
fn capture_defaults_to_denying_everything() {
    let cfg = Config::default();
    assert!(cfg.capture.allow.is_empty());
    assert!(cfg.capture.redact_app_id.is_empty());
    assert!(cfg.capture.hide_layer.is_empty());
}

#[test]
fn parses_capture_hide_layer() {
    let doc: KdlDocument = r#"
            capture { hide-layer "hyperion:eclipse-eye" "foo:ns:with:colons" }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(
        cfg.capture.hide_layer,
        [
            ("hyperion".to_string(), "eclipse-eye".to_string()),
            ("foo".to_string(), "ns:with:colons".to_string()),
        ]
    );
}

#[test]
fn malformed_hide_layer_entries_are_rejected_and_dropped() {
    let doc: KdlDocument = r#"
            capture { hide-layer "hyperion:eclipse-eye" "nocolon" ":ns" "exe:" 7 }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.errors.len(), 4, "{:?}", cfg.errors);
    assert_eq!(
        cfg.capture.hide_layer,
        [("hyperion".to_string(), "eclipse-eye".to_string())]
    );
}

#[test]
fn shipped_policy_hides_the_taskbar_eye() {
    let text = include_str!("../../../../packaging/etc/policy.kdl");
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((policy_src("/etc/eclipse/policy.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(
        cfg.capture.hide_layer,
        [("ec-hyperion-bar".to_string(), "eclipse-eye".to_string())]
    );
}

#[test]
fn repeated_hide_layer_is_rejected() {
    let doc: KdlDocument = r#"
            capture { hide-layer "a:b"; hide-layer "c:d" }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    cfg.apply(&doc, &mut Vec::new());
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
}

#[test]
fn parses_capture() {
    let doc: KdlDocument = r#"
            capture { allow "grim" "xdg-desktop-portal-wlr"; redact-app-id "bitwarden" }
        "#
    .parse()
    .unwrap();
    let mut cfg = Config::default();
    let mut binds = Vec::new();
    cfg.apply(&doc, &mut binds);
    assert_eq!(cfg.capture.allow, ["grim", "xdg-desktop-portal-wlr"]);
    assert_eq!(cfg.capture.redact_app_id, ["bitwarden"]);
}

#[test]
fn bad_nodes_are_skipped_not_fatal() {
    let doc: KdlDocument = "general { layout \"bogus\" }\nbind \"SUPER\" \"Escape\" { quit; }\n"
        .parse()
        .unwrap();
    let mut cfg = Config::default();
    let mut binds = Vec::new();
    cfg.apply(&doc, &mut binds);
    assert_eq!(cfg.general.layout, LayoutKind::Radiant);
    assert!(binds.is_empty(), "Super+Escape must stay reserved");
}

#[test]
fn both_agent_chords_stay_reserved() {
    let doc: KdlDocument =
            "bind \"SUPER\" \"Escape\" { quit; }\nbind \"SUPER\" \"space\" { quit; }\nbind \"SUPER\" \"F1\" { quit; }\n"
                .parse()
                .unwrap();
    let mut cfg = Config::default();
    let mut binds = Vec::new();
    cfg.apply(&doc, &mut binds);
    assert_eq!(binds.len(), 1, "only the unreserved bind survives");
    assert_eq!(binds[0].key, Keysym::F1);
}

#[test]
fn agent_attention_action_parses() {
    let doc: KdlDocument = "bind \"CTRL\" \"space\" { agent-attention; }".parse().unwrap();
    let mut cfg = Config::default();
    let mut binds = Vec::new();
    cfg.apply(&doc, &mut binds);
    assert!(matches!(binds[0].action, crate::input::Action::AgentAttention));
}

#[test]
fn annotation_chords_parse_and_are_not_bound_by_default() {
    use crate::input::Action;
    for (name, want) in [
        ("annotation-select", Action::AnnotationSelect),
        ("annotation-dismiss", Action::AnnotationDismiss),
        ("annotation-expand", Action::AnnotationExpand),
        ("annotation-auto-toggle", Action::AnnotationAutoToggle),
    ] {
        let doc: KdlDocument = format!("bind \"SUPER CTRL\" \"o\" {{ {name}; }}")
            .parse()
            .unwrap();
        let mut cfg = Config::default();
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        assert_eq!(binds[0].action, want, "{name}");
    }
    // The addon is optional, so its chords cost the default config nothing.
    assert!(!default_binds().iter().any(|b| matches!(
        b.action,
        Action::AnnotationSelect
            | Action::AnnotationDismiss
            | Action::AnnotationExpand
            | Action::AnnotationAutoToggle
    )));
}

fn gestures(text: &str) -> Config {
    let doc: KdlDocument = text.parse().unwrap();
    let mut cfg = Config {
        cur: Some((abyss_src("a.kdl"), text.to_owned())),
        ..Config::default()
    };
    cfg.apply(&doc, &mut Vec::new());
    cfg
}

#[test]
fn gesture_node_parses() {
    use crate::input::Action;
    let cfg = gestures("gesture \"swipe\" 4 \"up\" { spawn \"foot\"; }\n");
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(
        cfg.gesture_for(4, Direction::Up),
        Some(&Action::Spawn("foot".into()))
    );
    assert!(cfg.gesture_bound(4));
    // Additive: the defaults are still there.
    assert_eq!(cfg.gesture_binds.len(), default_gesture_binds().len() + 1);
    assert_eq!(cfg.gesture_for(3, Direction::Left), Some(&Action::WorkspaceNext));
    assert_eq!(cfg.gesture_for(3, Direction::Right), Some(&Action::WorkspacePrev));
    assert!(!cfg.gesture_bound(5));
}

#[test]
fn gesture_rejects_bad_fingers_kind_and_direction() {
    for bad in [
        "gesture \"swipe\" 2 \"left\" { workspace-next; }",
        "gesture \"swipe\" 5 \"left\" { workspace-next; }",
        "gesture \"swipe\" \"3\" \"left\" { workspace-next; }",
        "gesture \"swipe\" 3 \"sideways\" { workspace-next; }",
        "gesture \"pinch\" 3 \"left\" { workspace-next; }",
        "gesture \"swipe\" 3 { workspace-next; }",
        "gesture \"swipe\" 3 \"left\"",
        "gesture \"swipe\" 3 \"left\" { no-such-action; }",
    ] {
        let cfg = gestures(bad);
        assert_eq!(cfg.errors.len(), 1, "{bad}: {:?}", cfg.errors);
        assert_eq!(cfg.gesture_binds, default_gesture_binds(), "{bad}");
    }
}

#[test]
fn config_gesture_overrides_the_default() {
    use crate::input::Action;
    let cfg = gestures(
        "gesture \"swipe\" 3 \"left\" { workspace-prev; }\ngesture \"swipe\" 3 \"left\" { toggle-layout; }\n",
    );
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(
        cfg.gesture_binds.len(),
        default_gesture_binds().len(),
        "override, not addition"
    );
    // The later entry wins, the same as a later file over an earlier one.
    assert_eq!(cfg.gesture_for(3, Direction::Left), Some(&Action::ToggleLayout));
    assert_eq!(cfg.gesture_for(3, Direction::Right), Some(&Action::WorkspacePrev));
}

#[test]
fn drag_gesture_defaults_to_super_two_fingers() {
    let cfg = Config::default();
    assert_eq!(cfg.drag_gestures, default_drag_gestures());
    let sup = ModifiersState {
        logo: true,
        ..Default::default()
    };
    assert!(cfg.drag_gesture_bound(2, &sup));
    assert!(!cfg.drag_gesture_bound(2, &ModifiersState::default()));
    assert!(!cfg.drag_gesture_bound(3, &sup));
    // A drag is not a swipe: two fingers stay unbound for swipes.
    assert!(!cfg.gesture_bound(2));
}

#[test]
fn drag_gesture_replaces_the_default_for_the_same_fingers() {
    let cfg = gestures(
        "gesture \"drag\" 2 \"Alt\" { move-window; }\ngesture \"drag\" 4 \"Super Shift\" { move-window; }\n",
    );
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.drag_gestures.len(), 2, "2 replaced, 4 added");
    let alt = ModifiersState {
        alt: true,
        ..Default::default()
    };
    let sup = ModifiersState {
        logo: true,
        ..Default::default()
    };
    assert!(cfg.drag_gesture_bound(2, &alt));
    assert!(!cfg.drag_gesture_bound(2, &sup));
    assert!(cfg.drag_gesture_bound(4, &ModifiersState { shift: true, ..sup }));
}

#[test]
fn drag_gesture_none_disables_it() {
    for off in [
        "gesture \"drag\" 2 { none; }",
        "gesture \"drag\" 2 \"Super\" { none; }",
    ] {
        let cfg = gestures(off);
        assert!(cfg.errors.is_empty(), "{off}: {:?}", cfg.errors);
        assert!(cfg.drag_gestures.is_empty(), "{off}");
    }
    // Disabling a finger count that has no drag is harmless, even one a
    // swipe is bound to.
    let cfg = gestures("gesture \"drag\" 3 { none; }");
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    assert_eq!(cfg.drag_gestures, default_drag_gestures());
}

#[test]
fn drag_gesture_may_not_share_fingers_with_a_bound_swipe() {
    // The default 3-finger swipes are bound, so a 3-finger drag is refused.
    let cfg = gestures("gesture \"drag\" 3 \"Super\" { move-window; }");
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    assert!(cfg.errors[0]
        .message
        .contains("collides with a bound 3-finger swipe"));
    assert_eq!(cfg.drag_gestures, default_drag_gestures());
    // And the other way round: a swipe on a dragged finger count.
    let cfg = gestures(
        "gesture \"drag\" 4 \"Super\" { move-window; }\ngesture \"swipe\" 4 \"up\" { toggle-layout; }",
    );
    assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
    assert!(cfg.errors[0]
        .message
        .contains("collides with the 4-finger drag gesture"));
    assert!(!cfg.gesture_bound(4));
}

#[test]
fn drag_gesture_rejects_bad_nodes_and_keeps_the_default() {
    for bad in [
        "gesture \"drag\" 2 { move-window; }",
        "gesture \"drag\" 2 \"\" { move-window; }",
        "gesture \"drag\" 2 \"none\" { move-window; }",
        "gesture \"drag\" 1 \"Super\" { move-window; }",
        "gesture \"drag\" 5 \"Super\" { move-window; }",
        "gesture \"drag\" 2 \"Hyper\" { move-window; }",
        "gesture \"drag\" 2 \"Super\" { resize-window; }",
        "gesture \"drag\" 2 \"Super\"",
        "gesture \"drag\" 2 \"Super\" \"left\" { move-window; }",
    ] {
        let cfg = gestures(bad);
        assert_eq!(cfg.errors.len(), 1, "{bad}: {:?}", cfg.errors);
        assert_eq!(cfg.drag_gestures, default_drag_gestures(), "{bad}");
    }
    let cfg = gestures("gesture \"drag\" 2 { move-window; }");
    assert!(cfg.errors[0].message.contains("at least one modifier"));
}

#[test]
fn mousebind_defaults_are_alt_drag() {
    use crate::input::MouseAction;
    let cfg = Config::default();
    let alt = Mods {
        alt: true,
        ..Mods::default()
    };
    let mut state = ModifiersState {
        alt: true,
        ..Default::default()
    };
    assert_eq!(cfg.mouse_bind_for(&state, 0x110), Some(MouseAction::MoveWindow));
    assert_eq!(cfg.mouse_bind_for(&state, 0x111), Some(MouseAction::ResizeWindow));
    assert_eq!(cfg.mouse_bind_for(&state, 0x112), None);
    assert_eq!(cfg.mouse_binds, default_mouse_binds());
    assert!(cfg.mouse_binds.iter().all(|b| b.mods == alt));
    // Exact match: Alt+Shift, and no modifier at all, are not Alt.
    state.shift = true;
    assert_eq!(cfg.mouse_bind_for(&state, 0x110), None);
    assert_eq!(cfg.mouse_bind_for(&ModifiersState::default(), 0x110), None);
}

#[test]
fn mousebind_node_parses_and_extends_the_defaults() {
    use crate::input::{MouseAction, MouseButton};
    let cfg = gestures("mousebind \"SUPER SHIFT\" \"middle\" { resize-window; }\n");
    assert_eq!(cfg.mouse_binds.len(), default_mouse_binds().len() + 1);
    assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    let added = cfg.mouse_binds.last().unwrap();
    assert_eq!(added.button, MouseButton::Middle);
    assert_eq!(added.action, MouseAction::ResizeWindow);
    assert_eq!(
        added.mods,
        Mods {
            logo: true,
            shift: true,
            ..Mods::default()
        }
    );
}

#[test]
fn mousebind_replaces_the_default_for_the_same_mods_and_button() {
    use crate::input::MouseAction;
    let cfg = gestures(
        "mousebind \"ALT\" \"left\" { resize-window; }\nmousebind \"MOD1\" \"left\" { move-window; }\n",
    );
    // Replaced once by the first node, again by the second: still one entry.
    assert_eq!(cfg.mouse_binds.len(), default_mouse_binds().len());
    let state = ModifiersState {
        alt: true,
        ..Default::default()
    };
    assert_eq!(cfg.mouse_bind_for(&state, 0x110), Some(MouseAction::MoveWindow));
    assert_eq!(cfg.mouse_bind_for(&state, 0x111), Some(MouseAction::ResizeWindow));
    let cfg = gestures("mousebind \"ALT\" \"left\" { resize-window; }\n");
    assert_eq!(cfg.mouse_bind_for(&state, 0x110), Some(MouseAction::ResizeWindow));
}

#[test]
fn mousebind_rejects_bad_nodes_and_keeps_the_defaults() {
    for bad in [
        "mousebind \"Alt\" \"side\" { move-window; }",
        "mousebind \"Alt\" \"left\" { spawn \"foot\"; }",
        "mousebind \"Alt\" \"left\" { }",
        "mousebind \"Alt\" \"left\"",
        "mousebind \"Alt\" { move-window; }",
        "mousebind \"Hyper\" \"left\" { move-window; }",
        "mousebind \"left\" { move-window; }",
        "mousebind \"\" \"left\" { move-window; }",
        "mousebind \"none\" \"left\" { move-window; }",
    ] {
        let cfg = gestures(bad);
        assert!(!cfg.errors.is_empty(), "{bad}");
        assert_eq!(cfg.mouse_binds, default_mouse_binds(), "{bad}");
    }
}

#[test]
fn workspace_step_actions_parse() {
    use crate::input::Action;
    let doc: KdlDocument =
        "bind \"SUPER\" \"n\" { workspace-next; }\nbind \"SUPER\" \"p\" { workspace-prev; }"
            .parse()
            .unwrap();
    let mut binds = Vec::new();
    Config::default().apply(&doc, &mut binds);
    assert_eq!(binds[0].action, Action::WorkspaceNext);
    assert_eq!(binds[1].action, Action::WorkspacePrev);
}

#[test]
fn swipe_direction_takes_the_dominant_axis_past_the_threshold() {
    use crate::input::{swipe_direction, SWIPE_THRESHOLD};
    let t = SWIPE_THRESHOLD;
    assert_eq!(swipe_direction(-t, 0.0, t), Some(Direction::Left));
    assert_eq!(swipe_direction(t * 2.0, t, t), Some(Direction::Right));
    assert_eq!(swipe_direction(10.0, -t * 1.5, t), Some(Direction::Up));
    assert_eq!(swipe_direction(-t, t * 1.1, t), Some(Direction::Down));
    // Short of the threshold on both axes: nothing, however diagonal.
    assert_eq!(swipe_direction(t - 1.0, -(t - 1.0), t), None);
    assert_eq!(swipe_direction(0.0, 0.0, t), None);
}

mod output_tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("DP-1", "DP-1"));
        assert!(!glob_match("DP-1", "DP-11"));
        assert!(glob_match("eDP-*", "eDP-1"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*Dell*", "Dell U2720Q ABC"));
        assert!(!glob_match("*Dell*", "LG U2720Q"));
    }

    #[test]
    fn later_blocks_win() {
        let mut c = Config::default();
        let doc: KdlDocument = "output \"*\" { scale 1.0 }\noutput \"DP-*\" { scale 2.0; position 100 0 }"
            .parse()
            .unwrap();
        let mut binds = Vec::new();
        c.apply(&doc, &mut binds);
        let r = c.output_rule("DP-1", "Dell X Y");
        assert_eq!(r.scale, Some(2.0));
        assert_eq!(r.position, Some((100, 0)));
        assert_eq!(c.output_rule("HDMI-A-1", "x").scale, Some(1.0));
    }
}

mod allowlist_tests {
    use super::Allowlist;

    #[test]
    fn empty_denies_everything() {
        let a = Allowlist::default();
        assert!(a.is_empty());
        assert!(!a.contains("grim"));
    }

    #[test]
    fn set_is_visible_to_an_existing_handle() {
        // The point of the type: the copy held by a global's bind filter sees
        // what the config reload path wrote, without a restart.
        let held = Allowlist::new(vec!["grim".to_string()]);
        let reload = held.clone();
        assert!(held.contains("grim"));

        reload.set(vec!["wf-recorder".to_string()]);
        assert!(!held.contains("grim"));
        assert!(held.contains("wf-recorder"));

        // Reloading to nothing revokes rather than leaving the old list live.
        reload.set(Vec::new());
        assert!(held.is_empty());
        assert!(!held.contains("wf-recorder"));
    }
}

mod rounding_tests {
    use super::*;

    #[test]
    fn rounding_parses_and_enables_the_effect_path() {
        let doc: KdlDocument = "decoration {\n    rounding 20\n}\n".parse().expect("kdl parses");
        let mut cfg = Config::default();
        cfg.apply(&doc, &mut Vec::new());
        assert_eq!(cfg.decoration.rounding, 20);
        assert!(cfg.decoration.any_window_effect());
    }
}

/// ADR 0064: an invalid `abyss.kdl` at startup drops the bad nodes and starts;
/// a short fail-closed list still refuses.
mod startup_tests {
    use super::*;

    fn parse(owner: schema::Owner, text: &str) -> Config {
        let doc: KdlDocument = text.parse().expect("kdl parses");
        let mut cfg = Config {
            cur: Some((
                Source {
                    path: PathBuf::from(match owner {
                        schema::Owner::Abyss => "abyss.kdl",
                        schema::Owner::Policy => "policy.kdl",
                    }),
                    owner,
                },
                text.to_owned(),
            )),
            ..Config::default()
        };
        let mut binds = Vec::new();
        cfg.apply(&doc, &mut binds);
        cfg.cur = None;
        cfg.binds = merge_binds(default_binds(), binds);
        cfg
    }

    fn abyss(text: &str) -> Config {
        parse(schema::Owner::Abyss, text)
    }

    #[test]
    fn an_unknown_key_is_dropped_and_its_neighbours_apply() {
        let mut cfg = abyss("general {\n    gaps-in 7\n    gaps-sideways 3\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(cfg.errors[0].message.contains("gaps-sideways"));
        assert_eq!(cfg.general.gaps_in, 7, "the valid key applies");
        let d = General::default();
        assert_eq!(cfg.general.gaps_out, d.gaps_out, "untouched keys keep defaults");
        assert_eq!(
            cfg.general.gaps_in_vertical, d.gaps_in_vertical,
            "the untouched vertical key keeps its default"
        );
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
    }

    #[test]
    fn parses_the_new_pointer_keys() {
        let cfg = abyss(
            "input {\n    accel-speed -0.5\n    scroll-method \"on-button-down\"\n    touchpad {\n        \
             tap-and-drag #false\n        scroll-method \"edge\"\n    }\n}\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.input.accel_speed, -0.5);
        assert_eq!(cfg.input.scroll_method, "on-button-down");
        assert!(!cfg.input.touchpad.tap_and_drag);
        assert_eq!(cfg.input.touchpad.scroll_method, "edge");
        // An integer is a number too.
        assert_eq!(abyss("input { accel-speed 1; }").input.accel_speed, 1.0);
    }

    #[test]
    fn bad_pointer_values_are_refused_and_keep_the_default() {
        for (kdl, needle) in [
            ("input { accel-speed 1.5; }", "accel-speed"),
            ("input { accel-speed \"fast\"; }", "accel-speed"),
            ("input { scroll-method \"two-finger\"; }", "scroll-method"),
            (
                "input { touchpad { scroll-method \"on-button-down\"; }; }",
                "scroll-method",
            ),
            ("input { touchpad { tap-and-drag \"yes\"; }; }", "tap-and-drag"),
        ] {
            let cfg = abyss(kdl);
            assert_eq!(cfg.errors.len(), 1, "{kdl}: {:?}", cfg.errors);
            assert!(cfg.errors[0].message.contains(needle), "{kdl}: {:?}", cfg.errors);
            let d = Input::default();
            assert_eq!(cfg.input.accel_speed, d.accel_speed);
            assert_eq!(cfg.input.scroll_method, d.scroll_method);
            assert_eq!(cfg.input.touchpad.scroll_method, d.touchpad.scroll_method);
            assert!(cfg.input.touchpad.tap_and_drag);
        }
    }

    #[test]
    fn parses_input_device_blocks() {
        let cfg = abyss(
            r#"input {
    device "Pad" {
        accel-profile "flat"
        accel-speed 0.25
        touchpad { natural-scroll #true; click-method "button-areas"; scroll-method "none"; }
    }
    device "Screen" { calibration 0 -1 1 1 0.5 0; }
    device "Pad" { accel-speed -1; scroll-method "none"; }
}
"#,
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.input.devices.len(), 2, "one entry per name");
        let pad = &cfg.input.devices[0];
        assert_eq!(pad.name, "Pad");
        assert_eq!(pad.accel_profile.as_deref(), Some("flat"));
        assert_eq!(
            pad.accel_speed,
            Some(-1.0),
            "the later block overrides key by key"
        );
        assert_eq!(pad.scroll_method.as_deref(), Some("none"));
        assert_eq!(pad.touchpad.natural_scroll, Some(true));
        assert_eq!(pad.touchpad.click_method.as_deref(), Some("button-areas"));
        assert_eq!(pad.touchpad.scroll_method.as_deref(), Some("none"));
        assert_eq!(pad.touchpad.tap_to_click, None);
        assert_eq!(pad.calibration, None);
        assert_eq!(
            cfg.input.devices[1].calibration,
            Some([0.0, -1.0, 1.0, 1.0, 0.5, 0.0])
        );
        // The globals are untouched by a device block.
        assert_eq!(cfg.input.accel_profile, "adaptive");
        assert!(!cfg.input.touchpad.natural_scroll);
    }

    #[test]
    fn bad_device_blocks_are_refused() {
        for (kdl, needle) in [
            ("input { calibration 1 0 0 0 1 0; }", "per device"),
            ("input { touchpad { calibration 1 0 0 0 1 0; }; }", "calibration"),
            ("input { device { accel-speed 0; }; }", "device name"),
            ("input { device \"\" { accel-speed 0; }; }", "device name"),
            (
                "input { device \"M\" { calibration 1 0 0 0 1; }; }",
                "six numbers",
            ),
            (
                "input { device \"M\" { calibration 1 0 0 0 1 0 0; }; }",
                "six numbers",
            ),
            (
                "input { device \"M\" { calibration 1 0 \"x\" 0 1 0; }; }",
                "six numbers",
            ),
            ("input { device \"M\" { accel-speed -2; }; }", "accel-speed"),
            (
                "input { device \"M\" { accel-profile \"slow\"; }; }",
                "accel-profile",
            ),
            (
                "input { device \"M\" { kb-layout \"de\"; }; }",
                "unknown input.device key",
            ),
            (
                "input { device \"M\" { touchpad { tap #true; }; }; }",
                "unknown input.device.touchpad key",
            ),
            (
                "input { device \"M\" { touchpad { scroll-method \"sideways\"; }; }; }",
                "scroll-method",
            ),
        ] {
            let cfg = abyss(kdl);
            assert_eq!(cfg.errors.len(), 1, "{kdl}: {:?}", cfg.errors);
            assert!(cfg.errors[0].message.contains(needle), "{kdl}: {:?}", cfg.errors);
            assert!(
                cfg.input
                    .devices
                    .iter()
                    .all(|d| d.calibration.is_none() && d.accel_speed.is_none() && d.accel_profile.is_none()),
                "{kdl}: nothing invalid applied"
            );
        }
    }

    /// The owner's login loop: a touchpad key this build does not know.
    #[test]
    fn an_unknown_touchpad_key_leaves_tap_to_click_alone() {
        let mut cfg = abyss(
            "input {\n    touchpad {\n        tap-to-click #true\n        drag-lock \"sticky\"\n    }\n}\n",
        );
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert_eq!(cfg.errors[0].line, 4);
        assert!(cfg.input.touchpad.tap_to_click);
        assert!(!cfg.input.touchpad.natural_scroll && !cfg.input.touchpad.dwt);
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });

        let ev = error_event(&cfg.errors, true);
        assert_eq!(
            ev["summary"],
            "abyss.kdl: 1 problem ignored \u{2014} line 4: touchpad key needs a boolean: \"drag-lock\""
        );
        assert_eq!(ev["errors"].as_array().map(Vec::len), Some(1));
        assert_eq!(ev["line"], 4);
    }

    #[test]
    fn a_config_with_no_errors_starts_clean() {
        assert_eq!(
            abyss("general { gaps-in 4; }\n").startup(),
            Startup::Start { ignored: 0 }
        );
    }

    /// Fail-closed case 1: nothing in `policy.kdl` is dropped to a default.
    #[test]
    fn any_policy_kdl_error_refuses() {
        let mut cfg = parse(
            schema::Owner::Policy,
            "clipboard {\n    data-control-alow \"x\"\n}\n",
        );
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert_eq!(cfg.startup(), Startup::Refuse { fatal: 1 });

        // Unparseable policy.kdl refuses; unparseable abyss.kdl is dropped whole.
        let bad = "capture { allow \"unterminated\n";
        let mut policy = Config {
            errors: Config::check_text(Path::new("policy.kdl"), schema::Owner::Policy, bad),
            ..Config::default()
        };
        assert_eq!(policy.startup(), Startup::Refuse { fatal: 1 });
        let mut ours = Config {
            errors: Config::check_text(Path::new("abyss.kdl"), schema::Owner::Abyss, bad),
            ..Config::default()
        };
        assert_eq!(ours.startup(), Startup::Start { ignored: 1 });
    }

    /// Fail-closed case 2: a policy-owned key misplaced in `abyss.kdl`.
    #[test]
    fn a_misplaced_policy_key_refuses() {
        for text in [
            "misc {\n    scripted-input #true\n}\n",
            "capture {\n    allow \"grim\"\n}\n",
            "clipboard {\n    data-control-allow \"wl-paste\"\n}\n",
            "windowrule \"no-agent\" {\n    app-id \"keepassxc\"\n}\n",
            "windowrule \"sensitivity secret\" {\n    app-id \"keepassxc\"\n}\n",
            "windowrule \"app-trust trusted\" {\n    app-id \"x\"\n}\n",
            "windowrule \"seat-compat lock\" {\n    app-id \"x\"\n}\n",
            "windowrule \"irreversible-capable false\" {\n    app-id \"x\"\n}\n",
        ] {
            let mut cfg = abyss(text);
            assert_eq!(
                cfg.startup(),
                Startup::Refuse { fatal: 1 },
                "{text}: {:?}",
                cfg.errors
            );
        }
    }

    /// Fail-closed case 3, parse half; the resolve half is in `backend::drm`.
    #[test]
    fn a_rejected_render_device_refuses() {
        let mut cfg = abyss("misc {\n    render-device 1\n}\n");
        assert_eq!(cfg.startup(), Startup::Refuse { fatal: 1 }, "{:?}", cfg.errors);
        // A sibling that is merely misspelt does not.
        let mut cfg = abyss("misc {\n    render-devcie \"/dev/dri/card1\"\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 }, "{:?}", cfg.errors);
    }

    /// ADR 0064 with ADR 0065: an invalid widget block is dropped and so is
    /// its id in `order`/`important`; the valid block beside it stays.
    #[test]
    fn a_rejected_widget_block_starts_without_it() {
        let text = "bar {\n    widget \"ok\" {\n        exec \"date\"\n    }\n    widget \"bad\" {\n        exec 3\n    }\n    widgets {\n        order \"clock\" \"custom:ok\" \"custom:bad\"\n        important \"custom:bad\"\n    }\n}\n";
        let mut cfg = abyss(text);
        assert_eq!(cfg.startup(), Startup::Start { ignored: 2 }, "{:?}", cfg.errors);
        let names: Vec<&str> = cfg.bar.custom_widgets.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["ok"]);
        assert_eq!(cfg.bar.widgets.order, ["clock", "custom:ok"]);
        assert!(cfg.bar.widgets.important.is_empty());
    }

    fn startup_summary(cfg: &Config) -> String {
        error_event(&cfg.errors, true)["summary"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn lock_bind_intact(cfg: &Config) -> bool {
        let sup_shift = m(true, true, false, false);
        cfg.binds.iter().any(|b| {
            b.mods == sup_shift
                && b.key == Keysym::L
                && matches!(&b.action, Action::Spawn(c) if c == "loginctl lock-session")
        })
    }

    /// Owner decision 2026-09-25: a rejected lock setting starts, auto-lock
    /// stays off, and the summary leads with that — ahead of an earlier,
    /// unrelated error, so "(and N more)" cannot hide it.
    #[test]
    fn a_rejected_idle_lock_setting_starts_with_auto_lock_off() {
        for (text, line) in [
            ("general {\n    nope 1\n}\nidle {\n    lock-timeout-seconds -5\n    lock-command \"swaylock\"\n}\n", 5),
            ("general {\n    nope 1\n}\nidle {\n    lock-timeout-seconds 600\n    lock-command 3\n}\n", 6),
        ] {
            let mut cfg = abyss(text);
            assert_eq!(cfg.startup(), Startup::Start { ignored: 2 }, "{text}: {:?}", cfg.errors);
            assert!(
                cfg.idle.lock_command.is_none() || cfg.idle.lock_timeout.is_none(),
                "auto-lock must be off: {:?}",
                cfg.idle
            );
            let s = startup_summary(&cfg);
            assert!(
                s.starts_with(&format!("abyss.kdl: auto-lock is OFF \u{2014} line {line}: ")),
                "{s}"
            );
            assert!(s.ends_with("(and 1 more; `ec-ctl config validate` lists them)"), "{s}");
            assert!(lock_bind_intact(&cfg), "Super+Shift+L must stay bound");
        }
        // DPMS is not a protection; its default (never) is safe and says so
        // in the ordinary way.
        let mut cfg = abyss("idle {\n    dpms-timeout-seconds -5\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
        assert!(startup_summary(&cfg).starts_with("abyss.kdl: 1 problem ignored"));
    }

    /// A misspelt lock key leaves the session never locking just as surely.
    #[test]
    fn a_misspelt_idle_key_warns_auto_lock_off() {
        let mut cfg = abyss("idle {\n    lock-timout-seconds 600\n    lock-command \"swaylock\"\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
        assert!(cfg.idle.lock_timeout.is_none());
        let s = startup_summary(&cfg);
        assert!(
            s.starts_with("abyss.kdl: auto-lock is OFF \u{2014} line 2: "),
            "{s}"
        );
        assert!(lock_bind_intact(&cfg));
    }

    /// So does a misspelt `idle` node (within edit distance 2).
    #[test]
    fn a_misspelt_idle_node_warns_auto_lock_off() {
        let mut cfg = abyss("idel {\n    lock-timeout-seconds 600\n    lock-command \"swaylock\"\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
        assert!(cfg.idle.lock_command.is_none());
        let s = startup_summary(&cfg);
        assert!(
            s.starts_with("abyss.kdl: auto-lock is OFF \u{2014} line 1: "),
            "{s}"
        );
        assert!(lock_bind_intact(&cfg));
        // A node nowhere near `idle` or `xwayland` is only an ordinary error.
        let mut cfg = abyss("generl {\n    gaps-in 1\n}\n");
        cfg.startup();
        assert!(startup_summary(&cfg).starts_with("abyss.kdl: 1 problem ignored"));
    }

    /// The notice only says what is true: if both lock keys still ended up
    /// set (here the misspelt key is an unrelated extra), auto-lock is on.
    #[test]
    fn auto_lock_notice_is_dropped_when_auto_lock_is_on() {
        let mut cfg = abyss(
            "idle {\n    lock-timeout-seconds 600\n    lock-command \"swaylock\"\n    lock-grace 5\n}\n",
        );
        assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
        assert!(startup_summary(&cfg).starts_with("abyss.kdl: 1 problem ignored"));
    }

    /// ADR 0026 / ADR 0064: any refusal touching `xwayland` starts with it
    /// OFF, the isolating value, never the `enable #true` default.
    #[test]
    fn a_rejected_xwayland_setting_starts_with_xwayland_off() {
        for (text, line) in [
            // misspelt child
            ("xwayland {\n    enabled #false\n}\n", 2),
            // misspelt node
            ("xwyland {\n    enable #false\n}\n", 1),
            // non-bool value
            ("xwayland {\n    enable \"false\"\n}\n", 2),
        ] {
            let mut cfg = abyss(text);
            assert_eq!(cfg.errors.len(), 1, "{text}: {:?}", cfg.errors);
            assert_eq!(cfg.startup(), Startup::Start { ignored: 1 });
            assert!(!cfg.xwayland.enable, "{text}: Xwayland must be off");
            let s = startup_summary(&cfg);
            assert!(
                s.starts_with(&format!("abyss.kdl: Xwayland is OFF \u{2014} line {line}: ")),
                "{s}"
            );
        }
        // A clean `enable #true` still starts it.
        let mut cfg = abyss("xwayland {\n    enable #true\n}\n");
        assert_eq!(cfg.startup(), Startup::Start { ignored: 0 });
        assert!(cfg.xwayland.enable);
    }

    #[test]
    fn both_fail_safes_lead_the_summary() {
        let mut cfg = abyss(
            "general {\n    nope 1\n}\nxwayland {\n    enabled #false\n}\nidle {\n    lock-command 3\n}\n",
        );
        assert_eq!(cfg.startup(), Startup::Start { ignored: 3 });
        let s = startup_summary(&cfg);
        assert!(
            s.starts_with("abyss.kdl: auto-lock is OFF \u{2014} line 8: "),
            "{s}"
        );
        assert!(s.contains("; Xwayland is OFF \u{2014} line 5: "), "{s}");
        assert!(
            s.ends_with("(and 1 more; `ec-ctl config validate` lists them)"),
            "{s}"
        );
        // A failed hot reload changes nothing live, so it claims nothing.
        let r = error_event(&cfg.errors, false)["summary"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(r.starts_with("abyss.kdl: 3 problems, change not applied"), "{r}");
    }

    /// C-00 §4.5 / COMP-04 §6: a dropped `bind` or `idle` node leaves the
    /// human override, agent-attention and the lock bind in force.
    #[test]
    fn dropped_nodes_leave_the_reserved_and_lock_binds_bound() {
        let mut cfg = abyss(concat!(
            "bind \"SUPER\" \"Escape\" { quit; }\n",
            "bind \"SUPER\" \"space\" { quit; }\n",
            "bind \"SUPER+SHIFT\" \"L\" { no-such-action; }\n",
            "idle {\n    dpms-timeout-seconds \"soon\"\n}\n",
        ));
        assert_eq!(cfg.errors.len(), 4, "{:?}", cfg.errors);
        assert_eq!(cfg.startup(), Startup::Start { ignored: 4 });
        let sup = m(true, false, false, false);
        let sup_shift = m(true, true, false, false);
        let has = |mods: Mods, key: Keysym, f: &dyn Fn(&Action) -> bool| {
            cfg.binds
                .iter()
                .any(|b| b.mods == mods && b.key == key && f(&b.action))
        };
        assert!(has(sup, Keysym::Escape, &|a| matches!(a, Action::AgentOverride)));
        assert!(has(sup, Keysym::space, &|a| matches!(a, Action::AgentAttention)));
        assert!(has(sup_shift, Keysym::L, &|a| {
            matches!(a, Action::Spawn(c) if c == "loginctl lock-session")
        }));
    }

    /// `bar.pinned-apps` (ADR 0074): ordered desktop-entry ids; bad entries
    /// are refused one by one and the rest stand.
    #[test]
    fn pinned_apps_parse_and_refuse_bad_entries() {
        let cfg = abyss("");
        assert!(cfg.errors.is_empty() && cfg.bar.pinned_apps.is_empty());

        let cfg = abyss("bar {\n    pinned-apps \"firefox\" \"org.gnome.Nautilus\" \"foot\"\n}\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.bar.pinned_apps, ["firefox", "org.gnome.Nautilus", "foot"]);

        let cfg = abyss(concat!(
            "bar {\n    pinned-apps \"a\" \"\" \"/usr/share/applications/b.desktop\" ",
            "\"c.desktop\" \"a\" 5 x=\"y\" \"d\"\n}\n"
        ));
        assert_eq!(cfg.errors.len(), 6, "{:?}", cfg.errors);
        assert_eq!(cfg.bar.pinned_apps, ["a", "d"]);

        // Round-trip through the editor.
        let ids = ["firefox", "foot"].map(|i| KdlValue::String(i.into()));
        let text = edit::set_list("", "bar.pinned-apps", &ids).unwrap();
        let cfg = abyss(&text);
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.bar.pinned_apps, ["firefox", "foot"]);

        // A repeated node is dropped whole; the first stands.
        let cfg = abyss("bar {\n    pinned-apps \"a\"\n    pinned-apps \"b\"\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert_eq!(cfg.bar.pinned_apps, ["a"]);
    }

    /// A rejected node is dropped whole, never applied as well.
    #[test]
    fn rejected_nodes_do_not_half_apply() {
        let cfg = abyss("bar {\n    tray {\n        pinned \"a\"\n        pinned \"b\"\n    }\n}\n");
        assert_eq!(cfg.errors.len(), 1);
        assert_eq!(cfg.bar.tray.pinned, Some(vec!["a".to_string()]));

        let cfg = abyss("animations {\n    animation \"windows\" duration=\"80ms\" speed=2\n}\n");
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(
            cfg.animations.overrides.is_empty(),
            "{:?}",
            cfg.animations.overrides
        );

        // One bad child drops the whole event block.
        let cfg = abyss(
            "animations {\n    window-open {\n        style \"fade\"\n        duration-ms 99999\n    }\n}\n",
        );
        assert_eq!(cfg.errors.len(), 1, "{:?}", cfg.errors);
        assert!(
            cfg.animations.overrides.is_empty(),
            "{:?}",
            cfg.animations.overrides
        );
    }

    #[test]
    fn parses_animation_presets_and_overrides() {
        use animations::{Curve, Event, Override, Preset};
        let cfg = abyss(
            "animations {\n    preset \"lively\"\n    speed 2.0\n    reduce-motion #true\n    \
             window-open { style \"slide\"; duration-ms \"1s\"; curve \"bounce\"; }\n    \
             window-close { style \"ec-anim-pack:embers\"; }\n    focus {\n    }\n}\n",
        );
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        let a = &cfg.animations;
        assert_eq!((a.preset, a.speed, a.reduce_motion), (Preset::Lively, 2.0, true));
        assert_eq!(
            a.overrides.get(&Event::WindowOpen),
            Some(&Override {
                style: Some("slide".into()),
                duration_ms: Some(1000),
                curve: Some(Curve::Bounce),
            })
        );
        // An add-on style is stored as written; whether the pack is there is
        // a render-time question, not a config error.
        assert_eq!(
            a.overrides
                .get(&Event::WindowClose)
                .and_then(|o| o.style.as_deref()),
            Some("ec-anim-pack:embers")
        );
        // An empty block is no override, so it does not make the config custom.
        assert!(!a.overrides.contains_key(&Event::Focus));
        assert_eq!(a.overrides.len(), 2);

        // Spring and bounce are animation curves now.
        let cfg = abyss("animations {\n    window-move { curve \"spring\"; }\n}\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    }

    #[test]
    fn bad_animation_values_are_refused() {
        for (text, needle) in [
            ("preset \"wild\"", "preset"),
            ("speed 8.0", "speed"),
            ("speed 0.1", "speed"),
            ("window-move { style \"pop\"; }", "style"),
            ("window-open { style \"Pack:x\"; }", "style"),
            ("window-open { curve \"wobble\"; }", "curve"),
            ("window-open { duration-ms 10001; }", "duration-ms"),
            ("window-open { speed 2; }", "speed"),
            ("window-open \"pop\"", "takes a block"),
            ("windows-open { style \"pop\"; }", "windows-open"),
        ] {
            let cfg = abyss(&format!("animations {{\n    {text}\n}}\n"));
            assert_eq!(cfg.errors.len(), 1, "{text}: {:?}", cfg.errors);
            assert!(cfg.errors[0].message.contains(needle), "{text}: {:?}", cfg.errors);
            assert_eq!(cfg.animations, Animations::default(), "{text}");
        }
        // 10s exactly is the limit, not past it.
        let cfg = abyss("animations {\n    window-open { duration-ms \"10s\"; }\n}\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
    }

    /// Fields merge key by key across files, like every other dotted key.
    #[test]
    fn animation_overrides_merge_across_files() {
        use animations::{Curve, Event};
        let mut cfg = Config::default();
        for text in [
            "animations { preset \"smooth\"; window-open { style \"fade\"; duration-ms 300; } }",
            "animations { window-open { curve \"linear\"; duration-ms 100; } }",
        ] {
            let doc: KdlDocument = text.parse().unwrap();
            cfg.apply(&doc, &mut Vec::new());
        }
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        let r = cfg.animations.resolve(Event::WindowOpen);
        assert_eq!(
            (r.style.as_str(), r.duration_ms, r.curve),
            ("fade", 100, Curve::Linear)
        );
    }

    /// Every legacy form maps as `ec-ctl config migrate` rewrites it.
    #[test]
    fn legacy_animation_forms_map() {
        use animations::{Curve, Event, Preset};
        // `enabled #false` is `preset "off"`; its legacy nodes were inert.
        let mut cfg = Config::default();
        cfg.animations.preset = Preset::Smooth; // as if a lower file chose it
        let doc: KdlDocument = "animations { enabled #false; animation \"windows\"; }"
            .parse()
            .unwrap();
        cfg.apply(&doc, &mut Vec::new());
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert_eq!(cfg.animations.preset, Preset::Off);
        assert!(cfg.animations.overrides.is_empty());
        // ...unless the same block names a preset.
        let cfg = abyss("animations {\n    preset \"subtle\"\n    enabled #false\n}\n");
        assert_eq!(cfg.animations.preset, Preset::Subtle);
        // Without `enabled #true` a legacy node does nothing, as before.
        let cfg = abyss("animations {\n    animation \"fade\" duration=90\n}\n");
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        assert!(cfg.animations.overrides.is_empty());
        // Each name, with the old defaults filled in.
        for (name, ev, style) in [
            ("windows", Event::WindowMove, "glide"),
            ("workspaces", Event::WorkspaceSwitch, "slide"),
            ("fade", Event::WindowOpen, "fade"),
            ("border", Event::Focus, "crossfade"),
        ] {
            let cfg = abyss(&format!(
                "animations {{\n    enabled #true\n    animation \"{name}\"\n}}\n"
            ));
            assert!(cfg.errors.is_empty(), "{name}: {:?}", cfg.errors);
            let r = cfg.animations.resolve(ev);
            assert_eq!(
                (r.style.as_str(), r.duration_ms, r.curve),
                (style, schema::ANIMATION_DEFAULT_MS, Curve::EaseOut),
                "{name}"
            );
            assert_eq!(cfg.animations.overrides.len(), 1, "{name}");
        }
        // The new form wins over a legacy node for the same event.
        let cfg = abyss(
            "animations {\n    enabled #true\n    animation \"border\" duration=500\n    focus { duration-ms 90; }\n}\n",
        );
        assert_eq!(cfg.animations.resolve(Event::Focus).duration_ms, 90);
    }

    #[test]
    fn the_summary_counts_and_points_at_the_list() {
        let cfg = abyss("general {\n    nope 1\n    nada 2\n}\n");
        let s = error_event(&cfg.errors, true)["summary"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(
            s.starts_with("abyss.kdl: 2 problems ignored \u{2014} line 2: "),
            "{s}"
        );
        assert!(s.contains("and 1 more; `ec-ctl config validate`"), "{s}");
        let r = error_event(&cfg.errors, false)["summary"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(r.contains("change not applied"), "{r}");
    }
}
