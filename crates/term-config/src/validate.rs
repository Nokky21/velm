use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use crate::ai::AiConfig;
use crate::claude::{ClaudeConfig, ModelPrice};
use crate::repos::ReposConfig;
use crate::sql::{RawSqlConfig, RawSqlConnection, SqlConfig, SqlConnection, SqlEnv};

/// Error returned when reading, parsing, or validating `[term]` configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Io { path: PathBuf, message: String },
    Parse { message: String },
    HomeNotSet,
    InvalidValue { key: String, message: String },
    InvalidAiUrl { url: String, message: String },
    ProdEnvForbidden { connection: String },
    DuplicateConnectionName { name: String },
    MissingDirectory { key: String, path: PathBuf },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => {
                write!(
                    f,
                    "no se pudo leer el fichero de configuración {}: {message}",
                    path.display()
                )
            }
            Self::Parse { message } => {
                write!(f, "error de sintaxis en la configuración: {message}")
            }
            Self::HomeNotSet => {
                write!(
                    f,
                    "no se pudo expandir '~': la variable HOME no está definida"
                )
            }
            Self::InvalidValue { key, message } => {
                write!(f, "valor inválido en {key}: {message}")
            }
            Self::InvalidAiUrl { url, message } => {
                write!(f, "URL de IA inválida ({url}): {message}")
            }
            Self::ProdEnvForbidden { connection } => {
                write!(
                    f,
                    "la conexión SQL '{connection}' tiene env = \"prod\", lo cual está prohibido"
                )
            }
            Self::DuplicateConnectionName { name } => {
                write!(f, "nombre de conexión SQL duplicado: '{name}'")
            }
            Self::MissingDirectory { key, path } => {
                write!(
                    f,
                    "la carpeta configurada en {key} no existe o no es un directorio: {}",
                    path.display()
                )
            }
        }
    }
}

impl Error for ConfigError {}

/// Validates and normalizes `[term.ai]`.
pub(crate) fn validate_and_normalize_ai(ai: &mut AiConfig) -> Result<(), ConfigError> {
    if !(0.0..=1.0).contains(&ai.temperature) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.ai.temperature"),
            message: format!("debe estar entre 0.0 y 1.0 (recibido: {})", ai.temperature),
        });
    }
    if !(64..=8192).contains(&ai.max_tokens_suggest) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.ai.max_tokens_suggest"),
            message: format!(
                "debe estar entre 64 y 8192 (recibido: {})",
                ai.max_tokens_suggest
            ),
        });
    }
    if !(256..=16384).contains(&ai.max_tokens_explain) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.ai.max_tokens_explain"),
            message: format!(
                "debe estar entre 256 y 16384 (recibido: {})",
                ai.max_tokens_explain
            ),
        });
    }
    if !(30..=86400).contains(&ai.lmstudio_ttl_seconds) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.ai.lmstudio_ttl_seconds"),
            message: format!(
                "debe estar entre 30 y 86400 (recibido: {})",
                ai.lmstudio_ttl_seconds
            ),
        });
    }
    if ai.zsh_key.trim().is_empty() {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.ai.zsh_key"),
            message: String::from("no puede estar vacío"),
        });
    }
    if ai.explain_tail_lines < 1 {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.ai.explain_tail_lines"),
            message: format!("debe ser >= 1 (recibido: {})", ai.explain_tail_lines),
        });
    }
    if !(1000..=100_000).contains(&ai.explain_max_chars) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.ai.explain_max_chars"),
            message: format!(
                "debe estar entre 1000 y 100000 (recibido: {})",
                ai.explain_max_chars
            ),
        });
    }
    if !ai.url.is_empty() {
        ai.url = normalize_ai_url(&ai.url)?;
    }
    Ok(())
}

/// Validates and normalizes an explicit `[term.ai].url` according to CONTRATO §A.1 and §C.6.
///
/// Empty strings are rejected here; callers treat an empty `url` as auto-detection.
/// Does not perform DNS resolution: only accepts literal loopback addresses (`127.0.0.0/8`,
/// `::1`, `[::1]`) or `localhost`, requires scheme `http`, and normalizes the path to end
/// with `/v1` without a trailing slash.
pub fn normalize_ai_url(raw: &str) -> Result<String, ConfigError> {
    if raw.is_empty() || raw.chars().any(char::is_whitespace) {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: String::from("URL vacía o con espacios"),
        });
    }

    let Some(scheme_end) = raw.find("://") else {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: String::from("falta el esquema http://"),
        });
    };
    let scheme = &raw[..scheme_end];
    if !scheme.eq_ignore_ascii_case("http") {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: format!("solo se permite el esquema http (recibido: '{scheme}')"),
        });
    }

    let rest = &raw[scheme_end + 3..];
    if rest.contains('?') || rest.contains('#') {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: String::from("no se permiten parámetros de consulta ni fragmentos"),
        });
    }

    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, ""),
    };

    if authority.is_empty() || authority.contains('@') {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: String::from("autoridad ausente o con credenciales"),
        });
    }

    let normalized_authority = validate_and_normalize_authority(raw, authority)?;
    let normalized_path = normalize_ai_path(raw, path)?;

    Ok(format!("http://{normalized_authority}{normalized_path}"))
}

/// Returns `true` if `host` is a literal local loopback address or `localhost`.
///
/// Does not perform DNS resolution.
pub fn is_local_ai_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower == "localhost" || is_ipv4_loopback(&lower) || is_ipv6_loopback(&lower)
}

fn validate_and_normalize_authority(raw: &str, authority: &str) -> Result<String, ConfigError> {
    if let Some(after_open) = authority.strip_prefix('[') {
        let Some(close_idx) = after_open.find(']') else {
            return Err(ConfigError::InvalidAiUrl {
                url: raw.to_owned(),
                message: String::from("dirección IPv6 sin corchete de cierre"),
            });
        };
        let ipv6_inner = &after_open[..close_idx];
        if !is_ipv6_loopback_inner(ipv6_inner) {
            return Err(ConfigError::InvalidAiUrl {
                url: raw.to_owned(),
                message: String::from("solo se permiten servidores de esta máquina"),
            });
        }
        let port_part = &after_open[close_idx + 1..];
        if port_part.is_empty() {
            return Ok(String::from("[::1]"));
        }
        let port = parse_port_suffix(raw, port_part)?;
        return Ok(format!("[::1]:{port}"));
    }

    if authority.contains(']') {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: String::from("corchete inesperado en el host"),
        });
    }

    if authority == "::1" {
        return Ok(String::from("[::1]"));
    }

    let (host, port_opt) = match authority.split_once(':') {
        Some((host, port_str)) => {
            if port_str.contains(':') {
                return Err(ConfigError::InvalidAiUrl {
                    url: raw.to_owned(),
                    message: String::from("formato de host o IPv6 no válido"),
                });
            }
            let port = parse_port(raw, port_str)?;
            (host, Some(port))
        }
        None => (authority, None),
    };

    let lower_host = host.to_ascii_lowercase();
    if lower_host != "localhost" && !is_ipv4_loopback(&lower_host) {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: String::from("solo se permiten servidores de esta máquina"),
        });
    }

    Ok(match port_opt {
        Some(port) => format!("{lower_host}:{port}"),
        None => lower_host,
    })
}

fn parse_port_suffix(raw: &str, port_part: &str) -> Result<u16, ConfigError> {
    let Some(port_str) = port_part.strip_prefix(':') else {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: String::from("sufijo inválido tras la dirección IPv6"),
        });
    };
    parse_port(raw, port_str)
}

fn parse_port(raw: &str, port_str: &str) -> Result<u16, ConfigError> {
    if port_str.is_empty() || !port_str.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: format!("puerto inválido: '{port_str}'"),
        });
    }
    let port = port_str
        .parse::<u16>()
        .map_err(|_| ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: format!("puerto fuera de rango: '{port_str}'"),
        })?;
    if port == 0 {
        return Err(ConfigError::InvalidAiUrl {
            url: raw.to_owned(),
            message: String::from("el puerto 0 no es válido"),
        });
    }
    Ok(port)
}

fn normalize_ai_path(raw: &str, path: &str) -> Result<String, ConfigError> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return Ok(String::from("/v1"));
    }
    if trimmed == "/v1" || trimmed.ends_with("/v1") {
        return Ok(trimmed.to_owned());
    }
    Err(ConfigError::InvalidAiUrl {
        url: raw.to_owned(),
        message: format!("la ruta debe terminar en /v1 (recibido: '{path}')"),
    })
}

fn parse_decimal_u8(s: &str) -> Option<u8> {
    if s.is_empty() || s.len() > 3 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if s.len() > 1 && s.starts_with('0') {
        return None;
    }
    s.parse::<u8>().ok()
}

fn is_ipv4_loopback(host: &str) -> bool {
    let mut parts = host.split('.');
    let (Some(a), Some(_b), Some(_c), Some(_d), None) = (
        parts.next().and_then(parse_decimal_u8),
        parts.next().and_then(parse_decimal_u8),
        parts.next().and_then(parse_decimal_u8),
        parts.next().and_then(parse_decimal_u8),
        parts.next(),
    ) else {
        return false;
    };
    a == 127
}

fn is_ipv6_loopback(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return is_ipv6_loopback_inner(inner);
    }
    is_ipv6_loopback_inner(host)
}

fn is_ipv6_loopback_inner(inner: &str) -> bool {
    let lower = inner.to_ascii_lowercase();
    lower == "::1"
        || lower == "0:0:0:0:0:0:0:1"
        || lower == "0000:0000:0000:0000:0000:0000:0000:0001"
}

/// Validates `[term.repos]`.
pub(crate) fn validate_repos(repos: &ReposConfig) -> Result<(), ConfigError> {
    if !(1..=5).contains(&repos.max_depth) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.repos.max_depth"),
            message: format!("debe estar entre 1 y 5 (recibido: {})", repos.max_depth),
        });
    }
    for entry in &repos.skip {
        if entry.is_empty()
            || entry == "."
            || entry == ".."
            || entry.contains('/')
            || entry.contains('\\')
        {
            return Err(ConfigError::InvalidValue {
                key: String::from("term.repos.skip"),
                message: format!(
                    "debe contener nombres de carpeta, no rutas (recibido: '{entry}')"
                ),
            });
        }
    }
    if !(1..=32).contains(&repos.parallel) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.repos.parallel"),
            message: format!("debe estar entre 1 y 32 (recibido: {})", repos.parallel),
        });
    }
    if !(500..=60000).contains(&repos.timeout_ms) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.repos.timeout_ms"),
            message: format!(
                "debe estar entre 500 y 60000 (recibido: {})",
                repos.timeout_ms
            ),
        });
    }
    if !(1000..=60000).contains(&repos.remote_timeout_ms) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.repos.remote_timeout_ms"),
            message: format!(
                "debe estar entre 1000 y 60000 (recibido: {})",
                repos.remote_timeout_ms
            ),
        });
    }
    if repos.enabled {
        for root in &repos.roots {
            if !root.is_dir() {
                return Err(ConfigError::MissingDirectory {
                    key: String::from("term.repos.roots"),
                    path: root.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Validates and resolves `[term.sql]`.
pub(crate) fn validate_and_resolve_sql(raw: RawSqlConfig) -> Result<SqlConfig, ConfigError> {
    if !(1..=100_000).contains(&raw.row_limit) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.sql.row_limit"),
            message: format!("debe estar entre 1 y 100000 (recibido: {})", raw.row_limit),
        });
    }
    if !(1..=600).contains(&raw.statement_timeout_s) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.sql.statement_timeout_s"),
            message: format!(
                "debe estar entre 1 y 600 (recibido: {})",
                raw.statement_timeout_s
            ),
        });
    }
    if !(10..=600).contains(&raw.idle_tx_timeout_s) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.sql.idle_tx_timeout_s"),
            message: format!(
                "debe estar entre 10 y 600 (recibido: {})",
                raw.idle_tx_timeout_s
            ),
        });
    }

    let mut seen_names = HashSet::with_capacity(raw.connections.len());
    let mut connections = Vec::with_capacity(raw.connections.len());

    for conn in raw.connections {
        let resolved = validate_and_resolve_connection(conn)?;
        if !seen_names.insert(resolved.name.clone()) {
            return Err(ConfigError::DuplicateConnectionName {
                name: resolved.name,
            });
        }
        connections.push(resolved);
    }

    Ok(SqlConfig {
        enabled: raw.enabled,
        row_limit: raw.row_limit,
        statement_timeout_s: raw.statement_timeout_s,
        idle_tx_timeout_s: raw.idle_tx_timeout_s,
        connections,
    })
}

fn validate_and_resolve_connection(raw: RawSqlConnection) -> Result<SqlConnection, ConfigError> {
    if !is_valid_connection_name(&raw.name) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.sql.connections.name"),
            message: format!("debe cumplir [a-z0-9-]{{1,40}} (recibido: '{}')", raw.name),
        });
    }

    let env = match raw.env.as_str() {
        "local" => SqlEnv::Local,
        "dev" => SqlEnv::Dev,
        "prod" => {
            return Err(ConfigError::ProdEnvForbidden {
                connection: raw.name,
            });
        }
        other => {
            return Err(ConfigError::InvalidValue {
                key: format!("term.sql.connections.{}.env", raw.name),
                message: format!("solo se admite 'local' o 'dev' (recibido: '{other}')"),
            });
        }
    };

    let color = raw.color.unwrap_or_else(|| env.default_color());

    Ok(SqlConnection {
        name: raw.name,
        env,
        color,
    })
}

/// Checks whether `name` matches `[a-z0-9-]{1,40}`.
pub fn is_valid_connection_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// Validates `[term.claude]`.
pub(crate) fn validate_claude(claude: &ClaudeConfig) -> Result<(), ConfigError> {
    if !(1..=365).contains(&claude.days) {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.claude.days"),
            message: format!("debe estar entre 1 y 365 (recibido: {})", claude.days),
        });
    }
    if claude.context_alert_tokens < 1000 {
        return Err(ConfigError::InvalidValue {
            key: String::from("term.claude.context_alert_tokens"),
            message: format!(
                "debe ser >= 1000 (recibido: {})",
                claude.context_alert_tokens
            ),
        });
    }
    for (model, price) in &claude.prices {
        if model.trim().is_empty() {
            return Err(ConfigError::InvalidValue {
                key: String::from("term.claude.prices"),
                message: String::from("el id de modelo no puede estar vacío"),
            });
        }
        validate_model_price(model, price)?;
    }
    if claude.enabled && !claude.projects_dir.is_dir() {
        return Err(ConfigError::MissingDirectory {
            key: String::from("term.claude.projects_dir"),
            path: claude.projects_dir.clone(),
        });
    }
    Ok(())
}

fn validate_model_price(model: &str, price: &ModelPrice) -> Result<(), ConfigError> {
    let fields = [
        ("input", price.input),
        ("output", price.output),
        ("cache_write_5m", price.cache_write_5m),
        ("cache_write_1h", price.cache_write_1h),
        ("cache_read", price.cache_read),
    ];
    for (field, value) in fields {
        if !value.is_finite() || value < 0.0 {
            return Err(ConfigError::InvalidValue {
                key: format!("term.claude.prices.{model}.{field}"),
                message: format!("debe ser un número finito >= 0.0 (recibido: {value})"),
            });
        }
    }
    Ok(())
}
