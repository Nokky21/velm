pub mod ai;
pub mod claude;
pub mod repos;
pub mod sql;
pub mod validate;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub use ai::{AiConfig, AiServer};
pub use claude::{ClaudeConfig, ModelPrice};
pub use repos::ReposConfig;
pub use sql::{SqlColor, SqlConfig, SqlConnection, SqlEnv};
pub use validate::ConfigError;

use sql::RawSqlConfig;

const DEFAULT_CONFIG_REL: &str = "~/.config/velm/config.toml";
const KNOWN_TERM_SECTIONS: &[&str] = &["ai", "repos", "sql", "claude"];

/// Complete `[term]` configuration across all `term-*` tools.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TermConfig {
    pub ai: AiConfig,
    pub repos: ReposConfig,
    pub sql: SqlConfig,
    pub claude: ClaudeConfig,
}

impl TermConfig {
    /// Loads `[term]` from `config_path` (or `~/.config/velm/config.toml` if `None`).
    pub fn load(config_path: Option<&Path>) -> Result<LoadedConfig, ConfigError> {
        load(config_path)
    }

    /// Loads `[term]` from an explicit configuration file path.
    pub fn load_from_path(path: &Path) -> Result<LoadedConfig, ConfigError> {
        load_from_path(path)
    }

    /// Parses and validates `[term]` from TOML document text.
    pub fn load_from_str(content: &str) -> Result<LoadedConfig, ConfigError> {
        load_from_str(content)
    }
}

/// Non-fatal warning emitted for an unknown key inside `[term]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigWarning {
    pub path: String,
    pub message: String,
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Result of loading and validating `[term]`, including non-fatal warnings.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: TermConfig,
    pub warnings: Vec<ConfigWarning>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawTermConfig {
    ai: AiConfig,
    repos: ReposConfig,
    sql: RawSqlConfig,
    claude: ClaudeConfig,
}

/// Returns the default configuration path (`~/.config/velm/config.toml` expanded).
pub fn default_config_path() -> Result<PathBuf, ConfigError> {
    expand_tilde(DEFAULT_CONFIG_REL)
}

/// Expands a leading `~` or `~/` using `HOME` (with `USERPROFILE` fallback on Windows).
pub fn expand_tilde(raw: &str) -> Result<PathBuf, ConfigError> {
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return Ok(join_home_relative(&home_dir()?, rest));
    }
    if cfg!(windows) {
        if let Some(rest) = raw.strip_prefix("~\\") {
            return Ok(join_home_relative(&home_dir()?, rest));
        }
    }
    Ok(PathBuf::from(raw))
}

fn expand_tilde_path(path: &Path) -> Result<PathBuf, ConfigError> {
    match path.to_str() {
        Some(s) => expand_tilde(s),
        None => Ok(path.to_path_buf()),
    }
}

fn join_home_relative(home: &Path, rest: &str) -> PathBuf {
    if rest.is_empty() {
        home.to_path_buf()
    } else {
        home.join(rest)
    }
}

fn home_dir() -> Result<PathBuf, ConfigError> {
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    #[cfg(windows)]
    if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(profile));
    }
    Err(ConfigError::HomeNotSet)
}

/// Loads `[term]` from `config_path` if provided, or from `~/.config/velm/config.toml`.
///
/// When `config_path` is `None` and the default config file does not exist yet,
/// returns validated default settings with `~` expanded and no warnings.
/// When `config_path` is `Some(_)`, the file must exist and be readable.
pub fn load(config_path: Option<&Path>) -> Result<LoadedConfig, ConfigError> {
    match config_path {
        Some(path) => load_from_path(path),
        None => {
            let path = default_config_path()?;
            match std::fs::read_to_string(&path) {
                Ok(content) => load_from_str(&content),
                Err(err) if err.kind() == io::ErrorKind::NotFound => load_from_str(""),
                Err(err) => Err(ConfigError::Io {
                    path,
                    message: err.to_string(),
                }),
            }
        }
    }
}

/// Loads `[term]` from an explicit file path (expanding leading `~` if present).
pub fn load_from_path(path: &Path) -> Result<LoadedConfig, ConfigError> {
    let expanded = expand_tilde_path(path)?;
    let content = std::fs::read_to_string(&expanded).map_err(|err| ConfigError::Io {
        path: expanded,
        message: err.to_string(),
    })?;
    load_from_str(&content)
}

/// Parses `[term]` from TOML text, ignoring unrelated top-level sections.
pub fn load_from_str(content: &str) -> Result<LoadedConfig, ConfigError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let doc: toml::Table = toml::from_str(content).map_err(|err| ConfigError::Parse {
        message: err.to_string(),
    })?;

    let mut warnings = Vec::new();
    let raw = match doc.get("term") {
        Some(term_value) => {
            collect_term_warnings(term_value, &mut warnings);
            term_value
                .clone()
                .try_into::<RawTermConfig>()
                .map_err(|err| ConfigError::Parse {
                    message: err.to_string(),
                })?
        }
        None => RawTermConfig::default(),
    };

    let mut ai = raw.ai;
    validate::validate_and_normalize_ai(&mut ai)?;

    let mut repos = raw.repos;
    repos.roots = repos
        .roots
        .iter()
        .map(|root| expand_tilde_path(root))
        .collect::<Result<Vec<_>, _>>()?;
    validate::validate_repos(&repos)?;

    let sql = validate::validate_and_resolve_sql(raw.sql)?;

    let mut claude = raw.claude;
    claude.projects_dir = expand_tilde_path(&claude.projects_dir)?;
    validate::validate_claude(&claude)?;

    Ok(LoadedConfig {
        config: TermConfig {
            ai,
            repos,
            sql,
            claude,
        },
        warnings,
    })
}

fn collect_term_warnings(term_value: &toml::Value, warnings: &mut Vec<ConfigWarning>) {
    let Some(term_table) = term_value.as_table() else {
        return;
    };

    for (key, value) in term_table {
        if !KNOWN_TERM_SECTIONS.contains(&key.as_str()) {
            push_warning(warnings, format!("term.{key}"));
            continue;
        }

        match key.as_str() {
            "ai" => warn_unknown_table_keys(value, "term.ai", ai::KNOWN_KEYS, warnings),
            "repos" => warn_unknown_table_keys(value, "term.repos", repos::KNOWN_KEYS, warnings),
            "sql" => collect_sql_warnings(value, warnings),
            "claude" => collect_claude_warnings(value, warnings),
            _ => {}
        }
    }
}

fn collect_sql_warnings(value: &toml::Value, warnings: &mut Vec<ConfigWarning>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        if !sql::KNOWN_KEYS.contains(&key.as_str()) {
            push_warning(warnings, format!("term.sql.{key}"));
            continue;
        }
        if key == "connections" {
            if let Some(items) = child.as_array() {
                for (idx, item) in items.iter().enumerate() {
                    warn_unknown_table_keys(
                        item,
                        &format!("term.sql.connections[{idx}]"),
                        sql::KNOWN_CONNECTION_KEYS,
                        warnings,
                    );
                }
            }
        }
    }
}

fn collect_claude_warnings(value: &toml::Value, warnings: &mut Vec<ConfigWarning>) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, child) in table {
        if !claude::KNOWN_KEYS.contains(&key.as_str()) {
            push_warning(warnings, format!("term.claude.{key}"));
            continue;
        }
        if key == "prices" {
            if let Some(models) = child.as_table() {
                for (model_id, price_val) in models {
                    warn_unknown_table_keys(
                        price_val,
                        &format!("term.claude.prices.{model_id}"),
                        claude::KNOWN_PRICE_KEYS,
                        warnings,
                    );
                }
            }
        }
    }
}

fn warn_unknown_table_keys(
    value: &toml::Value,
    prefix: &str,
    known: &[&str],
    warnings: &mut Vec<ConfigWarning>,
) {
    let Some(table) = value.as_table() else {
        return;
    };
    for key in table.keys() {
        if !known.contains(&key.as_str()) {
            push_warning(warnings, format!("{prefix}.{key}"));
        }
    }
}

fn push_warning(warnings: &mut Vec<ConfigWarning>, path: String) {
    let message = format!("clave desconocida en la configuración: {path}");
    warnings.push(ConfigWarning { path, message });
}
