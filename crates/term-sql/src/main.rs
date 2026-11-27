use std::io;
use std::process;

use term_config::TermConfig;
use term_sql::keychain::SystemKeychain;
use term_sql::ui::run_tui;
use term_sql::{
    parse_args, run_conn_add, run_conn_list, run_conn_rm, Command, EXIT_CONFIG_OR_USAGE, EXIT_OK,
    EXIT_RUNTIME_ERR,
};

fn main() {
    let code = run();
    process::exit(code);
}

fn run() -> i32 {
    let parsed = match parse_args(std::env::args()) {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("term-sql: {msg}");
            return EXIT_CONFIG_OR_USAGE;
        }
    };

    let loaded = match TermConfig::load(parsed.config_path.as_deref()) {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("term-sql: {err}");
            return EXIT_CONFIG_OR_USAGE;
        }
    };

    for warn in &loaded.warnings {
        eprintln!("aviso: {warn}");
    }

    let sql_cfg = &loaded.config.sql;
    let keychain = SystemKeychain;

    match parsed.command {
        Command::ConnList => {
            let mut stdout = io::stdout().lock();
            if let Err(err) = run_conn_list(sql_cfg, &mut stdout) {
                eprintln!("term-sql: {err}");
                return EXIT_RUNTIME_ERR;
            }
            EXIT_OK
        }
        Command::ConnRm { ref name } => match run_conn_rm(sql_cfg, name, &keychain) {
            Ok(()) => {
                eprintln!("Conexión '{name}' eliminada del Llavero");
                EXIT_OK
            }
            Err((code, msg)) => {
                eprintln!("term-sql: {msg}");
                code
            }
        },
        Command::ConnAdd { ref name } => {
            let res = run_conn_add(sql_cfg, name, &keychain, || {
                rpassword::prompt_password("Cadena de conexión (oculta): ")
                    .map_err(|e| format!("no se pudo leer la cadena de conexión: {e}"))
            });
            match res {
                Ok(()) => {
                    eprintln!("Conexión '{name}' guardada en el Llavero");
                    EXIT_OK
                }
                Err((code, msg)) => {
                    eprintln!("term-sql: {msg}");
                    code
                }
            }
        }
        Command::Tui => {
            if let Err(err) = run_tui(sql_cfg, &keychain) {
                eprintln!("term-sql: {err}");
                return EXIT_RUNTIME_ERR;
            }
            EXIT_OK
        }
    }
}
