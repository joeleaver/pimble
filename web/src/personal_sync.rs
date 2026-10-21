//! The personal document in the browser (docs/DISCUSSIONS_CONTRACT.md
//! section 8): what the desktop's local server does for `cloudPersonalSync`,
//! done here with this page's account keys and session. Fetch the sealed
//! copy, open it, merge this page's in, seal it again and write it back
//! naming the version it replaces; again when another device wrote first.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::http;
use crate::session;

const PATH: &str = "/api/v1/me/personal";

#[derive(Serialize, Deserialize)]
struct PersonalBody {
    version: u64,
    blob: String,
}

/// `(merged, off)`: `off` when the person turned it off (here, or on
/// another device: an empty hosted copy says so), or why it is neither.
pub async fn sync(local: &[u8], enabled: bool, turn_on: bool) -> Result<(Vec<u8>, bool), String> {
    if !enabled {
        let _: serde_json::Value = match http::delete(PATH).await {
            Ok(v) => v,
            Err(e) if e.status == 404 || e.status == 204 => serde_json::Value::Null,
            Err(e) => return Err(e.to_string()),
        };
        return Ok((local.to_vec(), true));
    }
    let keys = session::keys().ok_or("this page is locked")?;
    let user = session::user_id().ok_or("this page is locked")?;
    let key = pimble_crypto::personal_key(&keys);
    let aad = pimble_crypto::personal_aad(&user);
    for _ in 0..4 {
        let remote: Option<PersonalBody> = match http::get(PATH).await {
            Ok(body) => Some(body),
            Err(e) if e.status == 404 => None,
            Err(e) => return Err(e.to_string()),
        };
        if remote.as_ref().is_some_and(|b| b.blob.is_empty()) && !turn_on {
            return Ok((local.to_vec(), true));
        }
        let (version, plain) = match &remote {
            Some(body) if body.blob.is_empty() => (body.version, Vec::new()),
            Some(body) => {
                let sealed = URL_SAFE_NO_PAD.decode(&body.blob).map_err(|e| e.to_string())?;
                (body.version, pimble_crypto::Blob::decrypt(&key, &aad, &sealed).map_err(|e| e.to_string())?)
            }
            None => (0, Vec::new()),
        };
        let merged = pimble_discussion::Personal::load(&plain);
        let before = merged.save();
        merged.merge(local);
        let after = merged.save();
        if remote.as_ref().is_some_and(|b| !b.blob.is_empty()) && after == before {
            return Ok((after, false));
        }
        let sealed = pimble_crypto::Blob::encrypt(&key, uuid::Uuid::nil(), &aad, &after);
        let body = PersonalBody { version, blob: URL_SAFE_NO_PAD.encode(sealed) };
        match http::put::<_, PersonalBody>(PATH, &body).await {
            Ok(_) => return Ok((after, false)),
            Err(e) if e.status == 409 => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("the personal document kept changing on another device".into())
}

const ENABLED_KEY: &str = "pimble.personal_sync";
const DOC_KEY: &str = "pimble.personal";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The switch, as this browser keeps it (the app's own `persistence` key).
pub fn enabled() -> bool {
    storage().and_then(|s| s.get_item(ENABLED_KEY).ok().flatten()).map(|v| v != "false").unwrap_or(true)
}

/// The account page's switch: off empties the hosted copy (and so turns it
/// off on every device of theirs); on writes this browser's copy over it.
pub fn set_enabled(on: bool) {
    if let Some(s) = storage() {
        let _ = s.set_item(ENABLED_KEY, if on { "true" } else { "false" });
    }
    let local = storage()
        .and_then(|s| s.get_item(DOC_KEY).ok().flatten())
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .unwrap_or_default();
    wasm_bindgen_futures::spawn_local(async move {
        match sync(&local, on, on).await {
            Ok((merged, false)) => {
                if let Some(s) = storage() {
                    let _ = s.set_item(DOC_KEY, &base64::engine::general_purpose::STANDARD.encode(merged));
                }
            }
            Ok((_, true)) => {}
            Err(e) => tracing::warn!("personal document: {e}"),
        }
    });
}
