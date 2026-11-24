//! Links from outside the app (`openInApp`, `subscribeAppLinks`,
//! `appLinkTaken`): a `pimble:` link this computer was asked to open (a click
//! in a browser or a mail app, or a second plain launch of Pimble) handed to
//! a Pimble window already open (docs/SETTINGS_CONTRACT.md section 12,
//! docs/NOTIFICATIONS_CONTRACT.md "Built in wave 4").

use serde::{Deserialize, Serialize};

/// `openInApp`: a `pimble:` link to hand to a Pimble window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenInAppRequest {
    pub url: String,
    /// The activation token the program that was asked to open the link was
    /// started with (Wayland's `XDG_ACTIVATION_TOKEN`), so the window that
    /// takes the link may come to the front with it. A server from before it
    /// ignores the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_token: Option<String>,
    /// Answer `delivered` only once the window says it took the link
    /// (`appLinkTaken`), and `false` if it has not within
    /// [`APP_LINK_CONFIRM_WAIT_MS`]: a window that is hung, or partway
    /// through quitting, still has its subscription open, so a link merely
    /// queued for it may never be shown. A server from before it ignores the
    /// field and answers as soon as the link is queued.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub confirm: bool,
}

/// How long `openInApp` with `confirm` waits for the window to say it took
/// the link.
pub const APP_LINK_CONFIRM_WAIT_MS: u64 = 2000;

/// What `subscribeAppLinks` hands a window: a link, the activation token it
/// came with, and, when the caller asked for confirmation, the number the
/// window answers `appLinkTaken` with once it has acted on the link. Sent as
/// the bare URL string when there is neither (what a window from before the
/// token reads), as `{ url, activation_token?, ack? }` otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandedLink {
    pub url: String,
    pub activation_token: Option<String>,
    pub ack: Option<u64>,
}

impl HandedLink {
    /// A link with no token and nothing to confirm.
    pub fn bare(url: impl Into<String>) -> HandedLink {
        HandedLink { url: url.into(), activation_token: None, ack: None }
    }
}

impl Serialize for HandedLink {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Full<'a> {
            url: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            activation_token: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            ack: Option<u64>,
        }
        if self.activation_token.is_none() && self.ack.is_none() {
            return serializer.serialize_str(&self.url);
        }
        Full { url: &self.url, activation_token: self.activation_token.as_deref(), ack: self.ack }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for HandedLink {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Url(String),
            Full {
                url: String,
                #[serde(default)]
                activation_token: Option<String>,
                #[serde(default)]
                ack: Option<u64>,
            },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Url(url) => HandedLink::bare(url),
            Wire::Full { url, activation_token, ack } => HandedLink { url, activation_token, ack },
        })
    }
}

/// Whether a window took the link (with `confirm`: said it acted on it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenInAppResponse {
    pub delivered: bool,
}

/// `appLinkTaken`: the window acted on the link handed with `ack` (it came
/// to the front, and opened what the link names).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppLinkTakenRequest {
    pub ack: u64,
}
