//! Settings > Account's notification switches (docs/NOTIFICATIONS_CONTRACT.md
//! sections 4 and 6), one component for the desktop's Account page and the
//! browser's: the switch for the whole account, and under it the channels
//! and watched notes, which only mean anything while it is on. With it off
//! they are shown unticked and cannot be changed; what they hold is kept,
//! and comes back when the switch does.

use rinch::prelude::*;

use crate::state::AppStore;

/// The one line beside the notifications switch (section 6).
const NOTIFICATIONS_LINE: &str = "People who share notes with you can notify you; Pimble Cloud cannot read what they say.";

/// What the two channels below the switch do (section 4, wave 2).
const CHANNELS_LINE: &str = "When none of your devices sees a notice within 15 minutes, Pimble Cloud emails you who wrote and in which share, never what they said, at most once an hour. A browser you allowed shows notices even when Pimble is closed in it.";

/// The same place with notifications off.
const CHANNELS_OFF_LINE: &str = "Notifications are off, so Pimble Cloud neither emails you nor notifies your browsers, and no watched note tells you it changed.";

const EMAIL_LABEL: &str = "Email me what I miss";
const WATCHES_LABEL: &str = "Tell me when others change notes I watch";

/// The line under the channels, for whether notifications are on.
pub(crate) fn channels_line(on: bool) -> &'static str {
    if on { CHANNELS_LINE } else { CHANNELS_OFF_LINE }
}

/// Whether a switch under the main one shows ticked: only what is on while
/// notifications are.
pub(crate) fn shown_ticked(notifications_on: bool, value: bool) -> bool {
    notifications_on && value
}

/// The switches. `push_label` names web push as this build sees it (this
/// browser, or the person's browsers from the desktop).
pub(crate) fn render(__scope: &mut RenderScope, store: AppStore, push_label: &'static str) -> NodeHandle {
    rsx! {
        div {
            style: "display: flex; flex-direction: column; gap: 10px;",
            Checkbox {
                label: "Notifications",
                checked_fn: move || store.notifications_on.get(),
                onchange: move || crate::inbox::set_on(store, !untracked(|| store.notifications_on.get())),
            }
            div {
                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                {NOTIFICATIONS_LINE}
            }
            if store.notifications_on.get() {
                Checkbox {
                    label: EMAIL_LABEL,
                    checked_fn: move || shown_ticked(store.notifications_on.get(), store.notify_email.get()),
                    onchange: move || crate::inbox::set_email(store, !untracked(|| store.notify_email.get())),
                }
                Checkbox {
                    label: push_label,
                    checked_fn: move || shown_ticked(store.notifications_on.get(), store.notify_push.get()),
                    onchange: move || crate::inbox::set_push(store, !untracked(|| store.notify_push.get())),
                }
            }
            if !store.notifications_on.get() {
                Checkbox { label: EMAIL_LABEL, checked: false, disabled: true }
                Checkbox { label: push_label, checked: false, disabled: true }
            }
            div {
                style: "font-size: 12px; color: var(--rinch-color-dimmed);",
                {move || channels_line(store.notifications_on.get())}
            }
            if store.notifications_on.get() {
                Checkbox {
                    label: WATCHES_LABEL,
                    checked_fn: move || shown_ticked(store.notifications_on.get(), store.notify_watches.get()),
                    onchange: move || crate::watches::set_on(store, !untracked(|| store.notify_watches.get())),
                }
            }
            if !store.notifications_on.get() {
                Checkbox { label: WATCHES_LABEL, checked: false, disabled: true }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With notifications off, the switches under them show off whatever
    /// they hold, and the line says why.
    #[test]
    fn with_notifications_off_the_channels_show_off() {
        assert!(shown_ticked(true, true));
        assert!(!shown_ticked(true, false));
        assert!(!shown_ticked(false, true), "held, but shown off");
        assert_ne!(channels_line(true), channels_line(false));
        assert!(channels_line(false).starts_with("Notifications are off"));
    }
}
