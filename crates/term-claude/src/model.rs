use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use term_config::ClaudeConfig;

use crate::cost::{self, CostSource};
use crate::read::IncrementalReader;

/// Aggregated token counts matching CONTRATO §B.6.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenCounts {
    fn add_assign(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
    }

    /// Context window occupancy (`input + cacheRead + cacheWrite`).
    pub fn context_total(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }
}

/// Session summary matching the JSON schema in CONTRATO §B.6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub project: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub last_activity_at: String,
    pub models: Vec<String>,
    pub context_tokens: u64,
    pub context_alert: bool,
    pub tokens: TokenCounts,
    pub sidechain_tokens: TokenCounts,
    pub cost_usd: f64,
    pub cost_source: CostSource,
    pub cost_incomplete: bool,
}

/// Parsed fields from an `assistant` transcript line (CONTRATO §F.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantRecord {
    pub message_id: Option<String>,
    pub model: Option<String>,
    pub is_sidechain: bool,
    pub timestamp: Option<String>,
    pub tokens: TokenCounts,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

/// Parsed fields from a `cost-state` transcript line (CONTRATO §F.2).
#[derive(Debug, Clone, PartialEq)]
pub struct CostStateRecord {
    pub total_cost_usd: Option<f64>,
    pub has_unknown_model_cost: bool,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineEvent {
    Assistant(AssistantRecord),
    CostState(CostStateRecord),
}

/// Parsed single JSONL line; unknown `type` values keep common metadata and `event: None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedLine {
    pub session_id: Option<String>,
    pub timestamp: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub is_sidechain: Option<bool>,
    pub event: Option<LineEvent>,
}

/// Parses one JSONL line according to CONTRATO §F.2.
///
/// Returns `None` if the line is empty or invalid JSON.
pub fn parse_line(raw: &str) -> Option<ParsedLine> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let value: Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;

    let line_type = obj.get("type").and_then(Value::as_str);
    let session_id = non_empty_str(obj.get("sessionId"));
    let timestamp = non_empty_str(obj.get("timestamp"));
    let cwd = non_empty_str(obj.get("cwd"));
    let git_branch = non_empty_str(obj.get("gitBranch"));
    let is_sidechain = obj.get("isSidechain").and_then(Value::as_bool);

    let event = match line_type {
        Some("assistant") => Some(LineEvent::Assistant(parse_assistant_record(
            obj.get("message"),
            is_sidechain.unwrap_or(false),
            timestamp.clone(),
        ))),
        Some("cost-state") => Some(LineEvent::CostState(parse_cost_state_record(obj))),
        _ => None,
    };

    Some(ParsedLine {
        session_id,
        timestamp,
        cwd,
        git_branch,
        is_sidechain,
        event,
    })
}

fn parse_assistant_record(
    message_val: Option<&Value>,
    is_sidechain: bool,
    timestamp: Option<String>,
) -> AssistantRecord {
    let msg_obj = message_val.and_then(Value::as_object);
    let message_id = msg_obj.and_then(|m| non_empty_str(m.get("id")));
    let model = msg_obj.and_then(|m| non_empty_str(m.get("model")));
    let usage = msg_obj
        .and_then(|m| m.get("usage"))
        .and_then(Value::as_object);

    let input = usage.map_or(0, |u| u64_field(u, "input_tokens"));
    let output = usage.map_or(0, |u| u64_field(u, "output_tokens"));
    let cache_read = usage.map_or(0, |u| u64_field(u, "cache_read_input_tokens"));
    let cache_write = usage.map_or(0, |u| u64_field(u, "cache_creation_input_tokens"));

    let cache_creation = usage
        .and_then(|u| u.get("cache_creation"))
        .and_then(Value::as_object);
    let cache_write_5m = cache_creation.map_or(0, |c| u64_field(c, "ephemeral_5m_input_tokens"));
    let cache_write_1h = cache_creation.map_or(0, |c| u64_field(c, "ephemeral_1h_input_tokens"));

    AssistantRecord {
        message_id,
        model,
        is_sidechain,
        timestamp,
        tokens: TokenCounts {
            input,
            output,
            cache_read,
            cache_write,
        },
        cache_write_5m,
        cache_write_1h,
    }
}

fn parse_cost_state_record(obj: &serde_json::Map<String, Value>) -> CostStateRecord {
    let total_cost_usd = obj
        .get("totalCostUSD")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0);
    let has_unknown_model_cost = obj
        .get("hasUnknownModelCost")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut models = Vec::new();
    if let Some(model_usage) = obj.get("modelUsage").and_then(Value::as_object) {
        for model in model_usage.keys() {
            if !model.is_empty() {
                models.push(model.clone());
            }
        }
    }

    CostStateRecord {
        total_cost_usd,
        has_unknown_model_cost,
        models,
    }
}

fn non_empty_str(val: Option<&Value>) -> Option<String> {
    val.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn u64_field(obj: &serde_json::Map<String, Value>, key: &str) -> u64 {
    obj.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[derive(Debug)]
struct StoredFile {
    project: String,
    is_subagent: bool,
    modified: SystemTime,
    fallback_session_id: String,
    lines: Vec<ParsedLine>,
}

/// Stateful incremental transcript store supporting appends, truncation re-reads, and subagents.
#[derive(Debug, Default)]
pub struct TranscriptStore {
    reader: IncrementalReader,
    files: BTreeMap<PathBuf, StoredFile>,
}

impl TranscriptStore {
    /// Creates an empty transcript store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Scans `config.projects_dir` (for `days`) incrementally and returns aggregated sessions
    /// sorted by `lastActivityAt` descending.
    pub fn refresh(
        &mut self,
        config: &ClaudeConfig,
        days: u32,
        now: SystemTime,
    ) -> io::Result<Vec<SessionSummary>> {
        let (deltas, active_paths) = self.reader.scan_and_read(&config.projects_dir, days, now)?;
        self.files.retain(|path, _| active_paths.contains(path));

        for delta in deltas {
            let entry = self
                .files
                .entry(delta.file.path.clone())
                .or_insert_with(|| StoredFile {
                    project: delta.file.project.clone(),
                    is_subagent: delta.file.is_subagent,
                    modified: delta.file.modified,
                    fallback_session_id: infer_fallback_session_id(
                        &delta.file.path,
                        delta.file.is_subagent,
                    ),
                    lines: Vec::new(),
                });

            entry.project = delta.file.project;
            entry.is_subagent = delta.file.is_subagent;
            entry.modified = delta.file.modified;
            if delta.reset {
                entry.lines.clear();
            }
            for raw in delta.lines {
                if let Some(parsed) = parse_line(&raw) {
                    entry.lines.push(parsed);
                }
            }
        }

        Ok(self.summarize_all(config))
    }

    fn summarize_all(&self, config: &ClaudeConfig) -> Vec<SessionSummary> {
        let mut sessions: HashMap<(String, String), SessionAccumulator> = HashMap::new();

        for stored in self.files.values() {
            for line in &stored.lines {
                if line.session_id.is_none() && line.event.is_none() {
                    continue;
                }
                let session_id = line
                    .session_id
                    .as_deref()
                    .unwrap_or(&stored.fallback_session_id)
                    .to_owned();
                let key = (stored.project.clone(), session_id.clone());
                let acc = sessions.entry(key).or_insert_with(|| {
                    SessionAccumulator::new(session_id, stored.project.clone(), stored.modified)
                });
                if stored.modified > acc.latest_modified {
                    acc.latest_modified = stored.modified;
                }
                acc.apply_line(line, stored.is_subagent);
            }
        }

        let mut list: Vec<(Option<i128>, SessionSummary)> = sessions
            .into_values()
            .map(|acc| acc.finish(config))
            .collect();

        list.sort_by(|(ts_a, a), (ts_b, b)| {
            ts_b.cmp(ts_a)
                .then_with(|| b.last_activity_at.cmp(&a.last_activity_at))
                .then_with(|| a.session_id.cmp(&b.session_id))
        });

        list.into_iter().map(|(_, summary)| summary).collect()
    }
}

/// Convenience one-shot loader matching CONTRATO §B.6.
pub fn load_sessions(
    config: &ClaudeConfig,
    days: u32,
    now: SystemTime,
) -> io::Result<Vec<SessionSummary>> {
    let mut store = TranscriptStore::new();
    store.refresh(config, days, now)
}

fn infer_fallback_session_id(path: &Path, is_subagent: bool) -> String {
    if is_subagent {
        if let Some(parent_session) = path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .filter(|n| *n != "subagents" && !n.is_empty())
        {
            return parent_session.to_owned();
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

struct SessionAccumulator {
    session_id: String,
    project: String,
    latest_modified: SystemTime,
    cwd: Option<String>,
    cwd_from_main: bool,
    git_branch: Option<String>,
    branch_from_main: bool,
    last_ts: Option<(Option<i128>, String)>,
    seen_message_ids: HashSet<String>,
    deduped_assistants: Vec<AssistantRecord>,
    models: Vec<String>,
    seen_models: HashSet<String>,
    tokens: TokenCounts,
    sidechain_tokens: TokenCounts,
    last_context: Option<(Option<i128>, u64)>,
    last_cost_state: Option<CostStateRecord>,
}

impl SessionAccumulator {
    fn new(session_id: String, project: String, modified: SystemTime) -> Self {
        Self {
            session_id,
            project,
            latest_modified: modified,
            cwd: None,
            cwd_from_main: false,
            git_branch: None,
            branch_from_main: false,
            last_ts: None,
            seen_message_ids: HashSet::new(),
            deduped_assistants: Vec::new(),
            models: Vec::new(),
            seen_models: HashSet::new(),
            tokens: TokenCounts::default(),
            sidechain_tokens: TokenCounts::default(),
            last_context: None,
            last_cost_state: None,
        }
    }

    fn apply_line(&mut self, line: &ParsedLine, file_is_subagent: bool) {
        let is_sidechain = line.is_sidechain.unwrap_or(false) || file_is_subagent;

        if let Some(cwd) = &line.cwd {
            if !is_sidechain || !self.cwd_from_main {
                self.cwd = Some(cwd.clone());
                if !is_sidechain {
                    self.cwd_from_main = true;
                }
            }
        }
        if let Some(branch) = &line.git_branch {
            if !is_sidechain || !self.branch_from_main {
                self.git_branch = Some(branch.clone());
                if !is_sidechain {
                    self.branch_from_main = true;
                }
            }
        }

        let line_ts_key = line.timestamp.as_deref().and_then(parse_rfc3339_utc_nanos);
        if let Some(ts_str) = &line.timestamp {
            let replace = match &self.last_ts {
                None => true,
                Some((prev_key, prev_str)) => match (line_ts_key, *prev_key) {
                    (Some(a), Some(b)) => a >= b,
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    (None, None) => ts_str >= prev_str,
                },
            };
            if replace {
                self.last_ts = Some((line_ts_key, ts_str.clone()));
            }
        }

        match &line.event {
            Some(LineEvent::Assistant(rec)) => {
                let mut effective = rec.clone();
                effective.is_sidechain = effective.is_sidechain || is_sidechain;

                if let Some(model) = &effective.model {
                    self.push_model(model);
                }

                if !effective.is_sidechain {
                    let ctx = effective.tokens.context_total();
                    let replace_ctx = match &self.last_context {
                        None => true,
                        Some((prev_key, _)) => match (line_ts_key, *prev_key) {
                            (Some(a), Some(b)) => a >= b,
                            (Some(_), None) => true,
                            (None, Some(_)) => false,
                            (None, None) => true,
                        },
                    };
                    if replace_ctx {
                        self.last_context = Some((line_ts_key, ctx));
                    }
                }

                let is_new = match &effective.message_id {
                    Some(id) => self.seen_message_ids.insert(id.clone()),
                    None => true,
                };
                if is_new {
                    if effective.is_sidechain {
                        self.sidechain_tokens.add_assign(&effective.tokens);
                    } else {
                        self.tokens.add_assign(&effective.tokens);
                    }
                    self.deduped_assistants.push(effective);
                }
            }
            Some(LineEvent::CostState(cs)) => {
                for model in &cs.models {
                    self.push_model(model);
                }
                self.last_cost_state = Some(cs.clone());
            }
            None => {}
        }
    }

    fn push_model(&mut self, model: &str) {
        if self.seen_models.insert(model.to_owned()) {
            self.models.push(model.to_owned());
        }
    }

    fn finish(self, config: &ClaudeConfig) -> (Option<i128>, SessionSummary) {
        let context_tokens = self.last_context.map_or(0, |(_, ctx)| ctx);
        let context_alert = context_tokens >= config.context_alert_tokens;

        let cost = cost::compute_session_cost(
            &self.deduped_assistants,
            self.last_cost_state.as_ref(),
            config,
        );

        let (sort_key, last_activity_at) = match self.last_ts {
            Some((key, ts)) => (key, ts),
            None => {
                let fallback = format_system_time_rfc3339(self.latest_modified);
                let key = parse_rfc3339_utc_nanos(&fallback);
                (key, fallback)
            }
        };

        (
            sort_key,
            SessionSummary {
                session_id: self.session_id,
                project: self.project,
                cwd: self.cwd,
                git_branch: self.git_branch,
                last_activity_at,
                models: self.models,
                context_tokens,
                context_alert,
                tokens: self.tokens,
                sidechain_tokens: self.sidechain_tokens,
                cost_usd: cost.cost_usd,
                cost_source: cost.cost_source,
                cost_incomplete: cost.cost_incomplete,
            },
        )
    }
}

/// Parses an RFC3339 timestamp into nanoseconds since Unix epoch for accurate sorting.
pub fn parse_rfc3339_utc_nanos(input: &str) -> Option<i128> {
    let s = input.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    if bytes[4] != b'-' || bytes[7] != b'-' || (bytes[10] != b'T' && bytes[10] != b't') {
        return None;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }

    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: i64 = s[11..13].parse().ok()?;
    let min: i64 = s[14..16].parse().ok()?;
    let sec: i64 = s[17..19].parse().ok()?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&min)
        || !(0..=60).contains(&sec)
    {
        return None;
    }

    let mut idx = 19;
    let mut nanos: i128 = 0;
    if idx < bytes.len() && bytes[idx] == b'.' {
        idx += 1;
        let frac_start = idx;
        while idx < bytes.len() && bytes[idx].is_ascii_digit() {
            idx += 1;
        }
        if idx == frac_start {
            return None;
        }
        let frac = &s[frac_start..idx];
        let mut mul: i128 = 100_000_000;
        for &b in frac.as_bytes().iter().take(9) {
            nanos += i128::from(b - b'0') * mul;
            mul /= 10;
        }
    }

    let offset_secs: i64 = match bytes.get(idx) {
        Some(b'Z' | b'z') if idx + 1 == bytes.len() => 0,
        Some(sign @ (b'+' | b'-')) if idx + 6 == bytes.len() && bytes[idx + 3] == b':' => {
            let oh: i64 = s[idx + 1..idx + 3].parse().ok()?;
            let om: i64 = s[idx + 4..idx + 6].parse().ok()?;
            if oh > 23 || om > 59 {
                return None;
            }
            let total = oh * 3600 + om * 60;
            if *sign == b'-' {
                -total
            } else {
                total
            }
        }
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec - offset_secs;
    Some(i128::from(secs) * 1_000_000_000 + nanos)
}

/// Formats a `SystemTime` as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_system_time_rfc3339(time: SystemTime) -> String {
    let secs = match time.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hour = rem / 3600;
    let min = (rem % 3600) / 60;
    let sec = rem % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

// Howard Hinnant's civil-from-days / days-from-civil algorithms.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as u32;
    let m = month;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + i64::from(doe) - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2), m, d)
}
