//! [`super::Http::websocket_close_code`] in a browser (a worker, or the
//! page): the global `WebSocket`, with the token as the `access_token` query
//! parameter because a browser's WebSocket cannot set a header.

use futures::channel::mpsc;
use futures::future::{self, Either};
use futures::StreamExt;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use super::{FIRST_WORD_WAIT, SOCKET_CONNECT_TIMEOUT};

/// What the socket did, in the order it did it.
enum Said {
    Opened,
    /// Closed, with its close code. A socket closed before it opened (the
    /// handshake refused, or no network) is closed with 1006 by the browser.
    Closed(u16),
    Message,
    Failed,
}

pub(super) async fn close_code(url: &str, token: &str) -> Option<u16> {
    let address = web_sys::Url::new(url).ok()?;
    address.search_params().set("access_token", token);
    let socket = web_sys::WebSocket::new(&address.href()).ok()?;
    let (tx, mut said) = mpsc::unbounded::<Said>();

    let opened = {
        let tx = tx.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
            let _ = tx.unbounded_send(Said::Opened);
        })
    };
    let closed = {
        let tx = tx.clone();
        Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |event: web_sys::CloseEvent| {
            let _ = tx.unbounded_send(Said::Closed(event.code()));
        })
    };
    let message = {
        let tx = tx.clone();
        Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
            let _ = tx.unbounded_send(Said::Message);
        })
    };
    let failed = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
        let _ = tx.unbounded_send(Said::Failed);
    });
    socket.set_onopen(Some(opened.as_ref().unchecked_ref()));
    socket.set_onclose(Some(closed.as_ref().unchecked_ref()));
    socket.set_onmessage(Some(message.as_ref().unchecked_ref()));
    socket.set_onerror(Some(failed.as_ref().unchecked_ref()));

    // Only a socket that opened has a close code worth reading: one refused
    // before the upgrade is the browser's 1006, which says nothing of who
    // refused it or why.
    let open = within(SOCKET_CONNECT_TIMEOUT, async { matches!(said.next().await, Some(Said::Opened)) }).await.unwrap_or(false);
    let code = if open {
        match within(FIRST_WORD_WAIT, said.next()).await {
            Some(Some(Said::Closed(code))) => Some(code),
            _ => None,
        }
    } else {
        None
    };

    socket.set_onopen(None);
    socket.set_onclose(None);
    socket.set_onmessage(None);
    socket.set_onerror(None);
    let _ = socket.close();
    drop((opened, closed, message, failed));
    code
}

/// `work`'s answer if it comes within `wait`.
async fn within<T>(wait: std::time::Duration, work: impl std::future::Future<Output = T>) -> Option<T> {
    let timer = gloo_timers::future::TimeoutFuture::new(u32::try_from(wait.as_millis()).unwrap_or(u32::MAX));
    match future::select(Box::pin(work), timer).await {
        Either::Left((answer, _)) => Some(answer),
        Either::Right(_) => None,
    }
}
