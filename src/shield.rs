// src/shield.rs
//! Zero-Latency Indirect Prompt Injection Shield & Content Filter

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShieldMode {
    Off,
    Sanitize,
    Block,
}

impl Default for ShieldMode {
    fn default() -> Self {
        ShieldMode::Sanitize
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShieldConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub mode: ShieldMode,
    #[serde(default)]
    pub custom_block_patterns: Vec<String>,
}

fn default_true() -> bool {
    true
}

impl Default for ShieldConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: ShieldMode::Sanitize,
            custom_block_patterns: Vec::new(),
        }
    }
}

pub struct ShieldFinding {
    pub pattern_name: &'static str,
    pub matched_snippet: String,
}

pub struct InjectionShield {
    config: ShieldConfig,
}

static INJECTION_PATTERNS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();

fn get_patterns() -> &'static Vec<(&'static str, Regex)> {
    INJECTION_PATTERNS.get_or_init(|| {
        vec![
            (
                "IGNORE_INSTRUCTIONS",
                Regex::new(r"(?i)(ignore\s+(all\s+)?(prior|previous|existing)\s+instructions)").unwrap(),
            ),
            (
                "SYSTEM_ROLE_OVERRIDE",
                Regex::new(r"(?i)((\n|\A)\s*(system|admin|root)\s*:\s*(override|you are now|disregard))").unwrap(),
            ),
            (
                "DEVELOPER_MODE_JAILBREAK",
                Regex::new(r"(?i)(you are now in (developer|god|unrestricted|dan) mode)").unwrap(),
            ),
            (
                "SPOOFED_SYSTEM_TAGS",
                Regex::new(r"(?i)(<(system|system_warning|behavior_instructions|claude_behavior|ethics_reminders)[^>]*>)").unwrap(),
            ),
            (
                "DATA_EXFILTRATION_WEBHOOK",
                Regex::new(r"(?i)(https?://(webhook\.site|pipedream\.net|burpcollaborator\.net|requestbin\.net)[^\s\)]+)").unwrap(),
            ),
            (
                "DELIMITER_ESCAPE",
                Regex::new(r"(?i)(===\s*END OF INSTRUCTIONS\s*===)").unwrap(),
            ),
        ]
    })
}

impl InjectionShield {
    pub fn new(config: ShieldConfig) -> Self {
        Self { config }
    }

    /// Inspects raw text and returns findings if any suspicious patterns are matched
    pub fn scan(&self, text: &str) -> Vec<ShieldFinding> {
        if !self.config.enabled || self.config.mode == ShieldMode::Off {
            return Vec::new();
        }

        let mut findings = Vec::new();
        let patterns = get_patterns();

        for (name, re) in patterns {
            if let Some(mat) = re.find(text) {
                findings.push(ShieldFinding {
                    pattern_name: name,
                    matched_snippet: mat.as_str().to_string(),
                });
            }
        }

        findings
    }

    /// Processes text according to configured ShieldMode
    pub fn filter(&self, text: &str) -> (String, bool) {
        if !self.config.enabled || self.config.mode == ShieldMode::Off {
            return (text.to_string(), false);
        }

        let findings = self.scan(text);
        if findings.is_empty() {
            return (text.to_string(), false);
        }

        match self.config.mode {
            ShieldMode::Off => (text.to_string(), false),
            ShieldMode::Block => {
                let report = format!(
                    "[MCP-NEXUS SHIELD ERROR]: Upstream tool output blocked due to detected Prompt Injection: {} (Snippet: {:?})",
                    findings[0].pattern_name, findings[0].matched_snippet
                );
                (report, true)
            }
            ShieldMode::Sanitize => {
                let mut sanitized = text.to_string();
                let patterns = get_patterns();
                for (name, re) in patterns {
                    sanitized = re.replace_all(&sanitized, |_caps: &regex::Captures| {
                        format!("[Filtered by MCP-Nexus Shield: {}]", name)
                    }).to_string();
                }
                (sanitized, true)
            }
        }
    }
}
