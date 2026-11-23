//! jsonrpsee's HTTP/WebSocket server, fed from the dispatch.
//!
//! [`into_rpc`] registers every entry of `METHODS` on a jsonrpsee
//! `RpcModule`, and each one calls the server through [`super::call`] or
//! [`super::subscribe`]. The wire is jsonrpsee's: the same `result` bytes
//! (serde_json of the same value), the same params errors (the same
//! decoding over the same `Params`), the same subscription ids,
//! notifications, rejections and unsubscribes, because a subscription's
//! decision is carried out on jsonrpsee's own `PendingSubscriptionSink`.
//!
//! This is the one place jsonrpsee-core's `server` feature is used, and so
//! the one place its `tokio::spawn` runs (each subscription's future;
//! jsonrpsee spawns it).

use jsonrpsee::core::StringError;
use jsonrpsee::types::ResponsePayload;
use jsonrpsee::{PendingSubscriptionSink, RpcModule, SubscriptionSink};
use serde_json::value::RawValue;

use super::{Decision, MethodKind, Opening, SubscriptionMessage, METHODS};
use crate::PimbleApiServer;

/// `server` as a jsonrpsee `RpcModule` answering every method of the API.
pub fn into_rpc<S: PimbleApiServer>(server: S) -> RpcModule<S> {
    let mut rpc = RpcModule::new(server);
    for method in METHODS {
        let name = method.name;
        let registered = match method.kind {
            MethodKind::Call => rpc
                .register_async_method(name, move |params, server, ext| async move {
                    match super::call(&*server, &ext, name, params.as_str()).await {
                        Ok(result) => ResponsePayload::<Box<RawValue>>::success(result),
                        Err(e) => ResponsePayload::error(e),
                    }
                })
                .map(|_| ()),
            MethodKind::Subscription { notification, unsubscribe } => rpc
                .register_subscription(name, notification, unsubscribe, move |params, pending, server, ext| async move {
                    let params = params.as_str().map(str::to_owned);
                    let Some(subscribing) = super::subscribe(server, ext, name, params) else {
                        return Ok(());
                    };
                    let (result, ()) = futures::future::join(subscribing.run, carry_out(subscribing.opening, pending)).await;
                    result.map_err(|e| StringError::from(e.0))
                })
                .map(|_| ()),
        };
        // `METHODS` is generated with a compile error for a name defined
        // twice, so this cannot fail.
        if let Err(e) = registered {
            panic!("registering {name}: {e}");
        }
    }
    rpc
}

/// Carry the server's decision out on jsonrpsee's sink, then forward every
/// notification to it until either side goes: the subscriber (its sink
/// closes; dropping `messages` tells the server) or the server (every one
/// of its sinks dropped; dropping jsonrpsee's ends the subscription, as the
/// server dropping it did when it held it itself).
async fn carry_out(opening: Opening, pending: PendingSubscriptionSink) {
    let Opening { decision, mut messages } = opening;
    match decision.await {
        Ok(Decision::Accept(reply)) => match pending.accept().await {
            Ok(sink) => {
                let _ = reply.send(true);
                forward(&mut messages, sink).await;
            }
            Err(_) => {
                let _ = reply.send(false);
            }
        },
        Ok(Decision::Reject(err)) => pending.reject(err).await,
        // Dropped undecided: so is jsonrpsee's, which answers as it always has.
        Err(_) => drop(pending),
    }
}

async fn forward(messages: &mut tokio::sync::mpsc::Receiver<SubscriptionMessage>, sink: SubscriptionSink) {
    loop {
        tokio::select! {
            msg = messages.recv() => {
                let Some(msg) = msg else { break };
                // A `Box<RawValue>` serializes as the text it holds.
                let Ok(msg) = jsonrpsee::SubscriptionMessage::from_json(&msg.into_raw()) else { break };
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            _ = sink.closed() => break,
        }
    }
}
