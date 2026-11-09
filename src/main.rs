// src/main.rs
//! MCP-Nexus CLI Daemon & Gateway

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use mcp_nexus::config::NexusConfig;
use mcp_nexus::router::ToolRouter;
use mcp_nexus::server::StdioServer;
use mcp_nexus::shield::{InjectionShield, ShieldMode};
use mcp_nexus::telemetry::Telemetry;
use mcp_nexus::upstream::{StdioUpstream, UpstreamClient};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

#[derive(Parser, Debug)]
#[command(
    name = "mcp-nexus",
    version = "0.1.0",
    about = "High-Performance Gateway, Router & Injection Shield for Model Context Protocol (MCP)",
    long_about = "Multiplexes multiple MCP servers into a single lightweight stdio/SSE gateway for Claude Desktop, Claude Code, and other LLM agents."
)]
struct Cli {
    #[arg(short, long, value_name = "FILE", default_value = "nexus.json")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the standard IO (stdio) MCP gateway (default)
    Start {
        #[arg(long, default_value_t = false)]
        lazy_search: bool,
    },
    /// Inspect and list all available tools across upstream servers
    List,
    /// Verify health and connectivity to all upstream MCP servers
    Check,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr) // Crucial: logs must go to stderr, stdout is reserved for MCP JSON-RPC!
        .init();

    let cli = Cli::parse();

    // Resolve config path
    let config_path = if cli.config.exists() {
        cli.config
    } else {
        // Fallback checks
        let alt = PathBuf::from("claude_desktop_config.json");
        if alt.exists() {
            alt
        } else {
            cli.config
        }
    };

    let config = if config_path.exists() {
        NexusConfig::from_file(&config_path).await?
    } else {
        eprintln!(
            "Notice: Config file '{:?}' not found. Starting with empty default config.",
            config_path
        );
        NexusConfig::default()
    };

    let shield = Arc::new(InjectionShield::new(config.shield.clone()));
    let telemetry = Arc::new(Telemetry::new());

    match cli.command.unwrap_or(Commands::Start { lazy_search: false }) {
        Commands::Start { lazy_search } => {
            eprintln!(
                "⚡ [MCP-Nexus v{}] Starting Gateway with {} upstream server(s)...",
                config.server_version,
                config.mcp_servers.len()
            );

            let router = Arc::new(ToolRouter::new(shield.clone(), telemetry.clone(), lazy_search));

            // Spawn upstreams
            for (name, srv_cfg) in &config.mcp_servers {
                eprintln!("Connecting to upstream '{}' ({})...", name, srv_cfg.command);
                match StdioUpstream::new(
                    name.clone(),
                    srv_cfg.command.clone(),
                    srv_cfg.args.clone(),
                    srv_cfg.env.clone(),
                )
                .await
                {
                    Ok(client) => {
                        let _ = router.register_upstream(client).await;
                    }
                    Err(e) => {
                        eprintln!("⚠️ Failed to spawn upstream '{}': {}", name, e);
                    }
                }
            }

            let server = StdioServer::new(
                router,
                config.server_name.clone(),
                config.server_version.clone(),
            );

            server.run().await?;
        }
        Commands::List => {
            println!("⚡ [MCP-Nexus] Querying tools from configured upstreams...\n");
            let router = Arc::new(ToolRouter::new(shield.clone(), telemetry.clone(), false));

            for (name, srv_cfg) in &config.mcp_servers {
                match StdioUpstream::new(
                    name.clone(),
                    srv_cfg.command.clone(),
                    srv_cfg.args.clone(),
                    srv_cfg.env.clone(),
                )
                .await
                {
                    Ok(client) => {
                        let _ = router.register_upstream(client).await;
                    }
                    Err(e) => {
                        println!("❌ Upstream '{}' offline: {}", name, e);
                    }
                }
            }

            let tools = router.list_tools().await;
            println!("Discovered {} total tool(s):\n", tools.len());
            for t in tools {
                let desc = t.description.as_deref().unwrap_or("No description");
                println!("• {:<32} - {}", t.name, desc);
            }
        }
        Commands::Check => {
            println!("⚡ [MCP-Nexus] Health checking upstream configs...\n");
            for (name, srv_cfg) in &config.mcp_servers {
                print!("Checking '{:<16}' [{}] ... ", name, srv_cfg.command);
                match StdioUpstream::new(
                    name.clone(),
                    srv_cfg.command.clone(),
                    srv_cfg.args.clone(),
                    srv_cfg.env.clone(),
                )
                .await
                {
                    Ok(client) => {
                        if client.is_healthy().await {
                            println!("✅ Healthy");
                        } else {
                            println!("⚠️ Unhealthy handshake");
                        }
                    }
                    Err(e) => {
                        println!("❌ Error: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}
