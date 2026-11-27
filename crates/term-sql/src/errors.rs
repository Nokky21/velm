/// Maps a PostgreSQL SQLSTATE code and message to a Spanish user-facing error
/// according to CONTRATO §G.3.
pub fn map_sqlstate(sqlstate: &str, pg_message: &str, statement_timeout_s: u64) -> String {
    match sqlstate {
        "25006" => String::from("estás en modo lectura"),
        "25001" => {
            String::from("no puede ir en una transacción (no debería llegar: lo filtra §G.2)")
        }
        "25P03" => String::from("la transacción se deshizo por inactividad"),
        "57014" => format!("la sentencia tardó más de {statement_timeout_s} s"),
        "40P01" => String::from("bloqueo mutuo con otra sesión; se deshizo"),
        "55P03" => String::from("la fila está bloqueada por otra sesión"),
        "42601" => pg_message.to_owned(),
        "28P01" => String::from("usuario o contraseña incorrectos"),
        "3D000" => String::from("la base de datos no existe"),
        code if code.starts_with("08") => String::from("conexión perdida"),
        other => {
            if pg_message.is_empty() {
                other.to_owned()
            } else {
                format!("{other}: {pg_message}")
            }
        }
    }
}

/// Formats a `postgres::Error` safely without leaking any connection string or credentials.
pub fn format_postgres_error(err: &postgres::Error, statement_timeout_s: u64) -> String {
    if let Some(db) = err.as_db_error() {
        return map_sqlstate(db.code().code(), db.message(), statement_timeout_s);
    }
    if err.is_closed() {
        return String::from("conexión perdida");
    }
    redact_urls(&err.to_string())
}

/// Removes any `postgres://` or `postgresql://` URI from an error string so credentials
/// can never appear in logs or UI messages.
pub fn redact_urls(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    loop {
        let next = [rest.find("postgres://"), rest.find("postgresql://")]
            .into_iter()
            .flatten()
            .min();
        match next {
            Some(idx) => {
                out.push_str(&rest[..idx]);
                out.push_str("[URL OCULTA]");
                let after_scheme = &rest[idx..];
                let end = after_scheme
                    .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ')' | ']'))
                    .unwrap_or(after_scheme.len());
                rest = &after_scheme[end..];
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct FixtureError {
        sqlstate: String,
        #[allow(dead_code)]
        severity: String,
        message: String,
    }

    #[test]
    fn maps_every_fixture_sqlstate_per_contract() {
        let raw = include_str!("../tests/fixtures/pg.error.sqlstates.sintetico.json");
        let items: Vec<FixtureError> = serde_json::from_str(raw).unwrap();
        assert_eq!(items.len(), 10);

        for item in &items {
            let mapped = map_sqlstate(&item.sqlstate, &item.message, 30);
            match item.sqlstate.as_str() {
                "25006" => assert_eq!(mapped, "estás en modo lectura"),
                "25001" => assert!(mapped.contains("no puede ir en una transacción")),
                "25P03" => assert_eq!(mapped, "la transacción se deshizo por inactividad"),
                "57014" => assert_eq!(mapped, "la sentencia tardó más de 30 s"),
                "40P01" => assert_eq!(mapped, "bloqueo mutuo con otra sesión; se deshizo"),
                "55P03" => assert_eq!(mapped, "la fila está bloqueada por otra sesión"),
                "42601" => assert_eq!(mapped, item.message),
                "28P01" => assert_eq!(mapped, "usuario o contraseña incorrectos"),
                "3D000" => assert_eq!(mapped, "la base de datos no existe"),
                "08006" => assert_eq!(mapped, "conexión perdida"),
                other => panic!("unexpected sqlstate in fixture: {other}"),
            }
        }
    }

    #[test]
    fn other_sqlstate_preserves_code_and_message() {
        assert_eq!(
            map_sqlstate("23505", "duplicate key value", 30),
            "23505: duplicate key value"
        );
    }

    #[test]
    fn redact_urls_strips_credentials() {
        let msg = "failed to connect to postgres://admin:secret@127.0.0.1:5432/db: refused";
        let redacted = redact_urls(msg);
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("admin"));
        assert!(redacted.contains("[URL OCULTA]"));
    }
}
