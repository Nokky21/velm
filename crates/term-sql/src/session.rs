use std::error::Error;
use std::str::FromStr;
use std::time::Duration;

use postgres::types::{FromSql, Type};
use postgres::{Client, Config, NoTls, Row};

use crate::classify::{check_read_only_sql, ReadOnlyCheck};
use crate::errors::{format_postgres_error, redact_urls};
use crate::keychain::SecretUrl;

/// Tabular query output truncated at `row_limit`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub has_more: bool,
    pub rows_affected: Option<u64>,
}

/// Decodes any PostgreSQL wire value cleanly into a display string without failing on
/// types that `String::from_sql` does not accept (ints, bools, uuid, jsonb, numeric, etc.).
pub(crate) struct CellValue(pub String);

impl<'a> FromSql<'a> for CellValue {
    fn from_sql(ty: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        let formatted = match *ty {
            Type::BOOL => {
                let v = bool::from_sql(ty, raw)?;
                v.to_string()
            }
            Type::INT2 => {
                let v = i16::from_sql(ty, raw)?;
                v.to_string()
            }
            Type::INT4 => {
                let v = i32::from_sql(ty, raw)?;
                v.to_string()
            }
            Type::INT8 => {
                let v = i64::from_sql(ty, raw)?;
                v.to_string()
            }
            Type::OID => {
                let v = u32::from_sql(ty, raw)?;
                v.to_string()
            }
            Type::FLOAT4 => {
                let v = f32::from_sql(ty, raw)?;
                v.to_string()
            }
            Type::FLOAT8 => {
                let v = f64::from_sql(ty, raw)?;
                v.to_string()
            }
            Type::BYTEA => {
                let mut hex = String::with_capacity(2 + raw.len() * 2);
                hex.push_str("\\x");
                for b in raw {
                    hex.push_str(&format!("{b:02x}"));
                }
                hex
            }
            Type::JSONB => {
                // JSONB binary format starts with a 1-byte version prefix (0x01).
                let slice = raw.strip_prefix(&[1]).unwrap_or(raw);
                String::from_utf8_lossy(slice).into_owned()
            }
            Type::UUID if raw.len() == 16 => {
                format!(
                    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    raw[0], raw[1], raw[2], raw[3],
                    raw[4], raw[5],
                    raw[6], raw[7],
                    raw[8], raw[9],
                    raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
                )
            }
            _ => match std::str::from_utf8(raw) {
                Ok(s) => s.to_owned(),
                Err(_) => format!(
                    "0x{}",
                    raw.iter().map(|b| format!("{b:02x}")).collect::<String>()
                ),
            },
        };
        Ok(CellValue(formatted))
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }
}

pub(crate) fn decode_row(row: &Row) -> Vec<String> {
    (0..row.len())
        .map(|idx| match row.try_get::<_, Option<CellValue>>(idx) {
            Ok(Some(CellValue(val))) => val,
            Ok(None) => String::from("NULL"),
            Err(_) => String::from("<ilegible>"),
        })
        .collect()
}

pub(crate) fn build_pg_config(
    url: &SecretUrl,
    read_only: bool,
    statement_timeout_s: u64,
) -> Result<Config, String> {
    let mut cfg = Config::from_str(url.as_str())
        .map_err(|_| String::from("la cadena de conexión no tiene un formato PostgreSQL válido"))?;
    cfg.application_name("term-sql");
    cfg.connect_timeout(Duration::from_secs(10));
    let options = if read_only {
        format!("-c default_transaction_read_only=on -c statement_timeout={statement_timeout_s}s")
    } else {
        format!("-c statement_timeout={statement_timeout_s}s")
    };
    cfg.options(&options);
    Ok(cfg)
}

/// Tests connectivity to PostgreSQL in read-only mode (used by `term-sql conn add`).
pub fn test_connection(url: &SecretUrl, statement_timeout_s: u64) -> Result<(), String> {
    let mut session = ReadOnlySession::connect(url, statement_timeout_s, 1)?;
    session.execute("SELECT 1")?;
    Ok(())
}

/// Read-only PostgreSQL session configured with `default_transaction_read_only=on`,
/// `statement_timeout`, and `application_name=term-sql` (CONTRATO §G.1).
pub struct ReadOnlySession {
    client: Client,
    statement_timeout_s: u64,
    row_limit: usize,
}

impl ReadOnlySession {
    pub fn connect(
        url: &SecretUrl,
        statement_timeout_s: u64,
        row_limit: usize,
    ) -> Result<Self, String> {
        let cfg = build_pg_config(url, true, statement_timeout_s)?;
        let client = cfg
            .connect(NoTls)
            .map_err(|err| format_postgres_error(&err, statement_timeout_s))?;
        Ok(Self {
            client,
            statement_timeout_s,
            row_limit: row_limit.max(1),
        })
    }

    pub fn execute(&mut self, sql: &str) -> Result<QueryResult, String> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Err(String::from("la sentencia está vacía"));
        }

        if let ReadOnlyCheck::RejectWriteExplain { message } = check_read_only_sql(trimmed) {
            return Err(message);
        }

        let stmt = self
            .client
            .prepare(trimmed)
            .map_err(|err| format_postgres_error(&err, self.statement_timeout_s))?;
        let columns: Vec<String> = stmt.columns().iter().map(|c| c.name().to_owned()).collect();

        let empty_params: [&dyn postgres::types::ToSql; 0] = [];
        let mut iter = self
            .client
            .query_raw(&stmt, empty_params)
            .map_err(|err| format_postgres_error(&err, self.statement_timeout_s))?;

        collect_streaming_rows(&mut iter, columns, self.row_limit, self.statement_timeout_s)
    }

    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }
}

pub(crate) fn collect_streaming_rows(
    iter: &mut postgres::RowIter<'_>,
    columns: Vec<String>,
    row_limit: usize,
    statement_timeout_s: u64,
) -> Result<QueryResult, String> {
    use postgres::fallible_iterator::FallibleIterator;

    let mut rows = Vec::new();
    let mut has_more = false;

    while let Some(row) = iter
        .next()
        .map_err(|err| format_postgres_error(&err, statement_timeout_s))?
    {
        if rows.len() >= row_limit {
            has_more = true;
            break;
        }
        rows.push(decode_row(&row));
    }

    let rows_affected = if has_more { None } else { iter.rows_affected() };

    Ok(QueryResult {
        columns,
        rows,
        has_more,
        rows_affected,
    })
}

/// Helper to sanitize arbitrary error text before showing to the user.
pub fn sanitize_error(msg: &str) -> String {
    redact_urls(msg)
}
