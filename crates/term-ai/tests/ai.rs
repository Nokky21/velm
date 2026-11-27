use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use term_ai::bench::{load_battery, normalize_whitespace};
use term_ai::client::{
    chat_completions, list_models, ChatMessage, ChatRequest, ClientTimeouts, StreamHooks,
    ValidatedEndpoint,
};
use term_ai::detect::{detect_server_on, resolve_endpoint_with_candidates, ProbeCandidate};
use term_ai::errors::AiError;
use term_ai::explain::{read_and_maybe_delete_input, run_explain_with_endpoint};
use term_ai::suggest::{run_suggest_with_endpoint, SuggestContext, SuggestOutcome};
use term_config::{AiConfig, AiServer};

struct TempFileGuard {
    path: PathBuf,
}

impl TempFileGuard {
    fn new(name: &str, content: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("term-ai-it-{}-{nanos}-{name}", std::process::id()));
        fs::write(&path, content).unwrap();
        Self { path }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn spawn_one_shot(
    status_line: &'static str,
    content_type: &'static str,
    body: String,
) -> (SocketAddr, mpsc::Receiver<String>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let raw_req = read_http_request(&mut stream);
        let _ = tx.send(raw_req);
        let resp = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
    });
    (addr, rx, handle)
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
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
    String::from_utf8_lossy(&req_buf).into_owned()
}

fn make_endpoint(addr: SocketAddr) -> ValidatedEndpoint {
    ValidatedEndpoint::parse_and_resolve(&format!("http://{addr}/v1")).unwrap()
}

#[test]
fn streams_standard_and_split_think_fixtures_and_rejects_unclosed_think() {
    // 1. Standard SSE stream fixture
    let sse_ok = include_str!("fixtures/ai.chat.stream.sintetico.sse.txt").to_owned();
    let (addr, _rx, h) = spawn_one_shot("200 OK", "text/event-stream", sse_ok);
    let ep = make_endpoint(addr);
    let req = ChatRequest {
        model: String::from("qwen3.8-9b-distill-uncensored-heretic"),
        messages: vec![ChatMessage {
            role: String::from("user"),
            content: String::from("large files"),
        }],
        temperature: 0.2,
        max_tokens: 512,
        lmstudio_ttl_seconds: 600,
    };
    let mut hooks = StreamHooks::default();
    let res = chat_completions(
        &ep,
        AiServer::Ollama,
        &req,
        &[],
        ClientTimeouts::default(),
        &mut hooks,
    )
    .unwrap();
    assert_eq!(res.text, "```zsh\nfind . -type f -size +100M\n```");
    h.join().unwrap();

    // 2. Split <think> tag across SSE chunks fixture
    let sse_split =
        include_str!("fixtures/ai.chat.stream.think-split.sintetico.sse.txt").to_owned();
    let (addr2, _rx2, h2) = spawn_one_shot("200 OK", "text/event-stream", sse_split);
    let ep2 = make_endpoint(addr2);
    let mut hooks2 = StreamHooks::default();
    let res2 = chat_completions(
        &ep2,
        AiServer::Ollama,
        &req,
        &[],
        ClientTimeouts::default(),
        &mut hooks2,
    )
    .unwrap();
    assert_eq!(res2.text.trim(), "```zsh\nls -la\n```");
    h2.join().unwrap();

    // 3. Unclosed <think> fixture -> THINKING error
    let sse_unclosed =
        include_str!("fixtures/ai.chat.stream.think-unclosed.sintetico.sse.txt").to_owned();
    let (addr3, _rx3, h3) = spawn_one_shot("200 OK", "text/event-stream", sse_unclosed);
    let ep3 = make_endpoint(addr3);
    let mut hooks3 = StreamHooks::default();
    let err3 = chat_completions(
        &ep3,
        AiServer::Ollama,
        &req,
        &[],
        ClientTimeouts::default(),
        &mut hooks3,
    )
    .unwrap_err();
    assert_eq!(err3, AiError::Thinking);
    assert_eq!(err3.exit_code(), 1);
    h3.join().unwrap();
}

#[test]
fn ignores_reasoning_content_and_reasoning_fields_in_sse_stream() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"secret\",\"reasoning\":\"also secret\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"more\",\"content\":\"```zsh\\npwd\\n```\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n"
    )
    .to_owned();
    let (addr, _rx, h) = spawn_one_shot("200 OK", "text/event-stream", sse);
    let ep = make_endpoint(addr);
    let req = ChatRequest {
        model: String::from("qwen"),
        messages: vec![ChatMessage {
            role: String::from("user"),
            content: String::from("where am i"),
        }],
        temperature: 0.2,
        max_tokens: 512,
        lmstudio_ttl_seconds: 600,
    };
    let mut hooks = StreamHooks::default();
    let res = chat_completions(
        &ep,
        AiServer::Llamacpp,
        &req,
        &[],
        ClientTimeouts::default(),
        &mut hooks,
    )
    .unwrap();
    assert_eq!(res.text, "```zsh\npwd\n```");
    h.join().unwrap();
}

#[test]
fn checks_http_status_before_reading_sse_stream_on_404_json_fixture() {
    let body = include_str!("fixtures/ai.error.404.model-not-found.stream.json").to_owned();
    let (addr, _rx, h) = spawn_one_shot("404 Not Found", "application/json", body);
    let ep = make_endpoint(addr);
    let req = ChatRequest {
        model: String::from("nope"),
        messages: vec![ChatMessage {
            role: String::from("user"),
            content: String::from("hi"),
        }],
        temperature: 0.2,
        max_tokens: 512,
        lmstudio_ttl_seconds: 600,
    };
    let mut hooks = StreamHooks::default();
    let err = chat_completions(
        &ep,
        AiServer::Ollama,
        &req,
        &[String::from("qwen3.8-9b-distill-uncensored-heretic")],
        ClientTimeouts::default(),
        &mut hooks,
    )
    .unwrap_err();
    assert_eq!(
        err,
        AiError::ModelNotFound {
            model: String::from("nope"),
            available: vec![String::from("qwen3.8-9b-distill-uncensored-heretic")],
        }
    );
    assert!(err
        .to_string()
        .contains("disponibles: qwen3.8-9b-distill-uncensored-heretic"));
    h.join().unwrap();
}

#[test]
fn enforces_first_token_and_total_timeouts() {
    // First-token timeout: server sends 200 OK headers, then sleeps without sending tokens
    let listener1 = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr1 = listener1.local_addr().unwrap();
    let h1 = thread::spawn(move || {
        let (mut stream, _) = listener1.accept().unwrap();
        let _ = read_http_request(&mut stream);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
        let _ = stream.flush();
        thread::sleep(Duration::from_millis(350));
    });

    let ep1 = make_endpoint(addr1);
    let req = ChatRequest {
        model: String::from("qwen"),
        messages: vec![ChatMessage {
            role: String::from("user"),
            content: String::from("hi"),
        }],
        temperature: 0.2,
        max_tokens: 512,
        lmstudio_ttl_seconds: 600,
    };
    let fast_timeouts = ClientTimeouts {
        connect: Duration::from_millis(200),
        detect_connect: Duration::from_millis(100),
        first_token: Duration::from_millis(100),
        total: Duration::from_millis(500),
        loading_notice_after: Duration::from_secs(5),
    };
    let mut hooks = StreamHooks::default();
    let err1 =
        chat_completions(&ep1, AiServer::Ollama, &req, &[], fast_timeouts, &mut hooks).unwrap_err();
    assert_eq!(err1, AiError::FirstTokenTimeout);
    h1.join().unwrap();

    // Total timeout: server sends first token fast, then stalls past total timeout
    let listener2 = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr2 = listener2.local_addr().unwrap();
    let h2 = thread::spawn(move || {
        let (mut stream, _) = listener2.accept().unwrap();
        let _ = read_http_request(&mut stream);
        let _ = stream.write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        );
        let _ = stream.flush();
        thread::sleep(Duration::from_millis(350));
    });

    let ep2 = make_endpoint(addr2);
    let total_timeouts = ClientTimeouts {
        connect: Duration::from_millis(200),
        detect_connect: Duration::from_millis(100),
        first_token: Duration::from_millis(100),
        total: Duration::from_millis(150),
        loading_notice_after: Duration::from_secs(5),
    };
    let mut hooks2 = StreamHooks::default();
    let err2 = chat_completions(
        &ep2,
        AiServer::Ollama,
        &req,
        &[],
        total_timeouts,
        &mut hooks2,
    )
    .unwrap_err();
    assert_eq!(err2, AiError::TotalTimeout);
    h2.join().unwrap();
}

#[test]
fn detects_first_non_empty_server_and_picks_first_model_when_unset() {
    let empty = include_str!("fixtures/ai.models.ollama-empty.json").to_owned();
    let one = include_str!("fixtures/ai.models.one-model.sintetico.json").to_owned();

    let (addr_empty, _, h1) = spawn_one_shot("200 OK", "application/json", empty);
    let (addr_one, _, h2) = spawn_one_shot("200 OK", "application/json", one);

    let candidates = vec![
        ProbeCandidate {
            url: format!("http://{addr_empty}/v1"),
            default_server: AiServer::Ollama,
        },
        ProbeCandidate {
            url: format!("http://{addr_one}/v1"),
            default_server: AiServer::Llamacpp,
        },
    ];

    let cfg = AiConfig::default();
    let resolved =
        resolve_endpoint_with_candidates(&cfg, &candidates, ClientTimeouts::default()).unwrap();
    assert_eq!(resolved.server, AiServer::Llamacpp);
    assert_eq!(resolved.model, "qwen3.8-9b-distill-uncensored-heretic");
    h1.join().unwrap();
    h2.join().unwrap();
}

#[test]
fn cli_suggest_doctor_explain_and_bench_end_to_end() {
    let bin = env!("CARGO_BIN_EXE_term-ai");

    // 1. Non-local URL rejected with exit 2 before any network call
    let bad_cfg = TempFileGuard::new(
        "bad-url.toml",
        "[term.ai]\nenabled = true\nurl = \"http://10.0.0.5:1234/v1\"\n",
    );
    let out_bad = Command::new(bin)
        .args([
            "--config",
            bad_cfg.path.to_str().unwrap(),
            "suggest",
            "--",
            "list",
        ])
        .output()
        .unwrap();
    assert_eq!(out_bad.status.code(), Some(2));

    // 2. Mock server handling GET /models + POST /chat/completions
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let models_body = include_str!("fixtures/ai.models.one-model.sintetico.json").to_owned();
    let sse_safe = include_str!("fixtures/ai.chat.stream.sintetico.sse.txt").to_owned();
    let sse_danger = "data: {\"choices\":[{\"delta\":{\"content\":\"```zsh\\nrm -rf /tmp/x\\n```\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n".to_owned();
    let sse_nocmd = "data: {\"choices\":[{\"delta\":{\"content\":\"NO_COMMAND\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n".to_owned();
    let sse_explain = "data: {\"choices\":[{\"delta\":{\"content\":\"Falta el archivo.\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n".to_owned();

    let server = thread::spawn(move || {
        // Expect 9 HTTP requests from CLI invocations below
        for _ in 0..9 {
            let (mut stream, _) = listener.accept().unwrap();
            let raw = read_http_request(&mut stream);
            let first_line = raw.lines().next().unwrap_or("");
            let (status, ct, body) = if first_line.starts_with("GET ") {
                ("200 OK", "application/json", models_body.clone())
            } else if raw.contains("delete temp") {
                ("200 OK", "text/event-stream", sse_danger.clone())
            } else if raw.contains("monoid") {
                ("200 OK", "text/event-stream", sse_nocmd.clone())
            } else if raw.contains("No such file") {
                ("200 OK", "text/event-stream", sse_explain.clone())
            } else {
                ("200 OK", "text/event-stream", sse_safe.clone())
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });

    let cfg_file = TempFileGuard::new(
        "ok.toml",
        &format!("[term.ai]\nenabled = true\nurl = \"http://{addr}/v1\"\n"),
    );
    let cfg_path = cfg_file.path.to_str().unwrap();

    // doctor (1 GET /models) -> exit 0
    let out_doc = Command::new(bin)
        .args(["--config", cfg_path, "doctor"])
        .output()
        .unwrap();
    assert_eq!(out_doc.status.code(), Some(0));

    // suggest safe (1 GET + 1 POST) -> exit 0
    let out_safe = Command::new(bin)
        .args([
            "--config",
            cfg_path,
            "suggest",
            "--cwd",
            "/tmp",
            "--branch",
            "main",
            "--",
            "find big files",
        ])
        .output()
        .unwrap();
    assert_eq!(out_safe.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&out_safe.stdout).trim(),
        "find . -type f -size +100M"
    );

    // suggest dangerous (1 GET + 1 POST) -> exit 4, commented
    let out_danger = Command::new(bin)
        .args(["--config", cfg_path, "suggest", "--", "delete temp"])
        .output()
        .unwrap();
    assert_eq!(out_danger.status.code(), Some(4));
    assert_eq!(
        String::from_utf8_lossy(&out_danger.stdout).trim(),
        "# rm -rf /tmp/x"
    );

    // suggest NO_COMMAND (1 GET + 1 POST) -> exit 3
    let out_no = Command::new(bin)
        .args(["--config", cfg_path, "suggest", "--", "monoid"])
        .output()
        .unwrap();
    assert_eq!(out_no.status.code(), Some(3));

    // explain --delete-input --no-wait (1 GET + 1 POST) -> exit 0, deletes input
    let explain_input = TempFileGuard::new(
        "explain.txt",
        "command: cat nope\nexit: 1\ntruncated: 0\n---\nNo such file",
    );
    let explain_path = explain_input.path.to_str().unwrap().to_owned();
    let out_explain = Command::new(bin)
        .args([
            "--config",
            cfg_path,
            "explain",
            "--input-file",
            &explain_path,
            "--delete-input",
            "--no-wait",
        ])
        .output()
        .unwrap();
    assert_eq!(out_explain.status.code(), Some(0));
    assert!(!PathBuf::from(&explain_path).exists());
    assert_eq!(
        String::from_utf8_lossy(&out_explain.stdout).trim(),
        "Falta el archivo."
    );

    server.join().unwrap();
}

#[test]
fn battery_toml_has_25_valid_cases_and_whitespace_normalizes() {
    let battery_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/battery.toml");
    let cases = load_battery(&battery_path).unwrap();
    assert_eq!(cases.len(), 25);
    assert_eq!(
        normalize_whitespace("  find   .  -type\tf  "),
        "find . -type f"
    );

    // Also verify read_and_maybe_delete_input + run_suggest_with_endpoint + detect_server_on smoke
    let tmp = TempFileGuard::new("del.txt", "command: ls\nexit: 0\ntruncated: 0\n---\nok");
    let content = read_and_maybe_delete_input(&tmp.path, true).unwrap();
    assert!(!tmp.path.exists());
    assert!(content.contains("command: ls"));

    let _ = (
        run_explain_with_endpoint::<Vec<u8>>,
        run_suggest_with_endpoint,
        detect_server_on,
        SuggestContext::default(),
        SuggestOutcome::NoCommand,
        list_models,
    );
}
