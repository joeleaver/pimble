//! Pimble CLI - Command-line interface for debugging and management

use std::path::PathBuf;

use anyhow::{Context, Result};
use pimble_client::PimbleClient;
use pimble_core::{NodeId, StoreId};
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
        "server" => run_server().await?,
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

EXAMPLES:
    pimble-cli server
    pimble-cli create-store ./my-notes.pimble "My Notes"
    pimble-cli open-store ./my-notes.pimble
    pimble-cli list-stores
    pimble-cli import-scrivener ./project.scriv ./project.pimble
    pimble-cli create-node <store-id> <parent-id> document "My Note"
    pimble-cli set-node-text <store-id> <node-id> "Hello, world"
    pimble-cli show-node <store-id> <node-id>
    pimble-cli search "hello"
    pimble-cli rebuild-index <store-id>
"#
    );
}

async fn run_server() -> Result<()> {
    use pimble_server::{run_server, ServerConfig};

    println!("Starting Pimble server on 127.0.0.1:7462...");
    run_server(ServerConfig::default()).await?;
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
