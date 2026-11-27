use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use term_repos::git::inspect_repos;
use term_repos::scan::discover_repos;
use term_repos::ui::run_tui;

struct CliArgs {
    config_path: Option<PathBuf>,
    json: bool,
    roots: Vec<PathBuf>,
    help: bool,
}

fn main() -> ExitCode {
    let args = match parse_cli_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("error: {msg}");
            print_usage();
            return ExitCode::from(2);
        }
    };

    if args.help {
        print_usage();
        return ExitCode::SUCCESS;
    }

    let loaded = match term_config::load(args.config_path.as_deref()) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("error de configuración: {err}");
            return ExitCode::from(2);
        }
    };

    for warning in &loaded.warnings {
        eprintln!("aviso: {warning}");
    }

    let mut repos_cfg = loaded.config.repos;
    let roots = if args.roots.is_empty() {
        repos_cfg.roots.clone()
    } else {
        match expand_roots(&args.roots) {
            Ok(expanded) => expanded,
            Err(err) => {
                eprintln!("error de configuración: {err}");
                return ExitCode::from(2);
            }
        }
    };

    if roots.is_empty() {
        eprintln!("error de configuración: no hay carpetas raíz configuradas");
        return ExitCode::from(2);
    }

    for root in &roots {
        if !root.is_dir() {
            eprintln!(
                "error de configuración: la carpeta raíz no existe o no es un directorio: {}",
                root.display()
            );
            return ExitCode::from(2);
        }
    }

    repos_cfg.roots = roots.clone();

    if args.json {
        let discovered = discover_repos(
            &roots,
            repos_cfg.max_depth,
            repos_cfg.include_worktrees,
            &repos_cfg.skip,
        );
        let statuses = inspect_repos(&discovered, repos_cfg.parallel, repos_cfg.timeout_ms);
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        if let Err(err) = serde_json::to_writer_pretty(&mut handle, &statuses) {
            eprintln!("error al escribir JSON: {err}");
            return ExitCode::from(1);
        }
        use std::io::Write;
        if let Err(err) = writeln!(handle) {
            eprintln!("error al escribir JSON: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    if let Err(err) = run_tui(roots, &repos_cfg) {
        eprintln!("error: {err}");
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

fn expand_roots(raw_roots: &[PathBuf]) -> Result<Vec<PathBuf>, term_config::ConfigError> {
    raw_roots
        .iter()
        .map(|root| match root.to_str() {
            Some(s) => term_config::expand_tilde(s),
            None => Ok(root.clone()),
        })
        .collect()
}

fn parse_cli_args<I>(iter: I) -> Result<CliArgs, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = CliArgs {
        config_path: None,
        json: false,
        roots: Vec::new(),
        help: false,
    };

    let mut iter = iter.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--help" || arg == "-h" {
            args.help = true;
        } else if arg == "--json" {
            args.json = true;
        } else if arg == "--config" {
            let val = iter
                .next()
                .ok_or_else(|| String::from("falta la ruta tras --config"))?;
            args.config_path = Some(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--config=") {
            if val.is_empty() {
                return Err(String::from("falta la ruta en --config="));
            }
            args.config_path = Some(PathBuf::from(val));
        } else if arg == "--root" {
            let val = iter
                .next()
                .ok_or_else(|| String::from("falta la carpeta tras --root"))?;
            args.roots.push(PathBuf::from(val));
        } else if let Some(val) = arg.strip_prefix("--root=") {
            if val.is_empty() {
                return Err(String::from("falta la carpeta en --root="));
            }
            args.roots.push(PathBuf::from(val));
        } else {
            return Err(format!("opción desconocida: {arg}"));
        }
    }

    Ok(args)
}

fn print_usage() {
    eprintln!("Uso: term-repos [--config <ruta>] [--json] [--root <carpeta>]...");
}
