use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

use term_claude::model::load_sessions;
use term_claude::ui;

const EXIT_OK: u8 = 0;
const EXIT_RUNTIME_ERR: u8 = 1;
const EXIT_CONFIG_OR_USAGE: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliOptions {
    config: Option<PathBuf>,
    json: bool,
    days: Option<u32>,
    help: bool,
}

fn parse_args<I, S>(args: I) -> Result<CliOptions, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut opts = CliOptions {
        config: None,
        json: false,
        days: None,
        help: false,
    };

    let mut iter = args.into_iter().map(Into::into);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => opts.help = true,
            "--json" => opts.json = true,
            "--config" => {
                let val = iter
                    .next()
                    .ok_or_else(|| String::from("falta la ruta tras --config"))?;
                opts.config = Some(PathBuf::from(val));
            }
            "--days" => {
                let val = iter
                    .next()
                    .ok_or_else(|| String::from("falta el número de días tras --days"))?;
                opts.days = Some(parse_days(&val)?);
            }
            other if other.starts_with("--config=") => {
                let val = &other["--config=".len()..];
                if val.is_empty() {
                    return Err(String::from("falta la ruta en --config="));
                }
                opts.config = Some(PathBuf::from(val));
            }
            other if other.starts_with("--days=") => {
                let val = &other["--days=".len()..];
                opts.days = Some(parse_days(val)?);
            }
            other => {
                return Err(format!("argumento desconocido: {other}"));
            }
        }
    }

    Ok(opts)
}

fn parse_days(raw: &str) -> Result<u32, String> {
    let days = raw
        .parse::<u32>()
        .map_err(|_| format!("valor inválido para --days: '{raw}' (debe ser entre 1 y 365)"))?;
    if !(1..=365).contains(&days) {
        return Err(format!(
            "valor fuera de rango para --days: {days} (debe estar entre 1 y 365)"
        ));
    }
    Ok(days)
}

fn print_help<W: Write>(out: &mut W) -> io::Result<()> {
    writeln!(
        out,
        "Uso: term-claude [--config <ruta>] [--json] [--days <n>]"
    )?;
    writeln!(
        out,
        "  --config <ruta>  Fichero de configuración TOML alternativo"
    )?;
    writeln!(
        out,
        "  --json           Emite JSON por stdout (ordenado por lastActivityAt desc)"
    )?;
    writeln!(
        out,
        "  --days <n>       Días hacia atrás a inspeccionar (1..=365)"
    )
}

fn run<I, S, WOut, WErr>(args: I, out: &mut WOut, err: &mut WErr) -> u8
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
    WOut: Write,
    WErr: Write,
{
    let opts = match parse_args(args) {
        Ok(opts) => opts,
        Err(msg) => {
            let _ = writeln!(err, "term-claude: {msg}");
            return EXIT_CONFIG_OR_USAGE;
        }
    };

    if opts.help {
        let _ = print_help(out);
        return EXIT_OK;
    }

    let loaded = match term_config::load(opts.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(cfg_err) => {
            let _ = writeln!(err, "term-claude: {cfg_err}");
            return EXIT_CONFIG_OR_USAGE;
        }
    };

    for warning in &loaded.warnings {
        let _ = writeln!(err, "aviso: {warning}");
    }

    let days = opts.days.unwrap_or(loaded.config.claude.days);

    if opts.json {
        let sessions = match load_sessions(&loaded.config.claude, days, SystemTime::now()) {
            Ok(sessions) => sessions,
            Err(io_err) => {
                let _ = writeln!(err, "term-claude: error al leer transcripciones: {io_err}");
                return EXIT_RUNTIME_ERR;
            }
        };
        match serde_json::to_string_pretty(&sessions) {
            Ok(json) => {
                if writeln!(out, "{json}").is_err() {
                    return EXIT_RUNTIME_ERR;
                }
                EXIT_OK
            }
            Err(ser_err) => {
                let _ = writeln!(err, "term-claude: error al serializar JSON: {ser_err}");
                EXIT_RUNTIME_ERR
            }
        }
    } else if let Err(ui_err) = ui::run_tui(&loaded.config.claude, days) {
        let _ = writeln!(err, "term-claude: error en la interfaz: {ui_err}");
        EXIT_RUNTIME_ERR
    } else {
        EXIT_OK
    }
}

fn main() -> ExitCode {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let code = run(std::env::args().skip(1), &mut stdout, &mut stderr);
    ExitCode::from(code)
}
