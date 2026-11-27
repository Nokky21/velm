use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "roots",
    "max_depth",
    "include_worktrees",
    "skip",
    "parallel",
    "timeout_ms",
    "remote_timeout_ms",
];

pub(crate) const DEFAULT_ROOT: &str = "~/Desktop/WORK";

/// Configuration for the git repositories panel (`[term.repos]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReposConfig {
    pub enabled: bool,
    pub roots: Vec<PathBuf>,
    pub max_depth: u32,
    pub include_worktrees: bool,
    pub skip: Vec<String>,
    pub parallel: usize,
    pub timeout_ms: u64,
    pub remote_timeout_ms: u64,
}

impl Default for ReposConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            roots: vec![PathBuf::from(DEFAULT_ROOT)],
            max_depth: 2,
            include_worktrees: false,
            skip: vec![
                String::from("node_modules"),
                String::from("target"),
                String::from(".worktrees"),
            ],
            parallel: 8,
            timeout_ms: 5000,
            remote_timeout_ms: 10000,
        }
    }
}
