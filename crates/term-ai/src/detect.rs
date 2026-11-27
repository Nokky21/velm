use term_config::{AiConfig, AiServer};

use crate::client::{self, ClientTimeouts, ValidatedEndpoint};
use crate::errors::AiError;

/// Candidate port probed during local server auto-detection (CONTRATO §C.2).
#[derive(Debug, Clone)]
pub struct ProbeCandidate {
    pub url: String,
    pub default_server: AiServer,
}

/// Default local server candidates in CONTRATO §C.2 order.
pub fn default_candidates() -> Vec<ProbeCandidate> {
    vec![
        ProbeCandidate {
            url: String::from("http://127.0.0.1:1234/v1"),
            default_server: AiServer::Lmstudio,
        },
        ProbeCandidate {
            url: String::from("http://127.0.0.1:11434/v1"),
            default_server: AiServer::Ollama,
        },
        ProbeCandidate {
            url: String::from("http://127.0.0.1:8080/v1"),
            default_server: AiServer::Llamacpp,
        },
    ]
}

/// Result of probing a single candidate port (used by `term-ai doctor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    pub url: String,
    pub server: AiServer,
    pub models: Vec<String>,
    pub error: Option<AiError>,
}

/// Fully resolved endpoint, server flavor, selected model, and available models.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    pub endpoint: ValidatedEndpoint,
    pub server: AiServer,
    pub model: String,
    pub available_models: Vec<String>,
}

/// Probes `candidates` in order with a 300 ms timeout per port (CONTRATO §C.2).
///
/// Any server returning HTTP 200 with an empty model list (`[]`) is skipped while
/// continuing to probe remaining candidates. If all reachable servers report empty
/// models, returns [`AiError::NoModels`]. If none respond, returns [`AiError::NoServer`].
pub fn detect_server_on(
    candidates: &[ProbeCandidate],
    configured_server: AiServer,
    timeouts: ClientTimeouts,
) -> Result<(ValidatedEndpoint, AiServer, Vec<String>), AiError> {
    let mut tried = Vec::with_capacity(candidates.len());
    let mut saw_empty_models = false;

    for candidate in candidates {
        let ep = ValidatedEndpoint::parse_and_resolve(&candidate.url)?;
        tried.push(host_port_label(&ep.base_url));
        match client::list_models(&ep, timeouts.detect_connect, timeouts.detect_connect) {
            Ok(models) if !models.is_empty() => {
                let server = if configured_server != AiServer::Auto {
                    configured_server
                } else {
                    candidate.default_server
                };
                return Ok((ep, server, models));
            }
            Ok(_) => {
                saw_empty_models = true;
            }
            Err(AiError::Redirect) => return Err(AiError::Redirect),
            Err(_) => {}
        }
    }

    if saw_empty_models {
        Err(AiError::NoModels)
    } else {
        Err(AiError::NoServer { tried })
    }
}

/// Resolves the target endpoint, server flavor, and model from `[term.ai]` config.
pub fn resolve_endpoint(
    cfg: &AiConfig,
    timeouts: ClientTimeouts,
) -> Result<ResolvedEndpoint, AiError> {
    resolve_endpoint_with_candidates(cfg, &default_candidates(), timeouts)
}

/// Resolves endpoint using custom auto-detection candidates (used in tests).
pub fn resolve_endpoint_with_candidates(
    cfg: &AiConfig,
    candidates: &[ProbeCandidate],
    timeouts: ClientTimeouts,
) -> Result<ResolvedEndpoint, AiError> {
    if cfg.url.trim().is_empty() {
        let (endpoint, server, models) = detect_server_on(candidates, cfg.server, timeouts)?;
        let model = select_model(&cfg.model, &models)?;
        return Ok(ResolvedEndpoint {
            endpoint,
            server,
            model,
            available_models: models,
        });
    }

    let endpoint = ValidatedEndpoint::parse_and_resolve(&cfg.url)?;
    let server = if cfg.server != AiServer::Auto {
        cfg.server
    } else {
        infer_server_from_url(&endpoint.base_url)
    };

    match client::list_models(&endpoint, timeouts.connect, timeouts.connect) {
        Ok(models) => {
            if models.is_empty() {
                return Err(AiError::NoModels);
            }
            let model = select_model(&cfg.model, &models)?;
            Ok(ResolvedEndpoint {
                endpoint,
                server,
                model,
                available_models: models,
            })
        }
        Err(err) => {
            // If an explicit model is configured and `/models` is unimplemented (404),
            // allow proceeding to `/chat/completions` so server-side 404 handling runs.
            if !cfg.model.trim().is_empty()
                && matches!(
                    err,
                    AiError::BadRequest { .. } | AiError::ModelNotFound { .. }
                )
            {
                return Ok(ResolvedEndpoint {
                    endpoint,
                    server,
                    model: cfg.model.clone(),
                    available_models: Vec::new(),
                });
            }
            Err(err)
        }
    }
}

/// Probes all candidates for `term-ai doctor` and returns per-port reports.
pub fn probe_all(
    candidates: &[ProbeCandidate],
    configured_server: AiServer,
    timeouts: ClientTimeouts,
) -> Vec<ProbeReport> {
    candidates
        .iter()
        .map(|candidate| {
            let server = if configured_server != AiServer::Auto {
                configured_server
            } else {
                candidate.default_server
            };
            match ValidatedEndpoint::parse_and_resolve(&candidate.url) {
                Ok(ep) => {
                    match client::list_models(&ep, timeouts.detect_connect, timeouts.detect_connect)
                    {
                        Ok(models) => {
                            let error = if models.is_empty() {
                                Some(AiError::NoModels)
                            } else {
                                None
                            };
                            ProbeReport {
                                url: ep.base_url,
                                server,
                                models,
                                error,
                            }
                        }
                        Err(err) => ProbeReport {
                            url: ep.base_url,
                            server,
                            models: Vec::new(),
                            error: Some(err),
                        },
                    }
                }
                Err(err) => ProbeReport {
                    url: candidate.url.clone(),
                    server,
                    models: Vec::new(),
                    error: Some(err),
                },
            }
        })
        .collect()
}

fn select_model(configured_model: &str, available: &[String]) -> Result<String, AiError> {
    let trimmed = configured_model.trim();
    if trimmed.is_empty() {
        return available.first().cloned().ok_or(AiError::NoModels);
    }

    if available.iter().any(|m| m == trimmed) {
        Ok(trimmed.to_owned())
    } else {
        Err(AiError::ModelNotFound {
            model: trimmed.to_owned(),
            available: available.to_vec(),
        })
    }
}

fn infer_server_from_url(base_url: &str) -> AiServer {
    if base_url.contains(":1234/") || base_url.ends_with(":1234") {
        AiServer::Lmstudio
    } else if base_url.contains(":11434/") || base_url.ends_with(":11434") {
        AiServer::Ollama
    } else if base_url.contains(":8080/") || base_url.ends_with(":8080") {
        AiServer::Llamacpp
    } else {
        AiServer::OpenaiCompatible
    }
}

fn host_port_label(base_url: &str) -> String {
    let after_scheme = base_url.strip_prefix("http://").unwrap_or(base_url);
    after_scheme
        .split('/')
        .next()
        .unwrap_or(after_scheme)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn spawn_models_server(body: &'static str) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
        });
        (format!("http://{addr}/v1"), handle)
    }

    #[test]
    fn skips_port_with_empty_models_and_selects_next_server() {
        let empty_body = include_str!("../tests/fixtures/ai.models.ollama-empty.json");
        let one_body = include_str!("../tests/fixtures/ai.models.one-model.sintetico.json");

        // Port 1: closed (bind & drop)
        let closed_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let closed_addr = closed_listener.local_addr().unwrap();
        drop(closed_listener);

        // Port 2: returns 200 with empty model list
        let (empty_url, h_empty) = spawn_models_server(empty_body);
        // Port 3: returns 200 with one model
        let (one_url, h_one) = spawn_models_server(one_body);

        let candidates = vec![
            ProbeCandidate {
                url: format!("http://{closed_addr}/v1"),
                default_server: AiServer::Lmstudio,
            },
            ProbeCandidate {
                url: empty_url,
                default_server: AiServer::Ollama,
            },
            ProbeCandidate {
                url: one_url.clone(),
                default_server: AiServer::Llamacpp,
            },
        ];

        let (ep, server, models) =
            detect_server_on(&candidates, AiServer::Auto, ClientTimeouts::default()).unwrap();
        assert_eq!(ep.base_url, one_url);
        assert_eq!(server, AiServer::Llamacpp);
        assert_eq!(
            models,
            vec![String::from("qwen3.8-9b-distill-uncensored-heretic")]
        );

        h_empty.join().unwrap();
        h_one.join().unwrap();
    }

    #[test]
    fn returns_no_models_when_all_reachable_servers_are_empty() {
        let empty_body = include_str!("../tests/fixtures/ai.models.ollama-empty.json");
        let (empty_url, h_empty) = spawn_models_server(empty_body);

        let candidates = vec![ProbeCandidate {
            url: empty_url,
            default_server: AiServer::Ollama,
        }];
        let err =
            detect_server_on(&candidates, AiServer::Auto, ClientTimeouts::default()).unwrap_err();
        assert_eq!(err, AiError::NoModels);
        h_empty.join().unwrap();
    }

    #[test]
    fn returns_no_server_listing_all_tried_ports() {
        let l1 = TcpListener::bind("127.0.0.1:0").unwrap();
        let a1 = l1.local_addr().unwrap();
        drop(l1);
        let l2 = TcpListener::bind("127.0.0.1:0").unwrap();
        let a2 = l2.local_addr().unwrap();
        drop(l2);

        let candidates = vec![
            ProbeCandidate {
                url: format!("http://{a1}/v1"),
                default_server: AiServer::Lmstudio,
            },
            ProbeCandidate {
                url: format!("http://{a2}/v1"),
                default_server: AiServer::Ollama,
            },
        ];
        let err =
            detect_server_on(&candidates, AiServer::Auto, ClientTimeouts::default()).unwrap_err();
        assert_eq!(
            err,
            AiError::NoServer {
                tried: vec![a1.to_string(), a2.to_string()],
            }
        );
    }
}
