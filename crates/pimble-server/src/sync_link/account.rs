//! A plain link held through the signed-in Pimble Cloud account: a replica
//! of a plain store hosted on Pimble Cloud (`sync.json`'s `via_account`;
//! docs/ONE_CODEBASE_CONTRACT.md, "Built: wave 5, part 1").
//!
//! The same link as any other plain link, with the vault link's way of
//! reaching its remote: before every connect, how the account holds the
//! store is read again (`crate::grant::refresh`: a role the owner changed,
//! another share of it, a removal), and a token is minted from the account's
//! session (`crate::grant::mint`), so nothing in `credentials.json` is used
//! or written. While connected, the grant is asked about every
//! `RpcHandler::grant_check_every` and a change ends the connection, so the
//! next one is made with a token that carries the grant as it is now.

use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, StoreId};
use url::Url;

use crate::grant::GrantShape;
use crate::handler::RpcHandler;

/// A connection made with the account's token, and the grant it was made
/// under (`None` when the rows could not be read; nothing is concluded from
/// that).
pub(super) struct AccountConnection {
    pub client: PimbleClient,
    pub connected_as: Option<GrantShape>,
}

/// Read the grant again, mint a token and connect to where the store is
/// (`saved`, `sync.json`'s remote, unless the accounts service names an
/// endpoint of the store's own). Fails with `crate::grant::SessionExpired`
/// when the account's sign-in has expired, and with the mint's or the
/// connection's error otherwise.
pub(super) async fn connect(handler: &RpcHandler, store_id: StoreId, saved: &Url) -> anyhow::Result<AccountConnection> {
    if handler.link_faults().is_some_and(|faults| faults.is_offline()) {
        return Err(anyhow::anyhow!("this device has no network (a fault the test injected)"));
    }
    crate::grant::refresh(handler, store_id, "Sync link").await;
    let connected_as = crate::grant::grant_shape(handler, store_id).await;
    let (_, minted) = crate::grant::mint(handler).await?;
    let reach = minted.reach(store_id, Some(saved)).map_err(|e| anyhow::anyhow!(e))?;
    let auth = AuthMethod::Bearer { token: reach.token };
    let client = PimbleClient::connect_with_auth(reach.url.as_str(), &auth)
        .await
        .map_err(|e| anyhow::anyhow!("{}", pimble_client::describe_connect_error(&reach.url, &e)))?;
    Ok(AccountConnection { client, connected_as })
}

/// Whether the account's grant on the store is still the one `connected_as`
/// was read as. An answer that cannot be read keeps the connection.
pub(super) async fn grant_changed(handler: &RpcHandler, store_id: StoreId, connected_as: &Option<GrantShape>) -> bool {
    let now = crate::grant::grant_shape(handler, store_id).await;
    now.is_some() && now != *connected_as
}
