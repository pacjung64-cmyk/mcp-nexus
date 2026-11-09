// src/telemetry.rs
//! Observability, Latency Profiler and Token Telemetry for MCP Tool Calls

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub server_name: String,
    pub tool_name: String,
    pub latency_ms: u64,
    pub is_error: bool,
    pub output_bytes: usize,
    pub estimated_tokens: usize,
    pub timestamp_epoch_secs: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ToolStats {
    pub total_calls: u64,
    pub total_errors: u64,
    pub total_latency_ms: u64,
    pub min_latency_ms: u64,
    pub max_latency_ms: u64,
    pub total_output_bytes: usize,
    pub total_estimated_tokens: usize,
}

impl ToolStats {
    pub fn avg_latency_ms(&self) -> f64 {
        if self.total_calls == 0 {
            0.0
        } else {
            self.total_latency_ms as f64 / self.total_calls as f64
        }
    }
}

#[derive(Clone, Default)]
pub struct Telemetry {
    records: Arc<Mutex<Vec<ToolCallRecord>>>,
    stats: Arc<Mutex<HashMap<String, ToolStats>>>,
}

impl Telemetry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_call(
        &self,
        server_name: &str,
        tool_name: &str,
        duration: Duration,
        is_error: bool,
        output_bytes: usize,
    ) {
        let latency_ms = duration.as_millis() as u64;
        let estimated_tokens = (output_bytes + 3) / 4;
        let key = format!("{}::{}", server_name, tool_name);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let record = ToolCallRecord {
            server_name: server_name.to_string(),
            tool_name: tool_name.to_string(),
            latency_ms,
            is_error,
            output_bytes,
            estimated_tokens,
            timestamp_epoch_secs: now,
        };

        if let Ok(mut recs) = self.records.lock() {
            if recs.len() >= 5000 {
                recs.remove(0);
            }
            recs.push(record);
        }

        if let Ok(mut st) = self.stats.lock() {
            let entry = st.entry(key).or_insert_with(|| ToolStats {
                min_latency_ms: u64::MAX,
                ..Default::default()
            });

            entry.total_calls += 1;
            if is_error {
                entry.total_errors += 1;
            }
            entry.total_latency_ms += latency_ms;
            entry.total_output_bytes += output_bytes;
            entry.total_estimated_tokens += estimated_tokens;

            if latency_ms < entry.min_latency_ms {
                entry.min_latency_ms = latency_ms;
            }
            if latency_ms > entry.max_latency_ms {
                entry.max_latency_ms = latency_ms;
            }
        }
    }

    pub fn get_stats_snapshot(&self) -> HashMap<String, ToolStats> {
        self.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn generate_summary_table(&self) -> String {
        let stats = self.get_stats_snapshot();
        if stats.is_empty() {
            return "No tool calls recorded yet.".to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "{:<35} | {:>6} | {:>6} | {:>9} | {:>9} | {:>9} | {:>10}",
            "Tool", "Calls", "Errors", "Avg Lat.", "Min Lat.", "Max Lat.", "Est. Tokens"
        ));
        lines.push("-".repeat(98));

        for (tool, s) in stats {
            lines.push(format!(
                "{:<35} | {:>6} | {:>6} | {:>7.1}ms | {:>7}ms | {:>7}ms | {:>10}",
                tool,
                s.total_calls,
                s.total_errors,
                s.avg_latency_ms(),
                if s.min_latency_ms == u64::MAX { 0 } else { s.min_latency_ms },
                s.max_latency_ms,
                s.total_estimated_tokens
            ));
        }

        lines.join("\n")
    }
}
