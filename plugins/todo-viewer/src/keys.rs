//! The list's keys (docs/TODO_CONTRACT.md section 3): choosing, folding,
//! opening, checking off, renaming, deleting, and the moves (Alt+Shift+arrows,
//! Tab and Shift+Tab). The sortable list's own keyboard lift is Insert
//! (`rows::render_list`), since Space and Enter are taken here.

use pimble_plugin_sdk::prelude::*;

use crate::outline::{key_of, Step};
use crate::Todo;

impl Todo {
    /// The list's keys (docs/TODO_CONTRACT.md section 3). Answers whether it
    /// took the key.
    pub(crate) fn key(&self, key: &KeyEventData) -> bool {
        if untracked(|| self.renaming.get()).is_some() {
            return false;
        }
        let rows = untracked(|| self.rows());
        let outline = untracked(|| self.outline.get());
        let chosen = untracked(|| self.chosen.get());
        let at = chosen.and_then(|c| rows.iter().position(|r| r.addr == c));
        let item = chosen.and_then(|c| outline.get(c).cloned());
        let writable = item.as_ref().is_some_and(|i| i.writable);
        // Alt+Shift+arrows move, as in the explorer and the kanban; Tab and
        // Shift+Tab indent and outdent.
        let moving = match key.key.as_str() {
            "ArrowUp" if key.alt && key.shift => Some(Step::Up),
            "ArrowDown" if key.alt && key.shift => Some(Step::Down),
            "ArrowRight" if key.alt && key.shift => Some(Step::Indent),
            "ArrowLeft" if key.alt && key.shift => Some(Step::Outdent),
            "Tab" if key.shift => Some(Step::Outdent),
            "Tab" => Some(Step::Indent),
            _ => None,
        };
        if let Some(step) = moving {
            if writable {
                self.step(step);
            }
            return true;
        }
        match key.key.as_str() {
            "ArrowDown" => {
                let next = match at {
                    None => rows.first(),
                    Some(i) => rows.get(i + 1).or(rows.get(i)),
                };
                self.choose(next.map(|r| r.addr).or(chosen));
            }
            "ArrowUp" => {
                let prev = match at {
                    None => rows.first(),
                    Some(i) => rows.get(i.saturating_sub(1)),
                };
                self.choose(prev.map(|r| r.addr).or(chosen));
            }
            "ArrowLeft" => {
                let Some(item) = item else { return true };
                let folded = untracked(|| self.look.with(|l| l.folded.contains(&key_of(item.addr))));
                if !item.children.is_empty() && !folded {
                    self.set_folded(item.addr, true);
                } else if let Some(parent) = item.parent {
                    self.choose(Some(parent));
                }
            }
            "ArrowRight" => {
                if let Some(item) = item.filter(|i| !i.children.is_empty()) {
                    self.set_folded(item.addr, false);
                }
            }
            "Enter" => {
                let Some(c) = chosen else { return false };
                if key.ctrl || key.meta {
                    self.host.open(c);
                } else {
                    self.host.open_beside(c);
                }
            }
            " " => {
                let Some(c) = chosen else { return false };
                self.toggle_done(c);
            }
            "F2" if writable => {
                if let Some(item) = item {
                    self.start_rename(item.addr, item.label.clone());
                }
            }
            "Delete" if writable => {
                if let Some(c) = chosen {
                    self.delete(c);
                }
            }
            "Escape" if chosen.is_some() => self.choose(None),
            _ => return false,
        }
        true
    }
}
