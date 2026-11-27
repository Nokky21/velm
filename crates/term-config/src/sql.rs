use std::fmt;

use serde::{Deserialize, Serialize};

pub(crate) const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "row_limit",
    "statement_timeout_s",
    "idle_tx_timeout_s",
    "connections",
];

pub(crate) const KNOWN_CONNECTION_KEYS: &[&str] = &["name", "env", "color"];

/// Allowed target environment for a configured SQL connection.
///
/// `prod` is intentionally absent and rejected by configuration validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SqlEnv {
    Local,
    Dev,
}

impl SqlEnv {
    /// Returns the default UI badge color associated with this environment.
    pub const fn default_color(self) -> SqlColor {
        match self {
            Self::Local => SqlColor::Green,
            Self::Dev => SqlColor::Yellow,
        }
    }

    /// Returns the canonical TOML string representation of this environment.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Dev => "dev",
        }
    }
}

impl fmt::Display for SqlEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Allowed UI badge color for a SQL connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SqlColor {
    Green,
    Yellow,
    Blue,
    Magenta,
}

impl SqlColor {
    /// Returns the canonical TOML string representation of this color.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Blue => "blue",
            Self::Magenta => "magenta",
        }
    }
}

impl fmt::Display for SqlColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Metadata for a SQL connection stored in `[[term.sql.connections]]`.
///
/// Connection strings are never stored in config; they live in the OS keychain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SqlConnection {
    pub name: String,
    pub env: SqlEnv,
    pub color: SqlColor,
}

/// Configuration for the safe Postgres panel (`[term.sql]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SqlConfig {
    pub enabled: bool,
    pub row_limit: usize,
    pub statement_timeout_s: u64,
    pub idle_tx_timeout_s: u64,
    pub connections: Vec<SqlConnection>,
}

impl SqlConfig {
    /// Looks up a configured connection by exact name.
    pub fn connection(&self, name: &str) -> Option<&SqlConnection> {
        self.connections.iter().find(|conn| conn.name == name)
    }
}

impl Default for SqlConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            row_limit: 500,
            statement_timeout_s: 30,
            idle_tx_timeout_s: 60,
            connections: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub(crate) struct RawSqlConfig {
    pub enabled: bool,
    pub row_limit: usize,
    pub statement_timeout_s: u64,
    pub idle_tx_timeout_s: u64,
    pub connections: Vec<RawSqlConnection>,
}

impl Default for RawSqlConfig {
    fn default() -> Self {
        let defaults = SqlConfig::default();
        Self {
            enabled: defaults.enabled,
            row_limit: defaults.row_limit,
            statement_timeout_s: defaults.statement_timeout_s,
            idle_tx_timeout_s: defaults.idle_tx_timeout_s,
            connections: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawSqlConnection {
    pub name: String,
    pub env: String,
    pub color: Option<SqlColor>,
}
