// SPDX-License-Identifier: AGPL-3.0-only
//! Debug builds only: `EC_TOASTS_PREVIEW=restore` puts the session-restore
//! offer on screen without an unclean end, so the system-question card can be
//! screenshotted under a headless session. It builds the same notification the
//! service's system path does, and is compiled out of release builds.

use ec_services::notifications::{Action, Class, Notification, Urgency};

use crate::app::{App, Toast};

pub fn apply(app: &mut App) {
    if std::env::var("EC_TOASTS_PREVIEW").as_deref() != Ok("restore") {
        return;
    }
    let n = Notification {
        id: u32::MAX - 7,
        app_name: "Eclipse".into(),
        summary: "Eclipse ended unexpectedly".into(),
        body: "Reopen 4 apps where they were?".into(),
        urgency: Urgency::Normal,
        actions: vec![
            Action {
                key: "yes".into(),
                label: "Reopen".into(),
            },
            Action {
                key: "no".into(),
                label: "Start fresh".into(),
            },
        ],
        expires_in: None,
        class: Class::System,
        question: true,
        group: "eclipse-system".into(),
        ..Notification::default()
    };
    app.toasts.push(Toast::new(n));
}
