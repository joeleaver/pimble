//! A message's text (docs/DISCUSSIONS_CONTRACT.md section 5): plain text in
//! spans, each bold, italic or a link or not. Emoji are plain Unicode.
//!
//! A person types a message as a little Markdown, which becomes the spans
//! when it is sent, and a message being edited is written back as the same
//! Markdown: `**bold**`, `*italic*` or `_italic_`, `[text](url)`, and a bare
//! `http://`, `https://` or `pimble:` URL, which is a link to itself. A
//! backslash keeps the next character as it is. A marker with no partner is
//! text.

/// A run of text with one formatting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// The link's target (a web URL or a `pimble:` link).
    pub link: Option<String>,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { text: text.into(), ..Default::default() }
    }

    fn same_format(&self, other: &Span) -> bool {
        self.bold == other.bold && self.italic == other.italic && self.link == other.link
    }
}

/// A message's text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Body {
    pub spans: Vec<Span>,
}

impl Body {
    pub fn plain(text: impl Into<String>) -> Self {
        Self { spans: vec![Span::plain(text)] }.normalized()
    }

    /// The spans with empty ones dropped and neighbours of one format joined:
    /// the one shape of a given text, so two bodies compare by what they show.
    pub fn normalized(self) -> Self {
        let mut spans: Vec<Span> = Vec::with_capacity(self.spans.len());
        for span in self.spans.into_iter().filter(|s| !s.text.is_empty()) {
            match spans.last_mut() {
                Some(last) if last.same_format(&span) => last.text.push_str(&span.text),
                _ => spans.push(span),
            }
        }
        Self { spans }
    }

    /// The text without its formatting.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// Nothing but whitespace.
    pub fn is_blank(&self) -> bool {
        self.spans.iter().all(|s| s.text.trim().is_empty())
    }

    /// Every link's target, in order.
    pub fn links(&self) -> Vec<&str> {
        self.spans.iter().filter_map(|s| s.link.as_deref()).collect()
    }

    /// The text typed as Markdown (the module's rules), trimmed.
    pub fn from_markdown(md: &str) -> Self {
        let chars: Vec<char> = md.trim().chars().collect();
        let mut out = Vec::new();
        parse(&chars, Format::default(), &mut out);
        Self { spans: out }.normalized().with_mention_ids()
    }

    /// Every mention of a person in the body (docs/NOTIFICATIONS_CONTRACT.md
    /// section 1): a link to a `pimble-person:` href, adjacent spans with the
    /// same href one mention.
    pub fn mentions(&self) -> Vec<pimble_core::mention::Mention> {
        let mut out: Vec<pimble_core::mention::Mention> = Vec::new();
        let mut open: Option<(&str, String)> = None;
        for span in &self.spans {
            match (span.link.as_deref().filter(|h| pimble_core::mention::parse_href(h).is_some()), open.as_mut()) {
                (Some(href), Some((current, words))) if *current == href => words.push_str(&span.text),
                (Some(href), _) => {
                    if let Some((href, words)) = open.take() {
                        out.extend(pimble_core::mention::Mention::from_link(href, &words));
                    }
                    open = Some((href, span.text.clone()));
                }
                (None, _) => {
                    if let Some((href, words)) = open.take() {
                        out.extend(pimble_core::mention::Mention::from_link(href, &words));
                    }
                }
            }
        }
        if let Some((href, words)) = open.take() {
            out.extend(pimble_core::mention::Mention::from_link(href, &words));
        }
        out
    }

    /// The body with every `@Name` typed as plain text, for a `(name,
    /// account)` in `picked`, made a mention of that account (the message box
    /// shows `@Name` as it was picked, and this links it as the message is
    /// sent). The longest name is tried first, so `@Will Smith` is not taken
    /// for `@Will`; text in links is left alone.
    pub fn with_picked_mentions(self, picked: &[(String, String)]) -> Self {
        if picked.is_empty() {
            return self;
        }
        let mut names: Vec<&(String, String)> = picked.iter().filter(|(name, _)| !name.trim().is_empty()).collect();
        names.sort_by_key(|(name, _)| std::cmp::Reverse(name.chars().count()));
        let mut spans = Vec::new();
        for span in self.spans {
            if span.link.is_some() {
                spans.push(span);
                continue;
            }
            let mut rest = span.text.as_str();
            'scan: while !rest.is_empty() {
                let mut search_from = 0;
                while let Some(at) = rest[search_from..].find('@').map(|i| i + search_from) {
                    let starts_word = rest[..at].chars().last().map_or(true, |c| c.is_whitespace() || "([{\"'".contains(c));
                    let after = &rest[at + 1..];
                    let hit = starts_word.then(|| names.iter().find(|(name, _)| {
                        after.starts_with(name.as_str()) && after[name.len()..].chars().next().map_or(true, |c| !c.is_alphanumeric())
                    })).flatten();
                    if let Some((name, account)) = hit {
                        spans.push(Span { text: rest[..at].to_string(), ..span.clone() });
                        let mention = pimble_core::mention::Mention::new(account, name);
                        spans.push(Span { text: mention.words(), link: Some(mention.href()), ..span.clone() });
                        rest = &after[name.len()..];
                        continue 'scan;
                    }
                    search_from = at + 1;
                }
                spans.push(Span { text: rest.to_string(), ..span.clone() });
                break;
            }
        }
        Self { spans }.normalized()
    }

    /// Each mention written without an id given one, the spans of one mention
    /// one id.
    fn with_mention_ids(mut self) -> Self {
        let mut last: Option<(String, String)> = None;
        for span in &mut self.spans {
            let Some((account, None)) = span.link.as_deref().and_then(pimble_core::mention::parse_href) else {
                last = None;
                continue;
            };
            let written = span.link.clone().unwrap_or_default();
            let given = match &last {
                Some((was, given)) if *was == written => given.clone(),
                _ => pimble_core::mention::href(&account, Some(&pimble_core::mention::new_id())),
            };
            last = Some((written, given.clone()));
            span.link = Some(given);
        }
        self
    }

    /// The body as the Markdown [`Body::from_markdown`] reads back as it.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            let mut piece = match &span.link {
                // A link to itself is the bare URL, which reads back as one.
                Some(href) if *href == span.text && is_bare_url(href) => href.clone(),
                Some(href) => format!("[{}]({})", escape(&span.text), href.replace(')', "%29").replace(' ', "%20")),
                None => escape(&span.text),
            };
            if span.bold {
                piece = format!("**{piece}**");
            }
            if span.italic {
                // `_` outside `**` (`***` would read as bold first), unless a
                // letter comes right before it, where `_` is not a marker.
                let after_word = out.chars().last().is_some_and(char::is_alphanumeric);
                piece = if after_word { format!("*{piece}*") } else { format!("_{piece}_") };
            }
            out.push_str(&piece);
        }
        out
    }
}

#[derive(Debug, Clone, Default)]
struct Format {
    bold: bool,
    italic: bool,
    link: Option<String>,
}

fn push(out: &mut Vec<Span>, text: &str, f: &Format) {
    if text.is_empty() {
        return;
    }
    out.push(Span { text: text.to_string(), bold: f.bold, italic: f.italic, link: f.link.clone() });
}

const URL_SCHEMES: [&str; 3] = ["https://", "http://", "pimble:"];

fn is_bare_url(s: &str) -> bool {
    URL_SCHEMES.iter().any(|p| s.starts_with(p)) && !s.chars().any(char::is_whitespace) && s.len() > 8
}

fn starts_with(chars: &[char], at: usize, s: &str) -> bool {
    let mut i = at;
    for c in s.chars() {
        if chars.get(i) != Some(&c) {
            return false;
        }
        i += 1;
    }
    true
}

/// The index of the next unescaped `marker` at or after `from`.
fn find(chars: &[char], from: usize, marker: &str) -> Option<usize> {
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '\\' {
            i += 2;
            continue;
        }
        if starts_with(chars, i, marker) {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The length of a bare URL at `at`, trailing punctuation left out.
fn bare_url_at(chars: &[char], at: usize) -> Option<usize> {
    if at > 0 && chars[at - 1].is_alphanumeric() {
        return None;
    }
    if !URL_SCHEMES.iter().any(|p| starts_with(chars, at, p)) {
        return None;
    }
    let mut end = at;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }
    while end > at && matches!(chars[end - 1], '.' | ',' | ';' | ':' | '!' | '?' | ')' | '\'' | '"') {
        end -= 1;
    }
    let len = end - at;
    let s: String = chars[at..end].iter().collect();
    is_bare_url(&s).then_some(len)
}

fn parse(chars: &[char], f: Format, out: &mut Vec<Span>) {
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            text.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if f.link.is_none() {
            if let Some(len) = bare_url_at(chars, i) {
                push(out, &text, &f);
                text.clear();
                let url: String = chars[i..i + len].iter().collect();
                push(out, &url, &Format { link: Some(url.clone()), ..f.clone() });
                i += len;
                continue;
            }
        }
        if !f.bold && starts_with(chars, i, "**") {
            if let Some(end) = find(chars, i + 2, "**").filter(|&end| end > i + 2) {
                push(out, &text, &f);
                text.clear();
                parse(&chars[i + 2..end], Format { bold: true, ..f.clone() }, out);
                i = end + 2;
                continue;
            }
        }
        if !f.italic && (c == '*' || (c == '_' && (i == 0 || !chars[i - 1].is_alphanumeric()))) {
            let marker = if c == '*' { "*" } else { "_" };
            let close = find(chars, i + 1, marker).filter(|&end| {
                end > i + 1 && (c == '*' || chars.get(end + 1).is_none_or(|n| !n.is_alphanumeric())) && !(c == '*' && starts_with(chars, end, "**") && !starts_with(chars, end, "***"))
            });
            if let Some(end) = close {
                push(out, &text, &f);
                text.clear();
                parse(&chars[i + 1..end], Format { italic: true, ..f.clone() }, out);
                i = end + 1;
                continue;
            }
        }
        if f.link.is_none() && c == '[' {
            if let Some(close) = find(chars, i + 1, "]") {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(paren) = find(chars, close + 2, ")") {
                        let href: String = chars[close + 2..paren].iter().collect::<String>().trim().to_string();
                        if !href.is_empty() && !href.chars().any(char::is_whitespace) {
                            push(out, &text, &f);
                            text.clear();
                            parse(&chars[i + 1..close], Format { link: Some(href), ..f.clone() }, out);
                            i = paren + 1;
                            continue;
                        }
                    }
                }
            }
        }
        text.push(c);
        i += 1;
    }
    push(out, &text, &f);
}

/// Text with every character that could read as a marker kept as itself.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        if matches!(c, '\\' | '*' | '_' | '[' | ']') {
            out.push('\\');
        }
        // A URL in plain text would read back as a link.
        if c == ':' && URL_SCHEMES.iter().any(|p| text[..i + 1].ends_with(p.trim_end_matches('/'))) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, bold: bool, italic: bool, link: Option<&str>) -> Span {
        Span { text: text.into(), bold, italic, link: link.map(str::to_string) }
    }

    #[test]
    fn picked_names_become_mentions_as_the_message_is_sent() {
        let picked = vec![("Will".to_string(), "w1".to_string()), ("Will Smith".to_string(), "w2".to_string())];
        let body = Body::from_markdown("@Will Smith and @Will, not me@Will or @Willow, see [@Will](https://x)").with_picked_mentions(&picked);
        let found = body.mentions();
        assert_eq!(found.iter().map(|m| (m.account.as_str(), m.name.as_str())).collect::<Vec<_>>(), vec![("w2", "Will Smith"), ("w1", "Will")]);
        assert!(found.iter().all(|m| m.id.is_some()));
        assert!(body.text().contains("me@Will or @Willow"), "inside a word, or a longer word, is text: {}", body.text());
        assert_eq!(Body::from_markdown("plain").with_picked_mentions(&picked), Body::plain("plain"));
    }

    #[test]
    fn mentions_are_links_to_a_person_and_keep_their_ids() {
        let body = Body::from_markdown("[@Will](pimble-person:w1?m=aa) can you check? cc [@Sam **S**](pimble-person:s2)");
        let found = body.mentions();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!((found[0].account.as_str(), found[0].id.as_deref(), found[0].name.as_str()), ("w1", Some("aa"), "Will"));
        assert_eq!((found[1].account.as_str(), found[1].name.as_str()), ("s2", "Sam S"));
        let minted = found[1].id.clone().expect("a mention typed without an id is given one");
        // Editing the message writes it back with the ids, so an edit adds no mention.
        let again = Body::from_markdown(&body.to_markdown());
        assert_eq!(again.mentions(), found);
        assert!(again.to_markdown().contains(&format!("pimble-person:s2?m={minted}")));
        assert!(pimble_core::mention::added(&found, &again.mentions(), None).is_empty());
    }

    #[test]
    fn markdown_reads_bold_italic_and_links() {
        let body = Body::from_markdown("a **b** *c* _d_ [e](https://x.org/y) f");
        assert_eq!(
            body.spans,
            vec![
                span("a ", false, false, None),
                span("b", true, false, None),
                span(" ", false, false, None),
                span("c", false, true, None),
                span(" ", false, false, None),
                span("d", false, true, None),
                span(" ", false, false, None),
                span("e", false, false, Some("https://x.org/y")),
                span(" f", false, false, None),
            ]
        );
    }

    #[test]
    fn bare_urls_are_links_and_keep_their_underscores() {
        let body = Body::from_markdown("see https://x.org/a_b_c. and pimble://node/1");
        assert_eq!(body.links(), vec!["https://x.org/a_b_c", "pimble://node/1"]);
        assert_eq!(body.text(), "see https://x.org/a_b_c. and pimble://node/1");
    }

    #[test]
    fn unpaired_markers_and_snake_case_are_text() {
        assert_eq!(Body::from_markdown("2 * 3 = 6").spans, vec![span("2 * 3 = 6", false, false, None)]);
        assert_eq!(Body::from_markdown("snake_case_name").spans, vec![span("snake_case_name", false, false, None)]);
        assert_eq!(Body::from_markdown("a [b] c").spans, vec![span("a [b] c", false, false, None)]);
        assert_eq!(Body::from_markdown("\\*not italic\\*").spans, vec![span("*not italic*", false, false, None)]);
    }

    #[test]
    fn emoji_are_plain_text() {
        let body = Body::from_markdown("👍 **yes 🎉**");
        assert_eq!(body.spans, vec![span("👍 ", false, false, None), span("yes 🎉", true, false, None)]);
    }

    #[test]
    fn markdown_round_trips() {
        let cases = [
            Body { spans: vec![span("plain ", false, false, None), span("both", true, true, None), span(" end", false, false, None)] },
            Body { spans: vec![span("bold link", true, false, Some("https://x.org")), span(" and ", false, false, None), span("https://y.org/a_b", false, false, Some("https://y.org/a_b"))] },
            Body { spans: vec![span("a*b_c [d] \\ e", false, false, None)] },
            Body { spans: vec![span("word", false, false, None), span("italic", false, true, None)] },
            Body { spans: vec![span("not a link: https://x.org", false, false, None)] },
            Body { spans: vec![span("line one\nline two", false, false, None)] },
            Body { spans: vec![span("node", false, false, Some("pimble://node/abc"))] },
        ];
        for body in cases {
            let body = body.normalized();
            let md = body.to_markdown();
            assert_eq!(Body::from_markdown(&md), body, "through {md:?}");
        }
    }

    #[test]
    fn blank_is_whitespace_only() {
        assert!(Body::from_markdown("   ").is_blank());
        assert!(!Body::from_markdown(" x ").is_blank());
    }
}
