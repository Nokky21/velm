use std::fmt;

use serde::{Deserialize, Serialize};

pub(crate) const KNOWN_KEYS: &[&str] = &[
    "enabled",
    "server",
    "url",
    "model",
    "temperature",
    "max_tokens_suggest",
    "max_tokens_explain",
    "lmstudio_ttl_seconds",
    "zsh_key",
    "explain_head_lines",
    "explain_tail_lines",
    "explain_max_chars",
];

/// Local AI server flavor configured in `[term.ai]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiServer {
    #[default]
    Auto,
    Lmstudio,
    Ollama,
    Llamacpp,
    OpenaiCompatible,
}

impl AiServer {
    /// Returns the canonical TOML string representation of this server kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Lmstudio => "lmstudio",
            Self::Ollama => "ollama",
            Self::Llamacpp => "llamacpp",
            Self::OpenaiCompatible => "openai-compatible",
        }
    }
}

impl fmt::Display for AiServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Configuration for the local AI assistant (`[term.ai]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub enabled: bool,
    pub server: AiServer,
    pub url: String,
    pub model: String,
    pub temperature: f64,
    pub max_tokens_suggest: u32,
    pub max_tokens_explain: u32,
    pub lmstudio_ttl_seconds: u32,
    pub zsh_key: String,
    pub explain_head_lines: usize,
    pub explain_tail_lines: usize,
    pub explain_max_chars: usize,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            server: AiServer::Auto,
            url: String::new(),
            model: String::new(),
            temperature: 0.2,
            max_tokens_suggest: 512,
            max_tokens_explain: 2048,
            lmstudio_ttl_seconds: 600,
            zsh_key: String::from("^G"),
            explain_head_lines: 50,
            explain_tail_lines: 150,
            explain_max_chars: 12000,
        }
    }
}
