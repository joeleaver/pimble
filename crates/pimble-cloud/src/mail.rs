//! Sending the email verification link (docs/CLOUD_CONTRACT.md, "Phase 1b:
//! email verification").
//!
//! One `Mailer` trait, two implementations chosen once at startup by
//! whether `RESEND_API_KEY` is set (mirrors `JwtSigner`'s jkbase/local
//! split in `src/jwt.rs`): [`ResendMailer`] posts to Resend for real,
//! [`LogMailer`] logs the message and keeps it in memory so tests (and a
//! developer running the service locally without a Resend key) can recover
//! the verification link without an inbox.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::json;

use crate::config::Config;
use crate::error::{CloudError, CloudResult};

#[async_trait::async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, to: &str, subject: &str, text: &str, html: &str) -> CloudResult<()>;

    /// Test-only escape hatch: `Some(self)` for [`LogMailer`], `None` for
    /// [`ResendMailer`]. Lets integration tests reach the in-memory sent
    /// mail through `AppState::mailer: Arc<dyn Mailer>` without a generic
    /// downcast — the test stack always runs without `RESEND_API_KEY`, so
    /// this is always `Some` in practice, but a test that runs against a
    /// `ResendMailer` stack (there is none today) gets a clear panic message
    /// instead of a silent `None`-shaped bug.
    fn as_log_mailer(&self) -> Option<&LogMailer> {
        None
    }
}

/// Sends real mail through Resend (docs/CLOUD_CONTRACT.md: "`ResendMailer`
/// posts to `https://api.resend.com/emails` with `Authorization: Bearer
/// <RESEND_API_KEY>` and `{ "from", "to", "subject", "html", "text" }`").
pub struct ResendMailer {
    http: reqwest::Client,
    api_key: String,
    from: String,
}

impl ResendMailer {
    pub fn new(api_key: String, from: String) -> Self {
        Self { http: reqwest::Client::new(), api_key, from }
    }
}

#[async_trait::async_trait]
impl Mailer for ResendMailer {
    async fn send(&self, to: &str, subject: &str, text: &str, html: &str) -> CloudResult<()> {
        let resp = self
            .http
            .post("https://api.resend.com/emails")
            .bearer_auth(&self.api_key)
            .json(&json!({ "from": self.from, "to": to, "subject": subject, "html": html, "text": text }))
            .send()
            .await
            .map_err(|e| CloudError::Internal(format!("sending mail via Resend: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(CloudError::Internal(format!("Resend rejected the message ({status}): {body}")));
        }
        Ok(())
    }
}

#[derive(Clone)]
struct SentMail {
    text: String,
    /// How many messages this address has been sent in this process's
    /// lifetime. The body of two mails sent a moment apart can be identical
    /// (a repeated invitation says exactly the same thing), so "was a second
    /// one sent?" — what a rate-limit test asks — cannot be read off
    /// [`LogMailer::last_message`] alone.
    count: usize,
}

/// Logs the message at `info` and keeps the last one sent to each address in
/// memory, keyed by the exact `to` address `send` was called with. Used
/// whenever `RESEND_API_KEY` is unset — every local run and every test.
#[derive(Default)]
pub struct LogMailer {
    sent: Mutex<HashMap<String, SentMail>>,
}

impl LogMailer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Test-only accessor: the plain-text body of the last message sent to
    /// `to`, if any. Contains the verification link; pair with
    /// [`first_url`] to pull it out.
    pub fn last_message(&self, to: &str) -> Option<String> {
        self.sent.lock().expect("LogMailer mutex poisoned").get(to).map(|m| m.text.clone())
    }

    /// Test-only accessor: how many messages have been sent to `to` — see
    /// [`SentMail::count`].
    pub fn message_count(&self, to: &str) -> usize {
        self.sent.lock().expect("LogMailer mutex poisoned").get(to).map(|m| m.count).unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl Mailer for LogMailer {
    async fn send(&self, to: &str, subject: &str, text: &str, _html: &str) -> CloudResult<()> {
        tracing::info!(email = %to, %subject, body = %text, "dev mailer: not actually sent (no RESEND_API_KEY)");
        let mut sent = self.sent.lock().expect("LogMailer mutex poisoned");
        let entry = sent.entry(to.to_string()).or_insert_with(|| SentMail { text: String::new(), count: 0 });
        entry.text = text.to_string();
        entry.count += 1;
        Ok(())
    }

    fn as_log_mailer(&self) -> Option<&LogMailer> {
        Some(self)
    }
}

/// Chooses the mailer for this process: [`ResendMailer`] when
/// `RESEND_API_KEY` is set, [`LogMailer`] otherwise.
pub fn build_mailer(config: &Config) -> std::sync::Arc<dyn Mailer> {
    match &config.resend_api_key {
        Some(key) => std::sync::Arc::new(ResendMailer::new(key.clone(), config.mail_from.clone())),
        None => {
            tracing::warn!("RESEND_API_KEY is not set; using LogMailer (verification links are logged, not emailed)");
            std::sync::Arc::new(LogMailer::new())
        }
    }
}

/// The verify-your-account email's subject, plain-text body, and HTML body,
/// given the full `<PIMBLE_CLOUD_PUBLIC_URL>/api/v1/verify?token=...` link.
pub fn verification_email(link: &str) -> (&'static str, String, String) {
    let subject = "Verify your Pimble account";
    let text = format!(
        "Verify your Pimble account by visiting this link:\n\n{link}\n\n\
         If you did not sign up, ignore this."
    );
    let html = format!(
        "<p>Verify your Pimble account by clicking the link below.</p>\
         <p><a href=\"{link}\">{link}</a></p>\
         <p>If you did not sign up, ignore this.</p>"
    );
    (subject, text, html)
}

/// The recover-your-account email's subject, plain-text body, and HTML
/// body, given the full `<PIMBLE_CLOUD_PUBLIC_URL>/app/recover?token=...`
/// link (docs/CRYPTO_CONTRACT.md "Phase 2a-2": subject "Recover your Pimble
/// account").
pub fn recovery_email(link: &str) -> (&'static str, String, String) {
    let subject = "Recover your Pimble account";
    let text = format!(
        "Recover your Pimble account by visiting this link (valid for one hour):\n\n{link}\n\n\
         You will need the recovery code you saved when you signed up. Without it, the notes \
         in your account cannot be decrypted by anyone, including us.\n\n\
         If you did not request this, ignore this."
    );
    let html = format!(
        "<p>Recover your Pimble account by clicking the link below (valid for one hour).</p>\
         <p><a href=\"{link}\">{link}</a></p>\
         <p>You will need the recovery code you saved when you signed up. Without it, the \
         notes in your account cannot be decrypted by anyone, including us.</p>\
         <p>If you did not request this, ignore this.</p>"
    );
    (subject, text, html)
}

// ── Sharing mails: every interpolated value is attacker-controlled ───────
//
// A share's name is typed by whoever owns it and the inviter's address by
// whoever signed up, and both end up in a message Pimble's own sending domain
// puts its name to. Untouched, a name could carry `\r\n` (a header, e.g. a
// `Bcc:`, injected into the subject) or markup (a convincing "reset your
// password" link, or a tracking image, in a mail that really does come from
// us). So: [`one_line`] before anything else, a cap so a mail cannot be
// padded out into something else entirely, and [`escape_html`] on every value
// that reaches the HTML body — including the link, ours though it is, since
// the address inside it came from a request.

/// Everything but the template itself in an HTML body goes through this.
/// `'` is escaped as `&#39;` (not `&apos;`, which older mail clients don't
/// know) so a value can never break out of a single-quoted attribute either.
fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// One line of ordinary text: every control character dropped (CR and LF
/// among them, which is what makes a subject header injectable), every run of
/// whitespace collapsed to one space, and the result trimmed. Public because
/// `POST /stores` puts a store's name through the same rule before storing
/// it — one definition of "a name", not two.
pub fn one_line(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    // Set when whitespace is seen after at least one real character, so a
    // leading run is dropped and a trailing one is never flushed: this trims
    // and collapses in the one pass.
    let mut pending_space = false;
    for c in value.chars() {
        if c.is_control() {
            continue;
        }
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// How much of a share's name a mail shows. Long enough for any name
/// somebody means, short enough that the name cannot become the message.
const MAIL_NAME_MAX_CHARS: usize = 80;

/// `value` cut to `max_chars` characters (not bytes — cutting a UTF-8 string
/// by byte offset would panic mid-character), with an ellipsis when it was.
fn capped(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// A share's name as a mail may show it.
fn display_name(share_name: &str) -> String {
    capped(&one_line(share_name), MAIL_NAME_MAX_CHARS)
}

/// What both sharing mails say about the encryption, in the two forms the
/// bodies need. Kept in one place so the invitation and the "shared with you"
/// mail can never drift into promising different things: Pimble Cloud holds
/// only ciphertext, so a share really does stay shut until one of the
/// sender's devices has been online to wrap the key to the recipient
/// (docs/SHARING_CONTRACT.md, "Key sweep").
const E2EE_TEXT: &str = "These notes are end-to-end encrypted: Pimble Cloud stores them without \
     being able to read them. They will open for you once {sender}'s Pimble has been online \
     once to hand over the key, which usually takes a moment.";
const E2EE_HTML: &str = "<p>These notes are end-to-end encrypted: Pimble Cloud stores them without \
     being able to read them. They will open for you once {sender}'s Pimble has been online \
     once to hand over the key, which usually takes a moment.</p>";

/// `sender` must already be escaped for the HTML form — only the substituted
/// value is ever escaped, never the template around it.
fn e2ee(template: &str, sender: &str) -> String {
    template.replace("{sender}", sender)
}

/// The you-have-been-invited email's subject, plain-text body, and HTML body
/// (docs/SHARING_CONTRACT.md, "Accounts service"): `link` is
/// `<PIMBLE_CLOUD_PUBLIC_URL>/app/signup?email=<urlencoded address>`, for an
/// address with no verified account yet.
pub fn invitation_email(inviter_email: &str, share_name: &str, link: &str) -> (String, String, String) {
    let inviter = one_line(inviter_email);
    let name = display_name(share_name);
    let subject = format!("{inviter} invited you to \"{name}\" on Pimble");
    let text = format!(
        "{inviter} invited you to \"{name}\" on Pimble.\n\n\
         Create your Pimble account with this address to open it:\n\n{link}\n\n\
         {e2ee}\n\n\
         If you were not expecting this, ignore this.",
        e2ee = e2ee(E2EE_TEXT, &inviter),
    );
    let html = format!(
        "<p>{inviter} invited you to \"{name}\" on Pimble.</p>\
         <p>Create your Pimble account with this address to open it:</p>\
         <p><a href=\"{link}\">{link}</a></p>\
         {e2ee}\
         <p>If you were not expecting this, ignore this.</p>",
        inviter = escape_html(&inviter),
        name = escape_html(&name),
        link = escape_html(link),
        e2ee = e2ee(E2EE_HTML, &escape_html(&inviter)),
    );
    (subject, text, html)
}

/// The it-is-waiting-for-you email's subject, plain-text body, and HTML body
/// (docs/SHARING_CONTRACT.md): `link` is `<PIMBLE_CLOUD_PUBLIC_URL>/app/`,
/// for an address that already has a verified account and now has a grant.
pub fn shared_with_you_email(inviter_email: &str, share_name: &str, link: &str) -> (String, String, String) {
    let inviter = one_line(inviter_email);
    let name = display_name(share_name);
    let subject = format!("{inviter} shared \"{name}\" with you on Pimble");
    let text = format!(
        "{inviter} shared \"{name}\" with you on Pimble.\n\n\
         Open it in Pimble here:\n\n{link}\n\n\
         {e2ee}\n\n\
         If you were not expecting this, ignore this.",
        e2ee = e2ee(E2EE_TEXT, &inviter),
    );
    let html = format!(
        "<p>{inviter} shared \"{name}\" with you on Pimble.</p>\
         <p>Open it in Pimble here:</p>\
         <p><a href=\"{link}\">{link}</a></p>\
         {e2ee}\
         <p>If you were not expecting this, ignore this.</p>",
        inviter = escape_html(&inviter),
        name = escape_html(&name),
        link = escape_html(link),
        e2ee = e2ee(E2EE_HTML, &escape_html(&inviter)),
    );
    (subject, text, html)
}

/// Pulls the first `http://` or `https://` URL out of a plain-text email
/// body. Used by tests against [`LogMailer::last_message`] to recover the
/// verification link without parsing HTML.
pub fn first_url(body: &str) -> Option<String> {
    body.split_whitespace().find(|w| w.starts_with("http://") || w.starts_with("https://")).map(|s| s.trim_end_matches(['.', ',']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A share's name is typed by its owner and reaches an address that never
    /// asked for anything, in a mail from Pimble's own sending domain. The
    /// worst plausible name — markup that would forge a link, plus a CRLF and
    /// a header to smuggle into the subject — must come out as text.
    const HOSTILE_NAME: &str = "<a href=\"https://evil.example\">click</a>\r\nBcc: x@y";

    fn every_control_is_a_newline(body: &str) -> bool {
        body.chars().filter(|c| c.is_control()).all(|c| c == '\n')
    }

    #[test]
    fn a_hostile_share_name_cannot_forge_markup_or_a_header() {
        let (subject, text, html) = invitation_email("ann@example.com", HOSTILE_NAME, "http://cloud.test/app/signup?email=b%40x.test");

        // The subject is one line: the CRLF (and the rest of the control
        // characters) are gone, so nothing after them can be read as a header.
        assert!(!subject.contains('\r') && !subject.contains('\n'), "the subject must be one line: {subject:?}");
        assert!(subject.chars().all(|c| !c.is_control()), "the subject must hold no control characters: {subject:?}");
        assert!(subject.contains("Bcc: x@y"), "the name's text survives, only its line breaks do not: {subject:?}");

        // Nothing interpolated into the HTML body is markup any more: the
        // name's tag is escaped, and no `<a` exists beyond the one this
        // template writes itself for the real link.
        assert!(!html.contains("<a href=\"https://evil.example\">"), "the name must not become a link: {html}");
        assert!(html.contains("&lt;a href=&quot;https://evil.example&quot;&gt;click&lt;/a&gt;"), "the name must be escaped: {html}");
        assert_eq!(html.matches("<a ").count(), 1, "exactly one link, ours: {html}");

        // The text body's only control character is the newline it lays out
        // with — the name contributes none.
        assert!(every_control_is_a_newline(&text), "the text body must hold no raw control characters: {text:?}");
        assert!(!text.contains('\r'));
    }

    #[test]
    fn a_long_share_name_is_cut_with_an_ellipsis() {
        let long = "x".repeat(200);
        let (subject, text, html) = shared_with_you_email("ann@example.com", &long, "http://cloud.test/app/");
        let cut = format!("{}…", "x".repeat(MAIL_NAME_MAX_CHARS));
        assert!(subject.contains(&cut), "the name should be capped at {MAIL_NAME_MAX_CHARS}: {subject}");
        assert!(!subject.contains(&"x".repeat(MAIL_NAME_MAX_CHARS + 1)), "no more than the cap: {subject}");
        assert!(text.contains(&cut));
        assert!(html.contains(&cut));
    }

    #[test]
    fn an_inviter_address_is_escaped_and_kept_to_one_line() {
        // An address that reached signup's `contains('@')` check and nothing
        // else: it is in the subject and the body of somebody else's mail.
        let (subject, _text, html) = invitation_email("<b>ann</b>\r\n@example.com", "Recipes", "http://cloud.test/app/signup?email=b%40x.test");
        assert!(!subject.contains('\r') && !subject.contains('\n'));
        assert!(!html.contains("<b>ann</b>"), "the inviter must not become markup: {html}");
        assert!(html.contains("&lt;b&gt;ann&lt;/b&gt;"));
        // Both the sentence at the top and the encryption sentence carry the
        // inviter, and both must be escaped.
        assert_eq!(html.matches("&lt;b&gt;ann&lt;/b&gt;").count(), 2, "{html}");
    }

    #[test]
    fn one_line_collapses_trims_and_strips() {
        assert_eq!(one_line("  Recipes \r\n and  Notes\t "), "Recipes and Notes");
        assert_eq!(one_line("\u{0}\u{7}"), "");
        assert_eq!(one_line("Recipes"), "Recipes");
    }

    #[test]
    fn escape_html_covers_every_character_that_could_break_out() {
        assert_eq!(escape_html("&<>\"'"), "&amp;&lt;&gt;&quot;&#39;");
        assert_eq!(escape_html("plain"), "plain");
    }
}
