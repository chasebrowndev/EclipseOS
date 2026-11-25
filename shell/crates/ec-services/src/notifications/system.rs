// SPDX-License-Identifier: AGPL-3.0-only
//! System toasts and the session-restore offer (ADR 0127).
//!
//! A *system* toast is one the shell or the compositor originates. It reaches
//! the toast stack directly (like the config-error notice in `compositor.rs`),
//! never over D-Bus, and is the only kind that can carry a yes/no
//! [question](super::Notification::question). Three rules keep an app from
//! passing for one:
//!
//! * the D-Bus path builds every notification as [`Class::App`] with no
//!   question, and no hint can change that;
//! * ids at or above [`SYSTEM_ID_BASE`] are ours: an app's `replaces_id` or
//!   `CloseNotification` naming one is ignored (the server also never counts
//!   up into the range);
//! * a button press on a system toast is answered here, through a channel,
//!   and never becomes an `ActionInvoked` signal.
//!
//! A question never expires, holds its clock while the pointer is over it (the
//! stack's existing hover rule) and, if it leaves the screen unanswered, falls
//! to history as a non-live, unread entry (`Store::fall_to_history`). Security
//! prompts are not toasts at all; they stay compositor-drawn trusted UI.
//!
//! The driver at the bottom is the session-restore flow: arm the sentinel at
//! login, offer a reopen after an unclean end, relaunch on a yes, then keep the
//! manifest current from the compositor's window events (`restore.rs`).

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use ec_ipc::{Client, EventKind};
use serde_json::Value;

use super::history::{now_secs, Shared};
use super::{Action, Class, Event, Mode, Notification, Share, Urgency};
use crate::restore::{self, Decision, Manifest, Paths};

/// Ids from here up belong to the shell and the compositor. The D-Bus side
/// counts up from 1 and never reaches this range.
pub const SYSTEM_ID_BASE: u32 = 0xFFFF_0000;

/// The affirmative button's action id.
pub const ANSWER_YES: &str = "yes";
/// The negative button's action id.
pub const ANSWER_NO: &str = "no";

/// The restore offer's id.
const OFFER_ID: u32 = SYSTEM_ID_BASE + 1;

/// A system question waiting for its answer.
#[derive(Debug)]
pub(crate) struct Pending {
    pub n: Notification,
    pub reply: Sender<Reply>,
}

/// How a system question ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reply {
    /// The human pressed this action.
    Answered(String),
    /// It left the screen without an answer.
    Unanswered,
}

/// A yes/no question as a toast. Never expires.
pub(crate) fn question(id: u32, summary: &str, body: &str, yes: &str, no: &str) -> Notification {
    Notification {
        id,
        app_name: "Eclipse".into(),
        summary: summary.into(),
        body: body.into(),
        urgency: Urgency::Normal,
        actions: vec![
            Action {
                key: ANSWER_YES.into(),
                label: yes.into(),
            },
            Action {
                key: ANSWER_NO.into(),
                label: no.into(),
            },
        ],
        expires_in: None,
        group: "eclipse".into(),
        class: Class::System,
        question: true,
        ..Notification::default()
    }
}

/// "Eclipse ended unexpectedly. Reopen N apps?"
pub(crate) fn restore_offer(apps: usize) -> Notification {
    let body = if apps == 1 {
        "Reopen 1 app?".to_owned()
    } else {
        format!("Reopen {apps} apps?")
    };
    let mut n = question(
        OFFER_ID,
        "Eclipse ended unexpectedly",
        &body,
        "Reopen",
        "Start fresh",
    );
    n.category = "eclipse.session-restore".into();
    n
}

/// Block while toasts are kept off the screen by a fullscreen surface
/// (`ui.toasts-over-fullscreen` off). A question is held, not dropped.
pub(crate) fn hold_while_fullscreen(share: &Share, poll: Duration) {
    while share.fullscreen_suppressed() {
        std::thread::sleep(poll);
    }
}

/// Put a system question in front of the human. The receiver gets the reply.
///
/// The mode is respected unless `through_quiet` is set: in Quiet the question
/// goes straight to history as an unanswered entry, in DND it is dropped, and
/// either way the receiver says [`Reply::Unanswered`]. With `through_quiet` it
/// is shown once as in Normal, and the mode applies again afterwards
/// (`ui.restore-offer-in-quiet`).
pub(crate) fn ask(
    events: &Sender<Event>,
    store: &Shared,
    mut n: Notification,
    through_quiet: bool,
) -> Receiver<Reply> {
    let (tx, rx) = mpsc::channel();
    n.class = Class::System;
    n.question = true;
    n.expires_in = None;
    let mode = if through_quiet {
        Mode::Normal
    } else {
        store.lock().mode()
    };
    match mode {
        Mode::Dnd => {
            let _ = tx.send(Reply::Unanswered);
        }
        Mode::Quiet => {
            let id = n.id;
            let mut s = store.lock();
            s.post_system(n, tx);
            s.fall_to_history(id, now_secs());
            s.save();
        }
        Mode::Normal => {
            let id = n.id;
            store.lock().post_system(n.clone(), tx);
            if events.send(Event::Posted(Box::new(n))).is_err() {
                // Nobody draws toasts any more: it is unanswered, and kept.
                let mut s = store.lock();
                s.fall_to_history(id, now_secs());
                s.save();
            }
        }
    }
    rx
}

// --- The driver ---------------------------------------------------------------

/// Between connection attempts while the compositor is not there.
const RETRY: Duration = Duration::from_secs(5);
/// How often the offer's hold checks whether fullscreen has ended.
const HOLD_POLL: Duration = Duration::from_millis(250);

pub(super) fn spawn(events: Sender<Event>, store: Shared, share: Share) {
    let _ = std::thread::Builder::new()
        .name("eclipse-session-restore".into())
        .spawn(move || run(&events, &store, &share));
}

struct Driver {
    paths: Paths,
    begun: bool,
    installed: Vec<crate::apps::Entry>,
    scanned: Option<Instant>,
    last: Option<Manifest>,
    clock: restore::Debouncer,
}

fn run(events: &Sender<Event>, store: &Shared, share: &Share) {
    let Some(paths) = Paths::standard() else {
        return;
    };
    let mut d = Driver {
        paths,
        begun: false,
        installed: Vec::new(),
        scanned: None,
        last: None,
        clock: restore::Debouncer::default(),
    };
    loop {
        if let Ok(client) = Client::connect() {
            if !d.begun && !begin(&mut d, events, store, share) {
                // No compositor token: nothing to snapshot on this host.
                return;
            }
            // `Ok(true)` is the compositor's `session-end`: a clean exit, so
            // the sentinel and manifest go. A dropped socket without it is a
            // crash (or a restart) and leaves both for the next login.
            if matches!(follow(&mut d, client), Ok(true)) {
                restore::end_clean(&d.paths);
                return;
            }
        }
        std::thread::sleep(RETRY);
    }
}

/// Arm the sentinel and, after an unclean end, make the offer. False when
/// there is no compositor socket to identify this login by.
fn begin(d: &mut Driver, events: &Sender<Event>, store: &Shared, share: &Share) -> bool {
    let Some(token) = ec_ipc::default_socket().and_then(|p| restore::login_token(&p)) else {
        return false;
    };
    let Ok(start) = restore::begin(&d.paths, &token) else {
        // The state dir is not writable: no sentinel, so no promise to keep.
        return false;
    };
    d.begun = true;
    let manifest = match start {
        restore::Start::Unclean { .. } => Manifest::load(&d.paths.manifest),
        _ => Manifest::default(),
    };
    match restore::decide(start, manifest) {
        Decision::Nothing => {}
        Decision::SetAside => restore::set_aside(&d.paths),
        Decision::Offer(m) => offer(m, events, store, share),
    }
    true
}

/// Ask, and act on the answer. The writer stays off until it is answered, so
/// the old manifest is never overwritten while the offer stands.
fn offer(m: Manifest, events: &Sender<Event>, store: &Shared, share: &Share) {
    hold_while_fullscreen(share, HOLD_POLL);
    let reply = ask(
        events,
        store,
        restore_offer(m.unique_ids().len()),
        share.restore_offer_in_quiet(),
    )
    .recv();
    if let Ok(Reply::Answered(key)) = reply {
        if key == ANSWER_YES {
            let done = restore::relaunch_now(&m);
            eprintln!(
                "ec-session-restore: reopened {} app(s), skipped {}",
                done.launched.len(),
                done.skipped.len()
            );
        }
    }
}

/// Keep the manifest current until the connection fails (`Err`) or the
/// compositor says it is exiting cleanly (`Ok(true)`).
fn follow(d: &mut Driver, mut client: Client) -> Result<bool, ec_ipc::Error> {
    client.subscribe(&[EventKind::Window, EventKind::Workspace, EventKind::SessionEnd])?;
    d.clock.touch(Instant::now());
    loop {
        while let Some(ev) = client.poll_event()? {
            if ev.kind == EventKind::SessionEnd {
                return Ok(true);
            }
            d.clock.touch(Instant::now());
        }
        let now = Instant::now();
        if d.clock.due(now) {
            snapshot(d, &mut client)?;
            d.clock.done(now);
        }
        wait(
            &client,
            if d.clock.pending() {
                HOLD_POLL
            } else {
                Duration::from_secs(3600)
            },
        );
    }
}

fn snapshot(d: &mut Driver, client: &mut Client) -> Result<(), ec_ipc::Error> {
    let windows = client.call("get_windows", Value::Null)?;
    let outputs = client.call("get_outputs", Value::Null)?;
    // Rescan desktop entries only when needed: first time, or a window named
    // an app the cache does not know and the cache is old.
    let stale = d.scanned.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
    let known = |d: &Driver| Manifest::from_replies(&windows, &outputs, &d.installed);
    let mut m = known(d);
    let wanted = windows.as_array().map_or(0, |w| {
        w.iter()
            .filter(|x| x.get("app_id").is_some_and(|a| a.is_string()))
            .count()
    });
    if d.scanned.is_none() || (stale && m.windows.len() < wanted) {
        d.installed = crate::apps::scan(None);
        d.scanned = Some(Instant::now());
        m = known(d);
    }
    if d.last.as_ref() != Some(&m) {
        if let Err(e) = m.save(&d.paths.manifest) {
            eprintln!("ec-session-restore: could not save the manifest: {e}");
        } else {
            d.last = Some(m);
        }
    }
    Ok(())
}

/// Sleep until the socket is readable or `timeout` passes.
fn wait(client: &Client, timeout: Duration) {
    let mut pfd = libc::pollfd {
        fd: client.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one valid pollfd on the stack, count 1.
    unsafe {
        libc::poll(&mut pfd, 1, ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifications::history::Store;

    fn setup() -> (Sender<Event>, Receiver<Event>, Shared) {
        let (tx, rx) = mpsc::channel();
        (tx, rx, Shared::new(Store::default()))
    }

    #[test]
    fn the_offer_is_a_system_question_that_never_expires() {
        let n = restore_offer(3);
        assert_eq!(n.class, Class::System);
        assert!(n.question);
        assert_eq!(n.expires_in, None);
        assert!(n.id >= SYSTEM_ID_BASE);
        assert_eq!(n.summary, "Eclipse ended unexpectedly");
        assert_eq!(n.body, "Reopen 3 apps?");
        let keys: Vec<&str> = n.actions.iter().map(|a| a.key.as_str()).collect();
        assert_eq!(keys, [ANSWER_YES, ANSWER_NO]);
        assert_eq!(restore_offer(1).body, "Reopen 1 app?");
    }

    #[test]
    fn an_app_notification_is_never_system_or_a_question() {
        // The D-Bus builder sets these from nothing the client sends; the
        // default is the safe value, and the hint parsers have no such field.
        let n = Notification::default();
        assert_eq!(n.class, Class::App);
        assert!(!n.question);
    }

    #[test]
    fn an_answer_reaches_the_asker_and_the_question_is_not_kept() {
        let (tx, rx, store) = setup();
        let reply = ask(&tx, &store, restore_offer(2), false);
        let Ok(Event::Posted(n)) = rx.try_recv() else {
            panic!("the question must be posted");
        };
        assert!(store.lock().is_system(n.id));
        assert!(store.lock().answer_system(n.id, ANSWER_YES));
        assert_eq!(reply.recv().unwrap(), Reply::Answered("yes".into()));
        assert!(!store.lock().is_system(n.id));
        assert!(store.lock().list().is_empty());
    }

    #[test]
    fn a_key_the_question_did_not_offer_is_swallowed_and_changes_nothing() {
        let (tx, _rx, store) = setup();
        let reply = ask(&tx, &store, restore_offer(2), false);
        // The press belongs to the system path (true: do not forward) but is
        // not an answer.
        assert!(store.lock().answer_system(OFFER_ID, "rm -rf"));
        assert!(store.lock().is_system(OFFER_ID));
        assert!(reply.try_recv().is_err());
        // Nor can a D-Bus-style invoke reach it: the entry is not live.
        assert!(!store.lock().invoke(OFFER_ID, ANSWER_YES));
    }

    #[test]
    fn an_unanswered_question_falls_to_history_as_a_non_live_unread_entry() {
        let (tx, _rx, store) = setup();
        let reply = ask(&tx, &store, restore_offer(2), false);
        assert!(store.lock().fall_to_history(OFFER_ID, 7));
        assert_eq!(reply.recv().unwrap(), Reply::Unanswered);
        let s = store.lock();
        let list = s.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].summary, "Eclipse ended unexpectedly");
        assert!(!list[0].live);
        assert!(list[0].actions.is_empty());
        assert_eq!(s.unread(), 1);
        assert!(!s.is_system(OFFER_ID));
    }

    #[test]
    fn when_the_stack_is_gone_every_pending_question_is_kept() {
        let (tx, rx, store) = setup();
        drop(rx);
        let reply = ask(&tx, &store, restore_offer(2), false);
        // `ask` found no listener and kept it already.
        assert_eq!(reply.recv().unwrap(), Reply::Unanswered);
        assert_eq!(store.lock().list().len(), 1);
        assert!(!store.lock().fall_all_to_history(1));
        // And one still pending when the stack goes is kept by `fall_all`.
        let (tx, _rx, store) = setup();
        let _reply = ask(&tx, &store, restore_offer(2), false);
        assert!(store.lock().fall_all_to_history(1));
        assert_eq!(store.lock().list().len(), 1);
    }

    #[test]
    fn quiet_keeps_the_question_in_history_and_dnd_drops_it() {
        let (tx, rx, store) = setup();
        store.lock().set_mode(Mode::Quiet);
        let reply = ask(&tx, &store, restore_offer(2), false);
        assert!(rx.try_recv().is_err());
        assert_eq!(reply.recv().unwrap(), Reply::Unanswered);
        assert_eq!(store.lock().list().len(), 1);

        let (tx, rx, store) = setup();
        store.lock().set_mode(Mode::Dnd);
        let reply = ask(&tx, &store, restore_offer(2), false);
        assert!(rx.try_recv().is_err());
        assert_eq!(reply.recv().unwrap(), Reply::Unanswered);
        assert!(store.lock().list().is_empty());
    }

    #[test]
    fn the_offer_is_shown_once_through_quiet_and_dnd_when_allowed() {
        for mode in [Mode::Quiet, Mode::Dnd] {
            let (tx, rx, store) = setup();
            store.lock().set_mode(mode);
            let _reply = ask(&tx, &store, restore_offer(2), true);
            assert!(matches!(rx.try_recv(), Ok(Event::Posted(_))), "{mode:?}");
            assert!(store.lock().is_system(OFFER_ID));
            // The mode still holds for everything else.
            assert_eq!(store.lock().mode(), mode);
        }
    }

    #[test]
    fn a_question_is_held_while_fullscreen_and_shown_when_it_ends() {
        let share = Share::default();
        share.set_fullscreen(true);
        let s2 = share.clone();
        let ender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            s2.set_fullscreen(false);
        });
        let t = Instant::now();
        hold_while_fullscreen(&share, Duration::from_millis(5));
        assert!(t.elapsed() >= Duration::from_millis(50));
        ender.join().unwrap();
        // With the setting on nothing is held at all.
        share.set_fullscreen(true);
        share.set_over_fullscreen(true);
        let t = Instant::now();
        hold_while_fullscreen(&share, Duration::from_millis(5));
        assert!(t.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn the_range_is_above_anything_the_bus_side_hands_out() {
        const { assert!(SYSTEM_ID_BASE > u32::MAX / 2) };
    }
}
