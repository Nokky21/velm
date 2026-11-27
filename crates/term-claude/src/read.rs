use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Metadata for a discovered Claude Code transcript `.jsonl` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFile {
    pub path: PathBuf,
    pub project: String,
    pub is_subagent: bool,
    pub modified: SystemTime,
}

/// Incremental lines read from a single transcript file since the last check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDelta {
    pub file: TranscriptFile,
    /// `true` when the file shrank/truncated or was read from byte offset 0.
    pub reset: bool,
    /// Newly completed lines (terminated by `\n` in the file).
    pub lines: Vec<String>,
}

/// Tracks per-file byte offsets across incremental scans of `projects_dir`.
#[derive(Debug, Default)]
pub struct IncrementalReader {
    offsets: HashMap<PathBuf, u64>,
}

impl IncrementalReader {
    /// Creates an empty incremental reader with no tracked offsets.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the currently tracked byte offset for `path`, or `0` if unseen.
    pub fn offset_of(&self, path: &Path) -> u64 {
        self.offsets.get(path).copied().unwrap_or(0)
    }

    /// Reads newly completed lines from `file`, ignoring any trailing partial line without `\n`.
    ///
    /// If the file shrank below the previously recorded offset, resets offset to `0` and
    /// re-reads from the beginning with `reset = true`.
    pub fn read_file(&mut self, file: &TranscriptFile) -> io::Result<FileDelta> {
        let mut handle = File::open(&file.path)?;
        let len = handle.metadata()?.len();
        let prev_offset = self.offsets.get(&file.path).copied().unwrap_or(0);

        let (start_offset, reset) = if len < prev_offset {
            (0, true)
        } else {
            (prev_offset, prev_offset == 0)
        };

        if len == start_offset {
            if reset {
                self.offsets.insert(file.path.clone(), 0);
            }
            return Ok(FileDelta {
                file: file.clone(),
                reset: len < prev_offset,
                lines: Vec::new(),
            });
        }

        handle.seek(SeekFrom::Start(start_offset))?;
        let mut buf = Vec::with_capacity((len - start_offset) as usize);
        handle.read_to_end(&mut buf)?;

        let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') else {
            // Trailing partial line without newline: keep offset at start_offset and retry later.
            self.offsets.insert(file.path.clone(), start_offset);
            return Ok(FileDelta {
                file: file.clone(),
                reset,
                lines: Vec::new(),
            });
        };

        let new_offset = start_offset + (last_nl as u64) + 1;
        self.offsets.insert(file.path.clone(), new_offset);

        let lines = buf[..last_nl]
            .split(|&b| b == b'\n')
            .map(|raw| {
                let trimmed = raw.strip_suffix(b"\r").unwrap_or(raw);
                String::from_utf8_lossy(trimmed).into_owned()
            })
            .collect();

        Ok(FileDelta {
            file: file.clone(),
            reset,
            lines,
        })
    }

    /// Discovers transcript files modified within `days` and reads incremental deltas for each.
    ///
    /// Returns `(deltas, active_paths)` where `active_paths` is the set of discovered files.
    pub fn scan_and_read(
        &mut self,
        projects_dir: &Path,
        days: u32,
        now: SystemTime,
    ) -> io::Result<(Vec<FileDelta>, HashSet<PathBuf>)> {
        let files = discover_transcript_files(projects_dir, days, now)?;
        let active_paths: HashSet<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
        self.offsets.retain(|path, _| active_paths.contains(path));

        let mut deltas = Vec::with_capacity(files.len());
        for file in files {
            deltas.push(self.read_file(&file)?);
        }

        Ok((deltas, active_paths))
    }
}

/// Discovers `*/*.jsonl`, `*/subagents/*.jsonl`, and `*/*/subagents/*.jsonl` modified within `days`.
pub fn discover_transcript_files(
    projects_dir: &Path,
    days: u32,
    now: SystemTime,
) -> io::Result<Vec<TranscriptFile>> {
    if !projects_dir.exists() {
        return Ok(Vec::new());
    }

    let window = Duration::from_secs(u64::from(days).saturating_mul(86_400));
    let cutoff = now.checked_sub(window).unwrap_or(SystemTime::UNIX_EPOCH);

    let mut discovered = Vec::new();
    for entry in fs::read_dir(projects_dir)? {
        let entry = entry?;
        let project_path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let project = entry.file_name().to_string_lossy().into_owned();
        scan_project_dir(&project_path, &project, cutoff, &mut discovered)?;
    }

    discovered.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(discovered)
}

fn scan_project_dir(
    project_path: &Path,
    project: &str,
    cutoff: SystemTime,
    out: &mut Vec<TranscriptFile>,
) -> io::Result<()> {
    for entry in fs::read_dir(project_path)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_file() {
            if is_jsonl_file(&path) {
                push_if_recent(&path, project, false, cutoff, out)?;
            }
        } else if file_type.is_dir() {
            if entry.file_name() == "subagents" {
                scan_subagents_dir(&path, project, cutoff, out)?;
            } else {
                let nested_subagents = path.join("subagents");
                if nested_subagents.is_dir() {
                    scan_subagents_dir(&nested_subagents, project, cutoff, out)?;
                }
            }
        }
    }
    Ok(())
}

fn scan_subagents_dir(
    subagents_dir: &Path,
    project: &str,
    cutoff: SystemTime,
    out: &mut Vec<TranscriptFile>,
) -> io::Result<()> {
    for entry in fs::read_dir(subagents_dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file() && is_jsonl_file(&path) {
            push_if_recent(&path, project, true, cutoff, out)?;
        }
    }
    Ok(())
}

fn push_if_recent(
    path: &Path,
    project: &str,
    is_subagent: bool,
    cutoff: SystemTime,
    out: &mut Vec<TranscriptFile>,
) -> io::Result<()> {
    let modified = fs::metadata(path)?.modified()?;
    if modified >= cutoff {
        out.push(TranscriptFile {
            path: path.to_path_buf(),
            project: project.to_owned(),
            is_subagent,
            modified,
        });
    }
    Ok(())
}

fn is_jsonl_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
}
