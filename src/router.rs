// src/router.rs
//! Virtual Tool Registry, Namespacing, and Multiplexed Router

use crate::protocol::*;
use crate::shield::InjectionShield;
use crate::telemetry::Telemetry;
use crate::upstream::UpstreamClient;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub struct ToolRoute {
    pub server_name: String,
    pub original_tool_name: String,
    pub virtual_tool: Tool,
}

pub struct ToolRouter {
    upstreams: RwLock<HashMap<String, Arc<dyn UpstreamClient>>>,
    routes: RwLock<HashMap<String, ToolRoute>>,
    shield: Arc<InjectionShield>,
    telemetry: Arc<Telemetry>,
    enable_lazy_search: bool,
}

impl ToolRouter {
    pub fn new(shield: Arc<InjectionShield>, telemetry: Arc<Telemetry>, enable_lazy_search: bool) -> Self {
        Self {
            upstreams: RwLock::new(HashMap::new()),
            routes: RwLock::new(HashMap::new()),
            shield,
            telemetry,
            enable_lazy_search,
        }
    }

    /// Register an upstream client and refresh virtual routes
    pub async fn register_upstream(&self, client: Arc<dyn UpstreamClient>) -> Result<()> {
        let name = client.name().to_string();
        info!("Registering upstream server: {}", name);

        let tools = client.list_tools().await.unwrap_or_else(|e| {
            warn!("Failed to list tools for {}: {}", name, e);
            Vec::new()
        });

        let mut upstreams_guard = self.upstreams.write().await;
        upstreams_guard.insert(name.clone(), client);

        let mut routes_guard = self.routes.write().await;
        for tool in tools {
            let namespaced_name = format!("{}__{}", name, tool.name);
            let mut v_tool = tool.clone();
            v_tool.name = namespaced_name.clone();

            routes_guard.insert(
                namespaced_name,
                ToolRoute {
                    server_name: name.clone(),
                    original_tool_name: tool.name.clone(),
                    virtual_tool: v_tool,
                },
            );

            // Also register un-prefixed name if no collision exists yet
            if !routes_guard.contains_key(&tool.name) {
                routes_guard.insert(
                    tool.name.clone(),
                    ToolRoute {
                        server_name: name.clone(),
                        original_tool_name: tool.name.clone(),
                        virtual_tool: tool.clone(),
                    },
                );
            }
        }

        Ok(())
    }

    /// Lists all tools across all upstreams (including the nexus_search_tools meta-tool)
    pub async fn list_tools(&self) -> Vec<Tool> {
        let routes_guard = self.routes.read().await;
        let mut list: Vec<Tool> = routes_guard.values().map(|r| r.virtual_tool.clone()).collect();

        // Sort for deterministic presentation
        list.sort_by(|a, b| a.name.cmp(&b.name));

        if self.enable_lazy_search {
            list.insert(
                0,
                Tool {
                    name: "nexus_search_tools".to_string(),
                    description: Some("Search available MCP tools across all registered servers by keyword to save context window tokens.".to_string()),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "Keyword or capability to search for (e.g. 'git', 'database', 'scrape')"
                            }
                        },
                        "required": ["query"]
                    }),
                },
            );
        }

        list
    }

    /// Dispatches a tool call to the appropriate upstream and filters output with shield
    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<CallToolResult> {
        // Built-in meta tool: nexus_search_tools
        if name == "nexus_search_tools" {
            let query = arguments
                .as_ref()
                .and_then(|a| a.get("query"))
                .and_then(|q| q.as_str())
                .unwrap_or("")
                .to_lowercase();

            let routes = self.routes.read().await;
            let mut matches = Vec::new();

            for (t_name, route) in routes.iter() {
                if t_name.contains(&query)
                    || route
                        .virtual_tool
                        .description
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&query)
                {
                    matches.push(json!({
                        "name": t_name,
                        "server": route.server_name,
                        "description": route.virtual_tool.description,
                        "inputSchema": route.virtual_tool.input_schema
                    }));
                }
            }

            let result_text = serde_json::to_string_pretty(&json!({
                "query": query,
                "matchesCount": matches.len(),
                "tools": matches
            }))?;

            return Ok(CallToolResult {
                content: vec![ContentItem::text(result_text)],
                is_error: false,
            });
        }

        let route_opt = {
            let routes_guard = self.routes.read().await;
            routes_guard.get(name).map(|r| (r.server_name.clone(), r.original_tool_name.clone()))
        };

        let (server_name, original_name) = match route_opt {
            Some(pair) => pair,
            None => {
                anyhow::bail!("Tool not found: {}", name);
            }
        };

        let upstream = {
            let upstreams_guard = self.upstreams.read().await;
            upstreams_guard
                .get(&server_name)
                .cloned()
                .with_context(|| format!("Upstream server '{}' not connected", server_name))?
        };

        let start = Instant::now();
        let call_res = upstream.call_tool(&original_name, arguments).await;
        let duration = start.elapsed();

        match call_res {
            Ok(mut res) => {
                let mut total_bytes = 0;

                // Inspect and sanitize output through security shield
                for item in &mut res.content {
                    if let ContentItem::Text { text } = item {
                        let (filtered_text, was_flagged) = self.shield.filter(text);
                        if was_flagged {
                            warn!(
                                "[Shield] Filtered potential indirect prompt injection from {}:{}",
                                server_name, original_name
                            );
                        }
                        total_bytes += filtered_text.len();
                        *text = filtered_text;
                    }
                }

                self.telemetry.record_call(
                    &server_name,
                    name,
                    duration,
                    res.is_error,
                    total_bytes,
                );

                Ok(res)
            }
            Err(e) => {
                self.telemetry.record_call(
                    &server_name,
                    name,
                    duration,
                    true,
                    0,
                );
                Err(e)
            }
        }
    }
}
