// SPDX-License-Identifier: Apache-2.0
//! Golden cases: input bytes -> rendered, normalised action stream.
//! Format: P(text) X(hex c0) E(inter fin) C(private params inter fin; `;` or `:`
//! between params, empty = unset, `!` = overflow) S(kind:head, `~` partial, `T`
//! truncated) U(payload) N(st|bel|abort|cap|rej).

mod common;
use common::*;

fn g(input: &[u8], want: &str) {
    assert_eq!(render(&run(input)), want, "input {input:?}");
}

fn g_caps(input: &[u8], caps: (u32, u32, u32), want: &str) {
    let (ev, _) = run_with(Rec::with_caps(caps.0, caps.1, caps.2), &[input]);
    assert_eq!(render(&ev), want, "input {input:?}");
}

#[test]
fn text_and_c0() {
    g(b"abc", r#"P("abc")"#);
    g(b"a\nb\r\tc", r#"P("a") X(0a) P("b") X(0d) X(09) P("c")"#);
    g(b"a\x08b\x07", r#"P("a") X(08) P("b") X(07)"#);
    g(b"a\x7fb", r#"P("ab")"#);
    g(b"\x18\x1a", "X(18) X(1a)");
    g(b"", "");
}

#[test]
fn escapes() {
    g(b"\x1b7", "E(7)");
    g(b"\x1b(B", "E((B)");
    g(b"\x1b F", "E( F)");
    g(b"\x1b\\", r"E(\)");
    g(b"\x1b\x07c", "X(07) E(c)");
    g(b"\x1b !\"#$Ax", r#"P("x")"#);
    g(b"\x1b\x1b7", "E(7)");
    g(b"\x1b\x18a", r#"X(18) P("a")"#);
}

#[test]
fn csi_params() {
    g(b"\x1b[m", "C(m)");
    g(b"\x1b[1;31m", "C(1;31m)");
    g(b"\x1b[;m", "C(;m)");
    g(b"\x1b[0;m", "C(0;m)");
    g(b"\x1b[?1049h", "C(?1049h)");
    g(b"\x1b[38:2::1:2:3m", "C(38:2::1:2:3m)");
    g(b"\x1b[1;2 q", "C(1;2 q)");
    g(b"\x1b[99999999999m", "C(4294967295m)");
    g(b"\x1b[1\x7f2m", "C(12m)");
}

#[test]
fn csi_params_overflow_flag() {
    let mut input = b"\x1b[".to_vec();
    for i in 0..70 {
        input.extend_from_slice(format!("{i};").as_bytes());
    }
    input.push(b'm');
    let ev = run(&input);
    match &ev[..] {
        [
            Ev::Csi {
                params, overflow, ..
            },
        ] => {
            assert!(*overflow);
            assert_eq!(params.len(), 64);
            assert_eq!(params[63], Some(63));
        }
        other => panic!("{other:?}"),
    }
    // exactly 64 is not overflow
    let mut input = b"\x1b[".to_vec();
    for _ in 0..63 {
        input.extend_from_slice(b"1;");
    }
    input.extend_from_slice(b"1m");
    match &run(&input)[..] {
        [
            Ev::Csi {
                params, overflow, ..
            },
        ] => {
            assert!(!*overflow);
            assert_eq!(params.len(), 64);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn csi_abort_and_ignore() {
    g(b"\x1b[1\x07m", "X(07) C(1m)");
    g(b"\x1b[1\x18a", r#"X(18) P("a")"#);
    g(b"\x1b[1\x1a\x1a", "X(1a) X(1a)");
    g(b"\x1b[1\x1b[2m", "C(2m)");
    g(b"\x1b[?1?2hx", r#"P("x")"#);
    g(b"\x1b[ !\"#$mx", r#"P("x")"#);
    g(b"\x1b[1 2mx", r#"P("x")"#);
    g(b"\x1b[1\xc3\xa9", r#"P("é")"#);
}

#[test]
fn utf8() {
    g("é世😀".as_bytes(), r#"P("é世😀")"#);
    g(b"\xc3\x28", "P(\"\u{fffd}(\")");
    g(b"\x80", "P(\"\u{fffd}\")");
    g(b"\xe2\x82", ""); // carried
    g(b"\xe2\x82\n", "P(\"\u{fffd}\") X(0a)");
    g(b"\xed\xa0\x80", "P(\"\u{fffd}\u{fffd}\u{fffd}\")");
    g(b"\xf0\x9f\x98\x1b7", "P(\"\u{fffd}\") E(7)");
    g(b"\xff\xfe", "P(\"\u{fffd}\u{fffd}\")");
}

#[test]
fn c1_in_utf8_is_data() {
    assert_eq!(run("\u{85}".as_bytes()), vec![Ev::Print("\u{85}".into())]);
    // raw 0x9B is invalid UTF-8, never CSI
    g(b"\x9b1m", "P(\"\u{fffd}1m\")");
}

#[test]
fn osc() {
    g(b"\x1b]0;title\x07", r#"S(osc:0/title~) U("title") N(bel)"#);
    g(
        b"\x1b]8;;http://x\x1b\\",
        r#"S(osc:8/) U(";http://x") N(st)"#,
    );
    g(
        b"\x1b]1337;File=name=x:AAAA\x07",
        r#"S(osc:1337/File) U("File=name=x:AAAA") N(bel)"#,
    );
    g(b"\x1b]112\x07", "S(osc:112/~) N(bel)");
    g(b"\x1b]\x07", "S(osc:/~) N(bel)");
    g(b"\x1b]l title\x07", r#"S(osc:/) U("l title") N(bel)"#);
}

#[test]
fn osc_cap_abort_restart() {
    g_caps(
        b"\x1b]52;c;abcdefgh\x07x",
        (4, 9, 9),
        r#"S(osc:52/c) U("c;ab") N(cap) P("x")"#,
    );
    g_caps(
        b"\x1b]52;c;ab\x07",
        (4, 9, 9),
        r#"S(osc:52/c) U("c;ab") N(bel)"#,
    );
    g(
        b"\x1b]0;ab\x18c",
        r#"S(osc:0/ab~) U("ab") N(abort) X(18) P("c")"#,
    );
    g(b"\x1b]2;t\x1b[1m", r#"S(osc:2/t~) U("t") N(abort) C(1m)"#);
    g(
        b"\x1b]2;t\x1b\x1b[1m",
        r#"S(osc:2/t~) U("t") N(abort) C(1m)"#,
    );
}

#[test]
fn osc_head_truncates_at_32() {
    let a28 = "a".repeat(28);
    let mut input = b"\x1b]1337;".to_vec();
    input.extend_from_slice("a".repeat(40).as_bytes());
    input.push(7);
    g(
        &input,
        &format!("S(osc:1337/{a28}T) U(\"{}\") N(bel)", "a".repeat(40)),
    );
}

#[test]
fn dcs() {
    g(b"\x1bP1$qm\x1b\\", r#"S(dcs:[1]$q) U("m") N(st)"#);
    g(b"\x1bP>|abc\x1b\\", r#"S(dcs:>[]|) U("abc") N(st)"#);
    g(b"\x1bP1;2;3q\x1b\\", "S(dcs:[1;2;3]q) N(st)");
    g(b"\x1bP1\x18x", r#"S(dcs:[1]~) N(abort) X(18) P("x")"#);
    g(b"\x1bP1<q abc\x1b\\y", r#"P("y")"#); // DcsIgnore: silent
    g_caps(
        b"\x1bPqabcdef\x1b\\",
        (9, 3, 9),
        r#"S(dcs:[]q) U("abc") N(cap)"#,
    );
}

#[test]
fn apc_pm_sos() {
    g(b"\x1b_Gi=1;AAAA\x1b\\", r#"S(apc:G) U("Gi=1;AAAA") N(st)"#);
    g(b"\x1b_\x1b\\", "S(apc:~) N(st)");
    g(b"\x1b^secret\x1b\\x", r#"S(pm) N(rej) P("x")"#);
    g(b"\x1bXabc\x1b\\", "S(sos) N(rej)");
    g(b"\x1b_Gabc\x07d\x1b\\", r#"S(apc:G) U("Gabc\u{7}d") N(st)"#);
}

#[test]
fn string_payload_may_hold_high_bytes_and_c0() {
    let ev = run(b"\x1b_G\xff\x00\n\x1b\\");
    assert_eq!(
        ev,
        vec![
            Ev::Start(apoth_vt::StrKind::Apc, "G".into()),
            Ev::Put(b"G\xff\x00\n".to_vec()),
            Ev::End(apoth_vt::StrEnd::St),
        ]
    );
}

#[test]
fn parser_is_idle_after_complete_sequences() {
    let (_, p) = run_with(Rec::default(), &[b"\x1b[1m\x1b]0;x\x07abc"]);
    assert!(p.is_idle());
    let (_, p) = run_with(Rec::default(), &[b"\x1b[1"]);
    assert!(!p.is_idle());
    let (_, p) = run_with(Rec::default(), &[b"\xe2"]);
    assert!(!p.is_idle());
}
