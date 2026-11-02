// SPDX-License-Identifier: AGPL-3.0-only
//! Live model checks. `#[ignore]`d: each one spends a real `claude -p` call, so
//! they run only on request:
//!
//! ```text
//! cargo test -p ec-oracle-eyes tests_live -- --ignored --nocapture
//! ```
//!
//! `OE_LIVE_MODEL` picks the model (default: the configured command's own).

use std::sync::atomic::AtomicBool;

use crate::answer::{Answerer, Input};
use crate::config::Config;
use crate::ocr::Line;

fn screen(rows: &[&str]) -> Input {
    let lines = rows
        .iter()
        .enumerate()
        .map(|(i, t)| Line {
            id: i + 1,
            text: (*t).to_string(),
            x: 20,
            y: 30 * i as i32,
            w: 900,
            h: 24,
            para: false,
        })
        .collect();
    Input {
        lines,
        ..Input::default()
    }
}

/// Spelling-insensitive: mu / μ, subscripts, p-hat / phat / p̂, spaces.
fn norm(s: &str) -> String {
    let mut t = s.to_lowercase();
    for (from, to) in [
        ("\u{3bc}", "mu"),
        ("p\u{302}", "phat"),
        ("p-hat", "phat"),
        ("p hat", "phat"),
        ("\u{2081}", "1"),
        ("\u{2082}", "2"),
        ("_1", "1"),
        ("_2", "2"),
        ("\u{2265}", ">="),
    ] {
        t = t.replace(from, to);
    }
    t.retain(|c| !c.is_whitespace());
    t
}

#[test]
#[ignore = "calls the live model"]
fn a_two_part_word_problem_gets_both_alternative_hypotheses() {
    // Clinic 1 waits longer (18 > 12 min) and vaccinates more (150/200 = 0.75
    // vs 150/250 = 0.60): the answers are mu1 > mu2 and phat1 > phat2.
    let input = screen(&[
        "A health department compares the flu-shot programs of two clinics.",
        "Clinic 1: 200 patients, 150 vaccinated, mean wait 18 minutes.",
        "Clinic 2: 250 patients, 150 vaccinated, mean wait 12 minutes.",
        "(a) The department claims Clinic 1 has the longer true mean wait.",
        "Write the alternative hypothesis Ha for the means, mu1 versus mu2.",
        "(b) It also claims Clinic 1 vaccinates a larger share of its patients.",
        "Write the alternative hypothesis Ha for the proportions, p-hat1 versus p-hat2.",
    ]);
    let mut cfg = Config::default();
    if let Ok(m) = std::env::var("OE_LIVE_MODEL") {
        cfg.models = vec![m];
    }
    let model = cfg.models.first().cloned();
    let reply = Answerer::with_model(&cfg, model.as_deref())
        .ask(&input, None, &AtomicBool::new(false))
        .expect("the model answered");
    for a in &reply.answers {
        eprintln!(
            "answer: {:?} | {:?} | {:?}",
            a.headline, a.detail, a.confidence
        );
    }
    let all = norm(
        &reply
            .answers
            .iter()
            .map(|a| format!("{} {}", a.headline, a.detail))
            .collect::<Vec<_>>()
            .join(" "),
    );
    assert!(all.contains("mu1>mu2"), "means answer missing: {all}");
    assert!(
        all.contains("phat1>phat2"),
        "proportions answer missing: {all}"
    );
}
