use std::env;

use term_config::sql::{SqlColor, SqlConfig, SqlConnection, SqlEnv};
use term_sql::classify::{
    check_read_only_sql, classify_write_sql, ReadOnlyCheck, WriteClassification,
};
use term_sql::errors::{map_sqlstate, redact_urls};
use term_sql::keychain::{KeychainStore, MemoryKeychain, SecretUrl};
use term_sql::session::ReadOnlySession;
use term_sql::write::{CommitOutcome, WriteConfirmation, WriteExecuteOutcome, WriteSession};
use term_sql::{
    parse_args, run_conn_add, run_conn_list, run_conn_rm, Command, EXIT_CONFIG_OR_USAGE,
    EXIT_RUNTIME_ERR,
};

fn sample_sql_config() -> SqlConfig {
    SqlConfig {
        enabled: true,
        row_limit: 500,
        statement_timeout_s: 30,
        idle_tx_timeout_s: 60,
        connections: vec![
            SqlConnection {
                name: String::from("local-card"),
                env: SqlEnv::Local,
                color: SqlColor::Green,
            },
            SqlConnection {
                name: String::from("dev-card"),
                env: SqlEnv::Dev,
                color: SqlColor::Yellow,
            },
        ],
    }
}

#[test]
fn classify_every_contract_g2_case() {
    // 1. Unparseable -> reject with "ejecútala en psql"
    match classify_write_sql("SELECT FROM WHERE ???") {
        WriteClassification::Reject { message } => {
            assert!(
                message.contains("ejecútala en psql"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected Reject, got {other:?}"),
    }

    // 2. Empty / comment-only -> reject
    assert!(matches!(
        classify_write_sql("   -- solo comentario\n"),
        WriteClassification::Reject { .. }
    ));

    // 3. Multiple statements -> reject
    assert!(matches!(
        classify_write_sql("SELECT 1; SELECT 2;"),
        WriteClassification::Reject { .. }
    ));
    assert!(matches!(
        classify_write_sql("UPDATE t SET a = 1 WHERE id = 1; DELETE FROM t WHERE id = 2"),
        WriteClassification::Reject { .. }
    ));

    // 4. Semicolons and keywords inside strings and comments are NOT treated as separators or DML
    assert_eq!(
        classify_write_sql(
            "/* DROP TABLE fake; */ UPDATE items SET note = 'a; DELETE FROM items;' WHERE id = 1 -- TRUNCATE items;"
        ),
        WriteClassification::Allow
    );

    // 5. Non-transactional statements -> reject ("no puede ir en una transacción")
    let non_tx = [
        "CREATE INDEX CONCURRENTLY idx_items_name ON items (name)",
        "DROP INDEX CONCURRENTLY idx_items_name",
        "REINDEX TABLE CONCURRENTLY items",
        "REINDEX INDEX CONCURRENTLY idx_items_name",
        "VACUUM items",
        "VACUUM FULL",
        "CREATE DATABASE scratch",
        "DROP DATABASE scratch",
        "ALTER SYSTEM SET work_mem = '64MB'",
        "EXPLAIN ANALYZE VACUUM items",
        "EXPLAIN ANALYZE CREATE INDEX CONCURRENTLY idx_a ON items (a)",
    ];
    for sql in non_tx {
        match classify_write_sql(sql) {
            WriteClassification::Reject { message } => {
                assert!(
                    message.contains("no puede ir en una transacción"),
                    "for `{sql}`, got: {message}"
                );
            }
            other => panic!("expected Reject for `{sql}`, got {other:?}"),
        }
    }

    // 6. UPDATE / DELETE without WHERE (direct, CTEs, EXPLAIN ANALYZE) -> ConfirmSinWhere
    let sin_where = [
        "UPDATE items SET active = false",
        "DELETE FROM items",
        "WITH target AS (UPDATE items SET active = false RETURNING id) SELECT * FROM target",
        "WITH gone AS (DELETE FROM items RETURNING id) SELECT * FROM gone",
        "WITH src AS (SELECT id FROM other) DELETE FROM items",
        "EXPLAIN ANALYZE UPDATE items SET active = false",
        "EXPLAIN (ANALYZE, BUFFERS) DELETE FROM items",
    ];
    for sql in sin_where {
        match classify_write_sql(sql) {
            WriteClassification::ConfirmSinWhere { .. } => {}
            other => panic!("expected ConfirmSinWhere for `{sql}`, got {other:?}"),
        }
    }

    // 7. UPDATE / DELETE with WHERE (direct, CTEs, EXPLAIN ANALYZE) -> Allow
    let with_where = [
        "UPDATE items SET active = false WHERE id = 42",
        "DELETE FROM items WHERE id = 42",
        "WITH upd AS (UPDATE items SET active = false WHERE id = 1 RETURNING id) INSERT INTO audit (id) SELECT id FROM upd",
        "WITH del AS (DELETE FROM items WHERE id = 1 RETURNING id) SELECT * FROM del",
        "EXPLAIN ANALYZE UPDATE items SET active = false WHERE id = 1",
        "EXPLAIN DELETE FROM items",
    ];
    for sql in with_where {
        assert_eq!(
            classify_write_sql(sql),
            WriteClassification::Allow,
            "expected Allow for `{sql}`"
        );
    }

    // 8. DROP / TRUNCATE (direct and EXPLAIN ANALYZE) -> ConfirmConnectionName
    let destructive = [
        "DROP TABLE items",
        "DROP VIEW v_items",
        "DROP FUNCTION fn_items()",
        "TRUNCATE items",
        "TRUNCATE TABLE items RESTART IDENTITY",
        "EXPLAIN ANALYZE TRUNCATE items",
        "EXPLAIN ANALYZE DROP TABLE items",
    ];
    for sql in destructive {
        match classify_write_sql(sql) {
            WriteClassification::ConfirmConnectionName { .. } => {}
            other => panic!("expected ConfirmConnectionName for `{sql}`, got {other:?}"),
        }
    }
}

#[test]
fn read_only_check_detects_explain_analyze_writes() {
    assert_eq!(
        check_read_only_sql("SELECT * FROM items"),
        ReadOnlyCheck::Allow
    );
    assert_eq!(
        check_read_only_sql("EXPLAIN ANALYZE SELECT * FROM items"),
        ReadOnlyCheck::Allow
    );
    assert_eq!(
        check_read_only_sql("EXPLAIN DELETE FROM items WHERE id = 1"),
        ReadOnlyCheck::Allow
    );
    assert_eq!(
        check_read_only_sql("EXPLAIN (ANALYZE false) DELETE FROM items WHERE id = 1"),
        ReadOnlyCheck::Allow
    );
    assert!(matches!(
        check_read_only_sql("EXPLAIN ANALYZE DELETE FROM items WHERE id = 1"),
        ReadOnlyCheck::RejectWriteExplain { .. }
    ));
    assert!(matches!(
        check_read_only_sql("EXPLAIN (ANALYZE, BUFFERS) UPDATE items SET x = 1 WHERE id = 1"),
        ReadOnlyCheck::RejectWriteExplain { .. }
    ));
    assert!(matches!(
        check_read_only_sql(
            "EXPLAIN ANALYZE WITH d AS (DELETE FROM items WHERE id = 1 RETURNING *) SELECT * FROM d"
        ),
        ReadOnlyCheck::RejectWriteExplain { .. }
    ));
}

#[test]
fn errors_fixture_all_sqlstates_mapped() {
    #[derive(serde::Deserialize)]
    struct Row {
        sqlstate: String,
        message: String,
    }
    let raw = include_str!("fixtures/pg.error.sqlstates.sintetico.json");
    let rows: Vec<Row> = serde_json::from_str(raw).unwrap();
    assert_eq!(rows.len(), 10);

    for r in rows {
        let mapped = map_sqlstate(&r.sqlstate, &r.message, 45);
        assert!(!mapped.is_empty());
        if r.sqlstate == "57014" {
            assert_eq!(mapped, "la sentencia tardó más de 45 s");
        }
    }

    let redacted = redact_urls("connect failed: postgresql://edgar:topsecret@127.0.0.1:5432/app");
    assert!(!redacted.contains("topsecret"));
    assert!(!redacted.contains("edgar"));
}

#[test]
fn conn_cli_parsing_and_validation() {
    let parsed = parse_args([
        "term-sql",
        "--config",
        "/tmp/c.toml",
        "conn",
        "add",
        "local-card",
    ])
    .unwrap();
    assert_eq!(
        parsed.command,
        Command::ConnAdd {
            name: String::from("local-card")
        }
    );
    assert_eq!(
        parsed.config_path.as_deref(),
        Some(std::path::Path::new("/tmp/c.toml"))
    );

    assert!(parse_args(["term-sql", "conn", "bogus"]).is_err());
    assert!(parse_args(["term-sql", "--unknown"]).is_err());

    let cfg = sample_sql_config();
    let kc = MemoryKeychain::new();
    let secret = "postgres://postgres:supersecret@127.0.0.1:5432/card";
    kc.set("local-card", secret).unwrap();

    // `conn list` never prints the secret URL
    let mut out = Vec::new();
    run_conn_list(&cfg, &mut out).unwrap();
    let list_output = String::from_utf8(out).unwrap();
    assert!(list_output.contains("local-card\tlocal\tgreen"));
    assert!(list_output.contains("dev-card\tdev\tyellow"));
    assert!(!list_output.contains("supersecret"));
    assert!(!list_output.contains("postgres://"));

    // `conn add` unknown connection -> exit code 2
    let err = run_conn_add(&cfg, "unknown", &kc, || Ok(secret.to_owned())).unwrap_err();
    assert_eq!(err.0, EXIT_CONFIG_OR_USAGE);

    // `conn add` invalid scheme -> exit code 2, never echoes input
    let bad_url = "mysql://user:pw@localhost/db";
    let err = run_conn_add(&cfg, "local-card", &kc, || Ok(bad_url.to_owned())).unwrap_err();
    assert_eq!(err.0, EXIT_CONFIG_OR_USAGE);
    assert!(!err.1.contains("pw"));

    // `conn add` unreachable DB -> exit code 1, never echoes credentials
    let closed_port_url = "postgres://edgar:hiddenpw@127.0.0.1:1/card";
    let err = run_conn_add(&cfg, "local-card", &kc, || Ok(closed_port_url.to_owned())).unwrap_err();
    assert_eq!(err.0, EXIT_RUNTIME_ERR);
    assert!(!err.1.contains("hiddenpw"));

    // `conn rm` unknown -> 2; existing -> Ok; already deleted -> 1
    let err = run_conn_rm(&cfg, "unknown", &kc).unwrap_err();
    assert_eq!(err.0, EXIT_CONFIG_OR_USAGE);
    run_conn_rm(&cfg, "local-card", &kc).unwrap();
    let err = run_conn_rm(&cfg, "local-card", &kc).unwrap_err();
    assert_eq!(err.0, EXIT_RUNTIME_ERR);
}

/// Validates that `TERM_SQL_TEST_URL` strictly targets localhost or 127.0.0.1 (Rule 8).
fn load_safe_local_test_url() -> Option<SecretUrl> {
    let raw = env::var("TERM_SQL_TEST_URL").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let after_scheme = trimmed
        .strip_prefix("postgres://")
        .or_else(|| trimmed.strip_prefix("postgresql://"))
        .expect("TERM_SQL_TEST_URL must start with postgres:// or postgresql://");

    let authority = after_scheme.split(['/', '?']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or("")
    } else {
        host_port.split(':').next().unwrap_or("")
    };

    assert!(
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1",
        "REFUSING integration test: TERM_SQL_TEST_URL host '{host}' is not localhost/127.0.0.1"
    );

    Some(SecretUrl::new(trimmed.to_owned()))
}

#[test]
fn integration_postgres_read_and_write_flows() {
    let Some(url) = load_safe_local_test_url() else {
        eprintln!("skipping TERM_SQL_TEST_URL integration test (env var not set)");
        return;
    };

    let cfg = sample_sql_config();
    let local_conn = cfg.connection("local-card").unwrap();

    // Read-only session checks: row_limit + "estás en modo lectura" (25006)
    let mut ro = ReadOnlySession::connect(&url, 30, 3).expect("read-only connect");
    let series = ro
        .execute("SELECT g FROM generate_series(1, 5) AS g")
        .expect("read-only select");
    assert_eq!(series.rows.len(), 3);
    assert!(series.has_more);

    let ro_err = ro
        .execute("CREATE TEMP TABLE should_fail (id int)")
        .unwrap_err();
    assert_eq!(ro_err, "estás en modo lectura");

    // Write session checks: setup temp table, rollback vs commit, SIN WHERE, DROP confirmation, dev commit
    let mut ws = WriteSession::connect(local_conn, &url, &cfg).expect("write connect");
    assert!(matches!(
        ws.execute(
            "CREATE TEMP TABLE term_sql_test (id int PRIMARY KEY, val text)",
            None
        )
        .unwrap(),
        WriteExecuteOutcome::PendingTx { .. }
    ));
    assert_eq!(ws.commit(None).unwrap(), CommitOutcome::Committed);

    // Insert + commit
    ws.execute(
        "INSERT INTO term_sql_test (id, val) VALUES (1, 'uno')",
        None,
    )
    .unwrap();
    ws.commit(None).unwrap();

    // Update + rollback leaves table unchanged
    ws.execute(
        "UPDATE term_sql_test SET val = 'cambiado' WHERE id = 1",
        None,
    )
    .unwrap();
    ws.rollback().unwrap();

    // Update without WHERE requires SIN WHERE
    let needs = ws
        .execute("UPDATE term_sql_test SET val = 'dos'", None)
        .unwrap();
    assert!(matches!(
        needs,
        WriteExecuteOutcome::NeedsConfirmation(WriteConfirmation::SinWhere { .. })
    ));
    ws.execute("UPDATE term_sql_test SET val = 'dos'", Some("SIN WHERE"))
        .unwrap();
    ws.commit(None).unwrap();

    // DROP requires connection name even in local
    let needs_drop = ws.execute("DROP TABLE term_sql_test", None).unwrap();
    assert!(matches!(
        needs_drop,
        WriteExecuteOutcome::NeedsConfirmation(WriteConfirmation::ConnectionName { .. })
    ));
    ws.execute("DROP TABLE term_sql_test", Some("local-card"))
        .unwrap();
    ws.commit(None).unwrap();

    // Reinforced confirmation in dev
    let dev_conn = cfg.connection("dev-card").unwrap();
    let mut dev_ws = WriteSession::connect(dev_conn, &url, &cfg).expect("dev write connect");
    dev_ws.execute("SELECT 1", None).unwrap();
    assert_eq!(
        dev_ws.commit(None).unwrap(),
        CommitOutcome::NeedsDevConfirmation
    );
    assert_eq!(
        dev_ws.commit(Some("dev-card")).unwrap(),
        CommitOutcome::Committed
    );
}
