// src/config.rs
//! Configuration parsing for MCP-Nexus and Claude Desktop configs

use crate::shield::ShieldConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NexusConfig {
    #[serde(default = "default_server_name")]
    pub server_name: String,
    #[serde(default = "default_server_version")]
    pub server_version: String,
    #[serde(default)]
    pub mcp_servers: HashMap<String, UpstreamServerConfig>,
    #[serde(default)]
    pub shield: ShieldConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
}

fn default_server_name() -> String {
    "mcp-nexus".to_string()
}

fn default_server_version() -> String {
    "0.1.0".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub lazy: bool,
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_history_size")]
    pub max_history: usize,
}

fn default_true() -> bool {
    true
}

fn default_history_size() -> usize {
    1000
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_history: 1000,
        }
    }
}

impl Default for NexusConfig {
    fn default() -> Self {
        Self {
            server_name: default_server_name(),
            server_version: default_server_version(),
            mcp_servers: HashMap::new(),
            shield: ShieldConfig::default(),
            telemetry: TelemetryConfig::default(),
        }
    }
}

impl NexusConfig {
    /// Loads from JSON string (auto-detects nexus.json or claude_desktop_config.json)
    pub fn from_json_str(content: &str) -> Result<Self> {
        // Try parsing directly as NexusConfig
        if let Ok(cfg) = serde_json::from_str::<NexusConfig>(content) {
            if !cfg.mcp_servers.is_empty() {
                return Ok(cfg);
            }
        }

        // Try parsing Claude Desktop format: { "mcpServers": { ... } }
        #[derive(Deserialize)]
        struct ClaudeDesktopFormat {
            #[serde(rename = "mcpServers")]
            mcp_servers: HashMap<String, UpstreamServerConfig>,
        }

        if let Ok(desktop) = serde_json::from_str::<ClaudeDesktopFormat>(content) {
            let mut cfg = NexusConfig::default();
            cfg.mcp_servers = desktop.mcp_servers;
            return Ok(cfg);
        }

        anyhow::bail!("Invalid configuration format. Must contain either 'mcp_servers' or 'mcpServers'.")
    }

    /// Loads from file path
    pub async fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let content = tokio::fs::read_to_string(path_ref)
            .await
            .with_context(|| format!("Failed to read config file at {:?}", path_ref))?;
        Self::from_json_str(&content)
    }
}
