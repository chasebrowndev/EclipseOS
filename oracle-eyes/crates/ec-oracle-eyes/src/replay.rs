// SPDX-License-Identifier: AGPL-3.0-only

//! Offline replay of the question detector over the captured OCR corpus
//! (plan step 1). Ignored by default; run from `oracle-eyes/` with
//! `cargo test replay_corpus -- --ignored --nocapture`.
//!
//! The corpus is text only, so each line is synthesised with no real
//! geometry: a uniform pitch, and a blank line is a paragraph gap. The
//! numbers measure the text rules and the grouping, not the layout. Output
//! is counts only; no screen text is printed or logged.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::choice::{choose_of, detect, multi_of};
use crate::ocr::Line;
use crate::redact::redact;

const PITCH: i32 = 20;
const CHAR_W: i32 = 8;

/// Lines of one OCR dump, as the pipeline would see them after redaction.
fn synth(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut y = 0;
    let mut para = false;
    for raw in text.lines() {
        let t = redact(raw.trim());
        if t.trim().is_empty() {
            y += PITCH;
            para = true;
            continue;
        }
        out.push(Line {
            id: out.len() + 1,
            x: 0,
            y,
            w: CHAR_W * t.chars().count() as i32,
            h: PITCH,
            para,
            text: t,
        });
        y += PITCH;
        para = false;
    }
    out
}

#[derive(Default)]
struct Bucket {
    frames: usize,
    with_question: usize,
    options: usize,
    /// Frames whose detected question count equals `nq`, and the ones with
    /// fewer or more. Only frames that carry `nq`.
    nq_known: usize,
    nq_exact: usize,
    nq_under: usize,
    nq_over: usize,
    /// Frames the corpus marks multi-select (`multi` or `multiq > 0`).
    multi_expected: usize,
    /// Of those, frames with a `(Choose N)` / "select all" line parsed.
    choose_parsed: usize,
    /// Of those, frames where some detected group parsed as multi-select.
    group_multi: usize,
}

fn count(e: &Value, key: &str) -> Option<u64> {
    e.get(key).and_then(Value::as_u64)
}

#[test]
#[ignore = "needs the local OCR corpus at ~/.cache/oe-corpus"]
fn replay_corpus() {
    let Ok(home) = std::env::var("HOME") else {
        println!("replay: HOME unset, skipped");
        return;
    };
    let dir = std::path::PathBuf::from(home).join(".cache/oe-corpus");
    let Ok(index) = std::fs::read_to_string(dir.join("index.json")) else {
        println!("replay: no corpus at {}, skipped", dir.display());
        return;
    };
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(&index) else {
        panic!("replay: index.json is not an array");
    };

    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    let mut total = Bucket::default();
    for e in &entries {
        let Some(f) = e.get("f").and_then(Value::as_str) else {
            continue;
        };
        let stem = f.trim_end_matches(".png");
        let Ok(text) = std::fs::read_to_string(dir.join("txt").join(format!("{stem}.txt"))) else {
            continue;
        };
        let tag = e.get("tag").and_then(Value::as_str).unwrap_or("?");
        let nq = count(e, "nq");
        let multiq = count(e, "multiq").unwrap_or(0);
        let multi = e.get("multi").and_then(Value::as_bool).unwrap_or(false);
        let key = format!("{tag} nq={}", if nq.is_some() { "y" } else { "-" });

        let lines = synth(&text);
        let opts = detect(&lines);
        let groups: BTreeSet<usize> = opts.iter().map(|o| o.group).collect();
        let found = groups.len();
        let expected = multi || multiq > 0;
        let choose = lines.iter().any(|l| choose_of(&l.text).is_some());
        let group_multi = groups.iter().any(|&g| multi_of(&lines, &opts, g).is_some());

        for b in [buckets.entry(key).or_default(), &mut total] {
            b.frames += 1;
            b.options += opts.len();
            if found > 0 {
                b.with_question += 1;
            }
            if let Some(nq) = nq {
                b.nq_known += 1;
                match (found as u64).cmp(&nq) {
                    std::cmp::Ordering::Equal => b.nq_exact += 1,
                    std::cmp::Ordering::Less => b.nq_under += 1,
                    std::cmp::Ordering::Greater => b.nq_over += 1,
                }
            }
            if expected {
                b.multi_expected += 1;
                if choose {
                    b.choose_parsed += 1;
                }
                if group_multi {
                    b.group_multi += 1;
                }
            }
        }
    }

    let show = |name: &str, b: &Bucket| {
        println!(
            "{name:<24} frames={:<4} question={:<4} options={:<5} nq_known={:<4} exact={:<4} under={:<4} over={:<4} multi_expected={:<3} choose_parsed={:<3} group_multi={}",
            b.frames, b.with_question, b.options, b.nq_known, b.nq_exact, b.nq_under,
            b.nq_over, b.multi_expected, b.choose_parsed, b.group_multi
        );
    };
    show("TOTAL", &total);
    for (k, b) in &buckets {
        show(k, b);
    }
}
