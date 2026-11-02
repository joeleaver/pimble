//! Who is writing (docs/API_CONTRACT.md section 3b, `identity.rs`): the
//! account (or agent) this connection acts as, asked of the server once and
//! kept, so a write is stamped without a `whoAmI` before it. A canvas
//! element and a discussion message carry the same author.

/// This connection's principal as the server names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Me {
    /// The account's id (or the agent's).
    pub id: String,
    /// The name it goes by; may be empty.
    pub name: String,
}

impl Me {
    /// The name, or `fallback` when the server knows none.
    pub fn name_or(&self, fallback: &str) -> String {
        if self.name.trim().is_empty() {
            fallback.to_string()
        } else {
            self.name.clone()
        }
    }

    /// The author a canvas element is stamped with.
    pub fn board_author(&self, fallback: &str) -> pimble_board::Author {
        pimble_board::Author::new(self.id.clone(), self.name_or(fallback))
    }

    /// The author a discussion message is stamped with.
    pub fn discussion_author(&self, fallback: &str) -> pimble_discussion::Author {
        pimble_discussion::Author::new(self.id.clone(), self.name_or(fallback))
    }
}
