use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::Value;
use term_claude::cost::{compute_session_cost, CostSource};
use term_claude::model::{
    load_sessions, parse_line, AssistantRecord, CostStateRecord, LineEvent, SessionSummary,
    TokenCounts, TranscriptStore,
};
use term_claude::read::{discover_transcript_files, IncrementalReader, TranscriptFile};
use term_claude::ui::{render, session_matches_cwd, AppState};
use term_config::{ClaudeConfig, ModelPrice};

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("term-claude-{prefix}-{pid}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn read_fixture(name: &str) -> String {
    fs::read_to_string(fixture_path(name)).expect("read fixture")
}

fn compact_json_line(pretty: &str) -> String {
    let val: Value = serde_json::from_str(pretty).expect("valid fixture json");
    serde_json::to_string(&val).expect("compact json")
}

fn write_jsonl(path: &Path, lines: &[&str]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dir");
    }
    let mut file = File::create(path).expect("create jsonl");
    for line in lines {
        writeln!(file, "{line}").expect("write line");
    }
}

fn make_config(projects_dir: &Path) -> ClaudeConfig {
    ClaudeConfig {
        enabled: true,
        projects_dir: projects_dir.to_path_buf(),
        days: 7,
        context_alert_tokens: 250_000,
        prices: BTreeMap::new(),
    }
}

#[test]
fn parses_assistant_shape_fixture_and_missing_usage_defaults_to_zero() {
    let compact = compact_json_line(&read_fixture("claude.assistant-line.shape.json"));
    let parsed = parse_line(&compact).expect("assistant line should parse");

    assert_eq!(
        parsed.session_id.as_deref(),
        Some("00000000-0000-4000-8000-000000000001")
    );
    assert_eq!(
        parsed.cwd.as_deref(),
        Some("/Users/x/Desktop/WORK/ejemplo-api")
    );
    assert_eq!(parsed.git_branch.as_deref(), Some("feat/ejemplo"));
    assert_eq!(parsed.is_sidechain, Some(false));

    let Some(LineEvent::Assistant(rec)) = parsed.event else {
        panic!("expected assistant event");
    };
    assert_eq!(
        rec.message_id.as_deref(),
        Some("msg_011Cgu3hGBx31LEqKJUmfBE6")
    );
    assert_eq!(rec.model.as_deref(), Some("claude-fable-5-2"));
    assert_eq!(
        rec.tokens,
        TokenCounts {
            input: 2,
            output: 1379,
            cache_read: 28384,
            cache_write: 31312,
        }
    );
    assert_eq!(rec.cache_write_5m, 0);
    assert_eq!(rec.cache_write_1h, 31312);

    // Missing usage fields and missing usage object default to 0.
    let minimal = r#"{"type":"assistant","sessionId":"s1","message":{"id":"m1","model":"claude-fable-5-2","usage":{"input_tokens":5}}}"#;
    let parsed_min = parse_line(minimal).expect("minimal assistant parses");
    let Some(LineEvent::Assistant(min_rec)) = parsed_min.event else {
        panic!("expected assistant event");
    };
    assert_eq!(
        min_rec.tokens,
        TokenCounts {
            input: 5,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        }
    );
    assert_eq!(min_rec.cache_write_5m, 0);
    assert_eq!(min_rec.cache_write_1h, 0);

    let no_usage = r#"{"type":"assistant","sessionId":"s1","message":{"id":"m2"}}"#;
    let parsed_no_usage = parse_line(no_usage).expect("assistant without usage parses");
    let Some(LineEvent::Assistant(no_u_rec)) = parsed_no_usage.event else {
        panic!("expected assistant event");
    };
    assert_eq!(no_u_rec.tokens, TokenCounts::default());
}

#[test]
fn deduplicates_strictly_by_message_id_using_fixture() {
    let temp = TempDirGuard::new("dup");
    let projects_dir = temp.path.join("projects");
    let session_file = projects_dir
        .join("-Users-x-Desktop-WORK-ejemplo-api")
        .join("00000000-0000-4000-8000-000000000001.jsonl");
    fs::create_dir_all(session_file.parent().unwrap()).unwrap();
    fs::copy(
        fixture_path("claude.duplicate-message-id.jsonl"),
        &session_file,
    )
    .unwrap();

    let cfg = make_config(&projects_dir);
    let sessions = load_sessions(&cfg, 7, SystemTime::now()).unwrap();
    assert_eq!(sessions.len(), 1);

    let s = &sessions[0];
    assert_eq!(s.session_id, "00000000-0000-4000-8000-000000000001");
    assert_eq!(s.project, "-Users-x-Desktop-WORK-ejemplo-api");
    assert_eq!(s.cwd.as_deref(), Some("/Users/x/Desktop/WORK/ejemplo-api"));
    assert_eq!(s.git_branch.as_deref(), Some("feat/ejemplo"));
    assert_eq!(s.last_activity_at, "2026-11-10T09:56:25.471Z");
    assert_eq!(s.models, vec!["claude-fable-5-2"]);
    // Counted ONCE despite 2 lines with same message.id.
    assert_eq!(
        s.tokens,
        TokenCounts {
            input: 2,
            output: 1379,
            cache_read: 28384,
            cache_write: 31312,
        }
    );
    assert_eq!(s.sidechain_tokens, TokenCounts::default());
    // contextTokens = input + cache_read + cache_write = 2 + 28384 + 31312 = 59698
    assert_eq!(s.context_tokens, 59_698);
    assert!(!s.context_alert);
    // No cost-state and no configured prices => none + incomplete.
    assert_eq!(s.cost_source, CostSource::None);
    assert!(s.cost_incomplete);
}

#[test]
fn parses_cost_state_fixture_and_cost_estimation_cases() {
    let compact_cost = compact_json_line(&read_fixture("claude.cost-state.shape.json"));
    let parsed = parse_line(&compact_cost).expect("cost-state line parses");
    let Some(LineEvent::CostState(cs)) = parsed.event else {
        panic!("expected cost-state event");
    };
    assert!((cs.total_cost_usd.unwrap() - 2.517483499999999).abs() < 1e-12);
    assert!(cs.has_unknown_model_cost);
    assert_eq!(cs.models.len(), 2);

    // 1. cost-state with hasUnknownModelCost: true -> costIncomplete = true
    let cfg_empty = ClaudeConfig::default();
    let c1 = compute_session_cost(&[], Some(&cs), &cfg_empty);
    assert_eq!(c1.cost_source, CostSource::CostState);
    assert!((c1.cost_usd - 2.517483499999999).abs() < 1e-12);
    assert!(c1.cost_incomplete);

    // 2. cost-state with hasUnknownModelCost: false -> costIncomplete = false
    let cs_complete = CostStateRecord {
        total_cost_usd: Some(1.25),
        has_unknown_model_cost: false,
        models: vec![String::from("claude-fable-5-2")],
    };
    let c2 = compute_session_cost(&[], Some(&cs_complete), &cfg_empty);
    assert_eq!(c2.cost_source, CostSource::CostState);
    assert!((c2.cost_usd - 1.25).abs() < 1e-12);
    assert!(!c2.cost_incomplete);

    // 3. No cost-state, configured prices for all models -> estimated & complete
    let mut cfg_priced = ClaudeConfig::default();
    cfg_priced.prices.insert(
        String::from("claude-fable-5-2"),
        ModelPrice {
            input: 3.0,
            output: 15.0,
            cache_write_5m: 3.75,
            cache_write_1h: 6.0,
            cache_read: 0.30,
        },
    );
    let msg = AssistantRecord {
        message_id: Some(String::from("m1")),
        model: Some(String::from("claude-fable-5-2")),
        is_sidechain: false,
        timestamp: None,
        tokens: TokenCounts {
            input: 1_000_000,
            output: 500_000,
            cache_read: 2_000_000,
            cache_write: 1_000_000,
        },
        cache_write_5m: 0,
        cache_write_1h: 1_000_000,
    };
    // 3.0 + 7.5 + 0.6 + 6.0 = 17.1
    let c3 = compute_session_cost(std::slice::from_ref(&msg), None, &cfg_priced);
    assert_eq!(c3.cost_source, CostSource::Estimated);
    assert!(!c3.cost_incomplete);
    assert!((c3.cost_usd - 17.1).abs() < 1e-9);

    // 4. Missing price for one model alongside a priced model -> estimated & incomplete
    let unpriced_msg = AssistantRecord {
        message_id: Some(String::from("m2")),
        model: Some(String::from("claude-unknown-9")),
        is_sidechain: true,
        timestamp: None,
        tokens: TokenCounts {
            input: 100,
            output: 50,
            cache_read: 0,
            cache_write: 0,
        },
        cache_write_5m: 0,
        cache_write_1h: 0,
    };
    let c4 = compute_session_cost(&[msg, unpriced_msg.clone()], None, &cfg_priced);
    assert_eq!(c4.cost_source, CostSource::Estimated);
    assert!(c4.cost_incomplete);
    assert!((c4.cost_usd - 17.1).abs() < 1e-9);

    // 5. Only unpriced model -> none & incomplete
    let c5 = compute_session_cost(&[unpriced_msg], None, &cfg_empty);
    assert_eq!(c5.cost_source, CostSource::None);
    assert!(c5.cost_incomplete);
    assert!((c5.cost_usd - 0.0).abs() < 1e-12);
}

#[test]
fn incremental_reader_handles_partial_lines_appends_and_truncation() {
    let temp = TempDirGuard::new("incremental");
    let projects_dir = temp.path.join("projects");
    let proj = projects_dir.join("-Users-x-proj");
    fs::create_dir_all(&proj).unwrap();
    let file_path = proj.join("sess-1.jsonl");

    let line1 = r#"{"type":"assistant","timestamp":"2026-11-10T10:00:00Z","sessionId":"sess-1","message":{"id":"m1","model":"claude-fable-5-2","usage":{"input_tokens":10,"output_tokens":20}}}"#;
    let line2_part1 = r#"{"type":"assistant","timestamp":"2026-11-10T10:01:00Z","sessionId":"sess-1","message":{"id":"m2","model":"claude-fable-5-2","usage":{"input_tokens":30,"#;
    let line2_part2 = r#""output_tokens":40}}}"#;

    // Write line1 + newline + partial line2 (no newline).
    {
        let mut f = File::create(&file_path).unwrap();
        writeln!(f, "{line1}").unwrap();
        write!(f, "{line2_part1}").unwrap();
    }

    let cfg = make_config(&projects_dir);
    let mut store = TranscriptStore::new();
    let s1 = store.refresh(&cfg, 7, SystemTime::now()).unwrap();
    assert_eq!(s1.len(), 1);
    assert_eq!(s1[0].tokens.input, 10);
    assert_eq!(s1[0].tokens.output, 20);

    // Complete line2 + newline.
    {
        let mut f = OpenOptions::new().append(true).open(&file_path).unwrap();
        writeln!(f, "{line2_part2}").unwrap();
    }

    let s2 = store.refresh(&cfg, 7, SystemTime::now()).unwrap();
    assert_eq!(s2.len(), 1);
    assert_eq!(s2[0].tokens.input, 40);
    assert_eq!(s2[0].tokens.output, 60);
    assert_eq!(s2[0].context_tokens, 30);

    // Truncate / rewrite file with smaller content: must reset offset to 0 and re-read.
    let line_rewritten = r#"{"type":"assistant","timestamp":"2026-11-10T10:05:00Z","sessionId":"sess-1","message":{"id":"m3","model":"claude-fable-5-2","usage":{"input_tokens":7,"output_tokens":3}}}"#;
    {
        let mut f = File::create(&file_path).unwrap();
        writeln!(f, "{line_rewritten}").unwrap();
    }

    let s3 = store.refresh(&cfg, 7, SystemTime::now()).unwrap();
    assert_eq!(s3.len(), 1);
    assert_eq!(s3[0].tokens.input, 7);
    assert_eq!(s3[0].tokens.output, 3);
    assert_eq!(s3[0].context_tokens, 7);

    // Also verify IncrementalReader directly on offset tracking and empty truncation.
    let mut reader = IncrementalReader::new();
    let tf = TranscriptFile {
        path: file_path.clone(),
        project: String::from("-Users-x-proj"),
        is_subagent: false,
        modified: SystemTime::now(),
    };
    let d1 = reader.read_file(&tf).unwrap();
    assert!(d1.reset);
    assert_eq!(d1.lines.len(), 1);
    assert!(reader.offset_of(&file_path) > 0);

    // Second read with no changes returns empty.
    let d2 = reader.read_file(&tf).unwrap();
    assert!(!d2.reset);
    assert!(d2.lines.is_empty());

    // Truncate to 0 bytes -> reset = true, offset = 0.
    File::create(&file_path).unwrap();
    let d3 = reader.read_file(&tf).unwrap();
    assert!(d3.reset);
    assert!(d3.lines.is_empty());
    assert_eq!(reader.offset_of(&file_path), 0);
}

#[test]
fn handles_subagents_sidechains_unknown_types_and_context_alert() {
    let temp = TempDirGuard::new("subagents");
    let projects_dir = temp.path.join("projects");
    let proj = projects_dir.join("-Users-x-proj");

    let main_file = proj.join("sess-main.jsonl");
    let subagent_direct = proj.join("subagents").join("agent-1.jsonl");
    let subagent_nested = proj
        .join("sess-main")
        .join("subagents")
        .join("agent-2.jsonl");

    // Main file includes unknown types, malformed JSON, main assistant, and inline sidechain.
    write_jsonl(
        &main_file,
        &[
            r#"not valid json at all"#,
            r#"{"type":"user","timestamp":"2026-11-10T09:00:00Z","sessionId":"sess-main","cwd":"/Users/x/proj","gitBranch":"main"}"#,
            r#"{"type":"progress","timestamp":"2026-11-10T09:00:05Z","sessionId":"sess-main","data":{"step":1}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-10T09:01:00Z","sessionId":"sess-main","isSidechain":false,"message":{"id":"m-main-1","model":"claude-fable-5-2","usage":{"input_tokens":1000,"output_tokens":200,"cache_read_input_tokens":200000,"cache_creation_input_tokens":49000}}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-10T09:02:00Z","sessionId":"sess-main","isSidechain":true,"message":{"id":"m-side-inline","model":"claude-haiku-5-5","usage":{"input_tokens":500,"output_tokens":100}}}"#,
        ],
    );

    // Direct */subagents/*.jsonl file.
    write_jsonl(
        &subagent_direct,
        &[
            r#"{"type":"assistant","timestamp":"2026-11-10T09:03:00Z","sessionId":"sess-main","isSidechain":true,"message":{"id":"m-sub-1","model":"claude-haiku-5-5","usage":{"input_tokens":300,"output_tokens":50,"cache_read_input_tokens":20}}}"#,
        ],
    );

    // Nested */<session>/subagents/*.jsonl file (even without explicit isSidechain field).
    write_jsonl(
        &subagent_nested,
        &[
            r#"{"type":"assistant","timestamp":"2026-11-10T09:04:00Z","sessionId":"sess-main","message":{"id":"m-sub-2","model":"claude-haiku-5-5","usage":{"input_tokens":100,"output_tokens":25}}}"#,
        ],
    );

    let cfg = make_config(&projects_dir);
    let sessions = load_sessions(&cfg, 7, SystemTime::now()).unwrap();
    assert_eq!(sessions.len(), 1);

    let s = &sessions[0];
    assert_eq!(s.session_id, "sess-main");
    assert_eq!(
        s.tokens,
        TokenCounts {
            input: 1000,
            output: 200,
            cache_read: 200_000,
            cache_write: 49_000,
        }
    );
    assert_eq!(
        s.sidechain_tokens,
        TokenCounts {
            input: 900,
            output: 175,
            cache_read: 20,
            cache_write: 0,
        }
    );
    // contextTokens from last non-sidechain assistant message: 1000 + 200000 + 49000 = 250000
    assert_eq!(s.context_tokens, 250_000);
    // >= 250_000 triggers contextAlert
    assert!(s.context_alert);
    assert_eq!(s.last_activity_at, "2026-11-10T09:04:00Z");
}

#[test]
fn discovers_only_files_modified_within_days() {
    let temp = TempDirGuard::new("days");
    let projects_dir = temp.path.join("projects");
    let proj = projects_dir.join("-Users-x-proj");

    let recent = proj.join("recent.jsonl");
    let old = proj.join("old.jsonl");
    write_jsonl(&recent, &[r#"{"type":"user","sessionId":"recent"}"#]);
    write_jsonl(&old, &[r#"{"type":"user","sessionId":"old"}"#]);

    let now = SystemTime::now();
    let ten_days_ago = now - Duration::from_secs(10 * 86_400);
    File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(ten_days_ago)
        .unwrap();

    let found = discover_transcript_files(&projects_dir, 7, now).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, recent);
}

#[test]
fn ui_state_cwd_filter_quit_and_render_alert_highlight() {
    let sessions = vec![
        SessionSummary {
            session_id: String::from("s-alert"),
            project: String::from("-Users-x-Desktop-WORK-ejemplo-api"),
            cwd: Some(String::from("/Users/x/Desktop/WORK/ejemplo-api")),
            git_branch: Some(String::from("feat/ejemplo")),
            last_activity_at: String::from("2026-11-24T10:51:59Z"),
            models: vec![String::from("claude-fable-5-2")],
            context_tokens: 260_000,
            context_alert: true,
            tokens: TokenCounts {
                input: 100,
                output: 50,
                cache_read: 200_000,
                cache_write: 59_900,
            },
            sidechain_tokens: TokenCounts::default(),
            cost_usd: 1.5,
            cost_source: CostSource::CostState,
            cost_incomplete: false,
        },
        SessionSummary {
            session_id: String::from("s-other"),
            project: String::from("-Users-x-other-repo"),
            cwd: Some(String::from("/Users/x/other-repo")),
            git_branch: Some(String::from("main")),
            last_activity_at: String::from("2026-11-23T10:00:00Z"),
            models: vec![String::from("claude-haiku-5-5")],
            context_tokens: 10_000,
            context_alert: false,
            tokens: TokenCounts {
                input: 1000,
                output: 200,
                cache_read: 9000,
                cache_write: 0,
            },
            sidechain_tokens: TokenCounts::default(),
            cost_usd: 0.0,
            cost_source: CostSource::None,
            cost_incomplete: true,
        },
    ];

    assert!(session_matches_cwd(
        &sessions[0],
        "/Users/x/Desktop/WORK/ejemplo-api/crates/api"
    ));
    assert!(!session_matches_cwd(
        &sessions[1],
        "/Users/x/Desktop/WORK/ejemplo-api"
    ));

    let mut state = AppState::new(
        sessions,
        7,
        250_000,
        Some(String::from("/Users/x/Desktop/WORK/ejemplo-api")),
    );
    assert_eq!(state.visible_sessions().len(), 2);

    // Toggle CWD filter with 'c'.
    state.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(state.filter_cwd);
    assert_eq!(state.visible_sessions().len(), 1);
    assert_eq!(state.visible_sessions()[0].session_id, "s-alert");

    // Render using ratatui TestBackend and verify alert highlight text appears.
    let backend = TestBackend::new(110, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| render(frame, &state)).unwrap();
    let buffer_text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(buffer_text.contains("Filtro CWD: ON"));
    assert!(buffer_text.contains("⚠"));
    assert!(buffer_text.contains("ejemplo-api"));

    // Quit with 'q'.
    assert!(!state.should_quit);
    state.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(state.should_quit);
}

#[test]
fn cli_json_output_sorted_and_exit_codes() {
    let temp = TempDirGuard::new("cli");
    let projects_dir = temp.path.join("projects");
    let proj_a = projects_dir.join("-Users-x-a");
    let proj_b = projects_dir.join("-Users-x-b");

    let compact_cost = compact_json_line(&read_fixture("claude.cost-state.shape.json"));

    write_jsonl(
        &proj_a.join("older.jsonl"),
        &[
            r#"{"type":"assistant","timestamp":"2026-11-20T08:00:00Z","sessionId":"older","cwd":"/Users/x/a","gitBranch":"main","message":{"id":"m-old","model":"claude-fable-5-2","usage":{"input_tokens":10,"output_tokens":5}}}"#,
        ],
    );

    let newer_assistant = compact_json_line(&read_fixture("claude.assistant-line.shape.json"))
        .replace("2026-11-10T09:56:17.125Z", "2026-11-24T10:51:59Z");
    write_jsonl(
        &proj_b.join("00000000-0000-4000-8000-000000000001.jsonl"),
        &[&newer_assistant, &compact_cost],
    );

    let config_path = temp.path.join("config.toml");
    let projects_toml = projects_dir.to_string_lossy().replace('\\', "\\\\");
    fs::write(
        &config_path,
        format!(
            "[term.claude]\nenabled = true\nprojects_dir = \"{projects_toml}\"\ndays = 30\ncontext_alert_tokens = 250000\nunknown_future_key = 42\n"
        ),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_term-claude");

    // 1. Valid --config + --json + --days 30 -> exit 0, JSON sorted by lastActivityAt desc.
    let out = Command::new(bin)
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--json",
            "--days",
            "30",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stderr_str = String::from_utf8_lossy(&out.stderr);
    assert!(stderr_str.contains("unknown_future_key"));

    let parsed: Vec<Value> = serde_json::from_slice(&out.stdout).expect("valid JSON stdout");
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        parsed[0]["sessionId"].as_str(),
        Some("00000000-0000-4000-8000-000000000001")
    );
    assert_eq!(parsed[0]["costSource"].as_str(), Some("cost-state"));
    assert_eq!(parsed[0]["costIncomplete"].as_bool(), Some(true));
    assert_eq!(parsed[1]["sessionId"].as_str(), Some("older"));

    // Verify exact CONTRATO §B.6 keys exist on item.
    for key in [
        "sessionId",
        "project",
        "cwd",
        "gitBranch",
        "lastActivityAt",
        "models",
        "contextTokens",
        "contextAlert",
        "tokens",
        "sidechainTokens",
        "costUsd",
        "costSource",
        "costIncomplete",
    ] {
        assert!(parsed[0].get(key).is_some(), "missing key {key}");
    }

    // 2. Invalid --days 0 -> exit 2.
    let bad_days = Command::new(bin)
        .args([
            "--config",
            config_path.to_str().unwrap(),
            "--json",
            "--days",
            "0",
        ])
        .output()
        .unwrap();
    assert_eq!(bad_days.status.code(), Some(2));

    // 3. Unknown flag -> exit 2.
    let bad_flag = Command::new(bin).arg("--nope").output().unwrap();
    assert_eq!(bad_flag.status.code(), Some(2));

    // 4. Missing --config file -> exit 2.
    let missing_cfg = Command::new(bin)
        .args([
            "--config",
            temp.path.join("does-not-exist.toml").to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(missing_cfg.status.code(), Some(2));

    // 5. enabled = true with nonexistent projects_dir -> exit 2.
    let bad_cfg_path = temp.path.join("bad.toml");
    let missing_dir = temp
        .path
        .join("missing-projects")
        .to_string_lossy()
        .replace('\\', "\\\\");
    fs::write(
        &bad_cfg_path,
        format!("[term.claude]\nenabled = true\nprojects_dir = \"{missing_dir}\"\n"),
    )
    .unwrap();
    let bad_cfg = Command::new(bin)
        .args(["--config", bad_cfg_path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(bad_cfg.status.code(), Some(2));
}
