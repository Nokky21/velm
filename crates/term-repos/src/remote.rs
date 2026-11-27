use std::path::Path;
use std::time::Duration;

use crate::git::{run_git_with_timeout, GitExecOutcome};
use crate::parse::truncate_chars;

/// Result of querying a repository's remote default branch via `git ls-remote --symref origin HEAD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteResult {
    DefaultBranch(String),
    NoDefaultBranch,
    CredentialsRequired,
    TimedOut,
    Error(String),
}

impl RemoteResult {
    /// Formats the remote query outcome in Spanish for the UI detail panel.
    pub fn display_text(&self) -> String {
        match self {
            Self::DefaultBranch(branch) => format!("rama por defecto real: {branch}"),
            Self::NoDefaultBranch => String::from("sin rama por defecto en origin"),
            Self::CredentialsRequired => String::from("el remoto pide credenciales"),
            Self::TimedOut => String::from("tiempo agotado consultando el remoto"),
            Self::Error(msg) => format!("error remoto: {msg}"),
        }
    }
}

/// Queries `origin HEAD` via `git ls-remote --symref origin HEAD` using `remote_timeout_ms`.
///
/// Uses `GIT_TERMINAL_PROMPT=0` so credential prompts fail immediately rather than hanging.
pub fn query_remote_default_branch(repo_path: &Path, remote_timeout_ms: u64) -> RemoteResult {
    let timeout = Duration::from_millis(remote_timeout_ms);
    match run_git_with_timeout(
        repo_path,
        &["ls-remote", "--symref", "origin", "HEAD"],
        timeout,
    ) {
        GitExecOutcome::Ok(stdout) => match parse_ls_remote_symref(&stdout) {
            Some(branch) => RemoteResult::DefaultBranch(branch),
            None => RemoteResult::NoDefaultBranch,
        },
        GitExecOutcome::TimedOut => RemoteResult::TimedOut,
        GitExecOutcome::SpawnError(err) => RemoteResult::Error(truncate_chars(&err, 200)),
        GitExecOutcome::Failed { stderr, stdout } => {
            if is_credential_prompt(&stderr) || is_credential_prompt(&stdout) {
                return RemoteResult::CredentialsRequired;
            }
            let raw = if stderr.trim().is_empty() {
                &stdout
            } else {
                &stderr
            };
            let msg = if raw.trim().is_empty() {
                String::from("git ls-remote falló")
            } else {
                truncate_chars(raw, 200)
            };
            RemoteResult::Error(msg)
        }
    }
}

/// Extracts the default branch name from `git ls-remote --symref origin HEAD` output.
pub fn parse_ls_remote_symref(output: &str) -> Option<String> {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        let Some(rest) = line.strip_prefix("ref:") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let ref_name = parts.next()?;
        let target = parts.next()?;
        if target == "HEAD" {
            let branch = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
            if !branch.is_empty() {
                return Some(branch.to_owned());
            }
        }
    }
    None
}

/// Detects git failure messages caused by disabled terminal credential prompts.
pub fn is_credential_prompt(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    const PATTERNS: &[&str] = &[
        "terminal prompts disabled",
        "could not read username",
        "could not read password",
        "authentication failed",
        "invalid username or password",
        "authorization failed",
        "askpass",
        "credentials required",
        "prompt disabled",
    ];
    PATTERNS.iter().any(|pattern| lower.contains(pattern))
}
