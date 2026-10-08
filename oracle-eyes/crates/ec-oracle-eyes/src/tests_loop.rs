// SPDX-License-Identifier: AGPL-3.0-only

//! The whole loop, end to end, against fake stages: fake frames and words
//! in, the compositor calls out. Each test here pins a defect seen in a live
//! headless run (debug.log evidence in the comments), so the jank stays
//! fixed.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::answer::{Ask, Confidence, Reply};
use crate::beacon::{Beacon, Eye};
use crate::choice::Choice;
use crate::config::Config;
use crate::daemon::{Daemon, Shown};
use crate::fault::{is_busy, Failure, Fault};
use crate::frame::{Frame, Region};
use crate::hud::Control;
use crate::ocr::{Line, Ocr, Word};
use crate::pipeline::{Grab, Pipeline};

const OUT: Region = Region {
    x: 0,
    y: 0,
    w: 1600,
    h: 900,
};

/// What the fake screen shows. Shared between the fake grabber and OCR, and
/// changed by the test mid-run.
#[derive(Default)]
struct Screen {
    /// Stands in for the pixels: a new value is a visibly changed screen.
    paint: u8,
    words: Vec<Word>,
    grab_error: Option<String>,
    grabs: usize,
    ocrs: usize,
}

type Shared = Rc<RefCell<Screen>>;

struct FakeGrab(Shared);

impl Grab for FakeGrab {
    fn grab(&mut self, region: Region) -> Result<Frame, String> {
        let mut s = self.0.borrow_mut();
        s.grabs += 1;
        if let Some(e) = &s.grab_error {
            return Err(e.clone());
        }
        Ok(Frame {
            width: 4,
            height: 4,
            stride: 16,
            pixels: vec![s.paint; 64],
            origin: region,
        })
    }
    fn output_regions(&self) -> Vec<Region> {
        vec![OUT]
    }
}

struct FakeOcr(Shared);

impl Ocr for FakeOcr {
    fn recognise(&mut self, _: &Frame) -> Result<Vec<Word>, String> {
        let mut s = self.0.borrow_mut();
        s.ocrs += 1;
        Ok(s.words.clone())
    }
}

/// How the fake model answers the next call.
#[derive(Clone)]
enum Says {
    Answer,
    Fail(&'static str),
    /// Answers after this long, unless cancelled first.
    Slow(Duration),
    /// Answers naming these lines in `focus` and this label as its choice.
    Picks(&'static [usize], &'static str),
}

#[derive(Default)]
struct Asked {
    script: VecDeque<Says>,
    /// The line texts of every call, as the model received them.
    prompts: Vec<Vec<String>>,
    cancelled: usize,
}

#[derive(Clone, Default)]
struct FakeAsk(Arc<Mutex<Asked>>);

impl Ask for FakeAsk {
    fn ask(
        &self,
        lines: &[Line],
        options: &[Choice],
        cancel: &AtomicBool,
    ) -> Result<Reply, Failure> {
        let says = {
            let mut a = self.0.lock().unwrap();
            a.prompts
                .push(lines.iter().map(|l| l.text.clone()).collect());
            a.script.pop_front().unwrap_or(Says::Answer)
        };
        let mut reply = Reply {
            headline: "Jupiter".into(),
            detail: "The largest planet.".into(),
            focus: lines.first().map(|l| vec![l.id]).unwrap_or_default(),
            choice: options.get(1).map(|o| o.label.clone()),
            confidence: Some(Confidence::High),
        };
        match says {
            Says::Answer => Ok(reply),
            Says::Picks(focus, choice) => {
                reply.focus = focus.to_vec();
                reply.choice = Some(choice.into());
                Ok(reply)
            }
            Says::Fail(e) => {
                let fault = if is_busy(e) {
                    Fault::Busy
                } else {
                    Fault::Reply
                };
                Err(Failure::new(fault, e))
            }
            Says::Slow(d) => {
                let until = Instant::now() + d;
                while Instant::now() < until {
                    if cancel.load(Ordering::Relaxed) {
                        self.0.lock().unwrap().cancelled += 1;
                        return Err(Failure::new(Fault::Cancelled, "cancelled"));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(reply)
            }
        }
    }
}

/// The compositor: grants the four annotation methods and `get_outputs`,
/// and records every call in order.
#[derive(Default)]
struct Ctl {
    calls: Vec<(String, Value)>,
    next: u64,
}

impl Control for Ctl {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.calls.push((method.to_owned(), params));
        match method {
            "annotation_create" => {
                self.next += 1;
                Ok(json!({"ok": true, "id": self.next}))
            }
            "annotation_update" | "annotation_destroy" | "annotation_clear" => {
                Ok(json!({"ok": true}))
            }
            "get_outputs" => Ok(json!([{"focused": true, "position": {"x": 0, "y": 0},
                                        "mode": {"width": 1600, "height": 900}, "scale": 1.0}])),
            other => panic!("oracle-eyes called {other}, which it is not granted"),
        }
    }
}

impl Ctl {
    /// Annotation calls only, in order: what the user saw happen.
    fn hud(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(|(m, _)| m.as_str())
            .filter(|m| m.starts_with("annotation_"))
            .collect()
    }
    fn creates(&self) -> Vec<&Value> {
        self.calls
            .iter()
            .filter(|(m, _)| m == "annotation_create")
            .map(|(_, p)| p)
            .collect()
    }
}

fn word(text: &str, x: i32, y: i32) -> Word {
    Word {
        text: text.into(),
        conf: 95.0,
        x,
        y,
        w: 10 * text.len() as i32,
        h: 16,
    }
}

/// Words for a page of lines, 25 px apart, starting at y = 100.
fn page(lines: &[&str]) -> Vec<Word> {
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let mut x = 20;
        for w in l.split_whitespace() {
            out.push(word(w, x, 100 + 25 * i as i32));
            x += 10 * w.len() as i32 + 8;
        }
    }
    out
}

fn quiz() -> Vec<Word> {
    page(&[
        "Which planet is the largest in our solar system?",
        "A) Mercury",
        "B) Jupiter",
        "C) Mars",
        "D) Venus",
    ])
}

struct Rig {
    d: Daemon,
    ctl: Ctl,
    screen: Shared,
    ask: FakeAsk,
    t0: Instant,
}

impl Rig {
    fn new(auto: bool) -> Rig {
        let screen: Shared = Rc::default();
        screen.borrow_mut().words = quiz();
        let ask = FakeAsk::default();
        let pipeline = Pipeline::with_parts(
            Config::default(),
            Box::new(FakeGrab(screen.clone())),
            Box::new(FakeOcr(screen.clone())),
            Arc::new(ask.clone()),
        );
        let t0 = Instant::now();
        let cfg = Config::default();
        let d = Daemon::new(
            pipeline,
            Beacon::disabled(),
            auto,
            cfg.fail_indicator_ms,
            cfg.settle_ms,
            t0,
        );
        Rig {
            d,
            ctl: Ctl::default(),
            screen,
            ask,
            t0,
        }
    }

    fn at(&self, ms: u64) -> Instant {
        self.t0 + Duration::from_millis(ms)
    }

    fn chord(&mut self, data: Value, ms: u64) {
        let now = self.at(ms);
        self.d.chord(&mut self.ctl, &data, now);
    }

    /// Step at `ms`, then wait (in real time) for any model call in flight
    /// to land, as the loop would.
    fn step(&mut self, ms: u64) {
        let now = self.at(ms);
        self.d.step(&mut self.ctl, now);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.d.busy() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
            self.d.step(&mut self.ctl, now);
        }
    }

    /// Step once at `ms` without waiting for a reply.
    fn poke(&mut self, ms: u64) {
        let now = self.at(ms);
        self.d.step(&mut self.ctl, now);
    }

    fn script(&self, says: Says) {
        self.ask.0.lock().unwrap().script.push_back(says);
    }

    fn asked(&self) -> usize {
        self.ask.0.lock().unwrap().prompts.len()
    }
}

fn select(x: i32, y: i32, w: i32, h: i32) -> Value {
    json!({"action": "annotation-select", "region": {"x": x, "y": y, "w": w, "h": h}})
}

fn act(action: &str) -> Value {
    json!({ "action": action })
}

// --- select mode (§2.1) ---------------------------------------------------

#[test]
fn a_select_answer_stays_up_until_dismissed() {
    // Live: `pipeline: select done total_ms=240 hold_ms=4000`, then
    // `annotation expired` four seconds later. §2.1: a select overlay
    // persists until dismissed, with no timer.
    let mut r = Rig::new(false);
    r.chord(select(0, 0, 800, 450), 0);
    assert_eq!(r.d.eye(), Eye::Think, "the eye thinks while the model does");
    r.step(10);
    assert_eq!(r.ctl.hud(), ["annotation_create"]);
    assert_eq!(r.d.shown(), Some(Shown::Held));
    assert_eq!(r.d.eye(), Eye::Off);

    r.step(10 * 60 * 1000);
    assert_eq!(r.ctl.hud(), ["annotation_create"], "nothing took it down");

    r.chord(act("annotation-dismiss"), 10 * 60 * 1000 + 1);
    assert_eq!(r.ctl.hud(), ["annotation_create", "annotation_clear"]);
    assert_eq!(r.d.shown(), None);
}

#[test]
fn a_select_answer_brackets_the_question_and_marks_the_pick() {
    let mut r = Rig::new(false);
    r.chord(select(0, 0, 800, 450), 0);
    r.step(10);
    let c = r.ctl.creates()[0].clone();
    assert_eq!(c["title"], "Jupiter");
    assert_eq!(c["pick"]["label"], "B");
    // Stem through D), padded: inside the selection, not the whole of it.
    let (y, h) = (c["y"].as_i64().unwrap(), c["h"].as_i64().unwrap());
    assert!((90..100).contains(&y), "anchor top {y}");
    assert!(y + h <= 100 + 25 * 4 + 16 + 8, "anchor bottom {}", y + h);
}

#[test]
fn a_select_failure_is_shown_where_the_user_pointed_then_goes() {
    let mut r = Rig::new(false);
    r.script(Says::Fail(
        "the model call failed: API Error: 529 overloaded",
    ));
    r.chord(select(10, 20, 300, 200), 0);
    r.step(10);
    let c = r.ctl.creates()[0].clone();
    assert_eq!(c["title"], "Couldn't answer");
    assert_eq!(c["kind"], "error");
    assert_eq!(
        c["text"],
        Fault::Busy.sentence(),
        "the class sentence, no raw detail"
    );
    assert_eq!((c["x"].clone(), c["w"].clone()), (json!(10), json!(300)));
    r.step(1499);
    assert_eq!(r.ctl.hud(), ["annotation_create"]);
    r.step(1510);
    assert_eq!(r.ctl.hud(), ["annotation_create", "annotation_clear"]);
}

#[test]
fn dismiss_while_the_model_thinks_drops_the_reply() {
    // Live (slow:5): dismiss pressed 1 s in was only read after the answer
    // was drawn — `annotation_create` then `annotation_clear` back to back.
    let mut r = Rig::new(false);
    r.script(Says::Slow(Duration::from_millis(300)));
    r.chord(select(0, 0, 800, 450), 0);
    r.poke(5);
    r.chord(act("annotation-dismiss"), 10);
    std::thread::sleep(Duration::from_millis(400));
    r.step(500);
    assert!(
        r.ctl.creates().is_empty(),
        "the stale answer never appeared"
    );
    assert_eq!(
        r.ask.0.lock().unwrap().cancelled,
        1,
        "the model call was killed"
    );
    assert_eq!(r.d.eye(), Eye::Off);
}

#[test]
fn a_second_select_replaces_the_first_question() {
    // One query in flight (§3.4): the first is cancelled, not queued.
    let mut r = Rig::new(false);
    r.script(Says::Slow(Duration::from_millis(300)));
    r.chord(select(0, 0, 800, 450), 0);
    r.chord(select(0, 0, 400, 450), 20);
    r.step(30);
    assert_eq!(r.ctl.creates().len(), 1);
    std::thread::sleep(Duration::from_millis(400));
    r.step(600);
    assert_eq!(r.ctl.creates().len(), 1, "the first reply was dropped");
    // The worker counts the cancel when it next looks at the flag, on its
    // own thread: wait for it rather than race it.
    let until = Instant::now() + Duration::from_secs(2);
    while r.ask.0.lock().unwrap().cancelled == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(r.ask.0.lock().unwrap().cancelled, 1);
}

// --- automatic mode (§2.2, §6) --------------------------------------------

#[test]
fn automatic_mode_reads_a_still_screen_once() {
    // Live: a static quiz was OCR'd every 600 ms forever (~250 ms of
    // tesseract per tick), each read dropped by the dedup gate.
    let mut r = Rig::new(false);
    r.chord(act("annotation-auto-toggle"), 0);
    assert_eq!(r.d.eye(), Eye::Watch);
    r.step(0);
    assert_eq!(
        r.screen.borrow().ocrs,
        0,
        "the first grab only starts the settle"
    );
    r.step(600);
    assert_eq!(r.screen.borrow().ocrs, 1, "two equal grabs: settled, read");
    assert_eq!(r.asked(), 1);
    assert_eq!(r.ctl.hud(), ["annotation_create"]);
    for t in (1200..60_000).step_by(600) {
        r.step(t);
    }
    assert_eq!(
        r.screen.borrow().ocrs,
        1,
        "unchanged pixels are never re-read"
    );
    assert_eq!(r.asked(), 1);
    assert_eq!(
        r.ctl.hud(),
        ["annotation_create", "annotation_clear"],
        "shown once, expired once"
    );
}

#[test]
fn automatic_mode_waits_for_a_changing_screen_to_settle() {
    let mut r = Rig::new(true);
    for (i, t) in (600..6000).step_by(600).enumerate() {
        r.screen.borrow_mut().paint = i as u8;
        r.step(t);
    }
    assert_eq!(r.screen.borrow().ocrs, 0, "nothing read mid-change");
    r.step(6000);
    r.step(6600);
    assert_eq!(r.screen.borrow().ocrs, 1);
}

#[test]
fn an_automatic_answer_expires_after_its_reading_time() {
    let mut r = Rig::new(true);
    r.step(600);
    r.step(1200);
    let Some(Shown::Until(t)) = r.d.shown() else {
        panic!("auto answers are timed, got {:?}", r.d.shown())
    };
    assert!(t > r.at(1200 + 3999));
}

#[test]
fn an_automatic_failure_is_shown_once_unanchored_and_backs_off() {
    // Live: `annotation_create x=0 y=0 w=1600 h=900 title=no answer` every
    // ~1.8 s for as long as auto stayed on — a full-screen strobe.
    let mut r = Rig::new(true);
    r.screen.borrow_mut().grab_error = Some("cannot reach the Wayland display".into());
    for t in (600..120_000).step_by(100) {
        r.step(t);
    }
    let creates = r.ctl.creates();
    assert_eq!(creates.len(), 1, "one failure, one panel");
    assert_eq!(creates[0]["title"], "Couldn't answer");
    assert_eq!(creates[0]["text"], Fault::Capture.sentence());
    assert!(
        creates[0].get("w").is_none(),
        "unanchored, not the whole screen"
    );
    let grabs = r.screen.borrow().grabs;
    assert!(grabs < 20, "retries back off, {grabs} grabs in two minutes");
}

#[test]
fn a_different_automatic_failure_is_shown_again() {
    let mut r = Rig::new(true);
    r.screen.borrow_mut().grab_error = Some("capture denied".into());
    r.step(600);
    r.step(3000);
    r.screen.borrow_mut().grab_error = Some("output went away".into());
    for t in (3000..60_000).step_by(300) {
        r.step(t);
    }
    assert_eq!(r.ctl.creates().len(), 2);
}

#[test]
fn an_empty_screen_in_automatic_mode_is_not_a_failure() {
    let mut r = Rig::new(true);
    r.screen.borrow_mut().words.clear();
    for t in (600..10_000).step_by(600) {
        r.step(t);
    }
    assert!(r.ctl.hud().is_empty(), "no `no readable text` panel");
    assert_eq!(
        r.screen.borrow().ocrs,
        1,
        "and the blank screen is read once"
    );
}

#[test]
fn a_model_failure_in_automatic_mode_is_shown_once() {
    let mut r = Rig::new(true);
    r.script(Says::Fail("the model reply was not JSON"));
    r.step(600);
    r.step(1200);
    assert_eq!(r.ctl.creates().len(), 1);
    assert_eq!(r.ctl.creates()[0]["title"], "Couldn't answer");
    assert_eq!(r.ctl.creates()[0]["text"], Fault::Reply.sentence());
}

#[test]
fn a_select_during_an_automatic_call_takes_over() {
    let mut r = Rig::new(true);
    r.script(Says::Slow(Duration::from_millis(300)));
    r.step(600);
    r.poke(1200);
    assert!(r.d.busy(), "auto pass in flight");
    r.chord(select(0, 0, 800, 450), 1210);
    r.step(1220);
    assert_eq!(r.ctl.creates().len(), 1);
    assert_eq!(r.d.shown(), Some(Shown::Held), "the select answer, held");
    // The worker counts the cancel when it next looks at the flag, on its
    // own thread: wait for it rather than race it.
    let until = Instant::now() + Duration::from_secs(2);
    while r.ask.0.lock().unwrap().cancelled == 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(r.ask.0.lock().unwrap().cancelled, 1);
}

#[test]
fn switching_automatic_mode_off_cancels_its_call() {
    let mut r = Rig::new(true);
    r.script(Says::Slow(Duration::from_millis(300)));
    r.step(600);
    r.poke(1200);
    r.chord(act("annotation-auto-toggle"), 1210);
    assert_eq!(r.d.eye(), Eye::Off);
    std::thread::sleep(Duration::from_millis(400));
    r.step(2000);
    assert!(r.ctl.creates().is_empty());
}

#[test]
fn nothing_scheduled_means_no_wake_ups() {
    let mut r = Rig::new(false);
    assert_eq!(r.d.wake_in(r.at(0)), None);
    r.chord(select(0, 0, 800, 450), 0);
    r.step(10);
    // A held answer with auto off: sleep until a chord.
    assert_eq!(r.d.wake_in(r.at(20)), None);
}

// --- redaction (§3.2) and the debug log -----------------------------------

#[test]
fn a_password_under_its_label_never_reaches_the_model() {
    // Live: `ocr: line id=5 ... text=hunter2` in debug.log and
    // `has_hunter2: true` in the model stub's call log. The line trace now
    // runs on these same redacted lines, so this also covers the log.
    let mut r = Rig::new(false);
    r.screen.borrow_mut().words = page(&[
        "Sign in to your account",
        "Password",
        "hunter2",
        "Password: hunter2",
        "Why does the login form reject my password?",
    ]);
    r.chord(select(0, 0, 800, 450), 0);
    r.step(10);
    let sent = r.ask.0.lock().unwrap().prompts[0].join("\n");
    assert!(!sent.contains("hunter2"), "model saw: {sent}");
    assert!(sent.contains("Password\n[REDACTED:secret]"), "{sent}");
}

// --- where the brackets go, on large questions ------------------------------
//
// Line boxes below are the ones tesseract reported in live headless runs of
// each layout (foot, Noto Sans Mono 13, 25 px pitch), split back into words
// at 10 px a character. Words of x-height letters only are set 5 px lower and
// shorter, as tesseract boxes them.

/// One OCR'd line: left, top, height, text.
type At = (i32, i32, i32, &'static str);

fn live(lines: &[At]) -> Vec<Word> {
    let mut out = Vec::new();
    for &(x, y, h, text) in lines {
        let mut col = 0;
        for w in text.split(' ') {
            if !w.is_empty() {
                let low = w.chars().all(|c| "acemnorsuvwxz,.".contains(c));
                let (dy, dh) = if low { (5, -5) } else { (0, 0) };
                out.push(Word {
                    text: w.into(),
                    conf: 91.0,
                    x: x + 10 * col,
                    y: y + dy,
                    w: 10 * w.len() as i32 - 2,
                    h: h + dh,
                });
            }
            col += w.len() as i32 + 1;
        }
    }
    out
}

/// Bracket edges: (x0, y0, x1, y1).
type Edges = (i64, i64, i64, i64);

/// The one answer panel a full-screen select produced: bracket edges and the
/// pick's label and top, if any.
fn bracket_of(screen: Vec<Word>, says: Says) -> (Edges, Option<(String, i64)>, Rig) {
    let mut r = Rig::new(false);
    r.screen.borrow_mut().words = screen;
    r.script(says);
    r.chord(select(0, 0, 1600, 900), 0);
    r.step(10);
    let c = r.ctl.creates()[0].clone();
    let n = |k: &str| c[k].as_i64().unwrap();
    let b = (n("x"), n("y"), n("x") + n("w"), n("y") + n("h"));
    let pick = c.get("pick").map(|p| {
        let (px, py, pw, ph) = (
            p["x"].as_i64().unwrap(),
            p["y"].as_i64().unwrap(),
            p["w"].as_i64().unwrap(),
            p["h"].as_i64().unwrap(),
        );
        assert!(
            px >= b.0 && py >= b.1 && px + pw <= b.2 && py + ph <= b.3,
            "the pick lies inside its bracket, or the compositor drops it"
        );
        (p["label"].as_str().unwrap().to_string(), py)
    });
    (b, pick, r)
}

/// The bracket holds every one of `lines`, with room to spare.
fn encloses(b: Edges, lines: &[At]) {
    for &(x, y, h, text) in lines {
        let right = i64::from(x) + 10 * text.len() as i64 - 2;
        assert!(
            b.0 < i64::from(x) && b.1 < i64::from(y) && b.2 > right && b.3 > i64::from(y + h),
            "bracket {b:?} misses {text:?} at ({x}, {y})"
        );
    }
}

/// The bracket takes in none of `lines`.
fn excludes(b: Edges, lines: &[At]) {
    for &(x, y, h, text) in lines {
        let right = i64::from(x) + 10 * text.len() as i64 - 2;
        let apart =
            right < b.0 || i64::from(x) > b.2 || i64::from(y + h) < b.1 || i64::from(y) > b.3;
        assert!(apart, "bracket {b:?} takes in {text:?} at ({x}, {y})");
    }
}

const PREAMBLE: &[At] = &[
    (
        17,
        25,
        18,
        "Chapter 4 review notes. Read the passage before answering",
    ),
    (
        16,
        75,
        18,
        "A small town council is deciding how to spend a one-time grant of",
    ),
    (
        17,
        100,
        18,
        "two million dollars. Half of the residents want a new library and",
    ),
    (
        17,
        125,
        18,
        "the other half want the main road repaired before winter arrives",
    ),
];
const PASSAGE: &[At] = &[
    (
        17,
        175,
        18,
        "The council hired an economist who estimated that the road repair",
    ),
    (
        16,
        200,
        14,
        "would save local businesses about three hundred thousand dollars a",
    ),
    (
        17,
        225,
        18,
        "year in delivery costs, while the library would cost eighty thousand",
    ),
    (
        17,
        250,
        18,
        "dollars a year to staff and maintain once it is built",
    ),
];
const LONG_STEM: &[At] = &[
    (
        17,
        300,
        18,
        "The grant cannot be split, and any money left unspent must be",
    ),
    (
        17,
        325,
        18,
        "returned to the state at the end of the fiscal year",
    ),
    (
        18,
        350,
        18,
        "Based only on the economist's figures, which option is cheaper over ten years?",
    ),
    (36, 375, 18, "A) Building the library"),
    (38, 400, 18, "B) Repairing the road"),
    (37, 425, 18, "C) Both cost exactly the same"),
    (37, 450, 17, "D) It cannot be determined"),
];

fn long_question() -> Vec<Word> {
    live(&[PREAMBLE, PASSAGE, LONG_STEM].concat())
}

#[test]
fn a_long_question_brackets_the_passage_the_model_named() {
    // Live: `anchor=Region { x: 11, y: 294, w: 790, h: 179 }` — only the
    // last paragraph of a three-paragraph question; the stem walk stopped at
    // the first blank line. Lines 5-8 are the passage the question is about.
    let (b, pick, _) = bracket_of(long_question(), Says::Picks(&[5, 6, 7, 8, 11], "B"));
    encloses(b, &[PASSAGE, LONG_STEM].concat());
    excludes(b, PREAMBLE);
    assert_eq!(pick, Some(("B".into(), 400)));
}

#[test]
fn a_long_question_with_no_passage_named_brackets_its_own_paragraph() {
    // An unrelated paragraph a blank line above a question looks the same
    // as the question's own passage; without the model naming it, it stays out.
    let (b, pick, _) = bracket_of(long_question(), Says::Picks(&[11], "B"));
    encloses(b, LONG_STEM);
    excludes(b, &[PREAMBLE, PASSAGE].concat());
    assert_eq!(pick, Some(("B".into(), 400)));
}

const WRAPPED: &[At] = &[
    (
        16,
        25,
        18,
        "Which statement about TCP congestion control is correct?",
    ),
    (
        16,
        75,
        18,
        "A) Slow start doubles the congestion window every round trip until it",
    ),
    (
        47,
        100,
        14,
        "reaches the slow start threshold or a loss is detected",
    ),
    (
        18,
        125,
        17,
        "B) Fast retransmit waits for a full retransmission timeout before it",
    ),
    (
        17,
        150,
        18,
        "resends a segment that the receiver reported as missing",
    ),
    (
        17,
        175,
        18,
        "C) The receive window and the congestion window are the same value",
    ),
    (
        47,
        200,
        18,
        "and are both advertised by the receiver in every ACK it sends",
    ),
    (47, 225, 14, "back to the sender."),
    (
        17,
        250,
        18,
        "D) Congestion avoidance halves the window on every ACK",
    ),
];

#[test]
fn options_that_wrap_to_the_margin_are_whole_options() {
    // Live: `options=A=L2,B=L4` — B's second line starts under its label,
    // not under its text, and ended the list: C and D were lost, the bracket
    // stopped at B, and the stem (a blank line up) was left out too.
    let (b, pick, r) = bracket_of(live(WRAPPED), Says::Picks(&[1], "C"));
    encloses(b, WRAPPED);
    assert_eq!(pick, Some(("C".into(), 175)));
    let c = r.ctl.creates()[0]["pick"].clone();
    assert_eq!(
        c["h"],
        225 + 14 - 175,
        "C's pick covers all three of its lines"
    );
}

const UNEVEN: &[At] = &[
    (
        16,
        25,
        18,
        "What does `len([1, 2, 3] * 2)` evaluate to in Python 3.12?",
    ),
    (16, 51, 16, "A) 3"),
    (18, 101, 16, "B) 6"),
    (17, 125, 17, "Q) `[2, 4, 6]`"),
    (17, 200, 18, "D) TypeError: can't multiply sequence by int"),
];

#[test]
fn a_misread_label_and_uneven_spacing_keep_the_list_whole() {
    // Live: tesseract read `C)` before inline code as `Q)`, which ended the
    // list at B: `options=A=L2,B=L3`, and the bracket stopped above C and D.
    let (b, pick, _) = bracket_of(live(UNEVEN), Says::Picks(&[1], "D"));
    encloses(b, UNEVEN);
    assert_eq!(pick, Some(("D".into(), 200)));
}

const FIRST_Q: &[At] = &[
    (18, 25, 18, "1. What is the capital of Australia?"),
    (46, 50, 18, "A) Sydney"),
    (48, 75, 17, "B) Canberra"),
    (47, 100, 17, "C) Melbourne"),
    (47, 125, 17, "D) Perth"),
];
const SECOND_Q: &[At] = &[
    (
        17,
        175,
        18,
        "2. Which gas makes up most of Earth's atmosphere?",
    ),
    (46, 201, 17, "A) Oxygen"),
    (48, 225, 17, "B) Carbon dioxide"),
    (47, 250, 18, "C) Nitrogen"),
    (47, 276, 17, "D) Argon"),
];

fn two_questions() -> Vec<Word> {
    live(&[FIRST_Q, SECOND_Q].concat())
}

#[test]
fn with_two_questions_the_pick_lands_on_the_one_the_model_answered() {
    // Live (model answering question 2, `focus=[6] choice=B`): the pick was
    // `B) Canberra` in question 1 and the bracket was question 1's — only the
    // longest list was ever offered, and a label alone cannot say which.
    let (b, pick, r) = bracket_of(two_questions(), Says::Picks(&[6], "B"));
    encloses(b, SECOND_Q);
    excludes(b, FIRST_Q);
    assert_eq!(pick, Some(("B".into(), 225)));
    let prompt = r.ask.0.lock().unwrap().prompts.len();
    assert_eq!(prompt, 1);

    let (b, pick, _) = bracket_of(two_questions(), Says::Picks(&[1], "B"));
    encloses(b, FIRST_Q);
    excludes(b, SECOND_Q);
    assert_eq!(pick, Some(("B".into(), 75)));
}

#[test]
fn with_two_questions_a_bare_label_marks_nothing() {
    // No focus line: "B" is in both lists. Pointing at the wrong one is
    // worse than pointing at neither.
    let (_, pick, _) = bracket_of(two_questions(), Says::Picks(&[], "B"));
    assert_eq!(pick, None);
}

const AT_THE_EDGE: &[At] = &[
    (16, 750, 14, "Which factor matters most here?"),
    (16, 775, 18, "A) Temperature drift in the sensor"),
    (18, 800, 17, "B) Mechanical shock to the reference"),
    (17, 825, 18, "C) Low battery voltage"),
    (17, 851, 17, "D) Operator error"),
];

#[test]
fn a_question_at_the_bottom_edge_is_bracketed_on_screen() {
    let mut words = live(&[
        (
            16,
            0,
            9,
            "the instrument in exactly the way the manufacturer specifies",
        ),
        (
            16,
            25,
            18,
            "should the calibration be repeated after transport? Pick one.",
        ),
    ]);
    words.extend(live(AT_THE_EDGE));
    let (b, pick, _) = bracket_of(words, Says::Picks(&[1], "B"));
    encloses(b, AT_THE_EDGE);
    assert!(b.0 >= 0 && b.3 <= 900, "inside the output: {b:?}");
    assert_eq!(pick, Some(("B".into(), 800)));
}

const LEFT_COLUMN: &[At] = &[
    (17, 25, 18, "The French Revolution began in 1789"),
    (17, 50, 18, "and reshaped politics across Europe."),
    (18, 75, 17, "Its causes included debt, famine and"),
    (17, 100, 18, "resentment of privilege."),
    (17, 150, 18, "Napoleon rose to power a decade later"),
    (17, 175, 18, "and crowned himself emperor in 1804."),
    (17, 225, 18, "The Congress of Vienna redrew the map"),
    (17, 250, 14, "after his defeat in 1815."),
];
const RIGHT_COLUMN: &[At] = &[
    (537, 25, 18, "Quiz: In which year did the"),
    (538, 50, 18, "French Revolution begin?"),
    (536, 101, 16, "A) 1776"),
    (538, 126, 16, "B) 1789"),
    (537, 151, 16, "C) 1804"),
    (537, 176, 16, "D) 1815"),
];

#[test]
fn a_two_column_page_brackets_the_question_column_only() {
    // Live: tesseract returned each row of both columns as one line
    // (`The French Revolution began in 1789 Quiz: In which year did the`),
    // no options were found, and the bracket was 800 px across both columns.
    // Words in tesseract's order: row by row, left column then right.
    let mut words = Vec::new();
    for y in [25, 50, 75, 100, 126, 150, 176, 225, 250] {
        let row =
            |c: &[At]| -> Vec<At> { c.iter().copied().filter(|l| (l.1 - y).abs() <= 1).collect() };
        words.extend(live(&row(LEFT_COLUMN)));
        words.extend(live(&row(RIGHT_COLUMN)));
    }
    let (b, pick, _) = bracket_of(words, Says::Picks(&[10], "B"));
    encloses(b, RIGHT_COLUMN);
    excludes(b, LEFT_COLUMN);
    assert_eq!(pick, Some(("B".into(), 126)));
}

const PICTURE_STEM: &[At] = &[(
    18,
    25,
    18,
    "Look at the diagram below. Which planet is shown?",
)];
const PICTURE_OPTIONS: &[At] = &[
    (16, 276, 16, "A) Mars"),
    (18, 301, 16, "B) Saturn"),
    (17, 326, 16, "C) Venus"),
    (17, 351, 17, "D) Neptune"),
];

#[test]
fn a_question_above_a_picture_keeps_its_stem() {
    // Live: `anchor=Region { x: 10, y: 270, w: 111, h: 104 }` — the options
    // alone; a picture's worth of space ended the stem walk.
    let (b, pick, _) = bracket_of(
        live(&[PICTURE_STEM, PICTURE_OPTIONS].concat()),
        Says::Picks(&[1], "B"),
    );
    encloses(b, &[PICTURE_STEM, PICTURE_OPTIONS].concat());
    assert_eq!(pick, Some(("B".into(), 301)));
}

const NOTES: &[At] = &[
    (
        18,
        25,
        18,
        "Release notes for version 2.4 of the app. The sync engine was rewritten",
    ),
    (
        17,
        50,
        18,
        "and conflicts are now resolved per field instead of per record",
    ),
];
const PLANETS: &[At] = &[
    (
        16,
        100,
        18,
        "Which planet is the largest in our solar system?",
    ),
    (16, 126, 17, "A) Mercury"),
    (18, 150, 18, "B) Jupiter"),
    (17, 176, 16, "C) Mars"),
    (17, 201, 16, "D) Venus"),
];
const FOOTER: &[At] = &[(
    18,
    250,
    18,
    "Footer: page 3 of 7. Next section covers billing and invoices",
)];

#[test]
fn a_selection_larger_than_the_question_brackets_the_question() {
    let (b, pick, _) = bracket_of(
        live(&[NOTES, PLANETS, FOOTER].concat()),
        Says::Picks(&[3], "B"),
    );
    encloses(b, PLANETS);
    excludes(b, &[NOTES, FOOTER].concat());
    assert_eq!(pick, Some(("B".into(), 150)));
}

#[test]
fn a_selection_edge_through_a_line_drops_the_sliver() {
    // Live: a select from y = 60 cut through line 2 of the notes, and OCR
    // read the strip as a line of its own: `id=1 x=17 y=60 w=298 h=4 text=E e`.
    let mut r = Rig::new(false);
    let mut words = vec![
        Word {
            text: "E".into(),
            conf: 40.0,
            x: 17,
            y: 60,
            w: 9,
            h: 4,
        },
        Word {
            text: "e".into(),
            conf: 40.0,
            x: 300,
            y: 60,
            w: 15,
            h: 4,
        },
    ];
    words.extend(live(&[PLANETS, FOOTER].concat()));
    r.screen.borrow_mut().words = words;
    r.script(Says::Picks(&[1], "B"));
    r.chord(select(0, 60, 900, 300), 0);
    r.step(10);
    let asked = r.ask.0.lock().unwrap().prompts[0].clone();
    assert_eq!(asked[0], "Which planet is the largest in our solar system?");
    let c = r.ctl.creates()[0].clone();
    assert_eq!(c["pick"]["y"], 150);
    assert!(
        c["y"].as_i64().unwrap() > 90,
        "the bracket starts at the question"
    );
}

#[test]
fn expand_with_nothing_selected_says_so_unanchored() {
    let mut r = Rig::new(false);
    r.chord(json!({ "action": "annotation-expand" }), 0);
    let c = r.ctl.creates()[0].clone();
    assert_eq!(c["kind"], "error");
    assert_eq!(c["text"], Fault::NothingSelected.sentence());
    assert!(c.get("x").is_none());
}
