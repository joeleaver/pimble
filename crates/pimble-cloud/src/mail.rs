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
}

#[async_trait::async_trait]
impl Mailer for LogMailer {
    async fn send(&self, to: &str, subject: &str, text: &str, _html: &str) -> CloudResult<()> {
        tracing::info!(email = %to, %subject, body = %text, "dev mailer: not actually sent (no RESEND_API_KEY)");
        self.sent.lock().expect("LogMailer mutex poisoned").insert(to.to_string(), SentMail { text: text.to_string() });
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

/// Pulls the first `http://` or `https://` URL out of a plain-text email
/// body. Used by tests against [`LogMailer::last_message`] to recover the
/// verification link without parsing HTML.
pub fn first_url(body: &str) -> Option<String> {
    body.split_whitespace().find(|w| w.starts_with("http://") || w.starts_with("https://")).map(|s| s.trim_end_matches(['.', ',']).to_string())
}
