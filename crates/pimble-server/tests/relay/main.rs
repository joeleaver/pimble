//! End-to-end tests for the relay tier (docs/RELAY_CONTRACT.md): a store that
//! is **not** hosted on Pimble Cloud, shared from its owner's computer.
//!
//! The environment is `tests/share.rs`'s, with the accounts-service stub
//! grown by what the relay tier adds to the real service (`pimble-cloud`'s
//! `src/relay.rs` and `routes/stores.rs` are the authority on the wire):
//!
//! - `POST /stores` with `tier: "relay"` records a row and calls nobody;
//!   `GET /stores` rows carry `tier`; `DELETE /stores/{id}` removes the
//!   record and withdraws the store from the relay.
//! - `POST /token` lists every relay-tier store the account holds a grant on
//!   in `stores`, each with its own endpoint and **a token cut down to that
//!   one store**, which is the only token the relay takes for it.
//! - The relay itself, in memory: the owner's tunnel at `/api/v1/relay`
//!   (session auth, `{"serve": [...]}` / `{"serving": [...]}`, binary
//!   `[conn u32 BE][kind u8][payload]` frames) and members' connections at
//!   `/api/v1/relay/<store id>` (4404 `owner offline` with no tunnel).
//!
//! `H`, the hosted Pimble server, is there only to be looked at: nothing a
//! test here does may put anything on it.
//!
//! What is real: every "desktop" `PimbleServer`, the relay face each owner's
//! server starts inside itself, the tunnel client, the vault links, the
//! share upkeep, and the JWT verification between all of them.

#[path = "../common/settle.rs"]
mod settle;

mod accounts_stub;
mod env;
mod helpers;
mod relay_stub;

mod large;
mod pictures;
mod sharing;
mod twin;

use accounts_stub::*;
use env::*;
use helpers::*;
use relay_stub::*;
use settle::*;

use pimble_host::disk::testfs as tfs;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey, Verifier};
use futures::{SinkExt, StreamExt};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, Place, RelaySide, StoreAccess, StoreId, StoreKind, SyncState};
use pimble_crdt::NodeDoc;
use pimble_crypto::{derive_password_keys, wrap_account_keys, AccountKeyBlob, AccountKeys, KdfParams, KeyEnvelope};
use pimble_rpc::{EditOperation, MemberRole, VaultDocId};
use pimble_server::{PimbleServer, ServerConfig};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use uuid::Uuid;
