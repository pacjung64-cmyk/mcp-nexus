// src/lib.rs
//! MCP-Nexus: High-Performance Gateway, Router & Injection Shield for Model Context Protocol (MCP)

pub mod config;
pub mod protocol;
pub mod router;
pub mod server;
pub mod shield;
pub mod telemetry;
pub mod upstream;

pub use config::NexusConfig;
pub use protocol::*;
pub use router::ToolRouter;
pub use server::StdioServer;
pub use shield::{InjectionShield, ShieldConfig, ShieldMode};
pub use telemetry::Telemetry;
pub use upstream::{FakeUpstream, StdioUpstream, UpstreamClient};
