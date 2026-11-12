// SPDX-License-Identifier: AGPL-3.0-only

//! A repeating timer subscription for iced without a tokio or smol feature:
//! one std thread sleeps and sends the wake-up instant.

use std::time::{Duration, Instant};

use iced::Subscription;

/// `Instant`s at `period`, until the subscription is dropped.
pub fn every(period: Duration) -> Subscription<Instant> {
    Subscription::run_with(period, |period| {
        let period = *period;
        iced::stream::channel(1, async move |mut out| {
            std::thread::spawn(move || loop {
                std::thread::sleep(period);
                // Full: the UI is behind, skip this tick. Closed: it is gone.
                if matches!(out.try_send(Instant::now()), Err(e) if e.is_disconnected()) {
                    return;
                }
            });
        })
    })
}
