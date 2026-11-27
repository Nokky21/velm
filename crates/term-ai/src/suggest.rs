use std::io::{self, Write};

use term_config::AiConfig;

use crate::client::{self, ChatMessage, ChatRequest, ClientTimeouts, StreamHooks};
use crate::danger;
use crate::detect::{self, ResolvedEndpoint};
use crate::errors::AiError;

/// Outcome of `term-ai suggest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestOutcome {
    /// Safe command ready to place in the shell buffer (exit 0).
    Command(String),
    /// Dangerous command commented with `# ` on every line (exit 4).
    Dangerous(String),
    /// Model explicitly returned `NO_COMMAND` or prose without a single-line/fenced command (exit 3).
    NoCommand,
}

impl SuggestOutcome {
    /// Returns the process exit code defined in CONTRATO §B.1 and §H.
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Command(_) => 0,
            Self::NoCommand => 3,
            Self::Dangerous(_) => 4,
        }
    }
}

/// Optional shell context passed to `term-ai suggest`.
#[derive(Debug, Clone, Default)]
pub struct SuggestContext<'a> {
    pub cwd: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub macos_version: Option<&'a str>,
}

/// Builds the fixed English system prompt from CONTRATO §B.1.
pub fn system_prompt(macos_version: &str) -> String {
    format!(
        "You are a shell assistant on macOS {macos_version} using zsh. Reply with exactly one command inside a single ```zsh code block and nothing else. If the request cannot be done with a shell command, reply exactly NO_COMMAND."
    )
}

/// Detects the macOS version via `sw_vers -productVersion` when available.
pub fn detect_macos_version() -> String {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
        {
            if output.status.success() {
                let v = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if !v.is_empty() {
                    return v;
                }
            }
        }
    }
    String::from("15")
}

/// Builds the user message containing only the allowed context fields and prompt.
pub fn build_user_message(prompt: &str, ctx: &SuggestContext<'_>) -> String {
    let mut parts = Vec::new();
    if let Some(cwd) = ctx.cwd.filter(|s| !s.is_empty()) {
        parts.push(format!("cwd: {cwd}"));
    }
    if let Some(branch) = ctx.branch.filter(|s| !s.is_empty()) {
        parts.push(format!("branch: {branch}"));
    }
    if parts.is_empty() {
        prompt.to_owned()
    } else {
        format!("{}\n\n{prompt}", parts.join("\n"))
    }
}

/// Extracts a shell command from cleaned model output following CONTRATO §B.1.
pub fn extract_command(cleaned: &str) -> Result<Option<String>, AiError> {
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Err(AiError::Empty);
    }

    if trimmed.contains("NO_COMMAND") {
        return Ok(None);
    }

    let raw_cmd = if let Some(block) = extract_first_code_block(trimmed) {
        block
    } else if !trimmed.contains('\n') {
        trimmed.to_owned()
    } else {
        return Ok(None);
    };

    let stripped = strip_prompt_prefix(&raw_cmd);
    let final_cmd = stripped.trim().to_owned();
    if final_cmd.is_empty() {
        return Ok(None);
    }

    Ok(Some(final_cmd))
}

fn extract_first_code_block(text: &str) -> Option<String> {
    let open_idx = text.find("```")?;
    let after_ticks = &text[open_idx + 3..];
    let nl_pos = after_ticks.find('\n')?;
    let body_start = open_idx + 3 + nl_pos + 1;
    let rest = &text[body_start..];
    let close_rel = find_closing_fence(rest)?;
    let block = rest[..close_rel].trim_end_matches('\r');
    Some(block.to_owned())
}

fn find_closing_fence(rest: &str) -> Option<usize> {
    if rest.starts_with("```") {
        return Some(0);
    }
    if let Some(pos) = rest.find("\n```") {
        return Some(pos + 1);
    }
    rest.find("```")
}

fn strip_prompt_prefix(cmd: &str) -> String {
    cmd.lines()
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            line.strip_prefix("$ ").unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Evaluates extracted command text for danger and returns the final [`SuggestOutcome`].
pub fn classify_cleaned_response(cleaned: &str) -> Result<SuggestOutcome, AiError> {
    let Some(cmd) = extract_command(cleaned)? else {
        return Ok(SuggestOutcome::NoCommand);
    };

    if danger::is_dangerous(&cmd) {
        Ok(SuggestOutcome::Dangerous(danger::comment_command(&cmd)))
    } else {
        Ok(SuggestOutcome::Command(cmd))
    }
}

/// Executes the full `suggest` request against an already-resolved endpoint.
pub fn run_suggest_with_endpoint(
    cfg: &AiConfig,
    endpoint: &ResolvedEndpoint,
    prompt: &str,
    ctx: &SuggestContext<'_>,
    timeouts: ClientTimeouts,
    emit_stderr_progress: bool,
) -> Result<SuggestOutcome, AiError> {
    if prompt.trim().is_empty() {
        return Err(AiError::Usage(String::from(
            "falta el texto de la petición",
        )));
    }

    if emit_stderr_progress {
        let _ = writeln!(io::stderr(), "pensando…");
    }

    let version = ctx
        .macos_version
        .map(str::to_owned)
        .unwrap_or_else(detect_macos_version);
    let system = system_prompt(&version);
    let user = build_user_message(prompt, ctx);

    let req = ChatRequest {
        model: endpoint.model.clone(),
        messages: vec![
            ChatMessage {
                role: String::from("system"),
                content: system,
            },
            ChatMessage {
                role: String::from("user"),
                content: user,
            },
        ],
        temperature: cfg.temperature,
        max_tokens: cfg.max_tokens_suggest,
        lmstudio_ttl_seconds: cfg.lmstudio_ttl_seconds,
    };

    let mut hooks = StreamHooks {
        on_loading_model: if emit_stderr_progress {
            Some(Box::new(|| {
                let _ = writeln!(io::stderr(), "cargando modelo…");
            }))
        } else {
            None
        },
        on_delta: None,
    };

    let response = client::chat_completions(
        &endpoint.endpoint,
        endpoint.server,
        &req,
        &endpoint.available_models,
        timeouts,
        &mut hooks,
    )?;

    classify_cleaned_response(&response.text)
}

/// Resolves the configured/auto-detected AI server and executes `suggest`.
pub fn run_suggest(
    cfg: &AiConfig,
    prompt: &str,
    ctx: &SuggestContext<'_>,
) -> Result<SuggestOutcome, AiError> {
    if prompt.trim().is_empty() {
        return Err(AiError::Usage(String::from(
            "falta el texto de la petición",
        )));
    }
    let endpoint = detect::resolve_endpoint(cfg, ClientTimeouts::default())?;
    run_suggest_with_endpoint(cfg, &endpoint, prompt, ctx, ClientTimeouts::default(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_order_matches_contract() {
        // 1. Fenced zsh block
        let resp = "Here is the command:\n```zsh\nfind . -type f -size +100M\n```\nAnd another:\n```zsh\nls\n```";
        assert_eq!(
            extract_command(resp).unwrap(),
            Some(String::from("find . -type f -size +100M"))
        );

        // 2. Fenced block without language
        let no_lang = "```\n$ git status\n$ git diff\n```";
        assert_eq!(
            extract_command(no_lang).unwrap(),
            Some(String::from("git status\ngit diff"))
        );

        // 3. Single line without code block
        assert_eq!(
            extract_command("  $ ls -la  ").unwrap(),
            Some(String::from("ls -la"))
        );

        // 4. Multi-line prose without code block -> NoCommand (exit 3)
        assert_eq!(
            extract_command("You can list files.\nTry using ls.").unwrap(),
            None
        );

        // 5. NO_COMMAND -> exit 3
        assert_eq!(extract_command("NO_COMMAND").unwrap(), None);
        assert_eq!(extract_command("```zsh\nNO_COMMAND\n```").unwrap(), None);

        // 6. Empty after cleaning -> EMPTY (exit 1)
        assert_eq!(extract_command("   \n  "), Err(AiError::Empty));
    }

    #[test]
    fn classifies_safe_and_dangerous_suggestions() {
        let safe = classify_cleaned_response("```zsh\nls -la\n```").unwrap();
        assert_eq!(safe, SuggestOutcome::Command(String::from("ls -la")));
        assert_eq!(safe.exit_code(), 0);

        let danger = classify_cleaned_response("```zsh\nrm -rf /tmp/foo\n```").unwrap();
        assert_eq!(
            danger,
            SuggestOutcome::Dangerous(String::from("# rm -rf /tmp/foo"))
        );
        assert_eq!(danger.exit_code(), 4);

        let none = classify_cleaned_response("NO_COMMAND").unwrap();
        assert_eq!(none, SuggestOutcome::NoCommand);
        assert_eq!(none.exit_code(), 3);
    }

    #[test]
    fn builds_system_and_user_prompts_with_minimal_context() {
        assert_eq!(
            system_prompt("26.6.2"),
            "You are a shell assistant on macOS 26.6.2 using zsh. Reply with exactly one command inside a single ```zsh code block and nothing else. If the request cannot be done with a shell command, reply exactly NO_COMMAND."
        );

        let ctx = SuggestContext {
            cwd: Some("/tmp/repo"),
            branch: Some("main"),
            macos_version: Some("26.6.2"),
        };
        assert_eq!(
            build_user_message("list files", &ctx),
            "cwd: /tmp/repo\nbranch: main\n\nlist files"
        );

        let empty_ctx = SuggestContext::default();
        assert_eq!(build_user_message("list files", &empty_ctx), "list files");
    }
}
