//! A member's side of a relayed store (docs/RELAY_CONTRACT.md): whether the
//! relay says the store's owner is not there. The same on every host: the
//! question goes through the host's [`Http`] seam
//! (`Http::websocket_close_code`), which on the desktop sends the token as a
//! header and in a browser's worker as the `access_token` parameter.

use pimble_host::http::Http;
use url::Url;

/// The relay's close code for a member's connection to a store nobody is
/// serving right now (`pimble-cloud`'s `relay::CLOSE_OWNER_OFFLINE`).
pub(crate) const CLOSE_OWNER_OFFLINE: u16 = 4404;

/// Whether the relay at `url` says the store's owner is not there (close
/// code 4404, `owner offline`). The JSON-RPC client a vault link connects
/// with does not pass a close frame's code on, so a link whose connection to
/// a relayed store just failed asks here, with the token it had, to tell
/// "the owner's computer is off" (an ordinary `Offline`) from something
/// worth a warning. Anything else, a refusal before the upgrade included, is
/// `false`.
pub(crate) async fn owner_is_offline(http: &dyn Http, url: &Url, token: &str) -> bool {
    http.websocket_close_code(url.as_str(), token).await == Some(CLOSE_OWNER_OFFLINE)
}
