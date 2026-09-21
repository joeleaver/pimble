//! Pimble CLI - Command-line interface for debugging and management

use std::path::PathBuf;

use anyhow::{Context, Result};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, MountRef, MountState, NodeId, RemoteEndpoint, StoreId, StoreKind};
use base64::Engine;
use pimble_crdt::NodeDoc;
use pimble_rpc::{EditOperation, VaultDocId};
use pimble_server::auth;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("pimble=info".parse()?))
        .init();

    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_help();
        return Ok(());
    }

    let command = &args[1];

    match command.as_str() {
        "help" | "--help" | "-h" => print_help(),
        "server" => {
            let parsed = match parse_server_args(&args[2..]) {
                Ok(parsed) => parsed,
                Err(e) => {
                    eprintln!("{}", e);
                    eprintln!(
                        "Usage: pimble-cli server [--addr HOST:PORT] [--open PATH]... [--token-file PATH] \
                         [--jwks URL --issuer ISS] [--allow-origin ORIGIN]... [--stores-dir DIR]"
                    );
                    return Ok(());
                }
            };
            run_server(parsed).await?;
        }
        "token" => {
            let new = args.get(2).map(|s| s == "--new").unwrap_or(false);
            token_command(new)?;
        }
        "create-store" => {
            let (rest, kind) = extract_flag_value(&args[2..], "--kind");
            let (rest, id) = extract_flag_value(&rest, "--id");
            if rest.len() < 2 {
                eprintln!("Usage: pimble-cli create-store <path> <name> [--kind plain|vault] [--id <uuid>]");
                return Ok(());
            }
            create_store(&rest[0], &rest[1], kind, id).await?;
        }
        "vault-list" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli vault-list <store-id>");
                return Ok(());
            }
            vault_list(&args[2]).await?;
        }
        "vault-fetch" => {
            let (rest, after) = extract_flag_value(&args[2..], "--after");
            if rest.len() < 2 {
                eprintln!("Usage: pimble-cli vault-fetch <store-id> <doc> [--after N]");
                return Ok(());
            }
            let after_seq = after.map(|s| s.parse::<u64>()).transpose().context("--after must be a number")?.unwrap_or(0);
            vault_fetch(&rest[0], &rest[1], after_seq).await?;
        }
        "vault-append" => {
            if args.len() < 5 {
                eprintln!("Usage: pimble-cli vault-append <store-id> <doc> <file>");
                return Ok(());
            }
            vault_append(&args[2], &args[3], &args[4]).await?;
        }
        "vault-snapshot" => {
            if args.len() < 6 {
                eprintln!("Usage: pimble-cli vault-snapshot <store-id> <doc> <upto-seq> <file>");
                return Ok(());
            }
            vault_snapshot(&args[2], &args[3], &args[4], &args[5]).await?;
        }
        "delete-vault-store" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli delete-vault-store <store-id>");
                return Ok(());
            }
            delete_vault_store(&args[2]).await?;
        }
        "scopes" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli scopes <store-id>");
                return Ok(());
            }
            scopes(&args[2]).await?;
        }
        "set-scope" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli set-scope <store-id> <root-node-id> [<doc-node-id>...]");
                return Ok(());
            }
            set_scope(&args[2], &args[3], &args[4..], false).await?;
        }
        "remove-scope" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli remove-scope <store-id> <root-node-id>");
                return Ok(());
            }
            set_scope(&args[2], &args[3], &[], true).await?;
        }
        "import-scrivener" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli import-scrivener <scrivener-project.scriv> <output.pimble>");
                return Ok(());
            }
            import_scrivener(&args[2], &args[3]).await?;
        }
        "list-stores" => list_stores().await?,
        "open-store" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli open-store <path>");
                return Ok(());
            }
            open_store(&args[2]).await?;
        }
        "create-node" => {
            if args.len() < 6 {
                eprintln!("Usage: pimble-cli create-node <store-id> <parent-id> <type> <title>");
                return Ok(());
            }
            create_node(&args[2], &args[3], &args[4], &args[5]).await?;
        }
        "move-node" => {
            if args.len() < 5 {
                eprintln!("Usage: pimble-cli move-node <store-id> <node-id> <new-parent-id>");
                return Ok(());
            }
            move_node(&args[2], &args[3], &args[4]).await?;
        }
        "delete-node" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli delete-node <store-id> <node-id>");
                return Ok(());
            }
            delete_node(&args[2], &args[3]).await?;
        }
        "undelete-node" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli undelete-node <store-id> <node-id>");
                return Ok(());
            }
            undelete_node(&args[2], &args[3]).await?;
        }
        "set-node-text" => {
            if args.len() < 5 {
                eprintln!("Usage: pimble-cli set-node-text <store-id> <node-id> <text>");
                return Ok(());
            }
            // Join any remaining args so a multi-word text doesn't need quoting tricks
            // beyond normal shell quoting of args[4].
            let text = args[4..].join(" ");
            set_node_text(&args[2], &args[3], &text).await?;
        }
        "show-node" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli show-node <store-id> <node-id>");
                return Ok(());
            }
            show_node(&args[2], &args[3]).await?;
        }
        "search" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli search <query>");
                return Ok(());
            }
            // Join remaining args so a multi-word query doesn't need quoting
            // tricks beyond normal shell quoting of args[2].
            let query = args[2..].join(" ");
            search(&query).await?;
        }
        "rebuild-index" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli rebuild-index <store-id>");
                return Ok(());
            }
            rebuild_index(&args[2]).await?;
        }
        "create-mount" => {
            if args.len() < 6 {
                eprintln!("Usage: pimble-cli create-mount <store-id> <parent-id> <source-store-id> <source-node-id> [title]");
                return Ok(());
            }
            let title = if args.len() > 6 { Some(args[6..].join(" ")) } else { None };
            create_mount(&args[2], &args[3], &args[4], &args[5], title).await?;
        }
        "mount-state" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli mount-state <store-id> <node-id>");
                return Ok(());
            }
            mount_state(&args[2], &args[3]).await?;
        }
        "mount-remote-store" => {
            let (rest, token) = extract_flag_value(&args[2..], "--token");
            if rest.len() < 4 {
                eprintln!("Usage: pimble-cli mount-remote-store <store-id> <parent-id> <url> <remote-store-id> [title] [--token T]");
                return Ok(());
            }
            let title = if rest.len() > 4 { Some(rest[4..].join(" ")) } else { None };
            mount_remote_store(&rest[0], &rest[1], &rest[2], &rest[3], title, token).await?;
        }
        "list-children" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli list-children <store-id> <node-id>");
                return Ok(());
            }
            list_children(&args[2], &args[3]).await?;
        }
        "add-remote-store" => {
            let (rest, token) = extract_flag_value(&args[2..], "--token");
            if rest.len() < 2 {
                eprintln!("Usage: pimble-cli add-remote-store <url> <remote-store-id> [path] [--token T]");
                return Ok(());
            }
            let path = rest.get(2).map(PathBuf::from);
            add_remote_store(&rest[0], &rest[1], path, token).await?;
        }
        "link-store" => {
            let (rest, token) = extract_flag_value(&args[2..], "--token");
            if rest.len() < 2 {
                eprintln!("Usage: pimble-cli link-store <store-id> <url> [--token T]");
                return Ok(());
            }
            link_store(&rest[0], &rest[1], token).await?;
        }
        "unlink-store" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli unlink-store <store-id>");
                return Ok(());
            }
            unlink_store(&args[2]).await?;
        }
        "sync-state" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli sync-state <store-id>");
                return Ok(());
            }
            sync_state(&args[2]).await?;
        }
        "remote-stores" => {
            let (rest, token) = extract_flag_value(&args[2..], "--token");
            if rest.is_empty() {
                eprintln!("Usage: pimble-cli remote-stores <url> [--token T]");
                return Ok(());
            }
            remote_stores(&rest[0], token).await?;
        }
        "remove-replica" => {
            let (rest, force) = extract_switch(&args[2..], "--force");
            if rest.is_empty() {
                eprintln!("Usage: pimble-cli remove-replica <store-id> [--force]");
                return Ok(());
            }
            remove_replica(&rest[0], force).await?;
        }
        "cloud-sign-in" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli cloud-sign-in <url> <email>  (password via PIMBLE_CLOUD_PASSWORD or a prompt)");
                return Ok(());
            }
            cloud_sign_in(&args[2], &args[3]).await?;
        }
        "cloud-status" => cloud_status().await?,
        "cloud-sign-out" => cloud_sign_out().await?,
        "cloud-host-store" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli cloud-host-store <store-id>");
                return Ok(());
            }
            cloud_host_store(&args[2]).await?;
        }
        "cloud-list-hosted" => cloud_list_hosted().await?,
        "cloud-add-hosted" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli cloud-add-hosted <store-id>");
                return Ok(());
            }
            cloud_add_hosted(&args[2]).await?;
        }
        "cloud-share" => {
            let (rest, name) = extract_flag_value(&args[2..], "--name");
            if rest.len() < 2 {
                eprintln!("Usage: pimble-cli cloud-share <store-id> <node-id> [--name <name>]");
                return Ok(());
            }
            cloud_share(&rest[0], &rest[1], name).await?;
        }
        "cloud-share-info" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli cloud-share-info <store-id> <node-id>");
                return Ok(());
            }
            cloud_share_info(&args[2], &args[3]).await?;
        }
        "cloud-share-invite" => {
            if args.len() < 6 {
                eprintln!("Usage: pimble-cli cloud-share-invite <store-id> <node-id> <email> <editor|reader>");
                return Ok(());
            }
            cloud_share_invite(&args[2], &args[3], &args[4], &args[5]).await?;
        }
        "cloud-share-remove" => {
            if args.len() < 5 {
                eprintln!("Usage: pimble-cli cloud-share-remove <store-id> <node-id> <email>");
                return Ok(());
            }
            cloud_share_remove(&args[2], &args[3], &args[4]).await?;
        }
        "cloud-stop-sharing" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli cloud-stop-sharing <store-id> <node-id>");
                return Ok(());
            }
            cloud_stop_sharing(&args[2], &args[3]).await?;
        }
        _ => {
            eprintln!("Unknown command: {}", command);
            print_help();
        }
    }

    Ok(())
}

fn print_help() {
    println!(
        r#"Pimble CLI - Personal Information Manager

USAGE:
    pimble-cli <COMMAND> [OPTIONS]

COMMANDS:
    help                Show this help message
    server              Start the Pimble server
    token               Print this machine's default server token (creating it if needed)
    create-store        Create a new store (--kind plain|vault, --id <uuid>)
    open-store          Open an existing store
    list-stores         List all open stores
    import-scrivener    Import a Scrivener .scriv project into a Pimble store
    create-node         Create a node in a store
    move-node           Move a node under a new parent (appended last)
    delete-node         Delete a node and its whole subtree (a tombstone)
    undelete-node       Bring a deleted node and its subtree back
    set-node-text       Set a node's content from plain text
    show-node           Print a node's metadata and content text
    search              Search across all open stores
    rebuild-index       Rebuild a store's search index from scratch
    create-mount        Mount a subtree from one store under a node in another
    mount-state         Show a mount point's resolution state
    mount-remote-store  Add a remote's store as a replica and mount it here
    list-children       List a node's children (resolves mounts)
    add-remote-store    Create a local replica of a remote's store and link it
    link-store          Link an existing local store to its twin on a remote
    unlink-store        Unlink a store from its remote (stops the sync link)
    sync-state          Show a store's replica sync link and its state
    remote-stores       List the stores a remote Pimble server has open
    remove-replica      Stop a replica's sync link, close it, and delete it
    vault-list          List a vault store's documents with their heads
    vault-fetch         Fetch a vault document's snapshot and updates
    vault-append        Append a file's bytes as a vault document update
    vault-snapshot      Store a file's bytes as a vault document snapshot
    delete-vault-store  Close a vault store and delete its directory (hosted side)
    scopes              List a store's shares: each root and the documents under it
    set-scope           Publish a share's scope: the documents under its root
    remove-scope        Remove a share's scope
    cloud-sign-in       Sign in to a Pimble Cloud account
    cloud-status        Show whether a Pimble Cloud account is signed in
    cloud-sign-out      Forget the signed-in Pimble Cloud account
    cloud-host-store    Host a local store's encrypted twin on Pimble Cloud
    cloud-list-hosted   List the stores the signed-in account has a grant on
    cloud-add-hosted    Add an already-hosted store as a local replica
    cloud-share         Share a node of a hosted store (--name <name>; the node's title otherwise)
    cloud-share-info    Show a shared node's share, its state here and its members
    cloud-share-invite  Invite an address to a share as editor or reader, or change its role
    cloud-share-remove  Remove a member or a pending invitation from a share
    cloud-stop-sharing  Stop sharing a node (its documents stay hosted where they are)

ENVIRONMENT (client, for every command but `server` itself):
    PIMBLE_SERVER       Server URL (default: http://127.0.0.1:7462)
    PIMBLE_TOKEN        Bearer token sent with every request to PIMBLE_SERVER.
                        If unset and PIMBLE_SERVER is loopback, the default
                        server token file's token is used if it exists.
    PIMBLE_CLOUD_PASSWORD  Password for `cloud-sign-in`; prompted for if unset.

ENVIRONMENT (server, `pimble-cli server` only; every flag below wins over its
env fallback):
    PIMBLE_ADDR         Same as --addr.
    PIMBLE_SERVER_TOKEN The static token's value directly (--token-file reads
                        one from a file instead; either enables the check).
    PIMBLE_JWKS_URL     Same as --jwks.
    PIMBLE_JWT_ISSUER   Same as --issuer.
    PIMBLE_ALLOW_ORIGINS  Same as --allow-origin, comma separated.
    PIMBLE_STORES_DIR   Same as --stores-dir.

SERVER OPTIONS (`pimble-cli server`):
    --addr HOST:PORT    Address to bind (default: 127.0.0.1:7462).
    --open PATH         Open this store at start (repeatable).
    --token-file PATH   Static-token file; created with a fresh token if
                        missing. Enables the bearer/API-key check even on
                        loopback.
    --jwks URL          JWKS endpoint for JWT verification (needs --issuer).
    --issuer ISS        Required `iss` claim (needs --jwks). Audience is
                        always `pimble`.
    --allow-origin ORIGIN  A WebSocket `Origin` to accept (repeatable); any
                        other Origin is refused, and an Origin at all is
                        refused when this is never given.
    --stores-dir DIR    Open every `*.pimble` directory directly inside DIR
                        at start.

EXAMPLES:
    pimble-cli server
    pimble-cli server --addr 0.0.0.0:7462 --open /srv/family.pimble --token-file /etc/pimble/token
    pimble-cli server --addr 0.0.0.0:7462 --jwks https://auth.example/.well-known/jwks.json --issuer https://auth.example --allow-origin https://app.example --stores-dir /srv/stores
    pimble-cli token
    pimble-cli token --new
    pimble-cli create-store ./my-notes.pimble "My Notes"
    pimble-cli open-store ./my-notes.pimble
    pimble-cli list-stores
    pimble-cli import-scrivener ./project.scriv ./project.pimble
    pimble-cli create-node <store-id> <parent-id> document "My Note"
    pimble-cli move-node <store-id> <node-id> <new-parent-id>
    pimble-cli delete-node <store-id> <node-id>
    pimble-cli undelete-node <store-id> <node-id>
    pimble-cli set-node-text <store-id> <node-id> "Hello, world"
    pimble-cli show-node <store-id> <node-id>
    pimble-cli search "hello"
    pimble-cli rebuild-index <store-id>
    pimble-cli create-mount <store-id> <parent-id> <source-store-id> <source-node-id> "My Mount"
    pimble-cli mount-state <store-id> <node-id>
    pimble-cli mount-remote-store <store-id> <parent-id> http://127.0.0.1:7463 <remote-store-id> "Family" --token secret
    pimble-cli list-children <store-id> <node-id>
    pimble-cli add-remote-store http://127.0.0.1:7463 <remote-store-id> --token secret
    pimble-cli link-store <store-id> http://127.0.0.1:7463 --token secret
    pimble-cli unlink-store <store-id>
    pimble-cli sync-state <store-id>
    pimble-cli remote-stores http://127.0.0.1:7463 --token secret
    pimble-cli remove-replica <store-id> --force
    pimble-cli cloud-sign-in https://pimble.app alice@example.com
    pimble-cli cloud-status
    pimble-cli cloud-host-store <store-id>
    pimble-cli cloud-list-hosted
    pimble-cli cloud-add-hosted <store-id>
    pimble-cli cloud-share <store-id> <node-id> --name "Holiday Plans"
    pimble-cli cloud-share-invite <store-id> <node-id> bob@example.com editor
    pimble-cli cloud-share-info <store-id> <node-id>
    pimble-cli cloud-share-remove <store-id> <node-id> bob@example.com
    pimble-cli cloud-stop-sharing <store-id> <node-id>
    pimble-cli cloud-sign-out
"#
    );
}

/// Parsed `server` flags plus their env fallbacks resolved
/// (docs/CLOUD_CONTRACT.md "B: pimble-server" item 7: flags win over env).
struct ServerArgs {
    addr: String,
    open_paths: Vec<PathBuf>,
    token_file: Option<PathBuf>,
    /// The static token's literal value (`PIMBLE_SERVER_TOKEN`; there is no
    /// flag for the value itself, only `--token-file` for a file). Only
    /// consulted when `token_file` is `None`.
    server_token_env: Option<String>,
    jwks_url: Option<String>,
    jwt_issuer: Option<String>,
    allow_origins: Vec<String>,
    stores_dir: Option<PathBuf>,
}

/// Parse `server`'s own flags out of the args following the `server` word,
/// then fill in whatever wasn't given from its env fallback
/// (docs/CLOUD_CONTRACT.md "B: pimble-server" item 7): `--addr` /
/// `PIMBLE_ADDR`; `--token-file` / `PIMBLE_SERVER_TOKEN` (the token's own
/// value, not a file: `--token-file` still works as before); `--jwks` /
/// `PIMBLE_JWKS_URL`; `--issuer` / `PIMBLE_JWT_ISSUER`; repeatable
/// `--allow-origin` / `PIMBLE_ALLOW_ORIGINS` (comma separated); `--stores-dir`
/// / `PIMBLE_STORES_DIR`. `--open PATH` (repeatable) has no env form.
fn parse_server_args(rest: &[String]) -> std::result::Result<ServerArgs, String> {
    let mut addr = None;
    let mut open_paths = Vec::new();
    let mut token_file = None;
    let mut jwks_url = None;
    let mut jwt_issuer = None;
    let mut allow_origins = Vec::new();
    let mut stores_dir = None;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--addr" => {
                i += 1;
                addr = Some(rest.get(i).ok_or("--addr requires a value")?.clone());
            }
            "--open" => {
                i += 1;
                let path = rest.get(i).ok_or("--open requires a value")?;
                open_paths.push(PathBuf::from(path));
            }
            "--token-file" => {
                i += 1;
                let path = rest.get(i).ok_or("--token-file requires a value")?;
                token_file = Some(PathBuf::from(path));
            }
            "--jwks" => {
                i += 1;
                jwks_url = Some(rest.get(i).ok_or("--jwks requires a value")?.clone());
            }
            "--issuer" => {
                i += 1;
                jwt_issuer = Some(rest.get(i).ok_or("--issuer requires a value")?.clone());
            }
            "--allow-origin" => {
                i += 1;
                allow_origins.push(rest.get(i).ok_or("--allow-origin requires a value")?.clone());
            }
            "--stores-dir" => {
                i += 1;
                let path = rest.get(i).ok_or("--stores-dir requires a value")?;
                stores_dir = Some(PathBuf::from(path));
            }
            other => return Err(format!("Unknown server option: {}", other)),
        }
        i += 1;
    }

    let addr = addr.or_else(|| std::env::var("PIMBLE_ADDR").ok()).unwrap_or_else(|| "127.0.0.1:7462".to_string());
    let jwks_url = jwks_url.or_else(|| std::env::var("PIMBLE_JWKS_URL").ok());
    let jwt_issuer = jwt_issuer.or_else(|| std::env::var("PIMBLE_JWT_ISSUER").ok());
    let server_token_env = std::env::var("PIMBLE_SERVER_TOKEN").ok().filter(|t| !t.is_empty());
    // Flags accumulate rather than override one-for-one, so a repeatable
    // flag either wins outright (any given) or falls back to the env list
    // entirely — never a merge of the two.
    let allow_origins = if !allow_origins.is_empty() {
        allow_origins
    } else {
        std::env::var("PIMBLE_ALLOW_ORIGINS")
            .ok()
            .map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default()
    };
    let stores_dir = stores_dir.or_else(|| std::env::var("PIMBLE_STORES_DIR").ok().map(PathBuf::from));

    Ok(ServerArgs { addr, open_paths, token_file, server_token_env, jwks_url, jwt_issuer, allow_origins, stores_dir })
}

/// Pull `flag`'s value out of `args` (its first occurrence; `flag value`),
/// returning the remaining positional args in order and the value if the
/// flag was present. Used for `--token T` on the replica-sync commands,
/// which otherwise take only positional arguments.
fn extract_flag_value(args: &[String], flag: &str) -> (Vec<String>, Option<String>) {
    let mut rest = Vec::new();
    let mut value = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag && value.is_none() {
            i += 1;
            value = args.get(i).cloned();
        } else {
            rest.push(args[i].clone());
        }
        i += 1;
    }
    (rest, value)
}

/// Like [`extract_flag_value`], but for a boolean switch (`--force`) that
/// takes no value.
fn extract_switch(args: &[String], flag: &str) -> (Vec<String>, bool) {
    let mut rest = Vec::new();
    let mut present = false;
    for a in args {
        if a == flag {
            present = true;
        } else {
            rest.push(a.clone());
        }
    }
    (rest, present)
}

/// Every `*.pimble` directory directly inside `dir` (docs/CLOUD_CONTRACT.md
/// "B: pimble-server" item 6): what `pimble-cli server --stores-dir DIR`
/// opens at start, in addition to any `--open PATH`. A `dir` that doesn't
/// exist or can't be listed yields nothing rather than an error — the
/// accounts service is expected to have created it before pointing a server
/// at it, but an empty/missing directory on first run shouldn't stop the
/// server from starting.
fn pimble_stores_in(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut stores: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.extension().is_some_and(|ext| ext == "pimble"))
        .collect();
    stores.sort();
    stores
}

/// Start the embedded Pimble server and open every store `args` names, both
/// explicit (`--open PATH`, so a headless replica host can run as
/// `pimble-cli server --addr 0.0.0.0:7462 --open /srv/family.pimble`) and
/// discovered (`--stores-dir DIR`, item 6) — opened the same way any other
/// client would, over a loopback connection to the server it just started.
///
/// The static-token precedence (item 7): `--token-file PATH` (flag) wins;
/// else `PIMBLE_SERVER_TOKEN`'s literal value; else, if `addr` isn't
/// loopback and no JWT verifier is configured either, the default token
/// file ([`auth::default_token_path`]) — `PimbleServer::start` would
/// otherwise refuse to bind. A JWT verifier alone (`--jwks`/`--issuer`) also
/// satisfies that bind check, so this never force-creates a token file a
/// JWT-only deployment has no use for.
async fn run_server(args: ServerArgs) -> Result<()> {
    use pimble_server::{PimbleServer, ServerConfig};

    let socket_addr: std::net::SocketAddr = args.addr.parse().with_context(|| format!("Invalid --addr {}", args.addr))?;

    let jwt_configured = args.jwks_url.is_some() && args.jwt_issuer.is_some();
    if args.jwks_url.is_some() != args.jwt_issuer.is_some() {
        anyhow::bail!("--jwks/PIMBLE_JWKS_URL and --issuer/PIMBLE_JWT_ISSUER must both be given, or neither");
    }
    let jwks_url = args.jwks_url.as_deref().map(|s| s.parse()).transpose().context("Invalid --jwks URL")?;

    let auth_token = match args.token_file {
        Some(ref path) => Some(
            auth::load_or_create_token(path).with_context(|| format!("Failed to load or create token file {:?}", path))?,
        ),
        None if args.server_token_env.is_some() => args.server_token_env.clone(),
        None if !socket_addr.ip().is_loopback() && !jwt_configured => Some(
            auth::load_or_create_token(&auth::default_token_path()).context("Failed to load or create the default server token")?,
        ),
        None => None,
    };

    let mut open_paths = args.open_paths.clone();
    if let Some(stores_dir) = &args.stores_dir {
        open_paths.extend(pimble_stores_in(stores_dir));
    }

    let mut server = PimbleServer::with_config(ServerConfig {
        addr: socket_addr,
        auth_token: auth_token.clone(),
        jwks_url,
        jwt_issuer: args.jwt_issuer.clone(),
        allowed_origins: args.allow_origins.clone(),
        ..Default::default()
    });
    server.start().await?;
    let bound = server.addr();
    println!("Pimble server listening on {}", bound);

    if !open_paths.is_empty() {
        let bound_url = format!("http://{}", bound);
        let client = match &auth_token {
            Some(token) => PimbleClient::connect_with_auth(&bound_url, &AuthMethod::Bearer { token: token.clone() }).await?,
            None => PimbleClient::connect(&bound_url).await?,
        };
        for path in &open_paths {
            match client.open_store(path).await {
                Ok(store) => println!("Opened store {} ({}) from {:?}", store.id, store.name, path),
                Err(e) => eprintln!("Failed to open store at {:?}: {}", path, e),
            }
        }
    }

    pimble_server::wait_for_shutdown_signal().await;
    println!("Shutting down...");
    server.stop().await?;

    let manager = server.store_manager();
    let mut manager = manager.write().await;
    manager.flush_all().await?;

    Ok(())
}

/// Print this machine's default server token, creating it first if it
/// doesn't exist yet (`new: false`), or replace it with a freshly generated
/// one (`new: true`) — `pimble-cli token`/`token --new`.
fn token_command(new: bool) -> Result<()> {
    let path = auth::default_token_path();
    let token = if new { auth::regenerate_token(&path)? } else { auth::load_or_create_token(&path)? };
    println!("{}", token);
    Ok(())
}

async fn add_remote_store(url: &str, remote_store_id: &str, path: Option<PathBuf>, token: Option<String>) -> Result<()> {
    let remote_store_id = parse_store_id(remote_store_id)?;
    let remote = RemoteEndpoint {
        url: url.parse().with_context(|| format!("Invalid URL: {}", url))?,
        auth: auth_method_of(token),
    };

    let client = connect().await?;
    let store = client.add_remote_store(remote, remote_store_id, path).await?;
    println!("Added remote store: {}", store.id);
    println!("Name: {}", store.name);
    println!("Root node: {}", store.root_node_id);
    println!("Sync state: {:?}", store.sync_state);
    Ok(())
}

async fn link_store(store_id: &str, url: &str, token: Option<String>) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let remote = RemoteEndpoint {
        url: url.parse().with_context(|| format!("Invalid URL: {}", url))?,
        auth: auth_method_of(token),
    };

    let client = connect().await?;
    let (remote, state) = client.set_store_sync(store_id, Some(remote)).await?;
    match remote {
        Some(r) => println!("Store {} linked to {}", store_id, r.url),
        None => println!("Store {} has no remote (unexpected after linking)", store_id),
    }
    println!("Sync state: {:?}", state);
    Ok(())
}

async fn remove_replica(store_id: &str, force: bool) -> Result<()> {
    let store_id = parse_store_id(store_id)?;

    let client = connect().await?;
    client.remove_replica(store_id, force).await?;
    println!("Removed replica {}", store_id);
    Ok(())
}

async fn unlink_store(store_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;

    let client = connect().await?;
    let (was_remote, state) = client.set_store_sync(store_id, None).await?;
    match was_remote {
        Some(r) => println!("Store {} unlinked (was {})", store_id, r.url),
        None => println!("Store {} was already unlinked", store_id),
    }
    println!("Sync state: {:?}", state);
    Ok(())
}

async fn sync_state(store_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;

    let client = connect().await?;
    let (remote, state, mode, access) = client.get_store_sync_with_access(store_id).await?;
    match remote {
        Some(r) => println!("Remote: {}", r.url),
        None => println!("Remote: (none)"),
    }
    println!("State: {:?}", state);
    println!("Mode: {:?}", mode);
    println!("Access: {:?}", access);
    Ok(())
}

/// List the stores open on a remote server — through `PIMBLE_SERVER`'s own
/// `listRemoteStores`, never a direct connection from this CLI to `url`
/// (docs/history/HARDENING_CONTRACT.md decision 5).
async fn remote_stores(url: &str, token: Option<String>) -> Result<()> {
    let remote = RemoteEndpoint {
        url: url.parse().with_context(|| format!("Invalid URL: {}", url))?,
        auth: auth_method_of(token),
    };

    let client = connect().await?;
    let stores = client.list_remote_stores(remote).await?;

    if stores.is_empty() {
        println!("No stores open on {}", url);
    } else {
        println!("Stores open on {}:", url);
        for store in stores {
            println!("  {} - {}", store.id, store.name);
        }
    }
    Ok(())
}

// ── Cloud (Pimble Cloud account), docs/CRYPTO_CONTRACT.md ────────────────

/// `PIMBLE_CLOUD_PASSWORD` if set and non-empty, else a stderr prompt read
/// from stdin (visible, like the rest of this debugging CLI's input).
fn read_cloud_password() -> Result<String> {
    if let Ok(password) = std::env::var("PIMBLE_CLOUD_PASSWORD") {
        if !password.is_empty() {
            return Ok(password);
        }
    }
    use std::io::Write;
    eprint!("Password: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).context("reading password from stdin")?;
    Ok(line.trim_end_matches(['\n', '\r']).to_string())
}

async fn cloud_sign_in(url: &str, email: &str) -> Result<()> {
    let password = read_cloud_password()?;
    let client = connect().await?;
    client.cloud_sign_in(url, email, password).await?;
    println!("Signed in to {} as {}", url, email);
    Ok(())
}

async fn cloud_status() -> Result<()> {
    let client = connect().await?;
    let status = client.cloud_status().await?;
    if status.signed_in {
        println!("Signed in as {} on {}", status.email.unwrap_or_default(), status.url.unwrap_or_default());
    } else {
        println!("Not signed in");
    }
    Ok(())
}

async fn cloud_sign_out() -> Result<()> {
    let client = connect().await?;
    client.cloud_sign_out().await?;
    println!("Signed out");
    Ok(())
}

async fn cloud_host_store(store_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let client = connect().await?;
    let hosted_id = client.cloud_host_store(store_id).await?;
    println!("Hosting store {} on Pimble Cloud", hosted_id);
    Ok(())
}

async fn cloud_list_hosted() -> Result<()> {
    let client = connect().await?;
    let stores = client.cloud_list_hosted_stores().await?;
    if stores.is_empty() {
        println!("No hosted stores");
    } else {
        // One row per grant: a share of a store names its root and whose it is.
        for s in stores {
            let mut line = format!("{}  {}  role={}  kind={}  created={}", s.store_id, s.name, s.role, s.kind, s.created_at);
            if let Some(root) = s.root {
                line.push_str(&format!("  root={root}"));
            }
            if let Some(shared_by) = s.shared_by {
                line.push_str(&format!("  shared-by={shared_by}"));
            }
            println!("{line}");
        }
    }
    Ok(())
}

async fn cloud_add_hosted(store_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let client = connect().await?;
    let store = client.cloud_add_hosted_store(store_id).await?;
    println!("Added hosted store {} locally", store.id);
    println!("Name: {}", store.name);
    println!("Sync state: {:?}", store.sync_state);
    println!("Access: {:?}", store.access);
    if let Some(shared_by) = &store.shared_by {
        println!("Shared by: {}", shared_by);
    }
    for root in store.shown_roots() {
        println!("Root: {}", root);
    }
    Ok(())
}

// ── Sharing, docs/NODE_DOCUMENT_CONTRACT.md section 5 ────────────────────

fn print_share(answer: &pimble_rpc::CloudShareInfoResponse) {
    println!("Share \"{}\": node {} of store {}", answer.share.name, answer.share.node_id, answer.share.store_id);
    println!("State here: {:?}", answer.share.state);
    for member in &answer.members {
        let status = match member.status {
            pimble_rpc::ShareMemberStatus::Invited => "invited, no account yet",
            pimble_rpc::ShareMemberStatus::WaitingForKey => "waiting for the key",
            pimble_rpc::ShareMemberStatus::Active => "active",
        };
        println!("  {}  {}  {}", member.email, member.role.as_str(), status);
    }
}

async fn cloud_share(store_id: &str, node_id: &str, name: Option<String>) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;
    let client = connect().await?;
    // A share has a name of its own; the node's title is the obvious one.
    let name = match name {
        Some(name) => name,
        None => client.get_node(store_id, node_id).await?.metadata.title,
    };
    let answer = client.cloud_share_node(store_id, node_id, &name).await?;
    print_share(&answer);
    Ok(())
}

async fn cloud_share_info(store_id: &str, node_id: &str) -> Result<()> {
    let client = connect().await?;
    let answer = client.cloud_share_info(parse_store_id(store_id)?, parse_node_id(node_id)?).await?;
    print_share(&answer);
    Ok(())
}

async fn cloud_share_invite(store_id: &str, node_id: &str, email: &str, role: &str) -> Result<()> {
    let role = match pimble_rpc::MemberRole::parse(role) {
        Some(role @ (pimble_rpc::MemberRole::Editor | pimble_rpc::MemberRole::Reader)) => role,
        _ => anyhow::bail!("unknown role '{}': expected 'editor' or 'reader'", role),
    };
    let client = connect().await?;
    let answer = client.cloud_share_invite(parse_store_id(store_id)?, parse_node_id(node_id)?, email, role).await?;
    print_share(&answer);
    Ok(())
}

async fn cloud_share_remove(store_id: &str, node_id: &str, email: &str) -> Result<()> {
    let client = connect().await?;
    let answer = client.cloud_share_remove_member(parse_store_id(store_id)?, parse_node_id(node_id)?, email).await?;
    print_share(&answer);
    Ok(())
}

async fn cloud_stop_sharing(store_id: &str, node_id: &str) -> Result<()> {
    let (store_id, node_id) = (parse_store_id(store_id)?, parse_node_id(node_id)?);
    let client = connect().await?;
    client.cloud_stop_sharing(store_id, node_id).await?;
    println!("Stopped sharing node {} of store {}; its documents stay hosted where they are", node_id, store_id);
    Ok(())
}

async fn create_store(path: &str, name: &str, kind: Option<String>, id: Option<String>) -> Result<()> {
    let kind = match kind.as_deref() {
        None | Some("plain") => StoreKind::Plain,
        Some("vault") => StoreKind::Vault,
        Some(other) => anyhow::bail!("unknown store kind '{}': expected 'plain' or 'vault'", other),
    };
    let store_id = id.as_deref().map(parse_store_id).transpose()?;

    let client = connect().await?;
    let (store_id, root_id) = client.create_store_with(PathBuf::from(path), name, kind, store_id).await?;
    println!("Created store: {}", store_id);
    // A vault store's manifest root id is a meaningless placeholder (it has
    // no tree of its own on this server; docs/CRYPTO_CONTRACT.md) — printing
    // it would just invite confusion.
    if kind == StoreKind::Plain {
        println!("Root node: {}", root_id);
    }
    Ok(())
}

/// Parse a `VaultDocId` CLI argument: a node id, or the literal `tree` for
/// the retired tree document a twin hosted before docs/NODE_DOCUMENT_CONTRACT.md
/// may still list.
fn parse_vault_doc_id(s: &str) -> Result<VaultDocId> {
    VaultDocId::parse(s).with_context(|| format!("Invalid vault document id: {} (expected a node id, or 'tree')", s))
}

async fn vault_list(store_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let client = connect().await?;
    let docs = client.vault_list_docs(store_id).await?;
    if docs.is_empty() {
        println!("No documents");
    } else {
        for doc in docs {
            let dek = doc.dek_id.map(|id| format!(" dek={id}")).unwrap_or_default();
            println!("{} head={} snapshot_seq={}{}", doc.doc_id.as_str(), doc.head, doc.snapshot_seq, dek);
        }
    }
    Ok(())
}

async fn vault_fetch(store_id: &str, doc: &str, after_seq: u64) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let doc_id = parse_vault_doc_id(doc)?;
    let client = connect().await?;
    let response = client.vault_fetch(store_id, doc_id, after_seq).await?;

    println!("Head: {}", response.head);
    match &response.keys {
        Some(keys) => {
            let wrapped_under: Vec<String> = keys.wraps.iter().map(|w| w.scope_key_id.to_string()).collect();
            println!("Data key: {} (wrapped under {})", keys.dek_id, wrapped_under.join(", "));
        }
        None => println!("Data key: (none)"),
    }
    match &response.snapshot {
        Some(snapshot) => println!("Snapshot: seq={} bytes={}", snapshot.seq, snapshot.blob.len()),
        None => println!("Snapshot: (none)"),
    }
    if response.updates.is_empty() {
        println!("No updates after {}", after_seq);
    } else {
        for update in &response.updates {
            println!("  seq={} bytes={}", update.seq, update.blob.len());
        }
    }
    Ok(())
}

async fn vault_append(store_id: &str, doc: &str, file: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let doc_id = parse_vault_doc_id(doc)?;
    let bytes = std::fs::read(file).with_context(|| format!("failed to read {}", file))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);

    let client = connect().await?;
    let seq = client.vault_append(store_id, doc_id, blob).await?;
    println!("Appended seq {}", seq);
    Ok(())
}

async fn vault_snapshot(store_id: &str, doc: &str, upto_seq: &str, file: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let doc_id = parse_vault_doc_id(doc)?;
    let upto_seq: u64 = upto_seq.parse().context("upto-seq must be a number")?;
    let bytes = std::fs::read(file).with_context(|| format!("failed to read {}", file))?;
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);

    let client = connect().await?;
    client.vault_snapshot(store_id, doc_id, upto_seq, blob).await?;
    println!("Stored snapshot up to seq {}", upto_seq);
    Ok(())
}

/// Hosted side: what the accounts service calls when a hosted store is
/// deleted. Refused for anything but a vault store open on the server.
async fn delete_vault_store(store_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let client = connect().await?;
    client.delete_vault_store(store_id).await?;
    println!("Deleted vault store {}", store_id);
    Ok(())
}

async fn scopes(store_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let client = connect().await?;
    let scopes = client.get_scopes(store_id).await?;
    if scopes.is_empty() {
        println!("No scopes");
    }
    for scope in scopes {
        println!("{}  {} document(s)", scope.root, scope.doc_ids.len());
        for doc in scope.doc_ids {
            println!("  {}", doc);
        }
    }
    Ok(())
}

async fn set_scope(store_id: &str, root: &str, docs: &[String], remove: bool) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let root = parse_node_id(root)?;
    let doc_ids = docs.iter().map(|d| parse_node_id(d)).collect::<Result<Vec<_>>>()?;
    let client = connect().await?;
    let count = doc_ids.len();
    client.set_scope(store_id, pimble_rpc::Scope { root, doc_ids }, remove).await?;
    if remove {
        println!("Removed scope {}", root);
    } else {
        println!("Published scope {}: {} document(s)", root, count);
    }
    Ok(())
}

async fn open_store(path: &str) -> Result<()> {
    let client = connect().await?;
    let store = client.open_store(PathBuf::from(path)).await?;
    println!("Opened store: {}", store.id);
    println!("Name: {}", store.name);
    println!("Root node: {}", store.root_node_id);
    Ok(())
}

async fn list_stores() -> Result<()> {
    let client = connect().await?;
    let stores = client.list_stores().await?;

    if stores.is_empty() {
        println!("No stores open");
    } else {
        println!("Open stores:");
        for store in stores {
            println!("  {} - {}", store.id, store.name);
        }
    }
    Ok(())
}

async fn import_scrivener(scriv_path: &str, output_path: &str) -> Result<()> {
    use std::path::Path;
    pimble_import::scrivener::import_scrivener(Path::new(scriv_path), Path::new(output_path)).await?;
    println!("Scrivener project imported successfully to {}", output_path);
    Ok(())
}

async fn create_node(store_id: &str, parent_id: &str, node_type: &str, title: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let parent_id = parse_node_id(parent_id)?;

    let client = connect().await?;
    let node_id = client
        .create_node(store_id, Some(parent_id), node_type, title)
        .await?;
    println!("Created node: {}", node_id);
    Ok(())
}

async fn move_node(store_id: &str, node_id: &str, new_parent_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;
    let new_parent_id = parse_node_id(new_parent_id)?;

    let client = connect().await?;
    client.move_node(store_id, node_id, new_parent_id, None).await?;
    println!("Moved node {} under {}", node_id, new_parent_id);
    Ok(())
}

async fn delete_node(store_id: &str, node_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let client = connect().await?;
    client.delete_node(store_id, node_id).await?;
    println!("Deleted node {} and its subtree", node_id);
    Ok(())
}

async fn undelete_node(store_id: &str, node_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let client = connect().await?;
    client.undelete_node(store_id, node_id).await?;
    println!("Undeleted node {} and what its deletion took with it", node_id);
    Ok(())
}

async fn set_node_text(store_id: &str, node_id: &str, text: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let client = connect().await?;

    // An edit of the node's existing document, sent as an `applyEdit`
    // delta — the one way content is written. A fresh document built from
    // `text` (`updateNodeContent`) shares no history with the node's, so on
    // any replica it would merge in next to the old paragraphs instead of
    // replacing them.
    let node = client.get_node(store_id, node_id).await?;
    let mut doc = NodeDoc::load(&node.content)
        .map_err(|e| anyhow::anyhow!("failed to load the node's document: {e}"))?;
    let delta = doc
        .replace_plain_text(text)
        .map_err(|e| anyhow::anyhow!("failed to build the replacement edit: {e}"))?;
    let changes = base64::engine::general_purpose::STANDARD.encode(delta);
    client
        .apply_edit(store_id, node_id, "pimble-cli", EditOperation::IncrementalChanges { changes })
        .await?;
    println!("Updated content for node {}", node_id);
    Ok(())
}

async fn show_node(store_id: &str, node_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let client = connect().await?;
    let node = client.get_node(store_id, node_id).await?;

    println!("Node: {}", node.id);
    println!("Type: {}", node.node_type);
    println!("Title: {}", node.metadata.title);
    println!("Parent: {}", node.parent_id.map(|id| id.to_string()).unwrap_or_else(|| "(none)".to_string()));
    println!("Created: {}", node.metadata.created_at);
    println!("Modified: {}", node.metadata.modified_at);
    println!("Tags: {}", node.metadata.tags.join(", "));
    println!("Children: {}", node.children.len());
    println!("--- content ---");
    println!("{}", NodeDoc::text_of(&node.content));
    Ok(())
}

async fn search(query: &str) -> Result<()> {
    let client = connect().await?;
    // Empty store list = search all open stores.
    let results = client.search(query, Vec::new(), true, 20).await?;

    if results.is_empty() {
        println!("No results for '{}'", query);
    } else {
        println!("{} result(s) for '{}':", results.len(), query);
        for r in results {
            println!("  [{}] {} ({})", r.kind, r.title, r.store_id);
            println!("      {}", r.snippet);
        }
    }
    Ok(())
}

async fn rebuild_index(store_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;

    let client = connect().await?;
    let indexed = client.rebuild_index(store_id).await?;
    println!("Rebuilt index for store {}: {} node(s) indexed", store_id, indexed);
    Ok(())
}

async fn create_mount(
    store_id: &str,
    parent_id: &str,
    source_store_id: &str,
    source_node_id: &str,
    title: Option<String>,
) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let parent_id = parse_node_id(parent_id)?;
    let source_store_id = parse_store_id(source_store_id)?;
    let source_node_id = parse_node_id(source_node_id)?;

    let client = connect().await?;
    let (node_id, mount_ref) = client
        .create_mount(store_id, parent_id, source_store_id, source_node_id, title)
        .await?;
    println!("Created mount: {}", node_id);
    print_mount_ref(&mount_ref);
    Ok(())
}

/// Add `remote_store_id` from `url` as a replica of this server (unless it
/// is already open here) and mount its root under `parent_id`
/// (docs/history/REMOTE_MOUNTS_CONTRACT.md decision 9's two steps, headless).
async fn mount_remote_store(
    store_id: &str,
    parent_id: &str,
    url: &str,
    remote_store_id: &str,
    title: Option<String>,
    token: Option<String>,
) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let parent_id = parse_node_id(parent_id)?;
    let remote_store_id = parse_store_id(remote_store_id)?;
    let remote = RemoteEndpoint {
        url: url.parse().with_context(|| format!("Invalid URL: {}", url))?,
        auth: auth_method_of(token),
    };

    let client = connect().await?;

    let existing = client.list_stores().await?.into_iter().find(|s| s.id == remote_store_id);
    let source = match existing {
        Some(store) => {
            println!("Store {} is already open here; mounting it directly", store.id);
            store
        }
        None => {
            let store = client.add_remote_store(remote, remote_store_id, None).await?;
            println!("Added remote store: {} ({})", store.id, store.name);
            println!("Sync state: {:?}", store.sync_state);
            store
        }
    };

    let title = title.unwrap_or_else(|| source.name.clone());
    let (node_id, mount_ref) = client
        .create_mount(store_id, parent_id, source.id, source.root_node_id, Some(title))
        .await?;
    println!("Created mount: {}", node_id);
    print_mount_ref(&mount_ref);
    Ok(())
}

/// Where a mount's source is, as far as its `MountRef` knows: the canonical
/// pair always, plus the on-disk path and the remote Pimble server it can
/// be replicated from when those are set.
fn print_mount_ref(mount_ref: &MountRef) {
    println!("Source: {}:{}", mount_ref.source_store, mount_ref.source_node);
    if let Some(path) = &mount_ref.source_path {
        println!("Source path: {}", path.display());
    }
    if let Some(url) = &mount_ref.source_remote {
        println!("Source remote: {}", url);
    }
}

/// A [`MountState`] in one line, with what each state carries: when a
/// cached mount last synced, and why an unavailable one is unavailable.
fn describe_mount_state(state: &MountState) -> String {
    match state {
        MountState::Live => "Live".to_string(),
        MountState::Connecting => "Connecting".to_string(),
        MountState::Cached { last_sync } => format!("Cached (last synced {})", last_sync.to_rfc3339()),
        MountState::Unavailable { reason: Some(reason) } => format!("Unavailable: {}", reason),
        MountState::Unavailable { reason: None } => "Unavailable".to_string(),
    }
}

async fn mount_state(store_id: &str, node_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let client = connect().await?;
    let (state, mount_ref) = client.get_mount_state(store_id, node_id).await?;
    println!("Mount state: {}", describe_mount_state(&state));
    print_mount_ref(&mount_ref);
    Ok(())
}

async fn list_children(store_id: &str, node_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let client = connect().await?;
    let (canonical_store, children) = client.get_children(store_id, node_id).await?;
    if children.is_empty() {
        println!("No children");
    } else {
        for child in children {
            println!(
                "{} {} {} {}",
                canonical_store, child.id, child.node_type, child.metadata.title
            );
        }
    }
    Ok(())
}

fn parse_store_id(s: &str) -> Result<StoreId> {
    StoreId::parse(s).with_context(|| format!("Invalid store id: {}", s))
}

fn parse_node_id(s: &str) -> Result<NodeId> {
    NodeId::parse(s).with_context(|| format!("Invalid node id: {}", s))
}

/// `token` as an `AuthMethod`: `Bearer` when given, `None` otherwise.
fn auth_method_of(token: Option<String>) -> AuthMethod {
    match token {
        Some(token) => AuthMethod::Bearer { token },
        None => AuthMethod::None,
    }
}

/// Connect to `PIMBLE_SERVER`, authenticating per [`resolve_cli_auth`].
async fn connect() -> Result<PimbleClient> {
    let url = std::env::var("PIMBLE_SERVER").unwrap_or_else(|_| "http://127.0.0.1:7462".to_string());
    let client = match resolve_cli_auth(&url) {
        Some(token) => PimbleClient::connect_with_auth(&url, &AuthMethod::Bearer { token }).await?,
        None => PimbleClient::connect(&url).await?,
    };
    Ok(client)
}

/// The bearer token for a connection to `url`: `PIMBLE_TOKEN` if set and
/// non-empty, else the default server token file's token when `url`'s host
/// is loopback and the file exists (docs/history/HARDENING_CONTRACT.md "A: edge"),
/// else no token.
fn resolve_cli_auth(url: &str) -> Option<String> {
    if let Ok(token) = std::env::var("PIMBLE_TOKEN") {
        if !token.is_empty() {
            return Some(token);
        }
    }

    if is_loopback_url(url) {
        if let Ok(contents) = std::fs::read_to_string(auth::default_token_path()) {
            let token = contents.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }

    None
}

/// Whether `url`'s host is a loopback address or `localhost`.
fn is_loopback_url(url: &str) -> bool {
    // `host()` (not `host_str()`, which brackets an IPv6 address as
    // `[::1]` — not a valid `IpAddr` string) so `http://[::1]:7462` is
    // recognized as loopback too.
    let Ok(parsed) = url::Url::parse(url) else { return false };
    match parsed.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temp directory under the OS temp dir, cleaned up when the
    /// returned guard drops. No `tempfile` dev-dependency needed for one
    /// test: `uuid` is already a normal dependency.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("pimble-cli-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// docs/CLOUD_CONTRACT.md "B: pimble-server" item 6: `--stores-dir DIR`
    /// opens every `*.pimble` directory directly inside `DIR`. Only
    /// directories with that extension count — a same-named file, a
    /// differently-named directory, and anything nested deeper are all
    /// left out.
    #[test]
    fn pimble_stores_in_finds_only_pimble_directories_directly_inside() {
        let dir = TempDir::new();

        std::fs::create_dir_all(dir.0.join("alpha.pimble")).unwrap();
        std::fs::create_dir_all(dir.0.join("beta.pimble")).unwrap();
        std::fs::create_dir_all(dir.0.join("not-a-store")).unwrap();
        std::fs::write(dir.0.join("gamma.pimble"), b"not a directory").unwrap();
        std::fs::write(dir.0.join("readme.txt"), b"ignored").unwrap();
        std::fs::create_dir_all(dir.0.join("alpha.pimble").join("nested.pimble")).unwrap();

        let mut found = pimble_stores_in(&dir.0);
        found.sort();
        let mut expected = vec![dir.0.join("alpha.pimble"), dir.0.join("beta.pimble")];
        expected.sort();
        assert_eq!(found, expected);
    }

    #[test]
    fn pimble_stores_in_a_missing_directory_is_empty_not_an_error() {
        let dir = TempDir::new();
        assert_eq!(pimble_stores_in(&dir.0.join("does-not-exist")), Vec::<PathBuf>::new());
    }
}
