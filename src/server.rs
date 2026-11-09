// src/server.rs
//! Standard IO (stdio) JSON-RPC 2.0 Server for Claude Desktop & Claude Code

use crate::protocol::*;
use crate::router::ToolRouter;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, info};

pub struct StdioServer {
    router: Arc<ToolRouter>,
    server_name: String,
    server_version: String,
}

impl StdioServer {
    pub fn new(
        router: Arc<ToolRouter>,
        server_name: impl Into<String>,
        server_version: impl Into<String>,
    ) -> Self {
        Self {
            router,
            server_name: server_name.into(),
            server_version: server_version.into(),
        }
    }

    /// Runs the server event loop on stdin/stdout
    pub async fn run(&self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        self.run_with_io(stdin, stdout).await
    }

    /// Generic runner to support test mocking with duplex streams
    pub async fn run_with_io<R, W>(&self, reader_io: R, mut writer_io: W) -> Result<()>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        let mut lines = BufReader::new(reader_io).lines();

        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            debug!("Incoming request: {}", trimmed);
            let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
                Ok(r) => r,
                Err(e) => {
                    let err_resp = JsonRpcResponse::error(
                        None,
                        error_codes::PARSE_ERROR,
                        format!("Failed to parse JSON-RPC: {}", e),
                        None,
                    );
                    let mut out = serde_json::to_string(&err_resp)?;
                    out.push('\n');
                    writer_io.write_all(out.as_bytes()).await?;
                    writer_io.flush().await?;
                    continue;
                }
            };

            let response = self.handle_request(req).await;
            if let Some(resp) = response {
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                writer_io.write_all(out.as_bytes()).await?;
                writer_io.flush().await?;
            }
        }

        info!("Stdio server stream terminated cleanly.");
        Ok(())
    }

    pub async fn handle_request(&self, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
        // Notification check (no id = no response)
        let is_notification = req.id.is_none();
        let id = req.id;

        match req.method.as_str() {
            "initialize" => {
                let result = InitializeResult {
                    protocol_version: MCP_PROTOCOL_VERSION.to_string(),
                    capabilities: ServerCapabilities {
                        tools: Some(ToolsCapability { list_changed: true }),
                        resources: None,
                        prompts: None,
                        logging: None,
                        experimental: None,
                    },
                    server_info: ImplementationInfo {
                        name: self.server_name.clone(),
                        version: self.server_version.clone(),
                    },
                    instructions: Some(
                        "MCP-Nexus Gateway: Consolidates multiple upstream MCP servers with dynamic tool search, lazy-loading, and prompt injection defense."
                            .to_string(),
                    ),
                };

                let res_val = serde_json::to_value(result).unwrap_or(Value::Null);
                Some(JsonRpcResponse::success(id, res_val))
            }
            "notifications/initialized" => {
                debug!("Client acknowledged initialization");
                None
            }
            "ping" => Some(JsonRpcResponse::success(id, json!({}))),
            "tools/list" => {
                let tools = self.router.list_tools().await;
                let list_result = ListToolsResult {
                    tools,
                    next_cursor: None,
                };
                let res_val = serde_json::to_value(list_result).unwrap_or(Value::Null);
                Some(JsonRpcResponse::success(id, res_val))
            }
            "tools/call" => {
                let params: Result<CallToolParams, _> = match req.params {
                    Some(p) => serde_json::from_value(p),
                    None => {
                        return Some(JsonRpcResponse::error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "Missing params for tools/call",
                            None,
                        ));
                    }
                };

                let call_params = match params {
                    Ok(p) => p,
                    Err(e) => {
                        return Some(JsonRpcResponse::error(
                            id,
                            error_codes::INVALID_PARAMS,
                            format!("Invalid tools/call params: {}", e),
                            None,
                        ));
                    }
                };

                match self.router.call_tool(&call_params.name, call_params.arguments).await {
                    Ok(call_result) => {
                        let res_val = serde_json::to_value(call_result).unwrap_or(Value::Null);
                        Some(JsonRpcResponse::success(id, res_val))
                    }
                    Err(e) => {
                        // Return as structured tool error so Claude receives the explanation
                        let error_call = CallToolResult {
                            content: vec![ContentItem::text(format!("Tool execution failed: {}", e))],
                            is_error: true,
                        };
                        let res_val = serde_json::to_value(error_call).unwrap_or(Value::Null);
                        Some(JsonRpcResponse::success(id, res_val))
                    }
                }
            }
            _ => {
                if is_notification {
                    None
                } else {
                    Some(JsonRpcResponse::error(
                        id,
                        error_codes::METHOD_NOT_FOUND,
                        format!("Method not supported by MCP-Nexus: {}", req.method),
                        None,
                    ))
                }
            }
        }
    }
}
