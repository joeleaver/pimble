//! Small browser odds and ends shared by the backend loop and the pages.

use wasm_bindgen_futures::JsFuture;

/// `setTimeout` as a future, so a loop or a handler can yield to the browser.
pub async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Give the browser one frame before doing something that will hold the only
/// thread there is.
///
/// Argon2id at the contract's cost takes a noticeable fraction of a second in
/// wasm, and it blocks everything while it runs. A handler that sets a "busy"
/// signal and then derives in the same task paints neither: the effect updates
/// the DOM, the microtask starts Argon2, and the browser never gets a chance to
/// show the result. One turn through `setTimeout` is what makes the spinner real.
pub async fn yield_to_browser() {
    sleep_ms(16).await;
}

/// This page's query string as `(key, value)` pairs, already percent-decoded by
/// the URL parser. Empty when there is no query.
pub fn query_pairs() -> Vec<(String, String)> {
    let Some(window) = web_sys::window() else { return Vec::new() };
    let Ok(search) = window.location().search() else { return Vec::new() };
    let search = search.trim_start_matches('?');
    if search.is_empty() {
        return Vec::new();
    }
    search
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

/// One query parameter's value.
pub fn query_param(name: &str) -> Option<String> {
    query_pairs().into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// `%xx` and `+` decoding, enough for the banner parameters the accounts
/// service redirects with.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
