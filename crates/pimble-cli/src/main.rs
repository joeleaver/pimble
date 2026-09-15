//! Pimble CLI - Command-line interface for debugging and management

use std::path::PathBuf;

use anyhow::{Context, Result};
use pimble_client::PimbleClient;
use pimble_core::{AuthMethod, NodeId, RemoteEndpoint, StoreId};
use pimble_crdt::ContentDoc;
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
            let (addr, open_paths) = match parse_server_args(&args[2..]) {
                Ok(parsed) => parsed,
                Err(e) => {
                    eprintln!("{}", e);
                    eprintln!("Usage: pimble-cli server [--addr HOST:PORT] [--open PATH]...");
                    return Ok(());
                }
            };
            run_server(&addr, open_paths).await?;
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
        "list-children" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli list-children <store-id> <node-id>");
                return Ok(());
            }
            list_children(&args[2], &args[3]).await?;
        }
        "add-remote-store" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli add-remote-store <url> <remote-store-id> [path]");
                return Ok(());
            }
            let path = args.get(4).map(PathBuf::from);
            add_remote_store(&args[2], &args[3], path).await?;
        }
        "link-store" => {
            if args.len() < 4 {
                eprintln!("Usage: pimble-cli link-store <store-id> <url>");
                return Ok(());
            }
            link_store(&args[2], &args[3]).await?;
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
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli remote-stores <url>");
                return Ok(());
            }
            remote_stores(&args[2]).await?;
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
    create-store        Create a new store
    open-store          Open an existing store
    list-stores         List all open stores
    import-scrivener    Import a Scrivener .scriv project into a Pimble store
    create-node         Create a node in a store
    set-node-text       Set a node's content from plain text
    show-node           Print a node's metadata and content text
    search              Search across all open stores
    rebuild-index       Rebuild a store's search index from scratch
    create-mount        Mount a subtree from one store under a node in another
    mount-state         Show a mount point's resolution state
    list-children       List a node's children (resolves mounts)
    add-remote-store    Create a local replica of a remote's store and link it
    link-store          Link an existing local store to its twin on a remote
    unlink-store        Unlink a store from its remote (stops the sync link)
    sync-state          Show a store's replica sync link and its state
    remote-stores       List the stores a remote Pimble server has open

ENVIRONMENT:
    PIMBLE_SERVER       Server URL for every command but `server` itself
                        (default: http://127.0.0.1:7462)

EXAMPLES:
    pimble-cli server
    pimble-cli server --addr 0.0.0.0:7462 --open /srv/family.pimble
    pimble-cli create-store ./my-notes.pimble "My Notes"
    pimble-cli open-store ./my-notes.pimble
    pimble-cli list-stores
    pimble-cli import-scrivener ./project.scriv ./project.pimble
    pimble-cli create-node <store-id> <parent-id> document "My Note"
    pimble-cli set-node-text <store-id> <node-id> "Hello, world"
    pimble-cli show-node <store-id> <node-id>
    pimble-cli search "hello"
    pimble-cli rebuild-index <store-id>
    pimble-cli create-mount <store-id> <parent-id> <source-store-id> <source-node-id> "My Mount"
    pimble-cli mount-state <store-id> <node-id>
    pimble-cli list-children <store-id> <node-id>
    pimble-cli add-remote-store http://127.0.0.1:7463 <remote-store-id>
    pimble-cli link-store <store-id> http://127.0.0.1:7463
    pimble-cli unlink-store <store-id>
    pimble-cli sync-state <store-id>
    pimble-cli remote-stores http://127.0.0.1:7463
"#
    );
}

/// Parse `server`'s own flags (`--addr HOST:PORT`, repeatable `--open PATH`)
/// out of the args following the `server` word.
fn parse_server_args(rest: &[String]) -> std::result::Result<(String, Vec<PathBuf>), String> {
    let mut addr = "127.0.0.1:7462".to_string();
    let mut open_paths = Vec::new();

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
            other => return Err(format!("Unknown server option: {}", other)),
        }
        i += 1;
    }

    Ok((addr, open_paths))
}

/// Start the embedded Pimble server, bound to `addr`, opening every store in
/// `open_paths` at start (so a headless replica host can run as
/// `pimble-cli server --addr 0.0.0.0:7462 --open /srv/family.pimble`) —
/// opened the same way any other client would, over a loopback connection to
/// the server it just started.
async fn run_server(addr: &str, open_paths: Vec<PathBuf>) -> Result<()> {
    use pimble_server::{PimbleServer, ServerConfig};

    let socket_addr: std::net::SocketAddr = addr.parse().with_context(|| format!("Invalid --addr {}", addr))?;
    let mut server = PimbleServer::with_config(ServerConfig { addr: socket_addr });
    server.start().await?;
    let bound = server.addr();
    println!("Pimble server listening on {}", bound);

    if !open_paths.is_empty() {
        let client = PimbleClient::connect(format!("http://{}", bound)).await?;
        for path in &open_paths {
            match client.open_store(path).await {
                Ok(store) => println!("Opened store {} ({}) from {:?}", store.id, store.name, path),
                Err(e) => eprintln!("Failed to open store at {:?}: {}", path, e),
            }
        }
    }

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    println!("Shutting down...");
    server.stop().await?;

    let manager = server.store_manager();
    let mut manager = manager.write().await;
    manager.flush_all().await?;

    Ok(())
}

async fn add_remote_store(url: &str, remote_store_id: &str, path: Option<PathBuf>) -> Result<()> {
    let remote_store_id = parse_store_id(remote_store_id)?;
    let remote = RemoteEndpoint {
        url: url.parse().with_context(|| format!("Invalid URL: {}", url))?,
        auth: AuthMethod::None,
    };

    let client = connect().await?;
    let store = client.add_remote_store(remote, remote_store_id, path).await?;
    println!("Added remote store: {}", store.id);
    println!("Name: {}", store.name);
    println!("Root node: {}", store.root_node_id);
    println!("Sync state: {:?}", store.sync_state);
    Ok(())
}

async fn link_store(store_id: &str, url: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let remote = RemoteEndpoint {
        url: url.parse().with_context(|| format!("Invalid URL: {}", url))?,
        auth: AuthMethod::None,
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

async fn remote_stores(url: &str) -> Result<()> {
    let client = PimbleClient::connect(url).await?;
    let stores = client.list_stores().await?;

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

async fn set_node_text(store_id: &str, node_id: &str, text: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let content = ContentDoc::from_plain_text(text)
        .map_err(|e| anyhow::anyhow!("failed to build content document: {e}"))?
        .save();

    let client = connect().await?;
    client
        .set_node_content_bytes(store_id, node_id, content, None)
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
    println!("Source: {}:{}", mount_ref.source_store, mount_ref.source_node);
    if let Some(path) = &mount_ref.source_path {
        println!("Source path: {}", path.display());
    }
    Ok(())
}

async fn mount_state(store_id: &str, node_id: &str) -> Result<()> {
    let store_id = parse_store_id(store_id)?;
    let node_id = parse_node_id(node_id)?;

    let client = connect().await?;
    let (state, mount_ref) = client.get_mount_state(store_id, node_id).await?;
    println!("Mount state: {:?}", state);
    println!("Source: {}:{}", mount_ref.source_store, mount_ref.source_node);
    if let Some(path) = &mount_ref.source_path {
        println!("Source path: {}", path.display());
    }
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

async fn connect() -> Result<PimbleClient> {
    let url = std::env::var("PIMBLE_SERVER").unwrap_or_else(|_| "http://127.0.0.1:7462".to_string());
    let client = PimbleClient::connect(&url).await?;
    Ok(client)
}
