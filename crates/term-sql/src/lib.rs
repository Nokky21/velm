pub mod classify;
pub mod errors;
pub mod keychain;
pub mod session;
pub mod ui;
pub mod write;

use std::io::Write;
use std::path::PathBuf;

use term_config::sql::SqlConfig;

use crate::keychain::{validate_postgres_url_prefix, KeychainStore, SecretUrl};
use crate::session::test_connection;

/// Exit codes matching CONTRATO §B.5 and §H.
pub const EXIT_OK: i32 = 0;
pub const EXIT_RUNTIME_ERR: i32 = 1;
pub const EXIT_CONFIG_OR_USAGE: i32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Tui,
    ConnAdd { name: String },
    ConnRm { name: String },
    ConnList,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedArgs {
    pub config_path: Option<PathBuf>,
    pub command: Command,
}

/// Parses CLI arguments according to CONTRATO §B and §B.5.
pub fn parse_args<I, S>(args: I) -> Result<ParsedArgs, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut iter = args.into_iter();
    let _bin = iter.next();

    let mut config_path: Option<PathBuf> = None;
    let mut positional: Vec<String> = Vec::new();

    while let Some(arg) = iter.next() {
        let s = arg.as_ref();
        if s == "--config" {
            let Some(path) = iter.next() else {
                return Err(String::from("falta la ruta después de --config"));
            };
            config_path = Some(PathBuf::from(path.as_ref()));
        } else if let Some(path) = s.strip_prefix("--config=") {
            if path.is_empty() {
                return Err(String::from("falta la ruta en --config="));
            }
            config_path = Some(PathBuf::from(path));
        } else if s.starts_with('-') {
            return Err(format!("opción desconocida: {s}"));
        } else {
            positional.push(s.to_owned());
        }
    }

    let command = match positional.as_slice() {
        [] => Command::Tui,
        [sub, op, name] if sub == "conn" && op == "add" => Command::ConnAdd { name: name.clone() },
        [sub, op, name] if sub == "conn" && op == "rm" => Command::ConnRm { name: name.clone() },
        [sub, op] if sub == "conn" && op == "list" => Command::ConnList,
        [sub, ..] if sub == "conn" => {
            return Err(String::from(
                "uso: term-sql conn <add <nombre> | rm <nombre> | list>",
            ));
        }
        _ => {
            return Err(String::from(
                "uso: term-sql [--config <ruta>] [conn <add <nombre> | rm <nombre> | list>]",
            ));
        }
    };

    Ok(ParsedArgs {
        config_path,
        command,
    })
}

/// Executes `conn list`: writes connection names and environments (NEVER URLs) to `out`.
pub fn run_conn_list<W: Write>(sql_cfg: &SqlConfig, out: &mut W) -> Result<(), String> {
    for conn in &sql_cfg.connections {
        writeln!(out, "{}\t{}\t{}", conn.name, conn.env, conn.color)
            .map_err(|e| format!("error escribiendo la salida: {e}"))?;
    }
    Ok(())
}

/// Executes `conn rm <nombre>`. Returns `(exit_code, optional_error_message)`.
pub fn run_conn_rm(
    sql_cfg: &SqlConfig,
    name: &str,
    keychain: &dyn KeychainStore,
) -> Result<(), (i32, String)> {
    if sql_cfg.connection(name).is_none() {
        return Err((
            EXIT_CONFIG_OR_USAGE,
            format!("la conexión '{name}' no existe en [[term.sql.connections]]"),
        ));
    }
    keychain
        .delete(name)
        .map_err(|err| (EXIT_RUNTIME_ERR, err.to_string()))
}

/// Executes `conn add <nombre>` given a secret reader callback (e.g. `rpassword` in CLI).
///
/// Validates configuration membership, checks `postgres://` / `postgresql://` prefix,
/// tests connectivity, and stores the URL in the keychain. Never echoes the URL.
pub fn run_conn_add<F>(
    sql_cfg: &SqlConfig,
    name: &str,
    keychain: &dyn KeychainStore,
    read_secret_url: F,
) -> Result<(), (i32, String)>
where
    F: FnOnce() -> Result<String, String>,
{
    if sql_cfg.connection(name).is_none() {
        return Err((
            EXIT_CONFIG_OR_USAGE,
            format!("la conexión '{name}' no existe en [[term.sql.connections]]"),
        ));
    }

    let raw_url = read_secret_url().map_err(|msg| (EXIT_RUNTIME_ERR, msg))?;
    let trimmed = raw_url.trim().to_owned();
    if let Err(msg) = validate_postgres_url_prefix(&trimmed) {
        return Err((EXIT_CONFIG_OR_USAGE, msg.to_owned()));
    }

    let secret = SecretUrl::new(trimmed);
    test_connection(&secret, sql_cfg.statement_timeout_s).map_err(|msg| (EXIT_RUNTIME_ERR, msg))?;

    keychain
        .set(name, secret.as_str())
        .map_err(|err| (EXIT_RUNTIME_ERR, err.to_string()))?;

    Ok(())
}
