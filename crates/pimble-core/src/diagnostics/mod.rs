//! What a diagnostic record may hold (docs/DIAGNOSTICS_CONTRACT.md section 2).
//!
//! Every line that leaves a tester's computer goes through [`Scrubber::scrub`]
//! first, on that computer: a record that holds something it should not is a
//! leak whether or not anyone reads it. Kept: ids, store names, file paths
//! (the home folder written `~`). Taken out: email addresses, secrets by
//! their shape (bearer tokens, JWTs, `token=`/`key=` values, long opaque
//! strings), and note titles the scrubber was told about. Note text never
//! reaches a log line in the first place; a line that quotes a title is a
//! bug in that line, and the titles a caller knows are a second guard.

use std::collections::HashSet;

mod choice;
mod records;
mod sending;

pub use choice::*;
pub use records::*;
pub use sending::*;

/// The scrubbing rules for one computer: its home folder, and the note titles
/// known not to be sent.
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    homes: Vec<String>,
    titles: HashSet<String>,
}

/// What an email address becomes.
pub const EMAIL: &str = "<email>";
/// What a secret becomes.
pub const SECRET: &str = "<secret>";
/// What a known title becomes (inside its quotes).
pub const TITLE: &str = "…";

/// Opaque strings at least this long, with no separator a word or id has,
/// read as secrets (a key, a session token, a signature). A UUID has dashes
/// and a state vector or a node id is shorter.
const OPAQUE_MIN: usize = 40;

/// Query and header names whose value is a secret.
const SECRET_NAMES: &[&str] = &["access_token", "token", "key", "api_key", "apikey", "password", "secret", "session", "pimble_session", "authorization"];

impl Scrubber {
    /// Rules for a computer whose home folder is `home` (`None`: no home is
    /// rewritten).
    pub fn new(home: Option<&str>) -> Self {
        let mut homes = Vec::new();
        if let Some(home) = home.map(|h| h.trim_end_matches(['/', '\\'])).filter(|h| h.len() > 1) {
            homes.push(home.to_string());
            // A Windows home is written with either slash.
            let forward = home.replace('\\', "/");
            if forward != home {
                homes.push(forward);
            }
            let back = home.replace('/', "\\");
            if back != home && home.contains(':') {
                homes.push(back);
            }
            // And with every backslash doubled: a path printed with `{:?}`
            // ("C:\\Users\\will\\AppData...") kept the home folder
            // before 2026-11-26.
            let doubled = home.replace('/', "\\").replace('\\', "\\\\");
            if home.contains(':') && !homes.contains(&doubled) {
                homes.push(doubled);
            }
        }
        Scrubber { homes, titles: HashSet::new() }
    }

    /// A note title that must not be sent: quoted in a line, it becomes `'…'`.
    pub fn forget_title(&mut self, title: &str) {
        let title = title.trim();
        if title.len() >= 2 {
            self.titles.insert(title.to_string());
        }
    }

    /// `line` as it may leave this computer.
    pub fn scrub(&self, line: &str) -> String {
        let mut out = line.to_string();
        for home in &self.homes {
            out = replace_path_prefix(&out, home);
        }
        out = scrub_titles(&out, &self.titles);
        scrub_words(&out)
    }
}

/// Every `home` followed by a separator or the end becomes `~`.
fn replace_path_prefix(line: &str, home: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(home) {
        let after = &rest[at + home.len()..];
        let boundary = after.is_empty() || after.starts_with(['/', '\\', '"', '\'', ' ', ')', ',']);
        out.push_str(&rest[..at]);
        out.push_str(if boundary { "~" } else { home });
        rest = after;
    }
    out.push_str(rest);
    out
}

fn scrub_titles(line: &str, titles: &HashSet<String>) -> String {
    if titles.is_empty() {
        return line.to_string();
    }
    let mut out = line.to_string();
    for title in titles {
        for quote in ['\'', '"', '“'] {
            let close = if quote == '“' { '”' } else { quote };
            let quoted = format!("{quote}{title}{close}");
            if out.contains(&quoted) {
                out = out.replace(&quoted, &format!("{quote}{TITLE}{close}"));
            }
        }
    }
    out
}

/// Emails, bearer tokens, JWTs, secret-named values and long opaque strings.
fn scrub_words(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    let mut secret_next = false;
    let mut last = 0;
    while let Some(&(start, c)) = chars.peek() {
        if !is_word_char(c) {
            chars.next();
            continue;
        }
        let mut end = start;
        while let Some(&(i, c)) = chars.peek() {
            if is_word_char(c) {
                end = i + c.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        let word = &line[start..end];
        let replacement = if secret_next && word.len() >= 8 {
            Some(SECRET)
        } else if is_email(word) {
            Some(EMAIL)
        } else if is_jwt(word) || is_opaque(word) {
            Some(SECRET)
        } else {
            None
        };
        // A word followed by `=` or `: ` whose name says secret, or `Bearer`:
        // the next word is the secret.
        let following = &line[end..];
        let lower = word.to_ascii_lowercase();
        secret_next = (lower == "bearer")
            || (SECRET_NAMES.contains(&lower.trim_matches('"')) && (following.starts_with('=') || following.starts_with(": ") || following.starts_with("\": \"") || following.starts_with("=\"")));
        if let Some(r) = replacement {
            out.push_str(&line[last..start]);
            out.push_str(r);
            last = end;
        }
    }
    out.push_str(&line[last..]);
    out
}

/// Characters a word, an address or a token is made of (not quotes, spaces,
/// slashes, `=`, `:`, brackets or commas, which end one).
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@' | '%' | '~')
}

fn is_email(word: &str) -> bool {
    let word = word.trim_end_matches('.');
    let Some((local, domain)) = word.split_once('@') else { return false };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && domain.split('.').all(|p| !p.is_empty())
}

fn is_jwt(word: &str) -> bool {
    word.starts_with("eyJ") && word.split('.').count() >= 3 && word.len() > 30
}

/// A long run with no dash or dot: a key, a token, a signature.
fn is_opaque(word: &str) -> bool {
    word.len() >= OPAQUE_MIN && !word.contains(['-', '.', '@']) && word.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '%'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Scrubber {
        let mut s = Scrubber::new(Some("/home/tester"));
        s.forget_title("Holiday plans");
        s
    }

    #[test]
    fn home_folders_become_a_tilde_and_the_rest_of_the_path_stays() {
        assert_eq!(s().scrub("Opened local store 'Personal' from \"/home/tester/Dropbox/Personal.pimble\""), "Opened local store 'Personal' from \"~/Dropbox/Personal.pimble\"");
        assert_eq!(s().scrub("in /home/testerfoo/x"), "in /home/testerfoo/x", "only a whole folder");
        let w = Scrubber::new(Some(r"C:\Users\Will"));
        assert_eq!(w.scrub(r"Opening store at C:\Users\Will\Documents\Notes.pimble"), r"Opening store at ~\Documents\Notes.pimble");
        assert_eq!(w.scrub("Opening store at C:/Users/Will/Documents/Notes.pimble"), "Opening store at ~/Documents/Notes.pimble");
        let debug = format!("{:?}", r"C:\Users\Will\AppData\Local\pimble");
        assert_eq!(w.scrub(&format!("Opening store at {debug}")), r#"Opening store at "~\\AppData\\Local\\pimble""#);
    }

    #[test]
    fn addresses_and_secrets_go() {
        assert_eq!(s().scrub("Cloud account: signed_in=true email=Some(\"will.s+pimble@example.co.uk\")"), "Cloud account: signed_in=true email=Some(\"<email>\")");
        assert_eq!(s().scrub("Authorization: Bearer abcdefgh12345678"), "Authorization: Bearer <secret>");
        assert_eq!(s().scrub("connect wss://pimble.app/rpc?access_token=eyJhbGciOiJFZERTQSJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl"), "connect wss://pimble.app/rpc?access_token=<secret>");
        assert_eq!(s().scrub("key=0123456789abcdef0123"), "key=<secret>");
        let long = "A".repeat(20) + &"b9".repeat(15);
        assert_eq!(s().scrub(&format!("session {long} expired")), "session <secret> expired");
    }

    #[test]
    fn ids_store_names_and_numbers_stay() {
        let line = "Share 280e81c4-8e99-4f62-a045-c14f5d056a9b of store 1dacd405-1685-43fc-9730-d302dcc09a15 -> Synced { last_sync: 2026-11-19T14:07:06.771114976Z }";
        assert_eq!(s().scrub(line), line);
        let line = "Node 2b56676b-a412-42be-b04f-74500ac473ee: parked (missing client 5596681811603945 from 129)";
        assert_eq!(s().scrub(line), line);
        let line = "Relay tunnel up: serving 2 store(s) through wss://pimble.app/api/v1/relay";
        assert_eq!(s().scrub(line), line);
        let line = "Opened vault store 'Family Management' (a224c35b-fb95-4af3-97eb-00616e881159)";
        assert_eq!(s().scrub(line), line);
    }

    #[test]
    fn a_title_it_was_told_about_goes() {
        assert_eq!(s().scrub("A picture in \"Holiday plans\" was left out"), "A picture in \"…\" was left out");
        assert_eq!(s().scrub("Creating board attachment 'Holiday plans' of node x"), "Creating board attachment '…' of node x");
    }
}
