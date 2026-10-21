//! Back and forward through what a pane has shown (Joe, 2026-10-20: "forward/
//! back navigation buttons for the explorer, that affect the focused pane;
//! user only, not shared/synced").
//!
//! Each pane keeps its own history of the tree values it opened (a node's,
//! with any mount path, or a store row's), in memory on this device only: it
//! is never written to a store, never sent anywhere and never saved. The
//! explorer's arrows move the focused pane through it.

/// How many places back a pane remembers.
const LIMIT: usize = 100;

/// One pane's history: where it is, where it has been, and where it went
/// before stepping back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct History {
    back: Vec<String>,
    current: Option<String>,
    forward: Vec<String>,
}

impl History {
    /// The pane opened `value` some way other than back or forward: what it
    /// showed goes on the back list, and the forward list is gone, as in a
    /// browser. Opening what it already shows changes nothing.
    pub fn visit(&mut self, value: &str) {
        if self.current.as_deref() == Some(value) {
            return;
        }
        if let Some(current) = self.current.take() {
            self.back.push(current);
            if self.back.len() > LIMIT {
                self.back.remove(0);
            }
        }
        self.forward.clear();
        self.current = Some(value.to_string());
    }

    /// Step back: the value to open, if there is one.
    pub fn back(&mut self) -> Option<String> {
        let to = self.back.pop()?;
        if let Some(current) = self.current.replace(to.clone()) {
            self.forward.push(current);
        }
        Some(to)
    }

    /// Step forward again after stepping back.
    pub fn forward(&mut self) -> Option<String> {
        let to = self.forward.pop()?;
        if let Some(current) = self.current.replace(to.clone()) {
            self.back.push(current);
        }
        Some(to)
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// A value that no longer opens (its node was deleted, its store closed)
    /// leaves the history wherever it is, so the arrows never step onto it.
    pub fn forget(&mut self, value: &str) {
        self.back.retain(|v| v != value);
        self.forward.retain(|v| v != value);
        if self.current.as_deref() == Some(value) {
            self.current = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn back_and_forward_walk_what_was_opened() {
        let mut h = History::default();
        h.visit("a");
        h.visit("b");
        h.visit("c");
        assert_eq!(h.back().as_deref(), Some("b"));
        assert_eq!(h.back().as_deref(), Some("a"));
        assert_eq!(h.back(), None, "nothing before the first");
        assert_eq!(h.forward().as_deref(), Some("b"));
        assert_eq!(h.forward().as_deref(), Some("c"));
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn opening_something_new_after_stepping_back_drops_the_forward_list() {
        let mut h = History::default();
        h.visit("a");
        h.visit("b");
        h.back();
        h.visit("c");
        assert!(!h.can_go_forward());
        assert_eq!(h.back().as_deref(), Some("a"));
    }

    #[test]
    fn opening_what_is_shown_adds_nothing() {
        let mut h = History::default();
        h.visit("a");
        h.visit("a");
        assert!(!h.can_go_back());
    }

    #[test]
    fn it_remembers_a_hundred_places() {
        let mut h = History::default();
        for i in 0..150 {
            h.visit(&i.to_string());
        }
        let mut steps = 0;
        while h.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, LIMIT);
    }

    #[test]
    fn a_forgotten_value_is_never_stepped_onto() {
        let mut h = History::default();
        h.visit("a");
        h.visit("gone");
        h.visit("c");
        h.forget("gone");
        assert_eq!(h.back().as_deref(), Some("a"));
    }
}
