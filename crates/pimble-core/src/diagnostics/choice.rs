//! The person's diagnostics choice as a client that does not record for
//! itself keeps it (the browser page's `localStorage` `pimble.diagnostics`,
//! docs/ONE_CODEBASE_CONTRACT.md "Built: wave 5"): on or off, whether the
//! notice was shown, and a clear owed. The page's storage is the platform
//! part; what is kept, and when a clear stops being owed, is decided here.
//!
//! Turning diagnostics off owes the server's queue a clear. The browser's
//! worker writes an off down on its disk only once it has the queue's
//! folder, and it can end before that, so the page notes the clear beside
//! the choice and tells every worker it connects to (`setDiagnostics`
//! `clear_owed`) until one confirms it has cleared. Each off gets a tag of
//! its own, so the confirmation of an earlier off never drops the note of a
//! later one.

use serde_json::{json, Value};

/// The saved choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedChoice {
    /// Whether diagnostics record; on when nothing is saved.
    pub enabled: bool,
    /// Whether the person has been shown the notice.
    pub noticed: bool,
    /// The tag of the latest off whose clear no server has confirmed.
    pub clear_owed: Option<String>,
}

impl Default for SavedChoice {
    fn default() -> Self {
        SavedChoice { enabled: true, noticed: false, clear_owed: None }
    }
}

impl SavedChoice {
    /// What `saved` holds (the stored JSON), or the default when it is
    /// missing or unreadable.
    pub fn parse(saved: Option<&str>) -> Self {
        let Some(v) = saved.and_then(|t| serde_json::from_str::<Value>(t).ok()) else { return SavedChoice::default() };
        SavedChoice {
            enabled: v["enabled"].as_bool().unwrap_or(true),
            noticed: v["noticed"].as_bool().unwrap_or(false),
            clear_owed: v["clear_owed"].as_str().filter(|tag| !tag.is_empty()).map(str::to_string),
        }
    }

    /// The JSON to store.
    pub fn to_json(&self) -> String {
        let mut v = json!({ "enabled": self.enabled, "noticed": self.noticed });
        if let Some(tag) = &self.clear_owed {
            v["clear_owed"] = json!(tag);
        }
        v.to_string()
    }

    /// The person saved `enabled` (and `noticed`): an off owes a clear under
    /// a new tag (`new_tag`), and a clear already owed stays owed through an
    /// "on" until a server confirms it.
    pub fn saved(&self, enabled: bool, noticed: bool, new_tag: impl FnOnce() -> String) -> Self {
        let clear_owed = if enabled { self.clear_owed.clone() } else { Some(new_tag()) };
        SavedChoice { enabled, noticed, clear_owed }
    }

    /// A server confirmed the clear owed under `tag`: the note goes, unless
    /// a later off has owed another since.
    pub fn confirmed(&self, tag: &str) -> Self {
        let clear_owed = self.clear_owed.clone().filter(|owed| owed != tag);
        SavedChoice { clear_owed, ..self.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_saved_is_on_and_owes_nothing() {
        assert_eq!(SavedChoice::parse(None), SavedChoice::default());
        assert_eq!(SavedChoice::parse(Some("not json")), SavedChoice::default());
        let old = SavedChoice::parse(Some(r#"{"enabled":false,"noticed":true}"#));
        assert_eq!(old, SavedChoice { enabled: false, noticed: true, clear_owed: None }, "what a page before the note saved");
    }

    #[test]
    fn an_off_owes_a_clear_until_it_is_confirmed_even_through_an_on() {
        let off = SavedChoice::default().saved(false, true, || "t1".into());
        assert_eq!(off.clear_owed.as_deref(), Some("t1"));
        let on = SavedChoice::parse(Some(&off.to_json())).saved(true, true, || unreachable!("an on owes nothing new"));
        assert!(on.enabled);
        assert_eq!(on.clear_owed.as_deref(), Some("t1"), "the on keeps the off's clear owed");
        let confirmed = SavedChoice::parse(Some(&on.to_json())).confirmed("t1");
        assert_eq!(confirmed.clear_owed, None);
        assert_eq!(SavedChoice::parse(Some(&confirmed.to_json())), SavedChoice { enabled: true, noticed: true, clear_owed: None });
    }

    #[test]
    fn a_confirmation_of_an_earlier_off_keeps_a_later_one_owed() {
        let first = SavedChoice::default().saved(false, true, || "t1".into());
        let second = first.saved(true, true, || unreachable!()).saved(false, true, || "t2".into());
        assert_eq!(second.confirmed("t1").clear_owed.as_deref(), Some("t2"));
        assert_eq!(second.confirmed("t2").clear_owed, None);
    }
}
