//! Pimble CLI - Command-line interface for debugging and management

use std::path::PathBuf;

use anyhow::{Context, Result};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, MountRef, MountState, NodeId, RemoteEndpoint, StoreId};
use base64::Engine;
use pimble_crdt::ContentDoc;
use pimble_rpc::EditOperation;
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
            let (addr, open_paths, token_file) = match parse_server_args(&args[2..]) {
                Ok(parsed) => parsed,
                Err(e) => {
                    eprintln!("{}", e);
                    eprintln!("Usage: pimble-cli server [--addr HOST:PORT] [--open PATH]... [--token-file PATH]");
                    return Ok(());
                }
            };
            run_server(&addr, open_paths, token_file).await?;
        }
        "token" => {
            let new = args.get(2).map(|s| s == "--new").unwrap_or(false);
            token_command(new)?;
        }
        "create-store" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli create-store <path> <name>");
                return Ok(());
            }
            create_store(&args[2], &args[3]).await?;
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
    create-store        Create a new store
    open-store          Open an existing store
    list-stores         List all open stores
    import-scrivener    Import a Scrivener .scriv project into a Pimble store
    create-node         Create a node in a store
    move-node           Move a node under a new parent (appended last)
    delete-node         Delete a node and its whole subtree
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

ENVIRONMENT:
    PIMBLE_SERVER       Server URL for every command but `server` itself
                        (default: http://127.0.0.1:7462)
    PIMBLE_TOKEN        Bearer token sent with every request to PIMBLE_SERVER.
                        If unset and PIMBLE_SERVER is loopback, the default
                        server token file's token is used if it exists.

EXAMPLES:
    pimble-cli server
    pimble-cli server --addr 0.0.0.0:7462 --open /srv/family.pimble --token-file /etc/pimble/token
    pimble-cli token
    pimble-cli token --new
    pimble-cli create-store ./my-notes.pimble "My Notes"
    pimble-cli open-store ./my-notes.pimble
    pimble-cli list-stores
    pimble-cli import-scrivener ./project.scriv ./project.pimble
    pimble-cli create-node <store-id> <parent-id> document "My Note"
    pimble-cli move-node <store-id> <node-id> <new-parent-id>
    pimble-cli delete-node <store-id> <node-id>
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
"#
    );
}

/// Parse `server`'s own flags (`--addr HOST:PORT`, repeatable `--open PATH`,
/// `--token-file PATH`) out of the args following the `server` word.
fn parse_server_args(rest: &[String]) -> std::result::Result<(String, Vec<PathBuf>, Option<PathBuf>), String> {
    let mut addr = "127.0.0.1:7462".to_string();
    let mut open_paths = Vec::new();
    let mut token_file = None;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--addr" => {
                i += 1;
                addr = rest.get(i).ok_or("--addr requires a value")?.clone();
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
            other => return Err(format!("Unknown server option: {}", other)),
        }
        i += 1;
    }

    Ok((addr, open_paths, token_file))
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

/// Start the embedded Pimble server, bound to `addr`, opening every store in
/// `open_paths` at start (so a headless replica host can run as
/// `pimble-cli server --addr 0.0.0.0:7462 --open /srv/family.pimble`) —
/// opened the same way any other client would, over a loopback connection to
/// the server it just started.
///
/// `token_file` always enables the auth check, even on loopback (docs/
/// HARDENING_CONTRACT.md decision 3); without it, a non-loopback `addr`
/// still needs a token, so it falls back to the default token file
/// ([`auth::default_token_path`]) — `PimbleServer::start` would otherwise
/// refuse to bind.
async fn run_server(addr: &str, open_paths: Vec<PathBuf>, token_file: Option<PathBuf>) -> Result<()> {
    use pimble_server::{PimbleServer, ServerConfig};

    let socket_addr: std::net::SocketAddr = addr.parse().with_context(|| format!("Invalid --addr {}", addr))?;

    let auth_token = match token_file {
        Some(path) => Some(
            auth::load_or_create_token(&path).with_context(|| format!("Failed to load or create token file {:?}", path))?,
        ),
        None if !socket_addr.ip().is_loopback() => Some(
            auth::load_or_create_token(&auth::default_token_path()).context("Failed to load or create the default server token")?,
        ),
        None => None,
    };

    let mut server = PimbleServer::with_config(ServerConfig { addr: socket_addr, auth_token: auth_token.clone(), ..Default::default() });
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
    let (remote, state) = client.get_store_sync(store_id).await?;
    match remote {
        Some(r) => println!("Remote: {}", r.url),
        None => println!("Remote: (none)"),
    }
    println!("State: {:?}", state);
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

async fn create_store(path: &str, name: &str) -> Result<()> {
    let client = connect().await?;
    let (store_id, root_id) = client.create_store(PathBuf::from(path), name).await?;
    println!("Created store: {}", store_id);
    println!("Root node: {}", root_id);
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

async fn set_node_text(store_id: &str, node_id: &str, text: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let client = connect().await?;

    // An edit of the node's existing content document, sent as an `applyEdit`
    // delta — the one way content is written. A fresh document built from
    // `text` (`updateNodeContent`) shares no history with the node's, so on
    // any replica it would merge in next to the old paragraphs instead of
    // replacing them.
    let node = client.get_node(store_id, node_id).await?;
    let mut doc = ContentDoc::load(&node.content)
        .map_err(|e| anyhow::anyhow!("failed to load the node's content document: {e}"))?;
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
    println!("{}", ContentDoc::text_of(&node.content));
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
