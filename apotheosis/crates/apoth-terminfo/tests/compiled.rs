// SPDX-License-Identifier: Apache-2.0
//! Independent checks of the compiled output: a minimal reader written separately from
//! the writer, plus dev-only `tic -x` / `infocmp` comparisons (T-19 §2, §5).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use apoth_terminfo::{
    BOOL_NAMES, NUM_NAMES, NumFormat, STR_NAMES, Value, Variant, compile, entry, source,
};

#[derive(Debug, Default, PartialEq)]
struct Parsed {
    names: String,
    bools: Vec<String>,
    nums: BTreeMap<String, u32>,
    strs: BTreeMap<String, Vec<u8>>,
}

struct R<'a>(&'a [u8], usize);
impl R<'_> {
    fn u16(&mut self) -> usize {
        let v = u16::from_le_bytes([self.0[self.1], self.0[self.1 + 1]]);
        self.1 += 2;
        usize::from(v)
    }
    fn num(&mut self, w: usize) -> Option<u32> {
        let v = if w == 2 {
            let v = u32::from(u16::from_le_bytes([self.0[self.1], self.0[self.1 + 1]]));
            if v == 0xFFFF { u32::MAX } else { v }
        } else {
            u32::from_le_bytes(self.0[self.1..self.1 + 4].try_into().unwrap())
        };
        self.1 += w;
        (v != u32::MAX).then_some(v)
    }
    fn align(&mut self) {
        if self.1 % 2 == 1 {
            self.1 += 1;
        }
    }
}

fn cstr(t: &[u8], off: usize) -> Vec<u8> {
    let end = t[off..].iter().position(|&b| b == 0).unwrap();
    t[off..off + end].to_vec()
}

fn parse(b: &[u8]) -> Parsed {
    let mut r = R(b, 0);
    let w = match r.u16() {
        0x011A => 2,
        0x021E => 4,
        m => panic!("magic {m:#x}"),
    };
    let (nsz, nb, nn, ns, tsz) = (r.u16(), r.u16(), r.u16(), r.u16(), r.u16());
    let mut p = Parsed::default();
    let names = cstr(b, r.1);
    p.names = String::from_utf8(names).unwrap();
    r.1 += nsz;
    for i in 0..nb {
        if b[r.1 + i] == 1 {
            p.bools.push(BOOL_NAMES[i].to_owned());
        }
    }
    r.1 += nb;
    r.align();
    for name in NUM_NAMES.iter().take(nn) {
        if let Some(v) = r.num(w) {
            p.nums.insert((*name).to_owned(), v);
        }
    }
    let offs: Vec<usize> = (0..ns).map(|_| r.u16()).collect();
    let table = &b[r.1..r.1 + tsz];
    for (i, o) in offs.iter().enumerate() {
        if *o < 0xFFFE {
            p.strs.insert(STR_NAMES[i].to_owned(), cstr(table, *o));
        }
    }
    r.1 += tsz;
    if r.1 >= b.len() {
        return p;
    }
    r.align();
    let (eb, en, es, eo, et) = (r.u16(), r.u16(), r.u16(), r.u16(), r.u16());
    let eb_at = r.1;
    r.1 += eb;
    r.align();
    let enums: Vec<Option<u32>> = (0..en).map(|_| r.num(w)).collect();
    let eoffs: Vec<usize> = (0..eo).map(|_| r.u16()).collect();
    let etab = &b[r.1..r.1 + et];
    assert_eq!(r.1 + et, b.len(), "trailing bytes");
    // Value strings come first; names start after the last value string.
    let vals: Vec<Vec<u8>> = eoffs[..es].iter().map(|o| cstr(etab, *o)).collect();
    let names_base: usize = vals.iter().map(|v| v.len() + 1).sum();
    let nm = |i: usize| String::from_utf8(cstr(etab, names_base + eoffs[es + i])).unwrap();
    assert_eq!(eo, es + eb + en + es);
    for i in 0..eb {
        assert_eq!(b[eb_at + i], 1);
        p.bools.push(nm(i));
    }
    for (i, v) in enums.iter().enumerate() {
        p.nums.insert(nm(eb + i), v.unwrap());
    }
    for (i, v) in vals.into_iter().enumerate() {
        p.strs.insert(nm(eb + en + i), v);
    }
    p
}

fn expected(v: Variant) -> Parsed {
    let e = entry(v);
    let mut p = Parsed {
        names: e.names.clone(),
        ..Parsed::default()
    };
    for c in &e.caps {
        match c.value {
            Value::Bool => p.bools.push(c.name.to_owned()),
            Value::Num(n) => {
                p.nums.insert(c.name.to_owned(), n);
            }
            Value::Str(s) => {
                p.strs.insert(c.name.to_owned(), s.as_bytes().to_vec());
            }
        }
    }
    p.bools.sort();
    p
}

fn normalised(mut p: Parsed) -> Parsed {
    p.bools.sort();
    p
}

#[test]
fn reader_round_trips_both_formats_and_variants() {
    for v in Variant::ALL {
        for f in [None, Some(NumFormat::Extended32), Some(NumFormat::Legacy16)] {
            let bytes = match compile(&entry(v), f) {
                Ok(b) => b,
                Err(_) => {
                    // pairs#65536 (and direct's colors#0x1000000) do not fit 16 bits.
                    assert!(f == Some(NumFormat::Legacy16));
                    continue;
                }
            };
            assert_eq!(normalised(parse(&bytes)), expected(v), "{v:?} {f:?}");
        }
    }
}

#[test]
fn format_choice_follows_number_size() {
    // pairs#65536 exceeds 32767, so both variants need the 32-bit layout.
    let i = compile(&entry(Variant::Indexed), None).unwrap();
    let d = compile(&entry(Variant::Direct), None).unwrap();
    assert_eq!(&i[..2], &0x021Eu16.to_le_bytes());
    assert_eq!(&d[..2], &0x021Eu16.to_le_bytes());
}

#[test]
fn tree_layout() {
    let t = apoth_terminfo::tree().unwrap();
    let paths: Vec<String> = t.iter().map(|(p, _)| p.display().to_string()).collect();
    assert_eq!(
        paths,
        [
            "x/xterm-apotheosis",
            "78/xterm-apotheosis",
            "x/xterm-apotheosis-direct",
            "78/xterm-apotheosis-direct"
        ]
    );
}

fn tool(name: &str) -> bool {
    Command::new(name).arg("-V").output().is_ok()
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("apoth-terminfo-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn skip(tool_name: &str) -> bool {
    if tool(tool_name) {
        return false;
    }
    assert!(
        std::env::var("APOTH_REQUIRE_INFOCMP").as_deref() != Ok("1"),
        "{tool_name} required but absent"
    );
    eprintln!("skipping: {tool_name} not found");
    true
}

#[test]
fn tic_output_is_byte_identical() {
    if skip("tic") {
        return;
    }
    // T-19 §2: byte-identical to `tic -x`; skipped only when `tic` is absent.
    for v in Variant::ALL {
        let dir = tmpdir("tic");
        let src = dir.join("e.ti");
        std::fs::write(&src, source(&entry(v))).unwrap();
        let out = dir.join("db");
        let st = Command::new("tic")
            .args(["-x", "-o"])
            .arg(&out)
            .arg(&src)
            .output()
            .unwrap();
        assert!(
            st.status.success(),
            "{}",
            String::from_utf8_lossy(&st.stderr)
        );
        let name = v.term_name();
        let first = name.chars().next().unwrap();
        let theirs = [
            out.join(first.to_string()).join(name),
            out.join(format!("{:02x}", first as u8)).join(name),
        ]
        .into_iter()
        .find(|p| p.exists())
        .expect("tic wrote nothing");
        let theirs = std::fs::read(theirs).unwrap();
        assert_eq!(normalised(parse(&theirs)), expected(v), "tic output {v:?}");
        let ours = compile(&entry(v), None).unwrap();
        assert!(
            ours == theirs,
            "{v:?}: compiled bytes differ from tic -x (T-19 §2)"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn infocmp_matches_source_emitter() {
    if skip("infocmp") {
        return;
    }
    for v in Variant::ALL {
        let dir = tmpdir("infocmp");
        apoth_terminfo::write_tree(&dir).unwrap();
        let out = Command::new("infocmp")
            .args(["-x", "-1", "-A"])
            .arg(&dir)
            .arg(v.term_name())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        let body: String = text
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(body, source(&entry(v)), "{v:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
