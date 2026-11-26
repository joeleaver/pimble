//! [`super::Http::websocket_close_code`] on the desktop: `tokio-tungstenite`
//! with the token as `Authorization: Bearer`.

use futures::StreamExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::{HeaderValue, AUTHORIZATION};
use tokio_tungstenite::tungstenite::Message;

use super::{FIRST_WORD_WAIT, SOCKET_CONNECT_TIMEOUT};

pub(super) async fn close_code(url: &str, token: &str) -> Option<u16> {
    let mut request = url.into_client_request().ok()?;
    let value = HeaderValue::from_str(&format!("Bearer {token}")).ok()?;
    request.headers_mut().insert(AUTHORIZATION, value);
    let Ok(Ok((mut socket, _))) = tokio::time::timeout(SOCKET_CONNECT_TIMEOUT, tokio_tungstenite::connect_async(request)).await else {
        return None;
    };
    let first = tokio::time::timeout(FIRST_WORD_WAIT, socket.next()).await;
    let code = match first {
        Ok(Some(Ok(Message::Close(Some(close))))) => Some(u16::from(close.code)),
        _ => None,
    };
    let _ = tokio::time::timeout(FIRST_WORD_WAIT, socket.close(None)).await;
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;
    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;

    /// A server that takes one connection, checks its bearer, and then does
    /// what `answer` says: close with that code, or say hello and wait.
    async fn serve_once(answer: Option<u16>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let check = |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(request.headers().get(AUTHORIZATION).and_then(|v| v.to_str().ok()), Some("Bearer the-token"));
                Ok(response)
            };
            let mut socket = tokio_tungstenite::accept_hdr_async(stream, check).await.unwrap();
            match answer {
                Some(code) => {
                    let frame = CloseFrame { code: CloseCode::from(code), reason: "owner offline".into() };
                    let _ = socket.close(Some(frame)).await;
                }
                None => {
                    let _ = socket.send(Message::Text("hello".into())).await;
                    let _ = socket.next().await;
                }
            }
        });
        format!("ws://{addr}/api/v1/relay/some-store")
    }

    #[tokio::test]
    async fn answers_the_code_a_socket_is_closed_with_first() {
        let url = serve_once(Some(4404)).await;
        assert_eq!(close_code(&url, "the-token").await, Some(4404));
    }

    #[tokio::test]
    async fn a_socket_that_says_something_else_first_has_no_code() {
        let url = serve_once(None).await;
        assert_eq!(close_code(&url, "the-token").await, None);
    }

    #[tokio::test]
    async fn a_socket_that_does_not_open_has_no_code() {
        // A port nothing listens on: bound, then let go.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert_eq!(close_code(&format!("ws://{addr}/"), "the-token").await, None);
    }
}
