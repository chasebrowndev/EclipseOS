// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

fn schemes() -> Vec<String> {
    ["http", "https", "file", "mailto"]
        .map(String::from)
        .to_vec()
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("apoth-open-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn entry(id: &str, name: &str, exec: &str, mime: &[&str], terminal: bool) -> desktop::Entry {
    let text = format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nMimeType={}\nTerminal={terminal}\n",
        mime.join(";")
    );
    desktop::parse(
        &text,
        id,
        Path::new("/usr/share/applications/x.desktop"),
        &[],
    )
    .unwrap()
}

fn dbs(entries: Vec<desktop::Entry>, mimeapps: &str) -> Dbs {
    Dbs {
        mime: mime::MimeDb::builtin(),
        index: desktop::Index {
            dirs: vec![Vec::new(), entries],
        },
        lists: vec![mimeapps::Located {
            level: None,
            desktop_specific: false,
            list: mimeapps::List::parse(mimeapps, false),
        }],
    }
}

fn req(raw: &str, kind: RawKind, cwd: Option<&Path>) -> Request {
    Request {
        intent: Intent::Menu,
        raw: raw.to_string(),
        kind,
        visible: raw.to_string(),
        cwd: cwd.map(Path::to_path_buf),
        remote: None,
        hostname: "box".into(),
        schemes: schemes(),
        rules: Vec::new(),
        placement: Placement::Float,
        forced_placement: false,
        opener: vec!["xdg-open".into(), "{url}".into()],
        opener_custom: false,
        env: XdgEnv {
            home: PathBuf::from("/home/u"),
            path: String::new(),
            ..XdgEnv::default()
        },
    }
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

#[test]
fn remote_pane_paths_are_refused_with_the_host() {
    let r = classify("/etc/passwd", RawKind::Path, Some("db1"), &schemes(), "box");
    assert_eq!(r, Err(Refusal::Remote("db1".into())));
    assert_eq!(r.unwrap_err().message(), "path is on db1");
    // A selection shaped like a path from a remote pane: same.
    assert!(
        classify(
            "notes.txt",
            RawKind::Unknown,
            Some("db1"),
            &schemes(),
            "box"
        )
        .is_err()
    );
    // file:// from a remote pane, even naming no host.
    assert_eq!(
        classify(
            "file:///etc/hosts",
            RawKind::Link,
            Some("db1"),
            &schemes(),
            "box"
        ),
        Err(Refusal::Remote("db1".into()))
    );
    // file:// naming another host, from a local pane.
    assert_eq!(
        classify(
            "file://other/etc/hosts",
            RawKind::Link,
            None,
            &schemes(),
            "box"
        ),
        Err(Refusal::Remote("other".into()))
    );
    // URLs from a remote pane are fine.
    assert_eq!(
        classify(
            "https://x.org",
            RawKind::Url,
            Some("db1"),
            &schemes(),
            "box"
        ),
        Ok(Target::Url("https://x.org".into()))
    );
}

#[test]
fn remote_refusal_keeps_copy_and_disables_open() {
    let mut q = req("/tmp/a.png", RawKind::Path, None);
    q.remote = Some("db1".into());
    let r = resolve_with(&q, &dbs(Vec::new(), ""));
    assert_eq!(r.refusal.as_deref(), Some("path is on db1"));
    assert!(r.default.is_none());
    let (rows, choices) = menu_rows(&r);
    assert!(choices.is_empty());
    assert_eq!(rows[0].pick, OpenPick::Disabled);
    assert_eq!(rows[0].hint.as_deref(), Some("path is on db1"));
    assert_eq!(rows[1].pick, OpenPick::Disabled);
    assert!(
        rows.iter()
            .any(|r| r.pick == OpenPick::Copy("/tmp/a.png".into()))
    );
}

#[test]
fn links_pass_check_link() {
    assert_eq!(
        classify(
            "javascript:alert(1)",
            RawKind::Link,
            None,
            &schemes(),
            "box"
        ),
        Err(Refusal::Link(LinkRefused::Scheme))
    );
    assert_eq!(
        classify("https://a b", RawKind::Url, None, &schemes(), "box"),
        Err(Refusal::Link(LinkRefused::Malformed))
    );
    assert_eq!(
        classify("file:///tmp/x", RawKind::Link, None, &schemes(), "box"),
        Ok(Target::Path("/tmp/x".into()))
    );
    // A selection is a URL only with a scheme and `//` (or mailto).
    assert_eq!(
        classify("src/main.rs:12", RawKind::Unknown, None, &schemes(), "box"),
        Ok(Target::Path("src/main.rs:12".into()))
    );
    assert_eq!(
        classify("mailto:a@b.c", RawKind::Unknown, None, &schemes(), "box"),
        Ok(Target::Url("mailto:a@b.c".into()))
    );
    // Multi-byte text at the scheme boundary does not panic.
    assert!(classify("fïle:x", RawKind::Url, None, &schemes(), "box").is_err());
}

#[test]
fn paths_are_made_absolute() {
    let home = Path::new("/home/u");
    let cwd = Path::new("/w/proj");
    assert_eq!(
        absolute("~", Some(cwd), home),
        Some(PathBuf::from("/home/u"))
    );
    assert_eq!(
        absolute("~/a", None, home),
        Some(PathBuf::from("/home/u/a"))
    );
    assert_eq!(
        absolute("../x/./y", Some(cwd), home),
        Some(PathBuf::from("/w/x/y"))
    );
    assert_eq!(absolute("x", None, home), None);
    assert_eq!(absolute("/../..", None, home), Some(PathBuf::from("/")));
    // A leading `-` can never become an option: it is joined to the cwd.
    assert_eq!(
        absolute("-rf", Some(cwd), home),
        Some(PathBuf::from("/w/proj/-rf"))
    );
}

#[test]
fn line_col_suffixes() {
    assert_eq!(split_line_col("a.rs:12"), Some(("a.rs", 12, None)));
    assert_eq!(split_line_col("a.rs:12:5"), Some(("a.rs", 12, Some(5))));
    assert_eq!(split_line_col("a.rs"), None);
    assert_eq!(split_line_col(":12"), None);
    assert_eq!(split_line_col("a:b:7"), Some(("a:b", 7, None)));
}

#[test]
fn image_defaults_to_icat_in_a_float() {
    let d = tmpdir("img");
    // The name lies; the magic says PNG.
    std::fs::write(d.join("pic.dat"), PNG).unwrap();
    let viewer = entry(
        "viewer.desktop",
        "Viewer",
        "viewer %f",
        &["image/png"],
        false,
    );
    let r = resolve_with(
        &req("pic.dat", RawKind::Path, Some(&d)),
        &dbs(
            vec![viewer],
            "[Default Applications]\nimage/png=viewer.desktop\n",
        ),
    );
    assert_eq!(r.mime.as_deref(), Some("image/png"));
    let def = r.default.clone().unwrap();
    assert_eq!(
        def.handler,
        Handler::Term {
            term: Term::Icat,
            file: d.join("pic.dat")
        }
    );
    assert_eq!(def.placement, Placement::Float);
    assert!(r.fallback.is_none());
    // Menu: named default, the picker, the other two placements, copy rows.
    let (rows, choices) = menu_rows(&r);
    assert_eq!(rows[0].title, "Open with apoth icat in a float");
    assert_eq!(rows[1].pick, OpenPick::Picker);
    assert_eq!(choices.len(), 3);
    assert!(rows.iter().any(|r| r.title.ends_with("in a window")));
    assert!(rows.iter().any(|r| r.title.ends_with("in a split pane")));
    // Picker: terminal handlers first, then the declaring app.
    let (pinned, prow, pchoices) = picker_rows(&r, false);
    assert_eq!(pinned[0].title, "Remember for image/png");
    // Three placements, the declaring app, the opener.
    assert_eq!(prow.len(), 5);
    assert!(matches!(pchoices[3].handler, Handler::Desktop { .. }));
    assert!(matches!(pchoices[4].handler, Handler::Opener { .. }));
    assert_eq!(prow[0].hint.as_deref(), Some("in the terminal"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn video_defaults_to_play_with_the_desktop_fallback() {
    let d = tmpdir("vid");
    std::fs::write(d.join("clip.mkv"), b"\x1a\x45\xdf\xa3 not really").unwrap();
    let mpv = entry(
        "mpv.desktop",
        "mpv",
        "mpv -- %U",
        &["video/x-matroska"],
        false,
    );
    let r = resolve_with(
        &req(&d.join("clip.mkv").to_string_lossy(), RawKind::Path, None),
        &dbs(
            vec![mpv],
            "[Default Applications]\nvideo/x-matroska=mpv.desktop\n",
        ),
    );
    assert!(matches!(
        r.default.as_ref().unwrap().handler,
        Handler::Term {
            term: Term::Play,
            ..
        }
    ));
    let fb = r.fallback.unwrap();
    let Handler::Desktop { argv, .. } = fb.handler else {
        panic!("fallback is the desktop default")
    };
    assert_eq!(argv[0], "mpv");
    assert_eq!(argv[1], "--");
    // Local files go to `%U` as paths.
    assert_eq!(argv[2], d.join("clip.mkv").to_string_lossy());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn rules_beat_builtins_and_wildcards_lose_to_exact() {
    let d = tmpdir("rule");
    std::fs::write(d.join("a.png"), PNG).unwrap();
    let viewer = entry(
        "viewer.desktop",
        "Viewer",
        "viewer %f",
        &["image/png"],
        false,
    );
    let gimp = entry("gimp.desktop", "GIMP", "gimp %U", &["image/png"], false);
    let mut q = req("a.png", RawKind::Path, Some(&d));
    q.rules = vec![
        ("image/*".into(), vec!["gimp.desktop".into()]),
        ("image/png".into(), vec!["viewer.desktop".into()]),
    ];
    let r = resolve_with(&q, &dbs(vec![viewer.clone(), gimp.clone()], ""));
    let Handler::Desktop { id, argv, .. } = r.default.unwrap().handler else {
        panic!()
    };
    assert_eq!(id, "viewer.desktop");
    assert_eq!(
        argv,
        vec![
            "viewer".to_string(),
            d.join("a.png").to_string_lossy().into_owned()
        ]
    );
    // A rule's placement for a terminal handler; a forced placement wins.
    q.rules = vec![("image/*".into(), vec!["apoth-icat".into(), "window".into()])];
    let r = resolve_with(&q, &dbs(vec![], ""));
    assert_eq!(r.default.unwrap().placement, Placement::Window);
    q.forced_placement = true;
    q.placement = Placement::Inline;
    let r = resolve_with(&q, &dbs(vec![], ""));
    assert_eq!(r.default.unwrap().placement, Placement::Inline);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn urls_go_through_the_scheme_handler() {
    let ff = entry(
        "firefox.desktop",
        "Firefox",
        "firefox %u",
        &["x-scheme-handler/https"],
        false,
    );
    let q = req("https://example.org/a?b=1", RawKind::Url, None);
    let r = resolve_with(
        &q,
        &dbs(
            vec![ff.clone()],
            "[Default Applications]\nx-scheme-handler/https=firefox.desktop\n",
        ),
    );
    assert_eq!(r.mime.as_deref(), Some("x-scheme-handler/https"));
    let def = r.default.clone().unwrap();
    assert_eq!(def.title(), "Firefox");
    let Handler::Desktop { argv, .. } = &def.handler else {
        panic!()
    };
    assert_eq!(
        argv,
        &vec!["firefox".to_string(), "https://example.org/a?b=1".into()]
    );
    assert_eq!(menu_rows(&r).0[0].title, "Open with Firefox");
    assert!(r.copy_path.is_none());
    // A user-set opener wins for URLs.
    let mut q2 = q.clone();
    q2.opener_custom = true;
    q2.opener = vec!["my-browser".into(), "{url}".into()];
    let r2 = resolve_with(
        &q2,
        &dbs(
            vec![ff.clone()],
            "[Default Applications]\nx-scheme-handler/https=firefox.desktop\n",
        ),
    );
    assert!(matches!(
        r2.default.unwrap().handler,
        Handler::Opener { .. }
    ));
    // No desktop default: the opener.
    let r3 = resolve_with(&q, &dbs(vec![], ""));
    assert_eq!(
        r3.default.unwrap().handler,
        Handler::Opener {
            argv: vec!["xdg-open".into(), "https://example.org/a?b=1".into()]
        }
    );
}

#[test]
fn apps_that_need_a_local_file_skip_urls() {
    // `%f` cannot take a remote URL: the app is not offered.
    let viewer = entry("v.desktop", "V", "v %f", &["x-scheme-handler/https"], false);
    let r = resolve_with(
        &req("https://x.org", RawKind::Url, None),
        &dbs(
            vec![viewer],
            "[Default Applications]\nx-scheme-handler/https=v.desktop\n",
        ),
    );
    assert!(matches!(r.default.unwrap().handler, Handler::Opener { .. }));
    assert!(
        r.associated
            .iter()
            .all(|c| !matches!(c.handler, Handler::Desktop { .. }))
    );
}

#[test]
fn missing_files_and_line_suffixes() {
    let d = tmpdir("miss");
    std::fs::write(d.join("main.rs"), b"fn main() {}\n").unwrap();
    let r = resolve_with(
        &req("main.rs:3:7", RawKind::Path, Some(&d)),
        &dbs(vec![], ""),
    );
    assert!(r.refusal.is_none());
    assert_eq!(
        r.copy_path.as_deref(),
        Some(&*d.join("main.rs").to_string_lossy())
    );
    let r = resolve_with(&req("nope.rs", RawKind::Path, Some(&d)), &dbs(vec![], ""));
    assert_eq!(r.refusal.as_deref(), Some("file not found"));
    let (rows, _) = menu_rows(&r);
    assert_eq!(rows[0].pick, OpenPick::Disabled);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn directories_are_inode_directory() {
    let d = tmpdir("dir");
    let fm = entry(
        "files.desktop",
        "Files",
        "files %U",
        &["inode/directory"],
        false,
    );
    let r = resolve_with(
        &req(&d.to_string_lossy(), RawKind::Path, None),
        &dbs(
            vec![fm],
            "[Default Applications]\ninode/directory=files.desktop\n",
        ),
    );
    assert_eq!(r.mime.as_deref(), Some("inode/directory"));
    assert_eq!(r.default.unwrap().title(), "Files");
    assert!(r.terminal.is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn reveal_only_for_links_whose_text_differs() {
    let mut q = req("https://evil.example", RawKind::Link, None);
    q.visible = "https://bank.example".into();
    assert!(resolve_with(&q, &dbs(vec![], "")).reveal);
    q.visible = "https://evil.example".into();
    assert!(!resolve_with(&q, &dbs(vec![], "")).reveal);
}

#[test]
fn rule_values_round_trip_through_the_validator() {
    let d = tmpdir("rv");
    std::fs::write(d.join("a.png"), PNG).unwrap();
    let viewer = entry(
        "viewer.desktop",
        "Viewer",
        "viewer %f",
        &["image/png"],
        false,
    );
    let r = resolve_with(
        &req("a.png", RawKind::Path, Some(&d)),
        &dbs(vec![viewer], ""),
    );
    let key = r.remember_key().unwrap();
    for c in r.terminal.iter().chain(&r.associated) {
        let v = c.rule_value();
        apoth_config::lang::validate_open_handler(key, &v).unwrap();
    }
    assert_eq!(r.terminal[0].rule_value(), vec!["apoth-icat", "float"]);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn hosts_are_cleaned_for_display() {
    assert_eq!(clean_host("db1.lan"), "db1.lan");
    assert_eq!(clean_host("\x1b[31mx"), "31mx");
    assert_eq!(clean_host(""), "a remote host");
}

#[test]
fn env_defaults() {
    let e = XdgEnv::from(&|k: &str| match k {
        "HOME" => Some("/h".into()),
        "XDG_CURRENT_DESKTOP" => Some("Eclipse:GNOME".into()),
        "XDG_DATA_HOME" => Some("relative".into()),
        _ => None,
    });
    assert_eq!(e.data_home, PathBuf::from("/h/.local/share"));
    assert_eq!(e.config_dirs, vec![PathBuf::from("/etc/xdg")]);
    assert_eq!(e.desktops, vec!["eclipse", "gnome"]);
    assert_eq!(
        e.app_dirs()[0],
        PathBuf::from("/h/.local/share/applications")
    );
}

fn firefox() -> desktop::Entry {
    entry(
        "firefox.desktop",
        "Firefox",
        "firefox %u",
        &["x-scheme-handler/https", "text/html"],
        false,
    )
}

const FF_DEFAULT: &str = "[Default Applications]\nx-scheme-handler/https=firefox.desktop\n";

#[test]
fn no_desktop_default_falls_back_to_the_browser_for_renderable_types() {
    let d = tmpdir("browser");
    std::fs::write(d.join("a b.pdf"), b"%PDF-1.4\n").unwrap();
    let r = resolve_with(
        &req("a b.pdf", RawKind::Path, Some(&d)),
        &dbs(vec![firefox()], FF_DEFAULT),
    );
    let def = r.default.clone().unwrap();
    assert_eq!(def.title(), "Firefox");
    let Handler::Desktop { argv, .. } = &def.handler else {
        panic!()
    };
    assert_eq!(argv[0], "firefox");
    assert!(
        argv[1].starts_with("file:///") && argv[1].ends_with("/a%20b.pdf"),
        "{argv:?}"
    );
    assert_eq!(menu_rows(&r).0[0].title, "Open with Firefox");
    // A video's `apoth play` falls back to the browser when no app handles the type.
    std::fs::write(d.join("v.webm"), b"\x1aE\xdf\xa3").unwrap();
    let r = resolve_with(
        &req("v.webm", RawKind::Path, Some(&d)),
        &dbs(vec![firefox()], FF_DEFAULT),
    );
    assert!(matches!(
        r.default.unwrap().handler,
        Handler::Term {
            term: Term::Play,
            ..
        }
    ));
    assert_eq!(r.fallback.unwrap().title(), "Firefox");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn no_default_and_not_renderable_has_no_default_so_the_picker_opens() {
    let d = tmpdir("nodef");
    std::fs::write(d.join("blob.xyz"), b"\0\xff\xfe\0\x01\x02").unwrap();
    let r = resolve_with(
        &req("blob.xyz", RawKind::Path, Some(&d)),
        &dbs(vec![firefox()], FF_DEFAULT),
    );
    assert!(r.refusal.is_none());
    assert!(r.default.is_none(), "{:?} {:?}", r.mime, r.default);
    assert!(!browser_renders("application/octet-stream"));
    assert!(browser_renders("image/svg+xml") && browser_renders("text/plain"));
    assert!(!browser_renders("image/tiff") && !browser_renders("video/x-matroska"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_desktop_default_beats_the_browser() {
    let d = tmpdir("deskwins");
    std::fs::write(d.join("a.pdf"), b"%PDF-1.4\n").unwrap();
    let viewer = entry(
        "v.desktop",
        "Viewer",
        "viewer %f",
        &["application/pdf"],
        false,
    );
    let r = resolve_with(
        &req("a.pdf", RawKind::Path, Some(&d)),
        &dbs(
            vec![firefox(), viewer],
            &format!("{FF_DEFAULT}application/pdf=v.desktop\n"),
        ),
    );
    assert_eq!(r.default.unwrap().title(), "Viewer");
    let _ = std::fs::remove_dir_all(&d);
}
