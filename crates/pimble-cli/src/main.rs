//! Pimble CLI - Command-line interface for debugging and management

use std::path::PathBuf;

use anyhow::Result;
use pimble_client::PimbleClient;
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
        "list-stores" => list_stores().await?,
        "open-store" => {
            if args.len() < 3 {
                eprintln!("Usage: pimble-cli open-store <path>");
                return Ok(());
            }
            open_store(&args[2]).await?;
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
    help            Show this help message
    server          Start the Pimble server
    create-store    Create a new store
    open-store      Open an existing store
    list-stores     List all open stores

EXAMPLES:
    pimble-cli server
    pimble-cli create-store ./my-notes.pimble "My Notes"
    pimble-cli open-store ./my-notes.pimble
    pimble-cli list-stores
"#
    );
}

async fn run_server() -> Result<()> {
    use pimble_server::{run_server, ServerConfig};

    println!("Starting Pimble server on 127.0.0.1:9876...");
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

async fn connect() -> Result<PimbleClient> {
    let url = std::env::var("PIMBLE_SERVER").unwrap_or_else(|_| "http://127.0.0.1:9876".to_string());
    let client = PimbleClient::connect(&url).await?;
    Ok(client)
}
