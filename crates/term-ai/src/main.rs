use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process;

use term_ai::bench;
use term_ai::client::ClientTimeouts;
use term_ai::detect;
use term_ai::errors::AiError;
use term_ai::explain;
use term_ai::suggest::{self, SuggestContext, SuggestOutcome};
use term_config::{load, AiConfig};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let code = run_cli(&args);
    process::exit(code);
}

fn run_cli(args: &[String]) -> i32 {
    let parsed = match parse_global_args(args) {
        Ok(p) => p,
        Err(err) => {
            if !err.to_string().is_empty() {
                let _ = writeln!(io::stderr(), "{err}");
            }
            return err.exit_code();
        }
    };

    if parsed.show_help {
        print_usage();
        return 0;
    }

    let loaded = match load(parsed.config_path.as_deref()) {
        Ok(cfg) => cfg,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            return 2;
        }
    };

    for warning in &loaded.warnings {
        let _ = writeln!(io::stderr(), "aviso: {warning}");
    }

    let ai_cfg = &loaded.config.ai;

    match parsed.subcommand.as_str() {
        "suggest" => cmd_suggest(ai_cfg, &parsed.rest),
        "explain" => cmd_explain(ai_cfg, &parsed.rest),
        "doctor" => cmd_doctor(ai_cfg, &parsed.rest),
        "bench" => cmd_bench(ai_cfg, &parsed.rest),
        other => {
            let _ = writeln!(io::stderr(), "subcomando desconocido: '{other}'");
            print_usage();
            2
        }
    }
}

struct GlobalArgs {
    config_path: Option<PathBuf>,
    subcommand: String,
    rest: Vec<String>,
    show_help: bool,
}

fn parse_global_args(args: &[String]) -> Result<GlobalArgs, AiError> {
    let mut config_path: Option<PathBuf> = None;
    let mut remaining: Vec<String> = Vec::new();
    let mut i = 0;
    let mut after_double_dash = false;

    while i < args.len() {
        let arg = &args[i];
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            remaining.push(arg.clone());
            i += 1;
            continue;
        }
        if !after_double_dash && arg == "--config" {
            i += 1;
            let val = args
                .get(i)
                .ok_or_else(|| AiError::Usage(String::from("falta la ruta tras --config")))?;
            config_path = Some(PathBuf::from(val));
            i += 1;
            continue;
        }
        if !after_double_dash && (arg == "--help" || arg == "-h") && remaining.is_empty() {
            return Ok(GlobalArgs {
                config_path,
                subcommand: String::new(),
                rest: Vec::new(),
                show_help: true,
            });
        }
        remaining.push(arg.clone());
        i += 1;
    }

    if remaining.is_empty() {
        return Err(AiError::Usage(String::from(
            "uso: term-ai [--config <ruta>] <suggest|explain|doctor|bench>",
        )));
    }

    let subcommand = remaining[0].clone();
    let rest = remaining[1..].to_vec();
    Ok(GlobalArgs {
        config_path,
        subcommand,
        rest,
        show_help: false,
    })
}

fn cmd_suggest(cfg: &AiConfig, args: &[String]) -> i32 {
    let mut cwd: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut i = 0;
    let mut after_double_dash = false;

    while i < args.len() {
        let arg = &args[i];
        if !after_double_dash && arg == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }
        if !after_double_dash && arg == "--cwd" {
            i += 1;
            let Some(val) = args.get(i) else {
                let _ = writeln!(io::stderr(), "falta el valor tras --cwd");
                return 2;
            };
            cwd = Some(val.clone());
            i += 1;
            continue;
        }
        if !after_double_dash && arg == "--branch" {
            i += 1;
            let Some(val) = args.get(i) else {
                let _ = writeln!(io::stderr(), "falta el valor tras --branch");
                return 2;
            };
            branch = Some(val.clone());
            i += 1;
            continue;
        }
        if !after_double_dash && arg.starts_with('-') {
            let _ = writeln!(io::stderr(), "opción desconocida en suggest: '{arg}'");
            return 2;
        }
        prompt_parts.push(arg.clone());
        i += 1;
    }

    let prompt = prompt_parts.join(" ");
    if prompt.trim().is_empty() {
        let _ = writeln!(io::stderr(), "falta el texto de la petición");
        return 2;
    }

    let ctx = SuggestContext {
        cwd: cwd.as_deref(),
        branch: branch.as_deref(),
        macos_version: None,
    };

    match suggest::run_suggest(cfg, &prompt, &ctx) {
        Ok(SuggestOutcome::Command(cmd)) => {
            println!("{cmd}");
            0
        }
        Ok(SuggestOutcome::Dangerous(commented)) => {
            println!("{commented}");
            let _ = writeln!(
                io::stderr(),
                "comando peligroso: quita el # si de verdad lo quieres"
            );
            4
        }
        Ok(SuggestOutcome::NoCommand) => 3,
        Err(err) => {
            if !err.to_string().is_empty() {
                let _ = writeln!(io::stderr(), "{err}");
            }
            err.exit_code()
        }
    }
}

fn cmd_explain(cfg: &AiConfig, args: &[String]) -> i32 {
    let mut input_file: Option<PathBuf> = None;
    let mut delete_input = false;
    let mut no_wait = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--input-file" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    let _ = writeln!(io::stderr(), "falta la ruta tras --input-file");
                    return 2;
                };
                input_file = Some(PathBuf::from(val));
            }
            "--delete-input" => delete_input = true,
            "--no-wait" => no_wait = true,
            other => {
                let _ = writeln!(io::stderr(), "opción desconocida en explain: '{other}'");
                return 2;
            }
        }
        i += 1;
    }

    let Some(path) = input_file else {
        let _ = writeln!(io::stderr(), "falta --input-file <ruta>");
        return 2;
    };

    match explain::run_explain(cfg, &path, delete_input, no_wait) {
        Ok(()) => 0,
        Err(err) => {
            if !err.to_string().is_empty() {
                let _ = writeln!(io::stderr(), "{err}");
            }
            err.exit_code()
        }
    }
}

fn cmd_doctor(cfg: &AiConfig, args: &[String]) -> i32 {
    if !args.is_empty() {
        let _ = writeln!(
            io::stderr(),
            "argumento inesperado en doctor: '{}'",
            args[0]
        );
        return 2;
    }

    let timeouts = ClientTimeouts::default();
    if cfg.url.trim().is_empty() {
        let reports = detect::probe_all(&detect::default_candidates(), cfg.server, timeouts);
        let mut any_usable = false;
        for rep in &reports {
            if let Some(err) = &rep.error {
                println!("{} ({}): {}", rep.url, rep.server, err);
            } else {
                any_usable = true;
                println!(
                    "{} ({}): OK — modelos: {}",
                    rep.url,
                    rep.server,
                    rep.models.join(", ")
                );
            }
        }
        if any_usable {
            0
        } else {
            1
        }
    } else {
        match detect::resolve_endpoint(cfg, timeouts) {
            Ok(resolved) => {
                println!(
                    "{} ({}): OK — modelo activo: {} | disponibles: {}",
                    resolved.endpoint.base_url,
                    resolved.server,
                    resolved.model,
                    resolved.available_models.join(", ")
                );
                0
            }
            Err(err) => {
                if !err.to_string().is_empty() {
                    let _ = writeln!(io::stderr(), "{err}");
                }
                err.exit_code()
            }
        }
    }
}

fn cmd_bench(cfg: &AiConfig, args: &[String]) -> i32 {
    let mut battery: Option<PathBuf> = None;
    let mut no_interactive = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--battery" => {
                i += 1;
                let Some(val) = args.get(i) else {
                    let _ = writeln!(io::stderr(), "falta la ruta tras --battery");
                    return 2;
                };
                battery = Some(PathBuf::from(val));
            }
            "--no-interactive" => no_interactive = true,
            other => {
                let _ = writeln!(io::stderr(), "opción desconocida en bench: '{other}'");
                return 2;
            }
        }
        i += 1;
    }

    match bench::run_bench(cfg, battery.as_deref(), no_interactive) {
        Ok(_) => 0,
        Err(err) => {
            if !err.to_string().is_empty() {
                let _ = writeln!(io::stderr(), "{err}");
            }
            err.exit_code()
        }
    }
}

fn print_usage() {
    println!(
        "Uso: term-ai [--config <ruta>] <subcomando>\n\
         Subcomandos:\n\
         \tsuggest [--cwd <carpeta>] [--branch <rama>] -- <texto>\n\
         \texplain --input-file <ruta> [--delete-input] [--no-wait]\n\
         \tdoctor\n\
         \tbench [--battery <ruta>] [--no-interactive]"
    );
}
