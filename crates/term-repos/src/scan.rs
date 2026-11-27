use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::parse::parse_worktree_gitdir;

/// A git repository or worktree discovered on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRepo {
    pub path: PathBuf,
    pub name: String,
    pub worktree: bool,
}

/// Discovers git repositories under `roots` up to `max_depth`.
///
/// A directory is treated as a repository if it contains a `.git` directory or a `.git`
/// file containing a `gitdir:` pointer (worktree or submodule). Hidden directories and any
/// folder name in `skip` are skipped, except `.worktrees` is traversed when
/// `include_worktrees` is `true`.
pub fn discover_repos(
    roots: &[PathBuf],
    max_depth: u32,
    include_worktrees: bool,
    skip: &[String],
) -> Vec<DiscoveredRepo> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for root in roots {
        if !root.is_dir() {
            continue;
        }
        walk_dir(
            root,
            0,
            max_depth,
            include_worktrees,
            skip,
            &mut seen,
            &mut out,
        );
    }

    out.sort_by(|a, b| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()));
    out
}

fn walk_dir(
    dir: &Path,
    depth: u32,
    max_depth: u32,
    include_worktrees: bool,
    skip: &[String],
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<DiscoveredRepo>,
) {
    if let Some(worktree) = detect_git_marker(dir) {
        let dedup_key = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        if seen.insert(dedup_key) {
            let name = dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| dir.to_string_lossy().into_owned());
            out.push(DiscoveredRepo {
                path: dir.to_path_buf(),
                name,
                worktree,
            });
        }
    }

    if depth >= max_depth {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut children: Vec<(String, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            Some((name, entry.path()))
        })
        .collect();

    children.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, child_path) in children {
        if should_skip_dir(&name, include_worktrees, skip) {
            continue;
        }
        walk_dir(
            &child_path,
            depth + 1,
            max_depth,
            include_worktrees,
            skip,
            seen,
            out,
        );
    }
}

fn detect_git_marker(dir: &Path) -> Option<bool> {
    let dot_git = dir.join(".git");
    let meta = fs::metadata(&dot_git).ok()?;
    if meta.is_dir() {
        return Some(false);
    }
    if meta.is_file() {
        let content = fs::read_to_string(&dot_git).ok()?;
        if parse_worktree_gitdir(&content).is_some() {
            return Some(true);
        }
    }
    None
}

fn should_skip_dir(name: &str, include_worktrees: bool, skip: &[String]) -> bool {
    if name == ".git" {
        return true;
    }
    if name == ".worktrees" {
        return !include_worktrees;
    }
    if name.starts_with('.') {
        return true;
    }
    skip.iter().any(|pattern| pattern == name)
}
