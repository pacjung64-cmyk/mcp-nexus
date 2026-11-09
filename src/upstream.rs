// src/upstream.rs
//! Upstream MCP Server Process Management and Communication Client

use crate::protocol::*;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, info, warn};

#[async_trait]
pub trait UpstreamClient: Send + Sync {
    /// Send a generic JSON-RPC request and await response
    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value>;

    /// Lists tools exposed by this upstream
    async fn list_tools(&self) -> Result<Vec<Tool>>;

    /// Invokes a tool on this upstream
    async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<CallToolResult>;

    /// Health check
    async fn is_healthy(&self) -> bool;

    /// Server identifier
    fn name(&self) -> &str;
}

pub struct StdioUpstream {
    name: String,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: AtomicU64,
    child: Arc<Mutex<Option<Child>>>,
    initialized: Arc<Mutex<bool>>,
}

impl StdioUpstream {
    pub async fn new(
        name: impl Into<String>,
        command: impl Into<String>,
        args: Vec<String>,
        env: HashMap<String, String>,
    ) -> Result<Arc<Self>> {
        let upstream = Arc::new(Self {
            name: name.into(),
            command: command.into(),
            args,
            env,
            stdin: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            child: Arc::new(Mutex::new(None)),
            initialized: Arc::new(Mutex::new(false)),
        });

        upstream.spawn_and_handshake().await?;
        Ok(upstream)
    }

    async fn spawn_and_handshake(self: &Arc<Self>) -> Result<()> {
        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args);
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::inherit());

        let mut child = cmd.spawn().with_context(|| {
            format!("Failed to spawn upstream process: {} {:?}", self.command, self.args)
        })?;

        let stdin = child.stdin.take().context("Failed to open child stdin")?;
        let stdout = child.stdout.take().context("Failed to open child stdout")?;

        *self.stdin.lock().await = Some(stdin);
        *self.child.lock().await = Some(child);

        // Spawn background stdout reader loop
        let pending = self.pending.clone();
        let name_clone = self.name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_str::<JsonRpcResponse>(trimmed) {
                    Ok(resp) => {
                        if let Some(id_val) = &resp.id {
                            if let Some(num_id) = id_val.as_u64() {
                                if let Some(sender) = pending.lock().await.remove(&num_id) {
                                    let _ = sender.send(resp);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        debug!("[{}] Ignored non-response line: {} ({})", name_clone, trimmed, e);
                    }
                }
            }
            warn!("[{}] Upstream stdout closed", name_clone);
        });

        // Handshake: initialize
        let init_params = serde_json::to_value(InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            capabilities: ClientCapabilities::default(),
            client_info: ImplementationInfo {
                name: "mcp-nexus".to_string(),
                version: "0.1.0".to_string(),
            },
        })?;

        let _ = self.send_request("initialize", Some(init_params)).await?;
        let _ = self.send_notification("notifications/initialized", None).await;

        *self.initialized.lock().await = true;
        info!("[{}] Handshake initialized successfully", self.name);
        Ok(())
    }

    pub async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<()> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: method.to_string(),
            params,
        };
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        let mut stdin_guard = self.stdin.lock().await;
        if let Some(stdin) = stdin_guard.as_mut() {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }
        Ok(())
    }
}

#[async_trait]
impl UpstreamClient for StdioUpstream {
    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Value::from(id)),
            method: method.to_string(),
            params,
        };

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        {
            let mut stdin_guard = self.stdin.lock().await;
            let stdin = stdin_guard.as_mut().context("Upstream stdin not open")?;
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await?;
        }

        let resp = rx.await.context("Upstream dropped response channel")?;
        if let Some(err) = resp.error {
            anyhow::bail!("JSON-RPC Error {}: {}", err.code, err.message);
        }
        resp.result.context("Empty result in JSON-RPC response")
    }

    async fn list_tools(&self) -> Result<Vec<Tool>> {
        let res = self.send_request("tools/list", None).await?;
        let list_res: ListToolsResult = serde_json::from_value(res)?;
        Ok(list_res.tools)
    }

    async fn call_tool(&self, name: &str, arguments: Option<Value>) -> Result<CallToolResult> {
        let params = serde_json::to_value(CallToolParams {
            name: name.to_string(),
            arguments,
        })?;
        let res = self.send_request("tools/call", Some(params)).await?;
        let call_res: CallToolResult = serde_json::from_value(res)?;
        Ok(call_res)
    }

    async fn is_healthy(&self) -> bool {
        *self.initialized.lock().await
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Hand-written Mock Upstream for deterministic testing without external binaries
#[derive(Clone)]
pub struct FakeUpstream {
    pub name: String,
    pub tools: Vec<Tool>,
    pub responses: Arc<Mutex<HashMap<String, CallToolResult>>>,
}

impl FakeUpstream {
    pub fn new(name: impl Into<String>, tools: Vec<Tool>) -> Self {
        Self {
            name: name.into(),
            tools,
            responses: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn set_response(&self, tool_name: &str, result: CallToolResult) {
        self.responses.lock().await.insert(tool_name.to_string(), result);
    }
}

#[async_trait]
impl UpstreamClient for FakeUpstream {
    async fn send_request(&self, method: &str, _params: Option<Value>) -> Result<Value> {
        if method == "tools/list" {
            let res = ListToolsResult {
                tools: self.tools.clone(),
                next_cursor: None,
            };
            return Ok(serde_json::to_value(res)?);
        }
        Ok(Value::Null)
    }

    async fn list_tools(&self) -> Result<Vec<Tool>> {
        Ok(self.tools.clone())
    }

    async fn call_tool(&self, name: &str, _arguments: Option<Value>) -> Result<CallToolResult> {
        if let Some(resp) = self.responses.lock().await.get(name) {
            return Ok(resp.clone());
        }
        Ok(CallToolResult {
            content: vec![ContentItem::text(format!("Mock response from {}", name))],
            is_error: false,
        })
    }

    async fn is_healthy(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        &self.name
    }
}
