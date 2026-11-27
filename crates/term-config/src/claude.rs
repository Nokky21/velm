use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "projects_dir",
    "days",
    "context_alert_tokens",
    "prices",
];

pub(crate) const KNOWN_PRICE_KEYS: &[&str] = &[
    "input",
    "output",
    "cache_write_5m",
    "cache_write_1h",
    "cache_read",
];

pub(crate) const DEFAULT_PROJECTS_DIR: &str = "~/.claude/projects";

/// USD prices per million tokens for a Claude model (`[term.claude.prices."<model>"]`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// Configuration for the Claude Code sessions panel (`[term.claude]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    pub enabled: bool,
    pub projects_dir: PathBuf,
    pub days: u32,
    pub context_alert_tokens: u64,
    pub prices: BTreeMap<String, ModelPrice>,
}

impl ClaudeConfig {
    /// Returns the configured price entry for `model`, if present.
    pub fn price_for(&self, model: &str) -> Option<&ModelPrice> {
        self.prices.get(model)
    }
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            projects_dir: PathBuf::from(DEFAULT_PROJECTS_DIR),
            days: 7,
            context_alert_tokens: 250_000,
            prices: BTreeMap::new(),
        }
    }
}
