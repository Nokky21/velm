use std::path::Path;

use serde::{Deserialize, Serialize};

/// Repository status record matching CONTRATO §B.4 JSON schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoStatus {
    pub path: String,
    pub name: String,
    pub branch: Option<String>,
    pub detached: Option<bool>,
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub upstream_gone: Option<bool>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub modified: Option<u64>,
    pub untracked: Option<u64>,
    pub conflicted: Option<u64>,
    pub stashes: Option<u64>,
    pub last_commit_at: Option<String>,
    pub worktree: Option<bool>,
    pub empty: Option<bool>,
    pub timed_out: bool,
    pub error: Option<String>,
}

impl RepoStatus {
    /// Creates a status record for a repository whose `git` invocation timed out.
    ///
    /// Per CONTRATO §B.4, `timedOut` is `true` and all other status fields are `null`.
    pub fn timed_out(path: &Path, name: &str) -> Self {
        Self {
            path: path_to_string(path),
            name: name.to_owned(),
            branch: None,
            detached: None,
            oid: None,
            upstream: None,
            upstream_gone: None,
            ahead: None,
            behind: None,
            modified: None,
            untracked: None,
            conflicted: None,
            stashes: None,
            last_commit_at: None,
            worktree: None,
            empty: None,
            timed_out: true,
            error: None,
        }
    }

    /// Creates a status record for a repository whose `git` invocation failed.
    pub fn error(path: &Path, name: &str, worktree: bool, message: String) -> Self {
        Self {
            path: path_to_string(path),
            name: name.to_owned(),
            branch: None,
            detached: None,
            oid: None,
            upstream: None,
            upstream_gone: None,
            ahead: None,
            behind: None,
            modified: None,
            untracked: None,
            conflicted: None,
            stashes: None,
            last_commit_at: None,
            worktree: Some(worktree),
            empty: None,
            timed_out: false,
            error: Some(truncate_chars(&message, 200)),
        }
    }

    /// Returns `true` if the repository has pending work, divergences, errors, or timed out.
    pub fn is_dirty(&self) -> bool {
        if self.timed_out || self.error.is_some() {
            return true;
        }
        self.modified.unwrap_or(0) > 0
            || self.untracked.unwrap_or(0) > 0
            || self.conflicted.unwrap_or(0) > 0
            || self.stashes.unwrap_or(0) > 0
            || self.ahead.unwrap_or(0) > 0
            || self.behind.unwrap_or(0) > 0
            || self.upstream_gone.unwrap_or(false)
    }
}

/// Parses `git status --porcelain=v2 --branch --show-stash` output per CONTRATO §F.1 and §B.4.
pub fn parse_porcelain_v2(
    path: &Path,
    name: &str,
    worktree: bool,
    status_output: &str,
    last_commit_at: Option<String>,
) -> RepoStatus {
    let mut branch: Option<String> = None;
    let mut detached = false;
    let mut oid: Option<String> = None;
    let mut empty = false;
    let mut upstream: Option<String> = None;
    let mut ahead: Option<u64> = None;
    let mut behind: Option<u64> = None;
    let mut saw_ab = false;
    let mut stashes: u64 = 0;
    let mut modified: u64 = 0;
    let mut untracked: u64 = 0;
    let mut conflicted: u64 = 0;

    for raw_line in status_output.lines() {
        let line = raw_line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            let val = rest.trim();
            if val == "(initial)" {
                empty = true;
                oid = None;
            } else {
                empty = false;
                oid = Some(val.chars().take(7).collect());
            }
        } else if let Some(rest) = line.strip_prefix("# branch.head ") {
            let val = rest.trim();
            if val == "(detached)" {
                detached = true;
                branch = None;
            } else {
                detached = false;
                branch = Some(val.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            let val = rest.trim();
            if !val.is_empty() {
                upstream = Some(val.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            if let Some((a, b)) = parse_ab(rest) {
                ahead = Some(a);
                behind = Some(b);
                saw_ab = true;
            }
        } else if let Some(rest) = line.strip_prefix("# stash ") {
            if let Ok(n) = rest.trim().parse::<u64>() {
                stashes = n;
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            modified += 1;
        } else if line.starts_with("u ") {
            conflicted += 1;
        } else if line.starts_with("? ") {
            untracked += 1;
        }
    }

    let upstream_gone = upstream.is_some() && !saw_ab;
    let (ahead, behind) = if upstream.is_some() && !upstream_gone {
        (ahead, behind)
    } else {
        (None, None)
    };

    let last_commit_at = if empty { None } else { last_commit_at };

    RepoStatus {
        path: path_to_string(path),
        name: name.to_owned(),
        branch,
        detached: Some(detached),
        oid,
        upstream,
        upstream_gone: Some(upstream_gone),
        ahead,
        behind,
        modified: Some(modified),
        untracked: Some(untracked),
        conflicted: Some(conflicted),
        stashes: Some(stashes),
        last_commit_at,
        worktree: Some(worktree),
        empty: Some(empty),
        timed_out: false,
        error: None,
    }
}

/// Parses a `.git` file (`gitdir: <path>`) and returns the target gitdir string if valid.
pub fn parse_worktree_gitdir(content: &str) -> Option<&str> {
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("gitdir:") {
            let target = rest.trim();
            if !target.is_empty() {
                return Some(target);
            }
        }
        break;
    }
    None
}

/// Formats a Unix epoch timestamp (seconds) into UTC ISO-8601 (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn format_unix_timestamp_utc(timestamp_secs: i64) -> String {
    let days = timestamp_secs.div_euclid(86_400);
    let secs_of_day = timestamp_secs.rem_euclid(86_400);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;

    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Truncates `text` to at most `max_chars` Unicode scalar values.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }
    trimmed.chars().take(max_chars).collect()
}

fn parse_ab(rest: &str) -> Option<(u64, u64)> {
    let mut parts = rest.split_whitespace();
    let ahead_str = parts.next()?.strip_prefix('+')?;
    let behind_str = parts.next()?.strip_prefix('-')?;
    let ahead = ahead_str.parse::<u64>().ok()?;
    let behind = behind_str.parse::<u64>().ok()?;
    Some((ahead, behind))
}

pub(crate) fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
