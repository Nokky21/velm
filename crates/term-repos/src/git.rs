use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::parse::{format_unix_timestamp_utc, parse_porcelain_v2, truncate_chars, RepoStatus};
use crate::scan::DiscoveredRepo;

const POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) enum GitExecOutcome {
    Ok(String),
    Failed { stderr: String, stdout: String },
    TimedOut,
    SpawnError(String),
}

/// Inspects all discovered repositories with bounded parallelism and per-repo timeout.
///
/// Results are sorted deterministically by `path`.
pub fn inspect_repos(
    repos: &[DiscoveredRepo],
    parallel: usize,
    timeout_ms: u64,
) -> Vec<RepoStatus> {
    let (tx, rx) = mpsc::channel();
    inspect_repos_streaming(repos, parallel, timeout_ms, tx);
    let mut results: Vec<RepoStatus> = rx.into_iter().collect();
    results.sort_by(|a, b| a.path.cmp(&b.path));
    results
}

/// Inspects discovered repositories in parallel and streams each [`RepoStatus`] as it finishes.
pub fn inspect_repos_streaming(
    repos: &[DiscoveredRepo],
    parallel: usize,
    timeout_ms: u64,
    tx: Sender<RepoStatus>,
) {
    if repos.is_empty() {
        return;
    }

    let worker_count = parallel.max(1).min(repos.len());
    let next_idx = AtomicUsize::new(0);
    let timeout = Duration::from_millis(timeout_ms);

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let next_idx = &next_idx;
            let tx = tx.clone();
            scope.spawn(move || loop {
                let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                if idx >= repos.len() {
                    break;
                }
                let status = inspect_single_repo(&repos[idx], timeout);
                if tx.send(status).is_err() {
                    break;
                }
            });
        }
    });
}

/// Inspects a single repository using `git status` and `git log`.
pub fn inspect_single_repo(repo: &DiscoveredRepo, timeout: Duration) -> RepoStatus {
    let started = Instant::now();
    let status_out = run_git_with_timeout(
        &repo.path,
        &["status", "--porcelain=v2", "--branch", "--show-stash"],
        timeout,
    );

    let status_stdout = match status_out {
        GitExecOutcome::Ok(out) => out,
        GitExecOutcome::TimedOut => return RepoStatus::timed_out(&repo.path, &repo.name),
        GitExecOutcome::SpawnError(err) => {
            return RepoStatus::error(&repo.path, &repo.name, repo.worktree, err);
        }
        GitExecOutcome::Failed { stderr, stdout } => {
            let msg = if stderr.trim().is_empty() {
                stdout
            } else {
                stderr
            };
            let msg = if msg.trim().is_empty() {
                String::from("git status falló sin mensaje")
            } else {
                msg
            };
            return RepoStatus::error(&repo.path, &repo.name, repo.worktree, msg);
        }
    };

    let mut parsed =
        parse_porcelain_v2(&repo.path, &repo.name, repo.worktree, &status_stdout, None);
    if parsed.empty == Some(false) {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return RepoStatus::timed_out(&repo.path, &repo.name);
        }
        match run_git_with_timeout(&repo.path, &["log", "-1", "--format=%ct"], remaining) {
            GitExecOutcome::Ok(ts_out) => {
                if let Ok(ts) = ts_out.trim().parse::<i64>() {
                    parsed.last_commit_at = Some(format_unix_timestamp_utc(ts));
                }
            }
            GitExecOutcome::TimedOut => return RepoStatus::timed_out(&repo.path, &repo.name),
            GitExecOutcome::SpawnError(err) => {
                return RepoStatus::error(&repo.path, &repo.name, repo.worktree, err);
            }
            GitExecOutcome::Failed { stderr, stdout } => {
                let msg = if stderr.trim().is_empty() {
                    stdout
                } else {
                    stderr
                };
                if !msg.trim().is_empty() {
                    return RepoStatus::error(&repo.path, &repo.name, repo.worktree, msg);
                }
            }
        }
    }

    parsed
}

/// Runs `git diff --stat` (read-only) for the detail view.
pub fn git_diff_stat(repo_path: &Path, timeout_ms: u64, empty: bool) -> Result<String, String> {
    let timeout = Duration::from_millis(timeout_ms);
    let args: &[&str] = if empty {
        &["diff", "--stat", "--cached"]
    } else {
        &["diff", "--stat", "HEAD"]
    };

    match run_git_with_timeout(repo_path, args, timeout) {
        GitExecOutcome::Ok(out) => Ok(out),
        GitExecOutcome::TimedOut => Err(String::from("tiempo agotado al obtener diff --stat")),
        GitExecOutcome::SpawnError(err) => Err(truncate_chars(&err, 200)),
        GitExecOutcome::Failed { stderr, stdout } => {
            let msg = if stderr.trim().is_empty() {
                stdout
            } else {
                stderr
            };
            Err(truncate_chars(&msg, 200))
        }
    }
}

/// Reads the local cached `origin/HEAD` symbolic ref without touching the network.
pub fn cached_origin_head(repo_path: &Path, timeout_ms: u64) -> Option<String> {
    let timeout = Duration::from_millis(timeout_ms);
    match run_git_with_timeout(
        repo_path,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        timeout,
    ) {
        GitExecOutcome::Ok(out) => {
            let trimmed = out.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(
                    trimmed
                        .strip_prefix("origin/")
                        .unwrap_or(trimmed)
                        .to_owned(),
                )
            }
        }
        _ => None,
    }
}

pub(crate) fn run_git_with_timeout(
    repo_path: &Path,
    args: &[&str],
    timeout: Duration,
) -> GitExecOutcome {
    let mut command = Command::new("git");
    command
        .current_dir(repo_path)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return GitExecOutcome::SpawnError(err.to_string()),
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();

    if let Some(mut pipe) = stdout_pipe {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
            let _ = out_tx.send(buf);
        });
    } else {
        let _ = out_tx.send(Vec::new());
    }

    if let Some(mut pipe) = stderr_pipe {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
            let _ = err_tx.send(buf);
        });
    } else {
        let _ = err_tx.send(Vec::new());
    }

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout_bytes = out_rx
                    .recv_timeout(Duration::from_millis(500))
                    .unwrap_or_default();
                let stderr_bytes = err_rx
                    .recv_timeout(Duration::from_millis(500))
                    .unwrap_or_default();
                let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
                let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();

                if status.success() {
                    return GitExecOutcome::Ok(stdout);
                }
                return GitExecOutcome::Failed { stderr, stdout };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return GitExecOutcome::TimedOut;
                }
                thread::sleep(POLL_INTERVAL);
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return GitExecOutcome::SpawnError(err.to_string());
            }
        }
    }
}
