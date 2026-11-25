//! The accounts service's refusals that a person meets while signing in,
//! changing the password or making a new recovery code, said as whole
//! sentences (docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 4"). The
//! browser's worker server and the desktop's embedded one answer these, and
//! both pages show the answer as it is.
//!
//! A sign-in refused for an address with no account reads exactly as one
//! refused for a wrong password: the service answers `GET /kdf` with a decoy
//! for an unknown address and `POST /login` with the same 401 for both, so
//! nothing here can tell them apart, and nothing tries to. "Not verified" is
//! only answered once the password matched.

use super::CloudError;

/// What a person is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// `POST /login` answered 401: a wrong password, or no such account.
    #[error("That email and password do not match.")]
    WrongPassword,
    /// `POST /login` answered 403 `email_unverified` (the password matched).
    #[error("Check your inbox: this address has not been verified yet.")]
    Unverified,
    /// `POST /me/password` answered 401 `wrong_current_password`, or the
    /// current password did not open the keys the service holds.
    #[error("That is not your current password.")]
    WrongCurrentPassword,
    /// A call made with the session answered 401: the session expired, was
    /// signed out, or ended with a password change on another device.
    #[error("Your Pimble Cloud sign-in has ended. Sign in again.")]
    SignedOut,
    /// 429.
    #[error("Pimble Cloud asks to slow down. Wait a moment and try again.")]
    TooFast,
    /// A 5xx: the service, or what stands in front of it, failed.
    #[error("Pimble Cloud could not answer just now. Try again in a moment.")]
    Unavailable,
}

/// The `error` code of a refusal's body (`{ "error": "<code>", "message": ... }`).
fn error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(value.get("error")?.as_str()?.to_string())
}

fn refused(status: u16, refusal: Refusal) -> CloudError {
    CloudError::Refused { status, refusal }
}

/// What every account call shares: too fast, or the service failed.
fn common(e: CloudError) -> CloudError {
    match e {
        CloudError::Status { status: 429, .. } => refused(429, Refusal::TooFast),
        CloudError::Status { status, .. } if (500..600).contains(&status) => refused(status, Refusal::Unavailable),
        other => other,
    }
}

/// `GET /kdf` and `POST /login`, which carry no session.
pub(crate) fn signing_in(e: CloudError) -> CloudError {
    match e {
        CloudError::Status { status: 401, .. } => refused(401, Refusal::WrongPassword),
        CloudError::Status { status: 403, ref body, .. } if error_code(body).as_deref() == Some("email_unverified") => refused(403, Refusal::Unverified),
        other => common(other),
    }
}

/// A call made with the account's session (`GET /me/keys`,
/// `POST /me/password`, `POST /me/recovery-code`).
pub(crate) fn with_session(e: CloudError) -> CloudError {
    match e {
        CloudError::Status { status: 401, ref body, .. } if error_code(body).as_deref() == Some("wrong_current_password") => {
            refused(401, Refusal::WrongCurrentPassword)
        }
        CloudError::Status { status: 401, .. } => refused(401, Refusal::SignedOut),
        other => common(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answered(status: u16, body: &str) -> CloudError {
        CloudError::Status { url: "https://pimble.app/api/v1/login".into(), status, body: body.into() }
    }

    /// The service's own answers (`pimble-cloud`'s `CloudError::parts`).
    const WRONG: &str = r#"{"error":"unauthorized","message":"invalid email or password"}"#;
    const UNVERIFIED: &str = r#"{"error":"email_unverified","message":"Check your inbox for the verification link."}"#;
    const NO_SESSION: &str = r#"{"error":"unauthorized","message":"invalid session"}"#;
    const WRONG_CURRENT: &str = r#"{"error":"wrong_current_password","message":"current password is incorrect"}"#;

    #[test]
    fn a_refused_sign_in_is_a_sentence_that_does_not_say_whether_the_account_exists() {
        let wrong = signing_in(answered(401, WRONG));
        assert_eq!(wrong.to_string(), "That email and password do not match.");
        assert_eq!(wrong.status(), Some(401), "the status is still there for code that reads it");
        // An unknown address gets the decoy KDF and then this same 401.
        assert_eq!(signing_in(answered(401, WRONG)).to_string(), wrong.to_string());
        assert_eq!(signing_in(answered(401, "")).to_string(), wrong.to_string());

        let unverified = signing_in(answered(403, UNVERIFIED));
        assert_eq!(unverified.to_string(), "Check your inbox: this address has not been verified yet.");
        assert_eq!(unverified.status(), Some(403));

        assert_eq!(signing_in(answered(429, "")).to_string(), "Pimble Cloud asks to slow down. Wait a moment and try again.");
        assert_eq!(signing_in(answered(502, "Bad Gateway")).to_string(), "Pimble Cloud could not answer just now. Try again in a moment.");
        for refused in [wrong, unverified, signing_in(answered(500, ""))] {
            assert!(!refused.to_string().contains("answered"), "no raw answer: {refused}");
        }
    }

    #[test]
    fn a_refusal_with_the_session_says_which_it_was() {
        assert_eq!(with_session(answered(401, WRONG_CURRENT)).to_string(), "That is not your current password.");
        let ended = with_session(answered(401, NO_SESSION));
        assert_eq!(ended.to_string(), "Your Pimble Cloud sign-in has ended. Sign in again.");
        assert_eq!(ended.status(), Some(401));
        assert_eq!(with_session(answered(503, "")).to_string(), "Pimble Cloud could not answer just now. Try again in a moment.");
    }

    /// Anything else stays as it was, for the caller to say in its own words
    /// (a 400's message, a 409's) or to read the status of.
    #[test]
    fn other_answers_pass_through() {
        assert!(matches!(signing_in(answered(400, "{}")), CloudError::Status { status: 400, .. }));
        assert!(matches!(with_session(answered(409, "{}")), CloudError::Status { status: 409, .. }));
        assert!(matches!(with_session(answered(403, UNVERIFIED)), CloudError::Status { status: 403, .. }));
    }
}
