use std::error::Error;
use std::fmt;

use serde::Deserialize;

/// Internal error codes defined in CONTRATO §C.4 plus CLI/config error categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    NoServer,
    ConnectTimeout,
    ModelNotFound,
    NoModels,
    BadRequest,
    ServerError,
    FirstTokenTimeout,
    TotalTimeout,
    Thinking,
    Empty,
    Protocol,
    Redirect,
    NotLocal,
    Cancelled,
    Config,
    Usage,
    Io,
}

impl ErrorCode {
    /// Returns the canonical identifier from CONTRATO §C.4.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoServer => "NO_SERVER",
            Self::ConnectTimeout => "CONNECT_TIMEOUT",
            Self::ModelNotFound => "MODEL_NOT_FOUND",
            Self::NoModels => "NO_MODELS",
            Self::BadRequest => "BAD_REQUEST",
            Self::ServerError => "SERVER_ERROR",
            Self::FirstTokenTimeout => "FIRST_TOKEN_TIMEOUT",
            Self::TotalTimeout => "TOTAL_TIMEOUT",
            Self::Thinking => "THINKING",
            Self::Empty => "EMPTY",
            Self::Protocol => "PROTOCOL",
            Self::Redirect => "REDIRECT",
            Self::NotLocal => "NOT_LOCAL",
            Self::Cancelled => "CANCELLED",
            Self::Config => "CONFIG",
            Self::Usage => "USAGE",
            Self::Io => "IO",
        }
    }

    /// Returns the process exit code defined in CONTRATO §C.4 and §H.
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::NotLocal | Self::Config | Self::Usage => 2,
            Self::Cancelled => 130,
            _ => 1,
        }
    }
}

/// Errors produced by `term-ai` with Spanish user-facing messages and exit codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiError {
    NoServer {
        tried: Vec<String>,
    },
    ConnectTimeout,
    ModelNotFound {
        model: String,
        available: Vec<String>,
    },
    NoModels,
    BadRequest {
        message: String,
    },
    ServerError {
        status: u16,
        message: Option<String>,
    },
    FirstTokenTimeout,
    TotalTimeout,
    Thinking,
    Empty,
    Protocol,
    Redirect,
    NotLocal,
    Cancelled,
    Config(String),
    Usage(String),
    Io(String),
}

impl AiError {
    /// Returns the internal error code for this error.
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::NoServer { .. } => ErrorCode::NoServer,
            Self::ConnectTimeout => ErrorCode::ConnectTimeout,
            Self::ModelNotFound { .. } => ErrorCode::ModelNotFound,
            Self::NoModels => ErrorCode::NoModels,
            Self::BadRequest { .. } => ErrorCode::BadRequest,
            Self::ServerError { .. } => ErrorCode::ServerError,
            Self::FirstTokenTimeout => ErrorCode::FirstTokenTimeout,
            Self::TotalTimeout => ErrorCode::TotalTimeout,
            Self::Thinking => ErrorCode::Thinking,
            Self::Empty => ErrorCode::Empty,
            Self::Protocol => ErrorCode::Protocol,
            Self::Redirect => ErrorCode::Redirect,
            Self::NotLocal => ErrorCode::NotLocal,
            Self::Cancelled => ErrorCode::Cancelled,
            Self::Config(_) => ErrorCode::Config,
            Self::Usage(_) => ErrorCode::Usage,
            Self::Io(_) => ErrorCode::Io,
        }
    }

    /// Returns the exit code associated with this error per CONTRATO §C.4 and §H.
    pub const fn exit_code(&self) -> i32 {
        self.code().exit_code()
    }
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoServer { tried } => {
                if tried.is_empty() {
                    write!(f, "no hay servidor de IA local")
                } else {
                    write!(
                        f,
                        "no hay servidor de IA local (probé {})",
                        tried.join(", ")
                    )
                }
            }
            Self::ConnectTimeout => write!(f, "el servidor de IA no responde"),
            Self::ModelNotFound { model, available } => {
                if available.is_empty() {
                    write!(
                        f,
                        "el servidor no tiene el modelo {model}; disponibles: (ninguno)"
                    )
                } else {
                    write!(
                        f,
                        "el servidor no tiene el modelo {model}; disponibles: {}",
                        available.join(", ")
                    )
                }
            }
            Self::NoModels => write!(f, "el servidor no tiene modelos"),
            Self::BadRequest { message } => f.write_str(message),
            Self::ServerError { status, message } => match message {
                Some(msg) if !msg.trim().is_empty() => f.write_str(msg),
                _ => write!(f, "error del servidor {status}"),
            },
            Self::FirstTokenTimeout => write!(f, "el modelo no ha respondido en 90 s"),
            Self::TotalTimeout => write!(f, "la respuesta tardó demasiado"),
            Self::Thinking => {
                write!(f, "el modelo se quedó pensando; sube max_tokens_suggest")
            }
            Self::Empty => write!(f, "respuesta vacía"),
            Self::Protocol => write!(f, "respuesta del servidor no válida"),
            Self::Redirect => write!(f, "el servidor intentó redirigir; no se sigue"),
            Self::NotLocal => write!(f, "solo se permiten servidores de esta máquina"),
            Self::Cancelled => Ok(()),
            Self::Config(msg) | Self::Usage(msg) | Self::Io(msg) => f.write_str(msg),
        }
    }
}

impl Error for AiError {}

#[derive(Debug, Deserialize)]
struct OpenAiErrorEnvelope {
    error: Option<OpenAiErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct OpenAiErrorDetail {
    message: Option<String>,
    #[serde(rename = "type")]
    error_type: Option<String>,
}

/// Classifies a non-200 HTTP response from an OpenAI-compatible server per CONTRATO §C.4.
pub fn classify_http_error(
    status: u16,
    body: &str,
    requested_model: Option<&str>,
    available_models: Vec<String>,
) -> AiError {
    if (300..400).contains(&status) {
        return AiError::Redirect;
    }

    let detail = parse_error_detail(body);

    if status == 404 {
        if let Some((msg, err_type)) = &detail {
            if is_model_not_found(err_type.as_deref(), msg.as_deref()) {
                let model = requested_model
                    .map(str::to_owned)
                    .or_else(|| msg.as_deref().and_then(extract_quoted_model_name))
                    .unwrap_or_default();
                return AiError::ModelNotFound {
                    model,
                    available: available_models,
                };
            }
        }
    }

    if (400..500).contains(&status) {
        return match detail {
            Some((Some(msg), _)) if !msg.is_empty() => AiError::BadRequest { message: msg },
            _ if body.trim().is_empty() => AiError::BadRequest {
                message: format!("error HTTP {status}"),
            },
            _ => AiError::Protocol,
        };
    }

    if (500..600).contains(&status) {
        let message = detail.and_then(|(msg, _)| msg).filter(|m| !m.is_empty());
        return AiError::ServerError { status, message };
    }

    AiError::Protocol
}

fn parse_error_detail(body: &str) -> Option<(Option<String>, Option<String>)> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed: OpenAiErrorEnvelope = serde_json::from_str(trimmed).ok()?;
    let detail = parsed.error?;
    Some((detail.message, detail.error_type))
}

fn is_model_not_found(error_type: Option<&str>, message: Option<&str>) -> bool {
    if error_type == Some("not_found_error") {
        return true;
    }
    if let Some(msg) = message {
        let lower = msg.to_ascii_lowercase();
        if lower.contains("model") && lower.contains("not found") {
            return true;
        }
    }
    false
}

fn extract_quoted_model_name(message: &str) -> Option<String> {
    let start = message.find('\'')?;
    let rest = &message[start + 1..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_and_messages_match_contract() {
        let cases = [
            (
                AiError::NoServer {
                    tried: vec!["127.0.0.1:1234".into(), "127.0.0.1:11434".into()],
                },
                "NO_SERVER",
                "no hay servidor de IA local (probé 127.0.0.1:1234, 127.0.0.1:11434)",
                1,
            ),
            (
                AiError::ConnectTimeout,
                "CONNECT_TIMEOUT",
                "el servidor de IA no responde",
                1,
            ),
            (
                AiError::ModelNotFound {
                    model: "nope".into(),
                    available: vec!["llama3".into(), "qwen".into()],
                },
                "MODEL_NOT_FOUND",
                "el servidor no tiene el modelo nope; disponibles: llama3, qwen",
                1,
            ),
            (
                AiError::NoModels,
                "NO_MODELS",
                "el servidor no tiene modelos",
                1,
            ),
            (
                AiError::BadRequest {
                    message: "invalid temperature".into(),
                },
                "BAD_REQUEST",
                "invalid temperature",
                1,
            ),
            (
                AiError::ServerError {
                    status: 503,
                    message: None,
                },
                "SERVER_ERROR",
                "error del servidor 503",
                1,
            ),
            (
                AiError::FirstTokenTimeout,
                "FIRST_TOKEN_TIMEOUT",
                "el modelo no ha respondido en 90 s",
                1,
            ),
            (
                AiError::TotalTimeout,
                "TOTAL_TIMEOUT",
                "la respuesta tardó demasiado",
                1,
            ),
            (
                AiError::Thinking,
                "THINKING",
                "el modelo se quedó pensando; sube max_tokens_suggest",
                1,
            ),
            (AiError::Empty, "EMPTY", "respuesta vacía", 1),
            (
                AiError::Protocol,
                "PROTOCOL",
                "respuesta del servidor no válida",
                1,
            ),
            (
                AiError::Redirect,
                "REDIRECT",
                "el servidor intentó redirigir; no se sigue",
                1,
            ),
            (
                AiError::NotLocal,
                "NOT_LOCAL",
                "solo se permiten servidores de esta máquina",
                2,
            ),
            (AiError::Cancelled, "CANCELLED", "", 130),
        ];

        for (err, code, msg, exit) in cases {
            assert_eq!(err.code().as_str(), code);
            assert_eq!(err.to_string(), msg);
            assert_eq!(err.exit_code(), exit);
        }
    }

    #[test]
    fn classifies_404_model_not_found_fixtures() {
        let fixture = include_str!("../tests/fixtures/ai.error.404.model-not-found.json");
        let err = classify_http_error(
            404,
            fixture,
            Some("qwen3.8-9b-distill-uncensored-heretic"),
            vec!["other-model".into()],
        );
        assert_eq!(
            err,
            AiError::ModelNotFound {
                model: "qwen3.8-9b-distill-uncensored-heretic".into(),
                available: vec!["other-model".into()],
            }
        );

        let stream_fixture =
            include_str!("../tests/fixtures/ai.error.404.model-not-found.stream.json");
        let err2 = classify_http_error(404, stream_fixture, None, vec![]);
        assert_eq!(
            err2,
            AiError::ModelNotFound {
                model: "nope".into(),
                available: vec![],
            }
        );
    }

    #[test]
    fn classifies_400_and_5xx_errors() {
        let body = r#"{"error":{"message":"bad param","type":"invalid_request_error"}}"#;
        assert_eq!(
            classify_http_error(400, body, None, vec![]),
            AiError::BadRequest {
                message: "bad param".into()
            }
        );

        assert_eq!(
            classify_http_error(502, "<html>bad gateway</html>", None, vec![]),
            AiError::ServerError {
                status: 502,
                message: None
            }
        );

        let body_500 = r#"{"error":{"message":"backend crashed"}}"#;
        assert_eq!(
            classify_http_error(500, body_500, None, vec![]),
            AiError::ServerError {
                status: 500,
                message: Some("backend crashed".into())
            }
        );
    }
}
