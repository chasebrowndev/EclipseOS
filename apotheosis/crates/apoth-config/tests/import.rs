// SPDX-License-Identifier: AGPL-3.0-only
use apoth_config::Editor;
use apoth_config::fs::MemFs;
use apoth_config::import::{Status, import_foot};
use apoth_config::load::{LoadInput, load};
use apoth_config::types::Value;
use std::path::PathBuf;

const FOOT: &str = "\
# my foot.ini
term=foot
shell=/bin/zsh -l
font=JetBrains Mono:size=11:weight=bold, Noto Color Emoji
font-bold=JetBrains Mono:style=ExtraBold
pad=8x4 center
bold-text-in-bright=yes
selection-target=primary
letter-spacing=1
line-height=18

[scrollback]
lines=5000
multiplier=2.5

[cursor]
style=beam
blink=yes
color=1d2021 ebdbb2

[colors]
alpha=0.9
foreground=ebdbb2
background=1d2021
regular0=282828
regular1=cc241d
bright7=ffffff
dim0=111111

[url]
launch=xdg-open ${url}
protocols=http, https

[security]
osc52=copy-enabled

[key-bindings]
scrollback-up-page=Shift+Page_Up

[tweak]
grapheme-shaping=no

[mystery]
x=1
";

fn entry<'a>(
    i: &'a apoth_config::import::Import,
    section: &str,
    key: &str,
) -> &'a apoth_config::import::ReportEntry {
    i.report
        .iter()
        .find(|e| e.section == section && e.key == key)
        .unwrap_or_else(|| panic!("no report entry for [{section}] {key}"))
}

#[test]
fn imported_kdl_loads_clean_with_expected_values() {
    let i = import_foot(FOOT);
    let fs = MemFs::new().with("/h/apotheosis.kdl", &i.kdl);
    let r = load(
        &fs,
        &LoadInput {
            user: Some(PathBuf::from("/h/apotheosis.kdl")),
            ..Default::default()
        },
        None,
    );
    assert!(r.is_valid(), "{}\n{}", r.render(), i.kdl);
    let c = &r.config;
    assert_eq!(c.str("font.family"), Some("JetBrains Mono"));
    assert_eq!(c.float("font.size"), 11.0);
    assert_eq!(c.list("font.fallback"), ["Noto Color Emoji"]);
    assert_eq!(c.str("font.bold-family"), Some("JetBrains Mono"));
    assert_eq!(c.str("shell.program"), Some("/bin/zsh"));
    assert_eq!(c.list("shell.args"), ["-l"]);
    assert_eq!(c.int("window.padding-x"), 8);
    assert_eq!(c.int("window.padding-y"), 4);
    assert!(c.bool("window.padding-balance"));
    assert!(c.bool("colors.bold-is-bright"));
    assert!(c.bool("selection.copy-on-select"));
    assert_eq!(c.str("font.adjust-cell-width"), Some("1px"));
    assert_eq!(c.int("scrollback.lines"), 5000);
    assert_eq!(c.float("mouse.scroll-multiplier"), 2.5);
    assert_eq!(c.str("cursor.style"), Some("bar"));
    assert!(c.bool("cursor.blink"));
    assert_eq!(c.color("colors.cursor"), Some([0xeb, 0xdb, 0xb2, 255]));
    assert_eq!(c.color("colors.cursor-text"), Some([0x1d, 0x20, 0x21, 255]));
    assert_eq!(c.float("window.opacity"), 0.9);
    assert_eq!(c.color("colors.foreground"), Some([0xeb, 0xdb, 0xb2, 255]));
    assert_eq!(c.list("links.open-command"), ["xdg-open", "{url}"]);
    assert_eq!(c.list("links.schemes"), ["http", "https"]);
    assert_eq!(c.list("colors.palette").len(), 16);
    assert_eq!(c.list("colors.palette")[1], "#cc241d");
    assert_eq!(c.list("colors.palette")[15], "#ffffff");
    assert!(c.bool("clipboard.osc52"));
}

#[test]
fn every_foot_line_is_reported() {
    let i = import_foot(FOOT);
    let key_lines = FOOT
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#') && !t.starts_with('[')
        })
        .count();
    let reported = i.report.iter().filter(|e| e.line > 0).count();
    assert_eq!(reported, key_lines);
}

#[test]
fn unmapped_and_approximate_are_named_not_dropped() {
    let i = import_foot(FOOT);
    assert_eq!(entry(&i, "main", "line-height").status, Status::Unmapped);
    assert_eq!(entry(&i, "colors", "dim0").status, Status::Unmapped);
    assert_eq!(
        entry(&i, "key-bindings", "scrollback-up-page").status,
        Status::Unmapped
    );
    assert_eq!(
        entry(&i, "tweak", "grapheme-shaping").status,
        Status::Unmapped
    );
    assert_eq!(entry(&i, "mystery", "x").status, Status::Unmapped);
    assert_eq!(entry(&i, "security", "osc52").status, Status::Approximate);
    assert_eq!(entry(&i, "main", "font").status, Status::Approximate);
    assert_eq!(entry(&i, "main", "term").status, Status::Mapped);
    let text = i.render_report();
    assert!(
        text.contains("line-height") && text.contains("not imported"),
        "{text}"
    );
    // Partial palette is flagged.
    assert!(
        i.report
            .iter()
            .any(|e| e.key == "palette" && e.status == Status::Approximate)
    );
}

#[test]
fn out_of_range_values_are_reported_not_written() {
    let i = import_foot("[main]\nfont=Mono:size=500\n[colors]\nalpha=7\nforeground=zzz\n");
    assert_eq!(entry(&i, "colors", "alpha").status, Status::Unmapped);
    assert_eq!(entry(&i, "colors", "foreground").status, Status::Unmapped);
    let ed = Editor::parse(&i.kdl).unwrap();
    assert_eq!(ed.get("window.opacity").unwrap(), None);
    assert_eq!(ed.get("font.size").unwrap(), None);
    assert_eq!(
        ed.get("font.family").unwrap(),
        Some(Value::Str("Mono".into()))
    );
}

#[test]
fn garbage_and_empty_input_never_panic() {
    let i = import_foot("");
    assert!(i.report.is_empty());
    assert!(i.kdl.contains("version 1"));
    let i = import_foot("[main\nnot a pair\n=\n[]\nfont=\n");
    assert!(i.unmapped().count() >= 2);
    let i = import_foot(&"a=b\n".repeat(300_000));
    assert!(i.unmapped().count() == 1);
}

#[test]
fn later_duplicate_wins() {
    let i = import_foot("[scrollback]\nlines=10\nlines=20\n");
    let ed = Editor::parse(&i.kdl).unwrap();
    assert_eq!(ed.get("scrollback.lines").unwrap(), Some(Value::Int(20)));
}
