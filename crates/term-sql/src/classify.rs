use sqlparser::ast::{
    Delete, Expr, FromTable, InTarget, Merge, ObjectType, Query, Select, SelectItem, SetExpr,
    Statement, TableFactor, TableWithJoins, Update, UtilityOption, Value, With,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer, Word};

/// Result of classifying a SQL string before executing it in write mode (CONTRATO §G.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteClassification {
    /// Safe to run inside the write transaction (`BEGIN ...`).
    Allow,
    /// Requires typing exact `SIN WHERE` before execution.
    ConfirmSinWhere { reason: String },
    /// Requires typing the connection name before execution (even in `local`).
    ConfirmConnectionName { reason: String },
    /// Must be rejected before sending to Postgres.
    Reject { message: String },
}

/// Result of inspecting a statement for read-only mode (CONTRATO §G.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOnlyCheck {
    Allow,
    RejectWriteExplain { message: String },
}

#[derive(Default)]
struct AstFindings {
    non_transactional: Option<&'static str>,
    destructive_ddl: Option<&'static str>,
    unfiltered_dml: Option<&'static str>,
    performs_write: bool,
}

impl AstFindings {
    fn record_non_transactional(&mut self, what: &'static str) {
        if self.non_transactional.is_none() {
            self.non_transactional = Some(what);
        }
        self.performs_write = true;
    }

    fn record_destructive_ddl(&mut self, what: &'static str) {
        if self.destructive_ddl.is_none() {
            self.destructive_ddl = Some(what);
        }
        self.performs_write = true;
    }

    fn record_unfiltered_dml(&mut self, what: &'static str) {
        if self.unfiltered_dml.is_none() {
            self.unfiltered_dml = Some(what);
        }
        self.performs_write = true;
    }
}

/// Classifies SQL for write mode according to CONTRATO §G.2.
pub fn classify_write_sql(sql: &str) -> WriteClassification {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return WriteClassification::Reject {
            message: String::from("la sentencia está vacía"),
        };
    }

    let dialect = PostgreSqlDialect {};
    let statements = match Parser::parse_sql(&dialect, trimmed) {
        Ok(stmts) => stmts,
        Err(_) => {
            if let Some(reason) = detect_non_transactional_from_tokens(trimmed) {
                return WriteClassification::Reject {
                    message: format!("{reason}: no puede ir en una transacción"),
                };
            }
            if count_token_statements(trimmed) > 1 {
                return WriteClassification::Reject {
                    message: String::from("solo se permite una sentencia por ejecución"),
                };
            }
            return WriteClassification::Reject {
                message: String::from("no se pudo analizar la sentencia; ejecútala en psql"),
            };
        }
    };

    if statements.is_empty() {
        return WriteClassification::Reject {
            message: String::from("la sentencia está vacía"),
        };
    }

    if statements.len() > 1 {
        return WriteClassification::Reject {
            message: String::from("solo se permite una sentencia por ejecución"),
        };
    }

    let mut findings = AstFindings::default();
    inspect_statement(&statements[0], &mut findings);

    if let Some(reason) = findings.non_transactional {
        return WriteClassification::Reject {
            message: format!("{reason}: no puede ir en una transacción"),
        };
    }

    if let Some(reason) = findings.destructive_ddl {
        return WriteClassification::ConfirmConnectionName {
            reason: format!("{reason}: requiere escribir el nombre de la conexión"),
        };
    }

    if let Some(reason) = findings.unfiltered_dml {
        return WriteClassification::ConfirmSinWhere {
            reason: format!("{reason}: requiere escribir SIN WHERE"),
        };
    }

    WriteClassification::Allow
}

/// Checks whether a statement in read-only mode attempts an `EXPLAIN ANALYZE`
/// over a write operation (CONTRATO §G.2).
///
/// Unparseable statements are allowed through in read-only mode so Postgres decides
/// (PLAN §7.4).
pub fn check_read_only_sql(sql: &str) -> ReadOnlyCheck {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return ReadOnlyCheck::Allow;
    }

    let dialect = PostgreSqlDialect {};
    let Ok(statements) = Parser::parse_sql(&dialect, trimmed) else {
        if looks_like_explain_analyze_write(trimmed) {
            return ReadOnlyCheck::RejectWriteExplain {
                message: String::from(
                    "estás en modo lectura: EXPLAIN ANALYZE ejecuta la sentencia de escritura",
                ),
            };
        }
        return ReadOnlyCheck::Allow;
    };

    for stmt in &statements {
        if explain_analyze_executes_write(stmt) {
            return ReadOnlyCheck::RejectWriteExplain {
                message: String::from(
                    "estás en modo lectura: EXPLAIN ANALYZE ejecuta la sentencia de escritura",
                ),
            };
        }
    }

    ReadOnlyCheck::Allow
}

fn is_explain_analyze_enabled(analyze: bool, options: Option<&Vec<UtilityOption>>) -> bool {
    if analyze {
        return true;
    }
    let Some(opts) = options else {
        return false;
    };
    opts.iter().any(|opt| {
        opt.name.value.eq_ignore_ascii_case("analyze")
            && !matches!(
                opt.arg.as_ref(),
                Some(Expr::Value(val)) if matches!(val.value, Value::Boolean(false))
            )
    })
}

fn explain_analyze_executes_write(stmt: &Statement) -> bool {
    match stmt {
        Statement::Explain {
            analyze,
            options,
            statement,
            ..
        } if is_explain_analyze_enabled(*analyze, options.as_ref()) => {
            let mut findings = AstFindings::default();
            inspect_statement(statement, &mut findings);
            findings.performs_write
        }
        _ => false,
    }
}

fn inspect_statement(stmt: &Statement, findings: &mut AstFindings) {
    match stmt {
        Statement::Explain {
            analyze,
            options,
            statement,
            ..
        } => {
            // Only EXPLAIN ANALYZE actually executes the inner statement; plain EXPLAIN
            // only computes the query plan without modifying data or running DDL.
            if is_explain_analyze_enabled(*analyze, options.as_ref()) {
                inspect_statement(statement, findings);
            }
        }
        Statement::CreateIndex(ci) => {
            findings.performs_write = true;
            if ci.concurrently {
                findings.record_non_transactional("CREATE INDEX CONCURRENTLY");
            }
        }
        Statement::Drop { object_type, .. } => {
            findings.performs_write = true;
            if matches!(object_type, ObjectType::Database) {
                findings.record_non_transactional("DROP DATABASE");
            } else {
                let repr = stmt.to_string().to_ascii_uppercase();
                if matches!(object_type, ObjectType::Index) && contains_word(&repr, "CONCURRENTLY")
                {
                    findings.record_non_transactional("DROP INDEX CONCURRENTLY");
                } else {
                    findings.record_destructive_ddl("DROP");
                }
            }
        }
        Statement::DropFunction(_)
        | Statement::DropTrigger { .. }
        | Statement::DropProcedure { .. }
        | Statement::DropDomain(_)
        | Statement::DropExtension(_)
        | Statement::DropPolicy(_)
        | Statement::DropOperator(_)
        | Statement::DropOperatorFamily(_)
        | Statement::DropOperatorClass(_) => {
            findings.record_destructive_ddl("DROP");
        }
        Statement::Truncate { .. } => {
            findings.record_destructive_ddl("TRUNCATE");
        }
        Statement::CreateDatabase { .. } => {
            findings.record_non_transactional("CREATE DATABASE");
        }
        Statement::Vacuum(_) => {
            findings.record_non_transactional("VACUUM");
        }
        Statement::Query(q) => {
            inspect_query(q, findings);
        }
        Statement::Insert(ins) => {
            findings.performs_write = true;
            if let Some(source) = &ins.source {
                inspect_query(source, findings);
            }
            if let Some(returning) = &ins.returning {
                for item in returning {
                    inspect_select_item(item, findings);
                }
            }
        }
        Statement::Update(upd) => {
            inspect_update(upd, findings);
        }
        Statement::Delete(del) => {
            inspect_delete(del, findings);
        }
        Statement::Merge(merge) => {
            inspect_merge(merge, findings);
        }
        Statement::CreateTable(ct) => {
            findings.performs_write = true;
            if let Some(q) = &ct.query {
                inspect_query(q, findings);
            }
        }
        Statement::CreateView(cv) => {
            findings.performs_write = true;
            inspect_query(&cv.query, findings);
        }
        Statement::AlterTable { .. }
        | Statement::AlterIndex { .. }
        | Statement::AlterView { .. }
        | Statement::AlterRole { .. }
        | Statement::AlterPolicy { .. }
        | Statement::CreateSchema { .. }
        | Statement::CreateFunction(_)
        | Statement::CreateTrigger(_)
        | Statement::CreateProcedure { .. }
        | Statement::CreateType { .. }
        | Statement::CreateDomain(_)
        | Statement::CreateExtension(_)
        | Statement::CreateSequence { .. }
        | Statement::CreatePolicy(_)
        | Statement::CreateRole { .. }
        | Statement::Copy { .. }
        | Statement::Grant { .. }
        | Statement::Revoke { .. }
        | Statement::Comment { .. }
        | Statement::Analyze(_) => {
            findings.performs_write = true;
        }
        other => {
            let repr = other.to_string().to_ascii_uppercase();
            if repr.starts_with("ALTER SYSTEM") {
                findings.record_non_transactional("ALTER SYSTEM");
            } else if repr.starts_with("REINDEX") && contains_word(&repr, "CONCURRENTLY") {
                findings.record_non_transactional("REINDEX CONCURRENTLY");
            }
        }
    }
}

fn inspect_query(query: &Query, findings: &mut AstFindings) {
    if let Some(with) = &query.with {
        inspect_with(with, findings);
    }
    inspect_set_expr(&query.body, findings);
    if let Some(limit_clause) = &query.limit_clause {
        match limit_clause {
            sqlparser::ast::LimitClause::LimitOffset { limit, offset, .. } => {
                if let Some(e) = limit {
                    inspect_expr(e, findings);
                }
                if let Some(o) = offset {
                    inspect_expr(&o.value, findings);
                }
            }
            sqlparser::ast::LimitClause::OffsetCommaLimit { offset, limit } => {
                inspect_expr(offset, findings);
                inspect_expr(limit, findings);
            }
        }
    }
}

fn inspect_with(with: &With, findings: &mut AstFindings) {
    for cte in &with.cte_tables {
        inspect_query(&cte.query, findings);
    }
}

fn inspect_set_expr(expr: &SetExpr, findings: &mut AstFindings) {
    match expr {
        SetExpr::Select(select) => inspect_select(select, findings),
        SetExpr::Query(q) => inspect_query(q, findings),
        SetExpr::SetOperation { left, right, .. } => {
            inspect_set_expr(left, findings);
            inspect_set_expr(right, findings);
        }
        SetExpr::Values(values) => {
            for row in &values.rows {
                for expr in row.iter() {
                    inspect_expr(expr, findings);
                }
            }
        }
        SetExpr::Insert(stmt)
        | SetExpr::Update(stmt)
        | SetExpr::Delete(stmt)
        | SetExpr::Merge(stmt) => {
            inspect_statement(stmt, findings);
        }
        SetExpr::Table(_) => {}
    }
}

fn inspect_select(select: &Select, findings: &mut AstFindings) {
    if select.into.is_some() {
        findings.performs_write = true;
    }
    for item in &select.projection {
        inspect_select_item(item, findings);
    }
    for table_with_joins in &select.from {
        inspect_table_with_joins(table_with_joins, findings);
    }
    if let Some(selection) = &select.selection {
        inspect_expr(selection, findings);
    }
    if let Some(having) = &select.having {
        inspect_expr(having, findings);
    }
}

fn inspect_select_item(item: &SelectItem, findings: &mut AstFindings) {
    match item {
        SelectItem::UnnamedExpr(e)
        | SelectItem::ExprWithAlias { expr: e, .. }
        | SelectItem::ExprWithAliases { expr: e, .. } => {
            inspect_expr(e, findings);
        }
        SelectItem::QualifiedWildcard(_, _) | SelectItem::Wildcard(_) => {}
    }
}

fn inspect_table_with_joins(twj: &TableWithJoins, findings: &mut AstFindings) {
    inspect_table_factor(&twj.relation, findings);
    for join in &twj.joins {
        inspect_table_factor(&join.relation, findings);
    }
}

fn inspect_table_factor(tf: &TableFactor, findings: &mut AstFindings) {
    match tf {
        TableFactor::Derived { subquery, .. } => inspect_query(subquery, findings),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => inspect_table_with_joins(table_with_joins, findings),
        TableFactor::Function { args, .. } => {
            for arg in args {
                if let sqlparser::ast::FunctionArg::Unnamed(
                    sqlparser::ast::FunctionArgExpr::Expr(e),
                )
                | sqlparser::ast::FunctionArg::Named {
                    arg: sqlparser::ast::FunctionArgExpr::Expr(e),
                    ..
                } = arg
                {
                    inspect_expr(e, findings);
                }
            }
        }
        _ => {}
    }
}

fn inspect_update(upd: &Update, findings: &mut AstFindings) {
    findings.performs_write = true;
    if upd.selection.is_none() {
        findings.record_unfiltered_dml("UPDATE sin WHERE");
    }
    inspect_table_with_joins(&upd.table, findings);
    if let Some(from) = &upd.from {
        match from {
            sqlparser::ast::UpdateTableFromKind::BeforeSet(tables)
            | sqlparser::ast::UpdateTableFromKind::AfterSet(tables) => {
                for twj in tables {
                    inspect_table_with_joins(twj, findings);
                }
            }
        }
    }
    for assign in &upd.assignments {
        inspect_expr(&assign.value, findings);
    }
    if let Some(selection) = &upd.selection {
        inspect_expr(selection, findings);
    }
    if let Some(returning) = &upd.returning {
        for item in returning {
            inspect_select_item(item, findings);
        }
    }
}

fn inspect_delete(del: &Delete, findings: &mut AstFindings) {
    findings.performs_write = true;
    if del.selection.is_none() {
        findings.record_unfiltered_dml("DELETE sin WHERE");
    }
    match &del.from {
        FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => {
            for twj in tables {
                inspect_table_with_joins(twj, findings);
            }
        }
    }
    if let Some(using) = &del.using {
        for twj in using {
            inspect_table_with_joins(twj, findings);
        }
    }
    if let Some(selection) = &del.selection {
        inspect_expr(selection, findings);
    }
    if let Some(returning) = &del.returning {
        for item in returning {
            inspect_select_item(item, findings);
        }
    }
}

fn inspect_merge(merge: &Merge, findings: &mut AstFindings) {
    findings.performs_write = true;
    inspect_table_factor(&merge.source, findings);
    inspect_expr(&merge.on, findings);
    for clause in &merge.clauses {
        if let Some(pred) = &clause.predicate {
            inspect_expr(pred, findings);
        }
    }
}

fn inspect_expr(expr: &Expr, findings: &mut AstFindings) {
    match expr {
        Expr::Subquery(q) | Expr::Exists { subquery: q, .. } => inspect_query(q, findings),
        Expr::In { expr, target, .. } => {
            inspect_expr(expr, findings);
            match target {
                InTarget::List(list) => {
                    for item in list {
                        inspect_expr(item, findings);
                    }
                }
                InTarget::Subquery(subquery) => inspect_query(subquery, findings),
                InTarget::Unnest(arr) => inspect_expr(arr, findings),
                InTarget::Table { .. } => {}
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            inspect_expr(left, findings);
            inspect_expr(right, findings);
        }
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) | Expr::Cast { expr, .. } => {
            inspect_expr(expr, findings);
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(op) = operand {
                inspect_expr(op, findings);
            }
            for branch in conditions {
                inspect_expr(&branch.condition, findings);
                inspect_expr(&branch.result, findings);
            }
            if let Some(el) = else_result {
                inspect_expr(el, findings);
            }
        }
        Expr::Between {
            expr, low, high, ..
        } => {
            inspect_expr(expr, findings);
            inspect_expr(low, findings);
            inspect_expr(high, findings);
        }
        _ => {}
    }
}

fn tokenize_non_whitespace(sql: &str) -> Vec<Token> {
    let dialect = PostgreSqlDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, sql).tokenize() else {
        return Vec::new();
    };
    tokens
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .collect()
}

fn count_token_statements(sql: &str) -> usize {
    let tokens = tokenize_non_whitespace(sql);
    let mut count = 0usize;
    let mut has_content = false;
    for tok in tokens {
        if matches!(tok, Token::SemiColon) {
            if has_content {
                count += 1;
                has_content = false;
            }
        } else {
            has_content = true;
        }
    }
    if has_content {
        count += 1;
    }
    count
}

fn is_word(tok: &Token, expected: &str) -> bool {
    matches!(
        tok,
        Token::Word(Word { value, quote_style: None, .. }) if value.eq_ignore_ascii_case(expected)
    )
}

fn unwrap_explain_analyze_prefix(tokens: &[Token]) -> &[Token] {
    if tokens.first().is_some_and(|t| is_word(t, "EXPLAIN")) {
        let mut idx = 1;
        let mut has_analyze = false;
        if tokens.get(idx).is_some_and(|t| matches!(t, Token::LParen)) {
            idx += 1;
            while idx < tokens.len() && !matches!(tokens[idx], Token::RParen) {
                if is_word(&tokens[idx], "ANALYZE") {
                    has_analyze = true;
                }
                idx += 1;
            }
            if idx < tokens.len() {
                idx += 1;
            }
        } else {
            while idx < tokens.len()
                && (is_word(&tokens[idx], "ANALYZE")
                    || is_word(&tokens[idx], "VERBOSE")
                    || is_word(&tokens[idx], "COSTS")
                    || is_word(&tokens[idx], "BUFFERS")
                    || is_word(&tokens[idx], "TIMING"))
            {
                if is_word(&tokens[idx], "ANALYZE") {
                    has_analyze = true;
                }
                idx += 1;
            }
        }
        if has_analyze {
            return &tokens[idx..];
        }
    }
    tokens
}

fn detect_non_transactional_from_tokens(sql: &str) -> Option<&'static str> {
    if count_token_statements(sql) != 1 {
        return None;
    }
    let all_tokens = tokenize_non_whitespace(sql);
    let tokens = unwrap_explain_analyze_prefix(&all_tokens);
    let w0 = tokens.first()?;
    let w1 = tokens.get(1);

    if is_word(w0, "VACUUM") {
        return Some("VACUUM");
    }
    if is_word(w0, "ALTER") && w1.is_some_and(|t| is_word(t, "SYSTEM")) {
        return Some("ALTER SYSTEM");
    }
    if is_word(w0, "CREATE") && w1.is_some_and(|t| is_word(t, "DATABASE")) {
        return Some("CREATE DATABASE");
    }
    if is_word(w0, "DROP") && w1.is_some_and(|t| is_word(t, "DATABASE")) {
        return Some("DROP DATABASE");
    }
    if is_word(w0, "REINDEX") && tokens.iter().any(|t| is_word(t, "CONCURRENTLY")) {
        return Some("REINDEX CONCURRENTLY");
    }
    if is_word(w0, "CREATE")
        && tokens.iter().any(|t| is_word(t, "INDEX"))
        && tokens.iter().any(|t| is_word(t, "CONCURRENTLY"))
    {
        return Some("CREATE INDEX CONCURRENTLY");
    }
    if is_word(w0, "DROP")
        && w1.is_some_and(|t| is_word(t, "INDEX"))
        && tokens.iter().any(|t| is_word(t, "CONCURRENTLY"))
    {
        return Some("DROP INDEX CONCURRENTLY");
    }
    None
}

fn looks_like_explain_analyze_write(sql: &str) -> bool {
    let all_tokens = tokenize_non_whitespace(sql);
    let inner = unwrap_explain_analyze_prefix(&all_tokens);
    if inner.len() == all_tokens.len() {
        return false;
    }
    inner.iter().any(|t| {
        is_word(t, "INSERT")
            || is_word(t, "UPDATE")
            || is_word(t, "DELETE")
            || is_word(t, "MERGE")
            || is_word(t, "DROP")
            || is_word(t, "TRUNCATE")
            || is_word(t, "CREATE")
            || is_word(t, "ALTER")
            || is_word(t, "VACUUM")
    })
}

fn contains_word(haystack_upper: &str, word: &str) -> bool {
    haystack_upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|part| part == word)
}
