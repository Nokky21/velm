use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use term_config::AiConfig;

use crate::client::{self, ChatMessage, ChatRequest, ClientTimeouts, StreamHooks};
use crate::detect::{self, ResolvedEndpoint};
use crate::errors::AiError;

/// Fixed English system prompt for `term-ai explain` (CONTRATO §B.2).
pub const EXPLAIN_SYSTEM_PROMPT: &str = "You are a shell assistant on macOS. Explain briefly, in Spanish, what this command output means and, if it is an error, the most likely cause and how to fix it. Do not invent facts that are not in the output.";

/// Parsed representation of the input file written by the terminal (CONTRATO §B.2 & §E.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplainInput {
    pub command: String,
    pub exit: String,
    pub truncated: String,
    pub output: String,
}

impl ExplainInput {
    /// Parses the structured UTF-8 input file from CONTRATO §B.2.
    pub fn parse(raw: &str) -> Self {
        let normalized = raw.replace("\r\n", "\n");
        if let Some((header, body)) = normalized.split_once("\n---\n") {
            let mut command = String::new();
            let mut exit = String::from("desconocido");
            let mut truncated = String::from("0");
            for line in header.lines() {
                if let Some(v) = line.strip_prefix("command:") {
                    command = v.trim().to_owned();
                } else if let Some(v) = line.strip_prefix("exit:") {
                    exit = v.trim().to_owned();
                } else if let Some(v) = line.strip_prefix("truncated:") {
                    truncated = v.trim().to_owned();
                }
            }
            return Self {
                command,
                exit,
                truncated,
                output: body.to_owned(),
            };
        }

        Self {
            command: String::new(),
            exit: String::from("desconocido"),
            truncated: String::from("0"),
            output: normalized,
        }
    }

    /// Formats the parsed input back into the user prompt sent to the model.
    pub fn to_user_message(&self) -> String {
        if self.command.is_empty() {
            return self.output.clone();
        }
        format!(
            "command: {}\nexit: {}\ntruncated: {}\n---\n{}",
            self.command, self.exit, self.truncated, self.output
        )
    }
}

/// Reads the input file and deletes it immediately when `delete_input` is `true`,
/// even if subsequent validation or server calls fail (CONTRATO §B.2).
pub fn read_and_maybe_delete_input(
    input_file: &Path,
    delete_input: bool,
) -> Result<String, AiError> {
    let content = fs::read_to_string(input_file)
        .map_err(|err| AiError::Io(format!("no se pudo leer {}: {err}", input_file.display())))?;

    if delete_input {
        let _ = fs::remove_file(input_file);
    }

    Ok(content)
}

/// Streams the explanation for `raw_input` to `out` against an already-resolved endpoint.
pub fn run_explain_with_endpoint<W: Write>(
    cfg: &AiConfig,
    endpoint: &ResolvedEndpoint,
    raw_input: &str,
    timeouts: ClientTimeouts,
    out: &mut W,
) -> Result<(), AiError> {
    let parsed = ExplainInput::parse(raw_input);
    let user_msg = parsed.to_user_message();
    if user_msg.trim().is_empty() {
        return Err(AiError::Usage(String::from(
            "el fichero de entrada está vacío",
        )));
    }

    let req = ChatRequest {
        model: endpoint.model.clone(),
        messages: vec![
            ChatMessage {
                role: String::from("system"),
                content: String::from(EXPLAIN_SYSTEM_PROMPT),
            },
            ChatMessage {
                role: String::from("user"),
                content: user_msg,
            },
        ],
        temperature: cfg.temperature,
        max_tokens: cfg.max_tokens_explain,
        lmstudio_ttl_seconds: cfg.lmstudio_ttl_seconds,
    };

    {
        let mut hooks = StreamHooks {
            on_loading_model: Some(Box::new(|| {
                let _ = writeln!(io::stderr(), "cargando modelo…");
            })),
            on_delta: Some(Box::new(|delta| {
                if !delta.text.is_empty() {
                    let _ = out.write_all(delta.text.as_bytes());
                    let _ = out.flush();
                }
            })),
        };

        client::chat_completions(
            &endpoint.endpoint,
            endpoint.server,
            &req,
            &endpoint.available_models,
            timeouts,
            &mut hooks,
        )?;
    }

    let _ = writeln!(out);
    let _ = out.flush();
    Ok(())
}

/// Full CLI entrypoint for `term-ai explain`.
pub fn run_explain(
    cfg: &AiConfig,
    input_file: &Path,
    delete_input: bool,
    no_wait: bool,
) -> Result<(), AiError> {
    let raw_input = read_and_maybe_delete_input(input_file, delete_input)?;
    let timeouts = ClientTimeouts::default();
    let endpoint = detect::resolve_endpoint(cfg, timeouts)?;
    let mut stdout = io::stdout();
    run_explain_with_endpoint(cfg, &endpoint, &raw_input, timeouts, &mut stdout)?;

    if !no_wait && stdout.is_terminal() {
        wait_for_q()?;
    }

    Ok(())
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn wait_for_q() -> Result<(), AiError> {
    let _ = writeln!(io::stderr(), "[pulsa q para cerrar]");
    let _guard = RawModeGuard::enter()
        .map_err(|err| AiError::Io(format!("no se pudo leer el teclado: {err}")))?;

    loop {
        match event::read() {
            Ok(Event::Key(key)) => {
                if key.code == KeyCode::Char('q') || key.code == KeyCode::Char('Q') {
                    return Ok(());
                }
                if key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    return Err(AiError::Cancelled);
                }
            }
            Ok(_) => {}
            Err(err) => {
                return Err(AiError::Io(format!("error al esperar tecla: {err}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;
    use std::thread;

    fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "term-ai-explain-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parses_contract_input_file() {
        let raw =
            "command: cargo build\nexit: 101\ntruncated: 12\n---\nerror[E0425]: cannot find value";
        let parsed = ExplainInput::parse(raw);
        assert_eq!(parsed.command, "cargo build");
        assert_eq!(parsed.exit, "101");
        assert_eq!(parsed.truncated, "12");
        assert_eq!(parsed.output, "error[E0425]: cannot find value");
    }

    #[test]
    fn delete_input_deletes_file_even_when_server_fails() {
        let path = temp_file(
            "fail.txt",
            "command: ls\nexit: 1\ntruncated: 0\n---\nNo such file",
        );
        assert!(path.exists());

        let raw = read_and_maybe_delete_input(&path, true).unwrap();
        assert!(
            !path.exists(),
            "file must be deleted immediately after reading"
        );

        // Closed port -> server failure afterwards
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_addr = listener.local_addr().unwrap();
        drop(listener);

        let ep = ResolvedEndpoint {
            endpoint: client::ValidatedEndpoint::parse_and_resolve(&format!(
                "http://{closed_addr}/v1"
            ))
            .unwrap(),
            server: term_config::AiServer::Ollama,
            model: String::from("qwen"),
            available_models: vec![String::from("qwen")],
        };

        let mut out = Vec::new();
        let res = run_explain_with_endpoint(
            &AiConfig::default(),
            &ep,
            &raw,
            ClientTimeouts::default(),
            &mut out,
        );
        assert!(res.is_err());
        assert!(!path.exists());
    }

    #[test]
    fn streams_explanation_while_stripping_think_tags() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let sse = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"<thi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"nk>oculto</think>\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"El comando falló \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"por falta de permisos.\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n"
        );
        let handle = thread::spawn(move || {
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
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(),
                sse
            );
            let _ = stream.write_all(resp.as_bytes());
        });

        let ep = ResolvedEndpoint {
            endpoint: client::ValidatedEndpoint::parse_and_resolve(&format!("http://{addr}/v1"))
                .unwrap(),
            server: term_config::AiServer::Ollama,
            model: String::from("qwen"),
            available_models: vec![String::from("qwen")],
        };
        let mut out = Vec::new();
        run_explain_with_endpoint(
            &AiConfig::default(),
            &ep,
            "command: cat /etc/shadow\nexit: 1\ntruncated: 0\n---\nPermission denied",
            ClientTimeouts::default(),
            &mut out,
        )
        .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.trim(), "El comando falló por falta de permisos.");
        handle.join().unwrap();
    }
}
