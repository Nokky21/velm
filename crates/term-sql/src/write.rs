use std::time::{Duration, Instant};

use postgres::{Client, NoTls};
use term_config::sql::{SqlConfig, SqlConnection, SqlEnv};

use crate::classify::{classify_write_sql, WriteClassification};
use crate::errors::format_postgres_error;
use crate::keychain::SecretUrl;
use crate::session::{build_pg_config, collect_streaming_rows, QueryResult};

/// Confirmation required before executing or committing a risky write operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteConfirmation {
    /// `UPDATE` or `DELETE` without `WHERE`: user must type `SIN WHERE`.
    SinWhere { reason: String },
    /// `DROP` / `TRUNCATE` (even in `local`) or `COMMIT` in `dev`: user must type connection name.
    ConnectionName { reason: String },
}

/// Outcome of attempting to execute a statement in write mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteExecuteOutcome {
    /// Statement executed inside an open `BEGIN` transaction awaiting `c`/`r`.
    PendingTx {
        result: QueryResult,
        idle_timeout_s: u64,
    },
    /// User confirmation is required before sending the statement to Postgres.
    NeedsConfirmation(WriteConfirmation),
    /// Statement was rejected by the pre-execution classifier.
    Rejected(String),
}

/// Outcome of attempting to commit a pending write transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    Committed,
    NeedsDevConfirmation,
}

#[derive(Debug, Clone)]
pub struct PendingTx {
    pub started_at: Instant,
    pub idle_timeout: Duration,
    pub result: QueryResult,
}

impl PendingTx {
    /// Returns remaining seconds (rounded up) before the idle timeout expires.
    pub fn remaining_seconds(&self) -> u64 {
        let elapsed = self.started_at.elapsed();
        if elapsed >= self.idle_timeout {
            0
        } else {
            let rem = self.idle_timeout - elapsed;
            rem.as_secs() + u64::from(rem.subsec_nanos() > 0)
        }
    }

    pub fn is_expired(&self) -> bool {
        self.started_at.elapsed() >= self.idle_timeout
    }
}

/// Dedicated PostgreSQL write session managing explicit `BEGIN` / `COMMIT` / `ROLLBACK`
/// transactions with statement & idle-in-transaction timeouts (CONTRATO §G.1 & §G.2).
pub struct WriteSession {
    client: Client,
    connection_name: String,
    env: SqlEnv,
    row_limit: usize,
    statement_timeout_s: u64,
    idle_tx_timeout_s: u64,
    pending: Option<PendingTx>,
}

impl WriteSession {
    pub fn connect(
        conn: &SqlConnection,
        url: &SecretUrl,
        sql_cfg: &SqlConfig,
    ) -> Result<Self, String> {
        let cfg = build_pg_config(url, false, sql_cfg.statement_timeout_s)?;
        let client = cfg
            .connect(NoTls)
            .map_err(|err| format_postgres_error(&err, sql_cfg.statement_timeout_s))?;
        Ok(Self {
            client,
            connection_name: conn.name.clone(),
            env: conn.env,
            row_limit: sql_cfg.row_limit.max(1),
            statement_timeout_s: sql_cfg.statement_timeout_s,
            idle_tx_timeout_s: sql_cfg.idle_tx_timeout_s,
            pending: None,
        })
    }

    pub fn connection_name(&self) -> &str {
        &self.connection_name
    }

    pub fn env(&self) -> SqlEnv {
        self.env
    }

    pub fn pending(&self) -> Option<&PendingTx> {
        self.pending.as_ref()
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Executes a single SQL statement inside a write transaction.
    ///
    /// When the classifier requires confirmation, `confirmation` must match `"SIN WHERE"`
    /// or the connection name respectively; otherwise `NeedsConfirmation` is returned
    /// without opening a transaction.
    pub fn execute(
        &mut self,
        sql: &str,
        confirmation: Option<&str>,
    ) -> Result<WriteExecuteOutcome, String> {
        if self.pending.is_some() {
            return Err(String::from(
                "ya hay una transacción pendiente; confirma (c) o deshaz (r) primero",
            ));
        }

        match classify_write_sql(sql) {
            WriteClassification::Reject { message } => {
                return Ok(WriteExecuteOutcome::Rejected(message));
            }
            WriteClassification::ConfirmSinWhere { reason } => {
                if confirmation != Some("SIN WHERE") {
                    return Ok(WriteExecuteOutcome::NeedsConfirmation(
                        WriteConfirmation::SinWhere { reason },
                    ));
                }
            }
            WriteClassification::ConfirmConnectionName { reason } => {
                if confirmation != Some(self.connection_name.as_str()) {
                    return Ok(WriteExecuteOutcome::NeedsConfirmation(
                        WriteConfirmation::ConnectionName { reason },
                    ));
                }
            }
            WriteClassification::Allow => {}
        }

        let begin_sql = format!(
            "BEGIN; SET LOCAL statement_timeout = '{}s'; SET LOCAL idle_in_transaction_session_timeout = '{}s';",
            self.statement_timeout_s, self.idle_tx_timeout_s
        );
        self.client
            .batch_execute(&begin_sql)
            .map_err(|err| format_postgres_error(&err, self.statement_timeout_s))?;

        let trimmed = sql.trim();
        let exec_result = self.run_single_in_tx(trimmed);
        match exec_result {
            Ok(result) => {
                self.pending = Some(PendingTx {
                    started_at: Instant::now(),
                    idle_timeout: Duration::from_secs(self.idle_tx_timeout_s),
                    result: result.clone(),
                });
                Ok(WriteExecuteOutcome::PendingTx {
                    result,
                    idle_timeout_s: self.idle_tx_timeout_s,
                })
            }
            Err(err_msg) => {
                let _ = self.client.batch_execute("ROLLBACK;");
                Err(err_msg)
            }
        }
    }

    fn run_single_in_tx(&mut self, sql: &str) -> Result<QueryResult, String> {
        let stmt = self
            .client
            .prepare(sql)
            .map_err(|err| format_postgres_error(&err, self.statement_timeout_s))?;
        let columns: Vec<String> = stmt.columns().iter().map(|c| c.name().to_owned()).collect();

        let empty_params: [&dyn postgres::types::ToSql; 0] = [];
        let mut iter = self
            .client
            .query_raw(&stmt, empty_params)
            .map_err(|err| format_postgres_error(&err, self.statement_timeout_s))?;

        collect_streaming_rows(&mut iter, columns, self.row_limit, self.statement_timeout_s)
    }

    /// Commits the pending transaction.
    ///
    /// On `dev` connections, `dev_confirmation` must equal `self.connection_name`
    /// (reinforced confirmation per PLAN §7.3).
    pub fn commit(&mut self, dev_confirmation: Option<&str>) -> Result<CommitOutcome, String> {
        let Some(pending) = &self.pending else {
            return Err(String::from("no hay ninguna transacción pendiente"));
        };

        if pending.is_expired() {
            let _ = self.rollback();
            return Err(String::from("la transacción se deshizo por inactividad"));
        }

        if self.env == SqlEnv::Dev && dev_confirmation != Some(self.connection_name.as_str()) {
            return Ok(CommitOutcome::NeedsDevConfirmation);
        }

        let res = self.client.batch_execute("COMMIT;");
        self.pending = None;
        res.map_err(|err| format_postgres_error(&err, self.statement_timeout_s))?;
        Ok(CommitOutcome::Committed)
    }

    /// Rolls back the pending transaction.
    pub fn rollback(&mut self) -> Result<(), String> {
        if self.pending.take().is_none() {
            return Ok(());
        }
        if self.client.is_closed() {
            return Ok(());
        }
        self.client
            .batch_execute("ROLLBACK;")
            .map_err(|err| format_postgres_error(&err, self.statement_timeout_s))
    }

    /// Checks whether the client-side countdown expired; if so, rolls back and clears pending state.
    pub fn expire_if_elapsed(&mut self) -> bool {
        let expired = self.pending.as_ref().is_some_and(PendingTx::is_expired);
        if expired {
            let _ = self.rollback();
            true
        } else {
            false
        }
    }
}

impl Drop for WriteSession {
    fn drop(&mut self) {
        if self.pending.is_some() && !self.client.is_closed() {
            let _ = self.client.batch_execute("ROLLBACK;");
        }
    }
}
