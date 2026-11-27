use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use term_config::AiConfig;

use crate::client::ClientTimeouts;
use crate::detect::{self, ResolvedEndpoint};
use crate::errors::AiError;
use crate::suggest::{self, SuggestContext, SuggestOutcome};

/// One test case from `battery.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchCase {
    pub id: String,
    pub prompt: String,
    pub expected: Vec<String>,
}

/// Classification of a single benchmark execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchStatus {
    Exact,
    Equivalent,
    Miss,
    Review,
    Error,
}

/// Result of running one benchmark case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    pub id: String,
    pub status: BenchStatus,
    pub output: String,
    pub total_ms: u128,
}

/// Aggregated results for a benchmark run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchSummary {
    pub cases: Vec<CaseResult>,
    pub exact: usize,
    pub equivalent: usize,
    pub miss: usize,
    pub review: usize,
    pub errors: usize,
    pub total_ms: u128,
}

/// Resolves the default `battery.toml` path from workspace root or crate directory.
pub fn default_battery_path() -> PathBuf {
    let from_root = PathBuf::from("crates/term-ai/tests/battery.toml");
    if from_root.is_file() {
        return from_root;
    }
    let from_crate = PathBuf::from("tests/battery.toml");
    if from_crate.is_file() {
        return from_crate;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/battery.toml")
}

/// Loads and parses `[[case]]` entries from a TOML battery file.
pub fn load_battery(path: &Path) -> Result<Vec<BenchCase>, AiError> {
    let content = fs::read_to_string(path).map_err(|err| {
        AiError::Config(format!(
            "no se pudo leer la batería {}: {err}",
            path.display()
        ))
    })?;
    parse_battery_toml(&content)
}

#[derive(Default)]
struct PendingCase {
    id: Option<String>,
    prompt: Option<String>,
    expected: Option<Vec<String>>,
}

/// Parses `[[case]]` entries (`id`, `prompt`, `expected`) from TOML text.
pub fn parse_battery_toml(content: &str) -> Result<Vec<BenchCase>, AiError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut cases = Vec::new();
    let mut current: Option<PendingCase> = None;

    let chars: Vec<char> = content.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        i = skip_ws_and_comments(&chars, i);
        if i >= chars.len() {
            break;
        }

        if starts_with_chars(&chars, i, "[[case]]") {
            if let Some(pending) = current.take() {
                cases.push(finalize_case(pending)?);
            }
            current = Some(PendingCase::default());
            i += "[[case]]".len();
            continue;
        }

        let Some(ref mut active) = current else {
            return Err(AiError::Config(String::from(
                "batería inválida: se esperaba [[case]]",
            )));
        };

        let (key, next_i) = read_ident(&chars, i)?;
        i = skip_inline_ws(&chars, next_i);
        if chars.get(i) != Some(&'=') {
            return Err(AiError::Config(format!(
                "batería inválida: falta '=' tras '{key}'"
            )));
        }
        i = skip_inline_ws(&chars, i + 1);

        match key.as_str() {
            "id" => {
                let (val, after) = parse_toml_string(&chars, i)?;
                active.id = Some(val);
                i = after;
            }
            "prompt" => {
                let (val, after) = parse_toml_string(&chars, i)?;
                active.prompt = Some(val);
                i = after;
            }
            "expected" => {
                let (arr, after) = parse_toml_string_array(&chars, i)?;
                active.expected = Some(arr);
                i = after;
            }
            other => {
                return Err(AiError::Config(format!(
                    "clave desconocida en battery.toml: '{other}'"
                )));
            }
        }
    }

    if let Some(pending) = current.take() {
        cases.push(finalize_case(pending)?);
    }

    if cases.is_empty() {
        return Err(AiError::Config(String::from(
            "la batería no contiene ningún [[case]]",
        )));
    }

    Ok(cases)
}

fn finalize_case(raw: PendingCase) -> Result<BenchCase, AiError> {
    let id = raw
        .id
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AiError::Config(String::from("caso en battery.toml sin 'id'")))?;
    let prompt = raw
        .prompt
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AiError::Config(format!("caso '{id}' sin 'prompt'")))?;
    let expected = raw
        .expected
        .filter(|v| !v.is_empty())
        .ok_or_else(|| AiError::Config(format!("caso '{id}' sin 'expected'")))?;
    Ok(BenchCase {
        id,
        prompt,
        expected,
    })
}

fn skip_ws_and_comments(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
        } else if chars[i] == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else {
            break;
        }
    }
    i
}

fn skip_inline_ws(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && (chars[i] == ' ' || chars[i] == '\t') {
        i += 1;
    }
    i
}

fn starts_with_chars(chars: &[char], idx: usize, pat: &str) -> bool {
    let pat_chars: Vec<char> = pat.chars().collect();
    chars.get(idx..idx + pat_chars.len()) == Some(pat_chars.as_slice())
}

fn read_ident(chars: &[char], mut i: usize) -> Result<(String, usize), AiError> {
    let start = i;
    while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
        i += 1;
    }
    if i == start {
        return Err(AiError::Config(String::from(
            "batería inválida: identificador esperado",
        )));
    }
    Ok((chars[start..i].iter().collect(), i))
}

fn parse_toml_string(chars: &[char], i: usize) -> Result<(String, usize), AiError> {
    match chars.get(i) {
        Some('"') => {
            let mut idx = i + 1;
            let mut out = String::new();
            while idx < chars.len() {
                let ch = chars[idx];
                if ch == '"' {
                    return Ok((out, idx + 1));
                }
                if ch == '\\' {
                    idx += 1;
                    let esc = *chars.get(idx).ok_or_else(|| {
                        AiError::Config(String::from("escape incompleto en string TOML"))
                    })?;
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        other => out.push(other),
                    }
                    idx += 1;
                    continue;
                }
                out.push(ch);
                idx += 1;
            }
            Err(AiError::Config(String::from("string TOML sin cerrar")))
        }
        Some('\'') => {
            let mut idx = i + 1;
            let mut out = String::new();
            while idx < chars.len() {
                if chars[idx] == '\'' {
                    return Ok((out, idx + 1));
                }
                out.push(chars[idx]);
                idx += 1;
            }
            Err(AiError::Config(String::from(
                "string literal TOML sin cerrar",
            )))
        }
        _ => Err(AiError::Config(String::from(
            "se esperaba un string entre comillas en battery.toml",
        ))),
    }
}

fn parse_toml_string_array(chars: &[char], mut i: usize) -> Result<(Vec<String>, usize), AiError> {
    if chars.get(i) != Some(&'[') {
        return Err(AiError::Config(String::from(
            "se esperaba '[' para 'expected'",
        )));
    }
    i += 1;
    let mut items = Vec::new();
    loop {
        i = skip_ws_and_comments(chars, i);
        if chars.get(i) == Some(&']') {
            return Ok((items, i + 1));
        }
        let (item, next_i) = parse_toml_string(chars, i)?;
        items.push(item);
        i = skip_ws_and_comments(chars, next_i);
        if chars.get(i) == Some(&',') {
            i += 1;
            continue;
        }
        if chars.get(i) == Some(&']') {
            return Ok((items, i + 1));
        }
        return Err(AiError::Config(String::from(
            "se esperaba ',' o ']' en lista 'expected'",
        )));
    }
}

/// Normalizes runs of whitespace for exact-match comparison in `bench`.
pub fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Runs all benchmark cases against an already-resolved endpoint.
pub fn run_bench_with_endpoint<W: Write>(
    cfg: &AiConfig,
    endpoint: &ResolvedEndpoint,
    cases: &[BenchCase],
    timeouts: ClientTimeouts,
    interactive: bool,
    out: &mut W,
) -> BenchSummary {
    let bench_start = std::time::Instant::now();
    let mut results = Vec::with_capacity(cases.len());
    let mut exact = 0;
    let mut equivalent = 0;
    let mut miss = 0;
    let mut review = 0;
    let mut errors = 0;

    let ctx = SuggestContext::default();

    for case in cases {
        let case_start = std::time::Instant::now();
        let outcome =
            suggest::run_suggest_with_endpoint(cfg, endpoint, &case.prompt, &ctx, timeouts, false);
        let elapsed_ms = case_start.elapsed().as_millis();

        let (status, output) = match outcome {
            Ok(SuggestOutcome::Command(cmd)) | Ok(SuggestOutcome::Dangerous(cmd)) => {
                let norm_cmd = normalize_whitespace(&cmd);
                let is_exact = case
                    .expected
                    .iter()
                    .any(|exp| normalize_whitespace(exp) == norm_cmd);
                if is_exact {
                    exact += 1;
                    (BenchStatus::Exact, cmd)
                } else if interactive {
                    if ask_equivalence(&case.id, &case.prompt, &case.expected, &cmd) {
                        equivalent += 1;
                        (BenchStatus::Equivalent, cmd)
                    } else {
                        miss += 1;
                        (BenchStatus::Miss, cmd)
                    }
                } else {
                    review += 1;
                    (BenchStatus::Review, cmd)
                }
            }
            Ok(SuggestOutcome::NoCommand) => {
                let is_exact = case.expected.iter().any(|exp| exp.trim() == "NO_COMMAND");
                if is_exact {
                    exact += 1;
                    (BenchStatus::Exact, String::from("NO_COMMAND"))
                } else {
                    miss += 1;
                    (BenchStatus::Miss, String::from("NO_COMMAND"))
                }
            }
            Err(err) => {
                errors += 1;
                (BenchStatus::Error, err.to_string())
            }
        };

        let label = match status {
            BenchStatus::Exact => "exacto",
            BenchStatus::Equivalent => "equivalente",
            BenchStatus::Miss => "fallo",
            BenchStatus::Review => "revisar",
            BenchStatus::Error => "error",
        };
        let _ = writeln!(out, "[{label}] {} ({} ms): {}", case.id, elapsed_ms, output);

        results.push(CaseResult {
            id: case.id.clone(),
            status,
            output,
            total_ms: elapsed_ms,
        });
    }

    let total_ms = bench_start.elapsed().as_millis();
    let _ = writeln!(
        out,
        "Total: {} | exactos: {} | equivalentes: {} | revisar: {} | fallos: {} | errores: {} | tiempo: {} ms",
        cases.len(),
        exact,
        equivalent,
        review,
        miss,
        errors,
        total_ms
    );

    BenchSummary {
        cases: results,
        exact,
        equivalent,
        miss,
        review,
        errors,
        total_ms,
    }
}

fn ask_equivalence(id: &str, prompt: &str, expected: &[String], actual: &str) -> bool {
    let mut err = io::stderr();
    let _ = writeln!(err, "\nCaso '{id}': {prompt}");
    let _ = writeln!(err, "  Esperado: {}", expected.join(" | "));
    let _ = writeln!(err, "  Obtenido: {actual}");
    let _ = write!(err, "  ¿Es equivalente? [e/N]: ");
    let _ = err.flush();

    let mut line = String::new();
    let stdin = io::stdin();
    if stdin.lock().read_line(&mut line).is_ok() {
        let ans = line.trim().to_ascii_lowercase();
        return ans == "e" || ans == "s" || ans == "y";
    }
    false
}

/// Full CLI entrypoint for `term-ai bench`.
pub fn run_bench(
    cfg: &AiConfig,
    battery_path: Option<&Path>,
    no_interactive: bool,
) -> Result<BenchSummary, AiError> {
    let path = battery_path
        .map(PathBuf::from)
        .unwrap_or_else(default_battery_path);
    let cases = load_battery(&path)?;
    let timeouts = ClientTimeouts::default();
    let endpoint = detect::resolve_endpoint(cfg, timeouts)?;
    let interactive = !no_interactive && io::stdin().is_terminal();
    let mut stdout = io::stdout();
    Ok(run_bench_with_endpoint(
        cfg,
        &endpoint,
        &cases,
        timeouts,
        interactive,
        &mut stdout,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn parses_bundled_25_case_battery_toml() {
        let raw = include_str!("../tests/battery.toml");
        let cases = parse_battery_toml(raw).unwrap();
        assert_eq!(cases.len(), 25);
        assert!(cases
            .iter()
            .all(|c| !c.id.is_empty() && !c.prompt.is_empty() && !c.expected.is_empty()));
    }

    #[test]
    fn runs_two_case_bench_against_mock_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = thread::spawn(move || {
            for idx in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut req_buf = Vec::new();
                let mut tmp = [0u8; 2048];
                loop {
                    let n = stream.read(&mut tmp).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    req_buf.extend_from_slice(&tmp[..n]);
                    if let Some(hdr_end) = req_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&req_buf[..hdr_end]);
                        let content_len = headers
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if req_buf.len() >= hdr_end + 4 + content_len {
                            break;
                        }
                    }
                }
                let cmd = if idx == 0 {
                    "find . -type f -size +100M"
                } else {
                    "echo different"
                };
                let sse = format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"```zsh\\n{cmd}\\n```\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n"
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    sse.len(),
                    sse
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let cases = vec![
            BenchCase {
                id: String::from("c1"),
                prompt: String::from("find big files"),
                expected: vec![String::from("find .  -type f -size +100M")],
            },
            BenchCase {
                id: String::from("c2"),
                prompt: String::from("list files"),
                expected: vec![String::from("ls -la")],
            },
        ];

        let ep = ResolvedEndpoint {
            endpoint: crate::client::ValidatedEndpoint::parse_and_resolve(&format!(
                "http://{addr}/v1"
            ))
            .unwrap(),
            server: term_config::AiServer::Ollama,
            model: String::from("qwen"),
            available_models: vec![String::from("qwen")],
        };

        let mut out = Vec::new();
        let summary = run_bench_with_endpoint(
            &AiConfig::default(),
            &ep,
            &cases,
            ClientTimeouts::default(),
            false,
            &mut out,
        );

        assert_eq!(summary.exact, 1);
        assert_eq!(summary.review, 1);
        assert_eq!(summary.errors, 0);
        handle.join().unwrap();
    }
}
