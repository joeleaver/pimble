//! Search-as-you-type: the last word of a query as a prefix term.

/// Turn the last word of a query into a prefix term (`came` -> `came*`) so
/// results update while the user is still typing. Leaves the query alone when
/// the last token already ends a phrase or a prefix, is inside an open quote,
/// or is shorter than two characters (the engine rejects those).
pub(crate) fn with_trailing_prefix(query: &str) -> String {
    let trimmed = query.trim_end();
    if trimmed.is_empty() || trimmed.len() != query.len() {
        // Trailing whitespace means the last word is finished.
        return query.to_string();
    }
    if trimmed.matches('"').count() % 2 == 1 {
        return query.to_string();
    }
    let last = trimmed.rsplit(char::is_whitespace).next().unwrap_or("");
    let word = last.trim_start_matches('+');
    if word.ends_with('*') || word.ends_with('"') {
        return query.to_string();
    }
    if word.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
        return query.to_string();
    }
    // rhypedb's english analyzer drops stop words, but a prefix term bypasses
    // that: "of*" would match "office" and "the*" "theocratic". A stop word the
    // user is still typing is far more often the word itself than a prefix.
    if PREFIX_STOP_WORDS.contains(&word.to_lowercase().as_str()) {
        return query.to_string();
    }
    format!("{trimmed}*")
}

/// Short English function words that are never expanded into prefix terms.
pub(super) const PREFIX_STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "do", "for", "from", "had",
    "has", "have", "he", "her", "his", "how", "i", "if", "in", "is", "it", "its", "me",
    "my", "no", "not", "of", "on", "or", "our", "she", "so", "that", "the", "their", "them",
    "they", "this", "to", "up", "us", "was", "we", "were", "what", "when", "who", "will",
    "with", "you", "your",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_prefix_rules() {
        assert_eq!(with_trailing_prefix("came"), "came*");
        assert_eq!(with_trailing_prefix("security came"), "security came*");
        assert_eq!(with_trailing_prefix("came "), "came ");
        assert_eq!(with_trailing_prefix("came*"), "came*");
        assert_eq!(with_trailing_prefix("\"security cam"), "\"security cam");
        assert_eq!(with_trailing_prefix("\"security cameras\""), "\"security cameras\"");
        assert_eq!(with_trailing_prefix("c"), "c");
        assert_eq!(with_trailing_prefix("+cam"), "+cam*");
        assert_eq!(with_trailing_prefix("of"), "of");
        assert_eq!(with_trailing_prefix("video of"), "video of");
        assert_eq!(with_trailing_prefix("The"), "The");
        assert_eq!(with_trailing_prefix("theo"), "theo*");
    }
}
