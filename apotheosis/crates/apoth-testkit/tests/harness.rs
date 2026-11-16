// SPDX-License-Identifier: AGPL-3.0-only
//! The harness tests itself with `MiniTerm`, independent of `apoth-term`.

use apoth_testkit::fixture::Fixture;
use apoth_testkit::replay;
use apoth_testkit::runner::{Chunking, Driver, Outcome, bless, execute, run_fixture};
use apoth_testkit::selftest::MiniTerm;

fn fx(src: &str) -> Fixture {
    Fixture::parse(src, "t.kdl".into()).expect("fixture parses")
}

#[test]
fn a_correct_fixture_passes() {
    let f = fx(
        "fixture \"t\" {\n  size rows=2 cols=5\n  feed \"AB\"\n  grid {\n    row \"AB\"\n  }\n  cursor row=0 col=2\n}\n",
    );
    assert_eq!(run_fixture::<MiniTerm>(&f), Outcome::Pass);
}

#[test]
fn a_wrong_fixture_fails_with_a_readable_report() {
    let f = fx(
        "fixture \"t\" {\n  size rows=2 cols=5\n  feed \"AB\"\n  grid {\n    row \"AX\"\n  }\n}\n",
    );
    match run_fixture::<MiniTerm>(&f) {
        Outcome::Fail(r) => assert!(r.contains("AX") && r.contains("AB"), "{r}"),
        o => panic!("expected failure, got {o:?}"),
    }
}

#[test]
fn chunking_dependence_is_caught() {
    let src = "fixture \"t\" {\n  size rows=2 cols=8\n  feed #\"AB\\e[1mCD\"#\n}\n";
    let f = fx(src);
    // The sound terminal is chunk-invariant.
    let a = execute::<MiniTerm>(&f, Chunking::Whole).unwrap();
    let b = execute::<MiniTerm>(&f, Chunking::Size(1)).unwrap();
    assert_eq!(apoth_testkit::runner::first_difference(&a, &b), None);
    // The broken one is not, and the harness sees it.
    struct Broken(MiniTerm);
    impl Driver for Broken {
        fn create(r: usize, c: usize, _v: Option<&str>) -> Self {
            Broken(MiniTerm::broken(r, c))
        }
        fn feed(&mut self, b: &[u8], t: u64) {
            self.0.feed(b, t);
        }
        fn resize(&mut self, r: usize, c: usize) {
            self.0.resize(r, c);
        }
        fn take_replies(&mut self) -> Vec<u8> {
            self.0.take_replies()
        }
        fn take_events(&mut self) -> Vec<String> {
            self.0.take_events()
        }
        fn snapshot(&self) -> apoth_testkit::snapshot::Snapshot {
            self.0.snapshot()
        }
    }
    let w = execute::<Broken>(&f, Chunking::Whole).unwrap();
    let s = execute::<Broken>(&f, Chunking::Size(1)).unwrap();
    assert!(apoth_testkit::runner::first_difference(&w, &s).is_some());
    assert!(matches!(run_fixture::<Broken>(&f), Outcome::Fail(_)));
}

#[test]
fn bless_round_trip() {
    let f = fx("fixture \"t\" {\n  size rows=2 cols=5\n  feed \"hi\"\n}\n");
    let b = bless::<MiniTerm>(&f).unwrap();
    assert_eq!(b.expect.grid.as_deref(), Some(&["hi".to_string()][..]));
    assert_eq!(run_fixture::<MiniTerm>(&b), Outcome::Pass);
    let again = Fixture::parse(&b.to_kdl(), "t.kdl".into()).unwrap();
    assert_eq!(again.expect, b.expect);
}

#[test]
fn replay_is_chunking_invariant() {
    let rec = replay::parse("apothrec 0 rows=3 cols=6\nd 0 61620d0a6364\nr 5 4 8\nd 9 6566\n")
        .expect("recording parses");
    let w = replay::play::<MiniTerm>(&rec, Chunking::Whole).snapshot();
    let s = replay::play::<MiniTerm>(&rec, Chunking::Size(1)).snapshot();
    assert_eq!(w, s);
    assert_eq!(replay::parse(&replay::serialize(&rec)).unwrap(), rec);
}
