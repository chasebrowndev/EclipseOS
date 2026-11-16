// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth-bench` (T-23 §6): `gen <corpus> <bytes>`, `hash <corpus> <bytes>`,
//! `run [--scale F] [--runs N] [--label L] [--baseline FILE] [--out FILE]`, `budgets`. `run` prints JSON to stdout; set
//! `APOTH_PERF=1` to make it exit non-zero on a violated headless budget. `--baseline FILE`
//! compares against a previous accepted result (T-23 §1 tolerances) and exits non-zero on a
//! regression; `--out FILE` also writes the JSON there (the results directory).

use std::io::Write;
use std::process::ExitCode;

use apoth_testkit::perf::budgets::{self, BUDGETS};
use apoth_testkit::perf::corpus::{self, Corpus};
use apoth_testkit::perf::{bench, from_json, regressions, to_json};

fn usage() -> ExitCode {
    eprintln!(
        "usage: apoth-bench gen|hash <corpus> <bytes> | run [--scale F] [--runs N] [--label L] [--baseline FILE] [--out FILE] | budgets"
    );
    ExitCode::from(2)
}

fn corpus_arg(args: &[String]) -> Option<(Corpus, usize)> {
    Some((
        Corpus::from_name(args.first()?)?,
        args.get(1)?.parse().ok()?,
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    match args.first().map(String::as_str) {
        Some("gen") => {
            let Some((c, n)) = corpus_arg(&args[1..]) else {
                return usage();
            };
            let _ = out.write_all(&corpus::generate(c, n));
        }
        Some("hash") => {
            let Some((c, n)) = corpus_arg(&args[1..]) else {
                return usage();
            };
            let _ = writeln!(
                out,
                "{} {n} {:016x}",
                c.name(),
                corpus::hash(&corpus::generate(c, n))
            );
        }
        Some("budgets") => {
            for b in BUDGETS {
                let _ = writeln!(
                    out,
                    "{} {:?} {} {} headless={} gate={}",
                    b.id, b.bound, b.limit, b.unit, b.headless, b.gate
                );
            }
        }
        Some("run") => {
            let (mut scale, mut runs, mut label) = (1.0f64, 10usize, String::from("local"));
            let (mut baseline, mut out_file) = (None::<String>, None::<String>);
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                let v = it.next();
                match (a.as_str(), v) {
                    ("--scale", Some(v)) => scale = v.parse().unwrap_or(1.0),
                    ("--runs", Some(v)) => runs = v.parse().unwrap_or(10),
                    ("--label", Some(v)) => label.clone_from(v),
                    ("--baseline", Some(v)) => baseline = Some(v.clone()),
                    ("--out", Some(v)) => out_file = Some(v.clone()),
                    _ => return usage(),
                }
            }
            let metrics = bench::run_all(scale, runs);
            let json = to_json(&label, scale, &metrics);
            let _ = out.write_all(json.as_bytes());
            if let Some(f) = &out_file
                && let Err(e) = std::fs::write(f, &json)
            {
                eprintln!("cannot write {f}: {:?}", e.kind());
                return ExitCode::FAILURE;
            }
            if let Some(f) = &baseline {
                let Ok(text) = std::fs::read_to_string(f) else {
                    eprintln!("cannot read baseline {f}");
                    return ExitCode::FAILURE;
                };
                let base = from_json(&text);
                if base.is_empty() {
                    eprintln!("baseline {f} holds no metrics");
                    return ExitCode::FAILURE;
                }
                let bad = regressions(&base, &metrics);
                for id in &bad {
                    eprintln!("regression: {id}");
                }
                if !bad.is_empty() {
                    return ExitCode::FAILURE;
                }
            }
            if budgets::enforced() {
                let v = budgets::violations(&metrics);
                for l in &v {
                    eprintln!("{l}");
                }
                if !v.is_empty() {
                    return ExitCode::FAILURE;
                }
            }
        }
        _ => return usage(),
    }
    ExitCode::SUCCESS
}
