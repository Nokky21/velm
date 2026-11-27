use std::io::{BufRead, BufReader};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use term_config::validate::normalize_ai_url;
use term_config::AiServer;
use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{ArrayVec, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use ureq::{Agent, Timeout};

use crate::errors::{classify_http_error, AiError};
use crate::think::{ThinkDelta, ThinkFilter};

/// Timeout settings used by the AI HTTP client (CONTRATO §C.4 and §C.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientTimeouts {
    pub connect: Duration,
    pub detect_connect: Duration,
    pub first_token: Duration,
    pub total: Duration,
    pub loading_notice_after: Duration,
}

impl Default for ClientTimeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(2),
            detect_connect: Duration::from_millis(300),
            first_token: Duration::from_secs(90),
            total: Duration::from_secs(180),
            loading_notice_after: Duration::from_secs(3),
        }
    }
}

/// A validated local HTTP endpoint with DNS resolved once and pinned to loopback addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedEndpoint {
    pub base_url: String,
    pub resolved_addrs: Vec<SocketAddr>,
}

impl ValidatedEndpoint {
    /// Parses, validates, and resolves `raw_url` once per CONTRATO §C.6.
    ///
    /// - Scheme must be `http`.
    /// - Host must be `127.0.0.1`, `::1`, `[::1]`, or `localhost` (with all resolved
    ///   addresses strictly in loopback `127.0.0.0/8` or `::1`).
    /// - Path is normalized to end with `/v1`.
    pub fn parse_and_resolve(raw_url: &str) -> Result<Self, AiError> {
        Self::parse_and_resolve_with(raw_url, |host, port| {
            (host, port)
                .to_socket_addrs()
                .map(|iter| iter.collect::<Vec<_>>())
        })
    }

    /// Parses, validates, and resolves using an injectable resolver (for testing non-loopback
    /// `localhost` resolution without modifying system hosts files).
    pub fn parse_and_resolve_with<F>(raw_url: &str, resolve_fn: F) -> Result<Self, AiError>
    where
        F: FnOnce(&str, u16) -> std::io::Result<Vec<SocketAddr>>,
    {
        let normalized = normalize_ai_url(raw_url).map_err(|_| AiError::NotLocal)?;
        let after_scheme = normalized
            .strip_prefix("http://")
            .ok_or(AiError::NotLocal)?;
        let (authority, _path) = match after_scheme.find('/') {
            Some(idx) => (&after_scheme[..idx], &after_scheme[idx..]),
            None => (after_scheme, ""),
        };

        let (host, port) = parse_host_port(authority).ok_or(AiError::NotLocal)?;
        let resolved_addrs = if host == "localhost" {
            let addrs = resolve_fn(host, port).map_err(|_| AiError::NotLocal)?;
            if addrs.is_empty() || !addrs.iter().all(|addr| addr.ip().is_loopback()) {
                return Err(AiError::NotLocal);
            }
            addrs
        } else if host.starts_with('[') && host.ends_with(']') {
            let inner = &host[1..host.len() - 1];
            let ip: Ipv6Addr = inner.parse().map_err(|_| AiError::NotLocal)?;
            if !ip.is_loopback() {
                return Err(AiError::NotLocal);
            }
            vec![SocketAddr::new(IpAddr::V6(ip), port)]
        } else {
            let ip: IpAddr = host.parse().map_err(|_| AiError::NotLocal)?;
            if !ip.is_loopback() {
                return Err(AiError::NotLocal);
            }
            vec![SocketAddr::new(ip, port)]
        };

        Ok(Self {
            base_url: normalized,
            resolved_addrs,
        })
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.base_url)
    }

    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

fn parse_host_port(authority: &str) -> Option<(&str, u16)> {
    if let Some(after_open) = authority.strip_prefix('[') {
        let close_idx = after_open.find(']')?;
        let host = &authority[..close_idx + 2];
        let rest = &after_open[close_idx + 1..];
        if rest.is_empty() {
            return Some((host, 80));
        }
        let port = rest.strip_prefix(':')?.parse::<u16>().ok()?;
        return Some((host, port));
    }

    match authority.split_once(':') {
        Some((host, port_str)) => {
            let port = port_str.parse::<u16>().ok()?;
            Some((host, port))
        }
        None => Some((authority, 80)),
    }
}

/// Custom `ureq` resolver that returns ONLY the pre-validated loopback `SocketAddr`s
/// without performing any further DNS lookup (CONTRATO §C.6).
#[derive(Debug)]
struct PinnedResolver {
    addrs: Vec<SocketAddr>,
}

impl Resolver for PinnedResolver {
    fn resolve(
        &self,
        _uri: &Uri,
        config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let mut out = ArrayVec::from_fn(|_| SocketAddr::from(([127, 0, 0, 1], 0)));
        for addr in config.ip_family().keep_wanted(self.addrs.iter().copied()) {
            if out.len() < 16 {
                out.push(addr);
            }
        }
        if out.is_empty() {
            Err(ureq::Error::HostNotFound)
        } else {
            Ok(out)
        }
    }
}

/// Chat message sent in OpenAI `/chat/completions` requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Parameters for `POST {url}/chat/completions`.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub temperature: f64,
    pub max_tokens: u32,
    pub lmstudio_ttl_seconds: u32,
}

#[derive(Debug, Serialize)]
struct OpenAiChatBody<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    temperature: f64,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    ttl: Option<u32>,
}

/// Result of a completed streaming `/chat/completions` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    pub text: String,
    pub truncated_by_length: bool,
    pub first_token_ms: u128,
    pub total_ms: u128,
}

/// Callback invoked whenever a filtered text chunk is ready during streaming.
pub type DeltaCallback<'a> = Box<dyn FnMut(&ThinkDelta) + 'a>;

/// Streaming callbacks invoked while reading `/chat/completions`.
#[derive(Default)]
pub struct StreamHooks<'a> {
    pub on_loading_model: Option<Box<dyn FnMut() + Send + 'static>>,
    pub on_delta: Option<DeltaCallback<'a>>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Option<Vec<ModelEntry>>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Debug, Deserialize)]
struct SseChunk {
    choices: Option<Vec<SseChoice>>,
}

#[derive(Debug, Deserialize)]
struct SseChoice {
    delta: Option<SseDelta>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SseDelta {
    content: Option<String>,
}

fn build_agent(
    endpoint: &ValidatedEndpoint,
    connect_timeout: Duration,
    recv_response_timeout: Option<Duration>,
    per_read_timeout: Option<Duration>,
    total_timeout: Option<Duration>,
) -> Agent {
    let mut builder = Agent::config_builder()
        .proxy(None)
        .https_only(false)
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(true)
        .timeout_connect(Some(connect_timeout));

    if let Some(recv_resp) = recv_response_timeout {
        builder = builder.timeout_recv_response(Some(recv_resp));
    }
    if let Some(per_read) = per_read_timeout {
        builder = builder.timeout_per_read(Some(per_read));
    }
    if let Some(total) = total_timeout {
        builder = builder.timeout_global(Some(total));
    }

    let config = builder.build();
    Agent::with_parts(
        config,
        DefaultConnector::default(),
        PinnedResolver {
            addrs: endpoint.resolved_addrs.clone(),
        },
    )
}

/// Fetches available model IDs from `GET {url}/models`.
pub fn list_models(
    endpoint: &ValidatedEndpoint,
    connect_timeout: Duration,
    total_timeout: Duration,
) -> Result<Vec<String>, AiError> {
    let agent = build_agent(
        endpoint,
        connect_timeout,
        Some(total_timeout),
        None,
        Some(total_timeout),
    );
    let url = endpoint.models_url();
    let mut response = agent
        .get(&url)
        .call()
        .map_err(|err| map_ureq_error(err, false, vec![endpoint.base_url.clone()]))?;

    let status = response.status().as_u16();
    if status != 200 {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        return Err(classify_http_error(status, &body, None, Vec::new()));
    }

    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|err| map_ureq_error(err, false, vec![endpoint.base_url.clone()]))?;

    let parsed: ModelsResponse = serde_json::from_str(&body).map_err(|_| AiError::Protocol)?;
    let data = parsed.data.ok_or(AiError::Protocol)?;
    Ok(data.into_iter().map(|entry| entry.id).collect())
}

/// Calls `POST {url}/chat/completions` with `stream: true` and applies [`ThinkFilter`]
/// after verifying HTTP status 200 (CONTRATO §C.1, §C.3, §C.4, §C.5).
pub fn chat_completions(
    endpoint: &ValidatedEndpoint,
    server: AiServer,
    request: &ChatRequest,
    available_models: &[String],
    timeouts: ClientTimeouts,
    hooks: &mut StreamHooks<'_>,
) -> Result<ChatResponse, AiError> {
    let start = Instant::now();
    let agent = build_agent(
        endpoint,
        timeouts.connect,
        Some(timeouts.first_token),
        Some(timeouts.first_token),
        Some(timeouts.total),
    );

    let ttl = match server {
        AiServer::Lmstudio => Some(request.lmstudio_ttl_seconds),
        _ => None,
    };

    let payload = OpenAiChatBody {
        model: &request.model,
        messages: &request.messages,
        stream: true,
        temperature: request.temperature,
        max_tokens: request.max_tokens,
        ttl,
    };
    let body_json = serde_json::to_string(&payload).map_err(|_| AiError::Protocol)?;

    let first_token_seen = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::new(AtomicBool::new(false));
    let timer_state = Arc::new((Mutex::new(false), Condvar::new()));

    let loading_cb = hooks.on_loading_model.take();
    let timer_handle = loading_cb.map(|mut cb| {
        let first_seen = Arc::clone(&first_token_seen);
        let stop = Arc::clone(&stop_flag);
        let pair = Arc::clone(&timer_state);
        let wait_dur = timeouts.loading_notice_after;
        thread::spawn(move || {
            let (lock, cvar) = &*pair;
            let guard = lock.lock().unwrap();
            let _ = cvar.wait_timeout_while(guard, wait_dur, |done| !*done);
            if !stop.load(Ordering::SeqCst) && !first_seen.load(Ordering::SeqCst) {
                cb();
            }
        })
    });

    let url = endpoint.chat_completions_url();
    let call_res = agent
        .post(&url)
        .header("Content-Type", "application/json")
        .send(body_json.as_bytes());

    let mut response = match call_res {
        Ok(resp) => resp,
        Err(err) => {
            stop_timer(&stop_flag, &timer_state, timer_handle);
            return Err(map_ureq_error(err, false, vec![endpoint.base_url.clone()]));
        }
    };

    let status = response.status().as_u16();
    if status != 200 {
        stop_timer(&stop_flag, &timer_state, timer_handle);
        if (300..400).contains(&status) {
            return Err(AiError::Redirect);
        }
        let err_body = response.body_mut().read_to_string().unwrap_or_default();
        let mut avail = available_models.to_vec();
        if status == 404 && avail.is_empty() {
            if let Ok(fetched) = list_models(endpoint, timeouts.connect, timeouts.connect) {
                avail = fetched;
            }
        }
        return Err(classify_http_error(
            status,
            &err_body,
            Some(&request.model),
            avail,
        ));
    }

    let reader = BufReader::new(response.into_body().into_reader());
    let result = read_sse_stream(
        reader,
        start,
        timeouts,
        &first_token_seen,
        &timer_state,
        &mut hooks.on_delta,
    );

    stop_timer(&stop_flag, &timer_state, timer_handle);
    result
}

fn stop_timer(
    stop_flag: &Arc<AtomicBool>,
    timer_state: &Arc<(Mutex<bool>, Condvar)>,
    handle: Option<thread::JoinHandle<()>>,
) {
    stop_flag.store(true, Ordering::SeqCst);
    let (lock, cvar) = &**timer_state;
    if let Ok(mut done) = lock.lock() {
        *done = true;
        cvar.notify_all();
    }
    if let Some(h) = handle {
        let _ = h.join();
    }
}

fn read_sse_stream<R: BufRead>(
    mut reader: R,
    start: Instant,
    timeouts: ClientTimeouts,
    first_token_seen: &Arc<AtomicBool>,
    timer_state: &Arc<(Mutex<bool>, Condvar)>,
    on_delta: &mut Option<DeltaCallback<'_>>,
) -> Result<ChatResponse, AiError> {
    let mut filter = ThinkFilter::new();
    let mut line = String::new();
    let mut first_token_ms: Option<u128> = None;
    let mut truncated_by_length = false;

    loop {
        line.clear();
        let bytes = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(err) => {
                let first_seen = first_token_seen.load(Ordering::SeqCst);
                let ureq_err: ureq::Error = err.into();
                return Err(match ureq_err {
                    ureq::Error::Timeout(timeout) => match timeout {
                        Timeout::Global | Timeout::PerCall | Timeout::RecvBody => {
                            AiError::TotalTimeout
                        }
                        Timeout::RecvResponse | Timeout::PerRead => {
                            if first_seen || start.elapsed() >= timeouts.total {
                                AiError::TotalTimeout
                            } else {
                                AiError::FirstTokenTimeout
                            }
                        }
                        _ => AiError::TotalTimeout,
                    },
                    ureq::Error::Io(io_err)
                        if io_err.kind() == std::io::ErrorKind::TimedOut
                            || io_err.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        if first_seen || start.elapsed() >= timeouts.total {
                            AiError::TotalTimeout
                        } else {
                            AiError::FirstTokenTimeout
                        }
                    }
                    _ => {
                        if !first_seen && start.elapsed() >= timeouts.first_token {
                            AiError::FirstTokenTimeout
                        } else if start.elapsed() >= timeouts.total {
                            AiError::TotalTimeout
                        } else {
                            AiError::Protocol
                        }
                    }
                });
            }
        };

        if bytes == 0 {
            if !first_token_seen.load(Ordering::SeqCst) && start.elapsed() >= timeouts.first_token {
                return Err(AiError::FirstTokenTimeout);
            }
            if start.elapsed() >= timeouts.total {
                return Err(AiError::TotalTimeout);
            }
            break;
        }

        if start.elapsed() > timeouts.total {
            return Err(AiError::TotalTimeout);
        }
        if !first_token_seen.load(Ordering::SeqCst) && start.elapsed() > timeouts.first_token {
            return Err(AiError::FirstTokenTimeout);
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() || trimmed.starts_with(':') {
            continue;
        }

        if trimmed.starts_with("event:")
            || trimmed.starts_with("id:")
            || trimmed.starts_with("retry:")
        {
            continue;
        }

        let Some(data) = trimmed.strip_prefix("data:") else {
            return Err(AiError::Protocol);
        };
        let payload = data.trim();
        if payload == "[DONE]" {
            break;
        }

        let parsed: SseChunk = serde_json::from_str(payload).map_err(|_| AiError::Protocol)?;
        let choices = parsed.choices.ok_or(AiError::Protocol)?;
        let Some(choice) = choices.into_iter().next() else {
            continue;
        };

        if choice.finish_reason.as_deref() == Some("length") {
            truncated_by_length = true;
        }

        if let Some(delta) = choice.delta {
            if let Some(content) = delta.content {
                if !content.is_empty() {
                    if !first_token_seen.swap(true, Ordering::SeqCst) {
                        first_token_ms = Some(start.elapsed().as_millis());
                        let (lock, cvar) = &**timer_state;
                        if let Ok(mut done) = lock.lock() {
                            *done = true;
                            cvar.notify_all();
                        }
                    }

                    let out = filter.push(&content);
                    if (!out.text.is_empty() || out.reset_prior) && on_delta.is_some() {
                        if let Some(cb) = on_delta.as_mut() {
                            cb(&out);
                        }
                    }
                }
            }
        }
    }

    let final_tail = filter.clone().finish_delta()?;
    if !final_tail.is_empty() {
        if let Some(cb) = on_delta.as_mut() {
            cb(&ThinkDelta {
                text: final_tail,
                reset_prior: false,
            });
        }
    }

    let cleaned = filter.finish()?;
    if cleaned.trim().is_empty() {
        return Err(AiError::Empty);
    }

    let total_ms = start.elapsed().as_millis();
    Ok(ChatResponse {
        text: cleaned,
        truncated_by_length,
        first_token_ms: first_token_ms.unwrap_or(total_ms),
        total_ms,
    })
}

fn map_ureq_error(err: ureq::Error, received_first_token: bool, tried: Vec<String>) -> AiError {
    match err {
        ureq::Error::Timeout(timeout) => match timeout {
            Timeout::Connect | Timeout::Resolve => AiError::ConnectTimeout,
            Timeout::RecvResponse => AiError::FirstTokenTimeout,
            Timeout::PerRead => {
                if received_first_token {
                    AiError::TotalTimeout
                } else {
                    AiError::FirstTokenTimeout
                }
            }
            Timeout::Global
            | Timeout::PerCall
            | Timeout::RecvBody
            | Timeout::SendRequest
            | Timeout::SendBody => AiError::TotalTimeout,
            _ => AiError::TotalTimeout,
        },
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => AiError::Redirect,
        ureq::Error::HostNotFound => AiError::NotLocal,
        ureq::Error::Io(io_err) => match io_err.kind() {
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                AiError::ConnectTimeout
            }
            _ => AiError::NoServer { tried },
        },
        ureq::Error::ConnectionFailed => AiError::NoServer { tried },
        ureq::Error::Protocol(_)
        | ureq::Error::BodyExceedsLimit(_)
        | ureq::Error::LargeResponseHeader(_, _) => AiError::Protocol,
        _ => AiError::Protocol,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn validates_and_pins_loopback_urls_and_rejects_non_loopback_dns() {
        let ep = ValidatedEndpoint::parse_and_resolve("http://127.0.0.1:1234").unwrap();
        assert_eq!(ep.base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(ep.resolved_addrs, vec!["127.0.0.1:1234".parse().unwrap()]);

        let ep6 = ValidatedEndpoint::parse_and_resolve("http://[::1]:8080/v1/").unwrap();
        assert_eq!(ep6.base_url, "http://[::1]:8080/v1");
        assert_eq!(ep6.resolved_addrs, vec!["[::1]:8080".parse().unwrap()]);

        // Inject localhost resolving to a non-loopback IP -> must fail with NOT_LOCAL
        let bad =
            ValidatedEndpoint::parse_and_resolve_with("http://localhost:1234/v1", |_, port| {
                Ok(vec![
                    SocketAddr::from(([127, 0, 0, 1], port)),
                    SocketAddr::from(([10, 0, 0, 5], port)),
                ])
            });
        assert_eq!(bad, Err(AiError::NotLocal));

        // Reject https and remote host
        assert_eq!(
            ValidatedEndpoint::parse_and_resolve("https://127.0.0.1:1234/v1"),
            Err(AiError::NotLocal)
        );
        assert_eq!(
            ValidatedEndpoint::parse_and_resolve("http://example.com:1234/v1"),
            Err(AiError::NotLocal)
        );
    }

    #[test]
    fn redirects_return_redirect_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://example.com/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });

        let ep = ValidatedEndpoint::parse_and_resolve(&format!("http://{addr}/v1")).unwrap();
        let err = list_models(&ep, Duration::from_secs(1), Duration::from_secs(1)).unwrap_err();
        assert_eq!(err, AiError::Redirect);
        handle.join().unwrap();
    }

    #[test]
    fn sends_ttl_only_for_lmstudio_and_omits_authorization() {
        for (server, expect_ttl) in [
            (AiServer::Lmstudio, true),
            (AiServer::Ollama, false),
            (AiServer::Llamacpp, false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let sse = include_str!("../tests/fixtures/ai.chat.stream.sintetico.sse.txt");
            let handle = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut req_buf = Vec::new();
                let mut tmp = [0u8; 2048];
                loop {
                    let n = stream.read(&mut tmp).unwrap();
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
                let raw = String::from_utf8_lossy(&req_buf).into_owned();
                tx.send(raw).unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    sse.len(),
                    sse
                );
                let _ = stream.write_all(resp.as_bytes());
            });

            let ep = ValidatedEndpoint::parse_and_resolve(&format!("http://{addr}/v1")).unwrap();
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
            let mut hooks = StreamHooks::default();
            let res = chat_completions(
                &ep,
                server,
                &req,
                &[],
                ClientTimeouts::default(),
                &mut hooks,
            )
            .unwrap();
            assert!(res.text.contains("find . -type f -size +100M"));

            let raw_req = rx.recv().unwrap();
            assert!(!raw_req.to_ascii_lowercase().contains("authorization:"));
            let (_, body) = raw_req.split_once("\r\n\r\n").unwrap();
            let json: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(json.get("ttl").is_some(), expect_ttl);
            if expect_ttl {
                assert_eq!(json["ttl"], 600);
            }
            handle.join().unwrap();
        }
    }
}
