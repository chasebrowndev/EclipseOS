// SPDX-License-Identifier: AGPL-3.0-only
//! T-18 rules owned by `apoth-term`: the reply-injection rule (§2), the sequence policy's
//! deny path (§3, §11 "denied means no observable effect and no reply"), string budgets
//! (§5), and the shell-integration state the paste guard reads (§4, T-14 §1).

use std::time::Instant;

use apoth_term::shell::BlockState;
use apoth_term::{Term, TermConfig, deny};

fn term_with(deny_bits: u32) -> Term {
    Term::new(TermConfig {
        rows: 4,
        cols: 20,
        deny: deny_bits,
        ..TermConfig::default()
    })
}

fn feed(t: &mut Term, s: &str) {
    t.feed(s.as_bytes(), Instant::now());
}

/// Everything observable: visible text, title, palette, colours, events and replies.
type Observed = (Vec<String>, String, Vec<(u8, u8, u8)>, Vec<String>, Vec<u8>);

fn observe(t: &mut Term) -> Observed {
    let rows = (0..t.rows()).map(|y| t.row_text(y)).collect();
    let pal = (0..=255u8).map(|i| t.palette(i)).collect::<Vec<_>>();
    let mut ev = t.events_ref().render();
    let (fg, bg, cur) = t.colors();
    ev.push(format!("colors {fg:?} {bg:?} {cur:?}"));
    let link = t.grid().link_at(0, 0).map(|(_, u)| u).unwrap_or_default();
    ev.push(format!("link {link}"));
    let replies = t.replies().take_all();
    (rows, t.title().to_string(), pal, ev, replies)
}

/// One sequence per policy class with its deny bit (T-18 §3).
const CORPUS: &[(&str, u32)] = &[
    ("\x1b]2;owned\x07", deny::TITLE_SET),
    ("\x1b]0;owned\x07", deny::TITLE_SET),
    ("\x1b[21t", deny::TITLE_REPORT),
    ("\x1b[20t", deny::TITLE_REPORT),
    ("\x1b[14t", deny::WINDOW_REPORT),
    ("\x1b[16t", deny::WINDOW_REPORT),
    ("\x1b[18t", deny::WINDOW_REPORT),
    ("\x1b[19t", deny::WINDOW_REPORT),
    ("\x1b]4;1;#123456\x07", deny::COLOUR),
    ("\x1b]4;1;?\x07", deny::COLOUR),
    ("\x1b]10;?\x07", deny::COLOUR),
    ("\x1b]11;#010203\x07", deny::COLOUR),
    ("\x1b]9;hello\x07", deny::NOTIFICATION),
    ("\x1b]777;notify;t;b\x07", deny::NOTIFICATION),
    ("\x1b]99;;body\x07", deny::NOTIFICATION),
    (
        "\x1b]8;;https://example.org\x07L\x1b]8;;\x07",
        deny::HYPERLINK,
    ),
    ("\x1b]52;c;aGk=\x07", deny::CLIPBOARD_WRITE),
    ("\x1b]52;c;?\x07", deny::CLIPBOARD_READ),
    ("\x1b]52;p;aGk=\x07", deny::PRIMARY),
    ("\x1b]52;p;?\x07", deny::PRIMARY),
];

#[test]
fn denied_means_no_effect_and_no_reply() {
    for (seq, bit) in CORPUS {
        let mut base = term_with(*bit);
        let before = observe(&mut base);
        feed(&mut base, seq);
        let mut after = observe(&mut base);
        // A denied hyperlink still prints its text; only the link itself must be absent.
        if *bit == deny::HYPERLINK {
            assert_eq!(after.0[0].trim_end(), "L");
            after.0 = before.0.clone();
        }
        assert_eq!(before, after, "denied {seq:?} had an effect");

        let mut allowed = term_with(0);
        let b2 = observe(&mut allowed);
        feed(&mut allowed, seq);
        assert_ne!(
            b2,
            observe(&mut allowed),
            "allowed {seq:?} had no effect at all"
        );
    }
}

#[test]
fn deny_word_is_installed_between_feeds() {
    let mut t = term_with(0);
    feed(&mut t, "\x1b]2;one\x07");
    t.set_deny(deny::TITLE_SET);
    assert_eq!(t.deny_bits(), deny::TITLE_SET);
    feed(&mut t, "\x1b]2;two\x07");
    assert_eq!(t.title(), "one");
}

/// Closed reply grammar for every query (T-18 §2): nothing echoes request bytes except as
/// hex of a known table name.
fn reply_is_closed(r: &[u8]) -> bool {
    let s = match std::str::from_utf8(r) {
        Ok(s) => s,
        Err(_) => return false,
    };
    // Every reply unit is ESC-introduced and made of a closed alphabet: digits, `;`, `?`,
    // `$`, `+`, `>`, `=`, `/`, `:`, uppercase hex, the final bytes we generate, and the
    // fixed `rgb:` colour form.
    s.split('\x1b').filter(|u| !u.is_empty()).all(|u| {
        u.chars().all(|c| {
            c.is_ascii_digit()
                || matches!(
                    c,
                    ';' | '?'
                        | '$'
                        | '+'
                        | '>'
                        | '='
                        | '/'
                        | ':'
                        | '['
                        | ']'
                        | '\\'
                        | '\x07'
                        | ' '
                        | '|'
                        | '('
                        | ')'
                        | '.'
                        | '-'
                )
                || c.is_ascii_uppercase()
                || c.is_ascii_lowercase()
        }) && !u.contains("pwn")
            && !u.contains("rm ")
    })
}

#[test]
fn reply_injection_corpus() {
    let hostile = "pwn;rm -rf ~\x07";
    let queries = [
        format!("\x1b[>{hostile}c"),
        "\x1b[c".to_string(),
        "\x1b[>c".to_string(),
        "\x1b[5n".to_string(),
        "\x1b[6n".to_string(),
        "\x1b[?6n".to_string(),
        "\x1b[>0q".to_string(),
        "\x1b[?2004$p".to_string(),
        "\x1b[4$p".to_string(),
        "\x1b[?996n".to_string(),
        "\x1b[14t".to_string(),
        "\x1b[18t".to_string(),
        "\x1bP$qpwn;rm -rf\x1b\\".to_string(),
        "\x1bP$qm\x1b\\".to_string(),
        "\x1bP$q q\x1b\\".to_string(),
        // XTGETTCAP: a hostile non-hex name and an unknown hex name.
        "\x1bP+qpwn\x1b\\".to_string(),
        "\x1bP+q70776e\x1b\\".to_string(),
        "\x1bP+q544e\x1b\\".to_string(),
        "\x1b]4;1;?\x07".to_string(),
        "\x1b]10;?;?;?\x07".to_string(),
        format!("\x1b]4;{hostile}"),
        format!("\x1b]10;{hostile}"),
        "\x1b[21t".to_string(),
        "\x1b[20t".to_string(),
        "\x05".to_string(),
    ];
    for q in &queries {
        let mut t = term_with(0);
        feed(&mut t, "\x1b]2;pwn;rm -rf ~\x07\x1b]1;pwn\x07");
        feed(&mut t, q);
        let r = t.replies().take_all();
        assert!(
            reply_is_closed(&r),
            "{q:?} replied {:?}",
            String::from_utf8_lossy(&r)
        );
    }
}

#[test]
fn title_reports_are_always_empty() {
    let mut t = term_with(0);
    feed(
        &mut t,
        "\x1b]2;secret title\x07\x1b]1;secret icon\x07\x1b[21t\x1b[20t",
    );
    assert_eq!(t.replies().take_all(), b"\x1b]l\x1b\\\x1b]L\x1b\\");
}

#[test]
fn enq_answerback_is_empty() {
    let mut t = term_with(0);
    feed(&mut t, "\x05\x05");
    assert!(t.replies().is_empty());
}

#[test]
fn decrqss_answers_from_state_never_from_the_request() {
    let mut t = term_with(0);
    // A request that is not a known setting is refused without echoing it.
    feed(&mut t, "\x1bP$q31mX;rm -rf\x1b\\");
    assert_eq!(t.replies().take_all(), b"\x1bP0$r\x1b\\");
    // ESC inside the request cancels it: nothing is answered, the CSI applies normally.
    feed(&mut t, "\x1bP$q\x1b[1mX\x1b\\");
    assert!(t.replies().is_empty());
}

#[test]
fn window_control_ops_are_consumed() {
    let mut t = term_with(0);
    for op in [
        "\x1b[3;10;10t",
        "\x1b[8;5;5t",
        "\x1b[5t",
        "\x1b[2t",
        "\x1b[9;1t",
        "\x1b[10;1t",
    ] {
        feed(&mut t, op);
    }
    assert!(t.replies().is_empty());
    assert_eq!(
        (t.rows(), t.cols()),
        (4, 20),
        "a resize request changed nothing"
    );
}

#[test]
fn font_osc_50_is_denied_without_reply() {
    let mut t = term_with(0);
    feed(&mut t, "\x1b]50;?\x07\x1b]50;#+1\x07");
    assert!(t.replies().is_empty());
    assert!(t.events_ref().is_empty());
}

#[test]
fn osc_666_and_unknown_777_subcommands_are_ignored() {
    let mut t = term_with(0);
    feed(
        &mut t,
        "\x1b]666;anything\x07\x1b]777;preexec\x07\x1b]777;notify\x07\x1b]777;foo;bar\x07",
    );
    assert!(t.replies().is_empty());
    assert!(t.events_ref().notifications.is_empty());
    // A 777 notification is sanitised (controls and bidi overrides stripped) and capped.
    feed(&mut t, "\x1b]777;notify;a\u{202e}b;c\x01d\x07");
    assert_eq!(
        t.events_ref().notifications.back().map(String::as_str),
        Some("ab: cd")
    );
    let long = "x".repeat(10_000);
    feed(&mut t, &format!("\x1b]777;notify;{long}\x07"));
    assert_eq!(
        t.events_ref().notifications.len(),
        1,
        "an over-cap 777 is dropped whole"
    );
}

#[test]
fn string_budgets() {
    let mut t = term_with(0);
    // Title over 4096 bytes is dropped whole (T-03 §5).
    feed(&mut t, &format!("\x1b]2;{}\x07", "t".repeat(5000)));
    assert_eq!(t.title(), "");
    // Hyperlink URIs over 2083 bytes are not attached.
    feed(
        &mut t,
        &format!("\x1b]8;;https://{}\x07X\x1b]8;;\x07", "a".repeat(2100)),
    );
    assert!(t.grid().link_at(0, 0).is_none());
    // OSC 52 over the configured cap is dropped.
    let mut small = Term::new(TermConfig {
        osc52_max: 16,
        ..TermConfig::default()
    });
    small.feed(
        format!("\x1b]52;c;{}\x07", "QUFB".repeat(20)).as_bytes(),
        Instant::now(),
    );
    assert!(small.events_ref().clipboard.is_empty());
    // The reply queue is bounded (64 KiB) and counts its drops.
    let mut q = term_with(0);
    let many = "\x1b[6n".repeat(20_000);
    feed(&mut q, &many);
    assert!(q.replies().len_bytes() <= apoth_term::events::REPLY_CAP);
    assert!(q.replies().dropped() > 0);
    // SOS / PM / APC payloads are skipped in constant memory and print nothing.
    let mut s = term_with(0);
    feed(
        &mut s,
        &format!(
            "\x1bX{}\x1b\\\x1b^{}\x1b\\ok",
            "z".repeat(100_000),
            "y".repeat(100_000)
        ),
    );
    assert_eq!(s.row_text(0).trim_end(), "ok");
}

#[test]
fn osc52_read_raises_a_request_and_never_replies() {
    let mut t = term_with(0);
    feed(&mut t, "\x1b]52;;?\x07");
    assert_eq!(t.events_ref().clip_read.as_deref(), Some("c"));
    assert!(t.replies().is_empty());
}

#[test]
fn osc52_write_follows_config() {
    let mut off = Term::new(TermConfig {
        osc52_write: false,
        ..TermConfig::default()
    });
    off.feed(b"\x1b]52;c;aGk=\x07", Instant::now());
    assert!(off.events_ref().clipboard.is_empty());
    let mut on = term_with(0);
    feed(&mut on, "\x1b]52;c;aGk=\x07");
    let c = on.events_ref().clipboard.back().unwrap();
    assert_eq!(c.data, b"hi");
    assert!(c.program_origin);
}

#[test]
fn bracketed_paste_counts_only_after_a_prompt() {
    let mut t = term_with(0);
    feed(&mut t, "\x1b[?2004h");
    assert!(
        !t.bracketed_in_force(),
        "2004 without a following prompt does not count"
    );
    feed(&mut t, "\x1b]133;A\x07$ ");
    assert!(t.bracketed_in_force());
    assert!(t.at_prompt());
    // Set again by other output: no longer counts until the next prompt.
    feed(&mut t, "\x1b[?2004l\x1b[?2004h");
    assert!(!t.bracketed_in_force());
    feed(&mut t, "\x1b]133;B\x07ls\r\n\x1b]133;C\x07");
    assert!(!t.at_prompt());
}

#[test]
fn blocks_record_zones_status_and_cmdline() {
    let mut t = term_with(0);
    feed(
        &mut t,
        "\x1b]7;file://h/tmp\x07\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C;cmdline=make\x07out1\r\nout2\r\n\x1b]133;D;2\x07",
    );
    let b = t.blocks().last().expect("one block");
    assert_eq!(b.state, BlockState::Done);
    assert_eq!(b.exit, Some(2));
    assert_eq!(b.cmdline.as_deref(), Some("make"));
    assert_eq!(b.cwd.as_deref(), Some("file://h/tmp"));
    assert!(b.prompt.is_some() && b.input.is_some() && b.output.is_some() && b.end.is_some());
    let g = t.grid();
    for y in 0..3 {
        assert_eq!(g.row_block(y), b.id, "row {y} carries the block id");
    }
    assert_ne!(g.row_flags(0) & apoth_grid::flags::PROMPT_START, 0);
}

#[test]
fn continuation_prompt_keeps_the_primary_block() {
    let mut t = term_with(0);
    feed(
        &mut t,
        "\x1b]133;A\x07$ \x1b]133;B\x07for x\r\n\x1b]133;A;k=s\x07> \x1b]133;B\x07end\r\n\x1b]133;C\x07",
    );
    assert_eq!(t.blocks().len(), 1);
    assert_eq!(t.blocks().last().unwrap().state, BlockState::Running);
    assert_eq!(t.grid().row_block(1), t.blocks().last().unwrap().id);
}

#[test]
fn block_marks_are_chunking_independent() {
    let s = "\x1b]133;A;k=i\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C;cmdline_url=ls%20-l\x07x\r\n\x1b]133;D;0\x07\x1b]133;L\x07";
    let mut whole = term_with(0);
    feed(&mut whole, s);
    let mut bytes = term_with(0);
    let now = Instant::now();
    for b in s.as_bytes() {
        bytes.feed(std::slice::from_ref(b), now);
    }
    let a: Vec<_> = whole
        .blocks()
        .iter()
        .map(|b| (b.id, b.state, b.exit, b.cmdline.clone()))
        .collect();
    let b: Vec<_> = bytes
        .blocks()
        .iter()
        .map(|b| (b.id, b.state, b.exit, b.cmdline.clone()))
        .collect();
    assert_eq!(a, b);
    assert_eq!(whole.events_ref().render(), bytes.events_ref().render());
}
