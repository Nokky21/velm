use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use term_config::validate::{is_local_ai_host, is_valid_connection_name, normalize_ai_url};
use term_config::{
    default_config_path, expand_tilde, load, load_from_path, load_from_str, AiServer, ConfigError,
    SqlColor, SqlEnv, TermConfig,
};

static HOME_LOCK: Mutex<()> = Mutex::new(());

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("term-config-{prefix}-{pid}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct HomeEnvGuard {
    prev_home: Option<std::ffi::OsString>,
}

impl HomeEnvGuard {
    fn set(path: &std::path::Path) -> Self {
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self { prev_home }
    }
}

impl Drop for HomeEnvGuard {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(val) => std::env::set_var("HOME", val),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn empty_file_uses_all_defaults_with_tilde_expanded_and_no_warnings() {
    let _lock = HOME_LOCK.lock().expect("lock home");
    let temp = TempDirGuard::new("defaults");
    let _home = HomeEnvGuard::set(&temp.path);

    let loaded = load_from_str("").expect("empty config should succeed");
    assert!(loaded.warnings.is_empty());

    let cfg = loaded.config;
    assert!(!cfg.ai.enabled);
    assert_eq!(cfg.ai.server, AiServer::Auto);
    assert_eq!(cfg.ai.url, "");
    assert_eq!(cfg.ai.model, "");
    assert!((cfg.ai.temperature - 0.2).abs() < f64::EPSILON);
    assert_eq!(cfg.ai.max_tokens_suggest, 512);
    assert_eq!(cfg.ai.max_tokens_explain, 2048);
    assert_eq!(cfg.ai.lmstudio_ttl_seconds, 600);
    assert_eq!(cfg.ai.zsh_key, "^G");
    assert_eq!(cfg.ai.explain_head_lines, 50);
    assert_eq!(cfg.ai.explain_tail_lines, 150);
    assert_eq!(cfg.ai.explain_max_chars, 12000);

    assert!(!cfg.repos.enabled);
    assert_eq!(cfg.repos.roots, vec![temp.path.join("Desktop/WORK")]);
    assert_eq!(cfg.repos.max_depth, 2);
    assert!(!cfg.repos.include_worktrees);
    assert_eq!(
        cfg.repos.skip,
        vec![
            String::from("node_modules"),
            String::from("target"),
            String::from(".worktrees"),
        ]
    );
    assert_eq!(cfg.repos.parallel, 8);
    assert_eq!(cfg.repos.timeout_ms, 5000);
    assert_eq!(cfg.repos.remote_timeout_ms, 10000);

    assert!(!cfg.sql.enabled);
    assert_eq!(cfg.sql.row_limit, 500);
    assert_eq!(cfg.sql.statement_timeout_s, 30);
    assert_eq!(cfg.sql.idle_tx_timeout_s, 60);
    assert!(cfg.sql.connections.is_empty());

    assert!(!cfg.claude.enabled);
    assert_eq!(cfg.claude.projects_dir, temp.path.join(".claude/projects"));
    assert_eq!(cfg.claude.days, 7);
    assert_eq!(cfg.claude.context_alert_tokens, 250_000);
    assert!(cfg.claude.prices.is_empty());
}

#[test]
fn unrelated_top_level_herdr_sections_are_ignored_without_warnings() {
    let toml = r#"
[keys]
prefix = "ctrl+b"

[ui]
show_sidebar = true

[unknown_top_level]
anything = 42
"#;
    let loaded = load_from_str(toml).expect("top-level non-term keys must be ignored");
    assert!(loaded.warnings.is_empty());
    assert!(!loaded.config.ai.enabled);
}

#[test]
fn contract_example_a5_and_full_config_parse_cleanly() {
    let temp = TempDirGuard::new("full");
    let repos_dir = temp.path.join("repos");
    let claude_dir = temp.path.join("claude-projects");
    fs::create_dir_all(&repos_dir).expect("create repos dir");
    fs::create_dir_all(&claude_dir).expect("create claude dir");

    let example = r#"
[term.ai]
enabled = true
model = "qwen3.8-9b-distill-uncensored-heretic"

[term.repos]
enabled = false

[term.sql]
enabled = true
[[term.sql.connections]]
name = "local-card"
env = "local"

[term.claude]
enabled = false
"#;
    let loaded = load_from_str(example).expect("contract A.5 example should parse");
    assert!(loaded.warnings.is_empty());
    assert!(loaded.config.ai.enabled);
    assert_eq!(
        loaded.config.ai.model,
        "qwen3.8-9b-distill-uncensored-heretic"
    );
    assert_eq!(loaded.config.sql.connections.len(), 1);
    let conn = loaded
        .config
        .sql
        .connection("local-card")
        .expect("local-card exists");
    assert_eq!(conn.env, SqlEnv::Local);
    assert_eq!(conn.color, SqlColor::Green);

    let full = format!(
        r#"
[term.ai]
enabled = true
server = "openai-compatible"
url = "http://localhost:1234/"
model = "local-model"
temperature = 0.0
max_tokens_suggest = 8192
max_tokens_explain = 16384
lmstudio_ttl_seconds = 30
zsh_key = "^X^G"
explain_head_lines = 0
explain_tail_lines = 1
explain_max_chars = 100000

[term.repos]
enabled = true
roots = ["{}"]
max_depth = 5
include_worktrees = true
skip = ["dist", ".git"]
parallel = 32
timeout_ms = 60000
remote_timeout_ms = 60000

[term.sql]
enabled = true
row_limit = 100000
statement_timeout_s = 600
idle_tx_timeout_s = 600

[[term.sql.connections]]
name = "dev-db-1"
env = "dev"

[[term.sql.connections]]
name = "local-blue"
env = "local"
color = "blue"

[[term.sql.connections]]
name = "dev-magenta"
env = "dev"
color = "magenta"

[term.claude]
enabled = true
projects_dir = "{}"
days = 365
context_alert_tokens = 1000

[term.claude.prices."claude-opus-5-5"]
input = 15.0
output = 75.0
cache_write_5m = 18.75
cache_write_1h = 30.0
cache_read = 1.5
"#,
        repos_dir.display().to_string().replace('\\', "/"),
        claude_dir.display().to_string().replace('\\', "/"),
    );

    let loaded_full = load_from_str(&full).expect("full valid config should parse");
    assert!(loaded_full.warnings.is_empty());
    let cfg = loaded_full.config;
    assert_eq!(cfg.ai.server, AiServer::OpenaiCompatible);
    assert_eq!(cfg.ai.url, "http://localhost:1234/v1");
    assert_eq!(cfg.repos.roots, vec![repos_dir]);
    assert_eq!(cfg.sql.connections[0].color, SqlColor::Yellow);
    assert_eq!(cfg.sql.connections[1].color, SqlColor::Blue);
    assert_eq!(cfg.sql.connections[2].color, SqlColor::Magenta);
    let price = cfg
        .claude
        .price_for("claude-opus-5-5")
        .expect("price entry exists");
    assert!((price.input - 15.0).abs() < f64::EPSILON);
    assert!((price.cache_read - 1.5).abs() < f64::EPSILON);
}

#[test]
fn load_from_default_and_explicit_paths_handles_missing_and_bom() {
    let _lock = HOME_LOCK.lock().expect("lock home");
    let temp = TempDirGuard::new("paths");
    let _home = HomeEnvGuard::set(&temp.path);

    let expected_default = temp.path.join(".config/velm/config.toml");
    assert_eq!(
        default_config_path().expect("default path"),
        expected_default
    );
    assert_eq!(expand_tilde("~").expect("expand ~"), temp.path);
    assert_eq!(
        expand_tilde("~/sub/file.toml").expect("expand ~/"),
        temp.path.join("sub/file.toml")
    );

    // Missing default config returns defaults cleanly.
    let missing_default = load(None).expect("missing default config uses defaults");
    assert_eq!(missing_default.config.ai.model, "");

    // Missing explicit --config path fails with Io error.
    let missing_explicit = temp.path.join("missing.toml");
    let err = load(Some(&missing_explicit)).expect_err("missing explicit path must error");
    assert!(matches!(err, ConfigError::Io { .. }));

    // Existing explicit --config path (with UTF-8 BOM) loads cleanly.
    let explicit_file = temp.path.join("custom.toml");
    fs::write(
        &explicit_file,
        "\u{feff}[term.ai]\nenabled = true\nmodel = \"bom-ok\"\n",
    )
    .expect("write custom config");
    let loaded = TermConfig::load(Some(&explicit_file)).expect("load explicit file");
    assert_eq!(loaded.config.ai.model, "bom-ok");

    // Existing default path is picked up by load(None).
    fs::create_dir_all(expected_default.parent().expect("parent")).expect("mkdir .config/velm");
    fs::write(&expected_default, "[term.ai]\nmodel = \"from-default\"\n")
        .expect("write default config");
    let loaded_default = load(None).expect("load existing default config");
    assert_eq!(loaded_default.config.ai.model, "from-default");

    let loaded_path = load_from_path(&explicit_file).expect("load_from_path");
    assert_eq!(loaded_path.config.ai.model, "bom-ok");
}

#[test]
fn ai_server_variants_and_out_of_range_keys_are_validated() {
    for server in [
        "auto",
        "lmstudio",
        "ollama",
        "llamacpp",
        "openai-compatible",
    ] {
        let toml = format!("[term.ai]\nserver = \"{server}\"\n");
        let loaded = load_from_str(&toml).expect("valid server variant");
        assert_eq!(loaded.config.ai.server.as_str(), server);
    }

    let invalid_cases = [
        "[term.ai]\nserver = \"anthropic\"\n",
        "[term.ai]\ntemperature = -0.01\n",
        "[term.ai]\ntemperature = 1.01\n",
        "[term.ai]\ntemperature = nan\n",
        "[term.ai]\nmax_tokens_suggest = 63\n",
        "[term.ai]\nmax_tokens_suggest = 8193\n",
        "[term.ai]\nmax_tokens_explain = 255\n",
        "[term.ai]\nmax_tokens_explain = 16385\n",
        "[term.ai]\nlmstudio_ttl_seconds = 29\n",
        "[term.ai]\nlmstudio_ttl_seconds = 86401\n",
        "[term.ai]\nzsh_key = \"\"\n",
        "[term.ai]\nzsh_key = \"   \"\n",
        "[term.ai]\nexplain_head_lines = -1\n",
        "[term.ai]\nexplain_tail_lines = 0\n",
        "[term.ai]\nexplain_max_chars = 999\n",
        "[term.ai]\nexplain_max_chars = 100001\n",
    ];

    for case in invalid_cases {
        assert!(
            load_from_str(case).is_err(),
            "expected AI validation/parse error for:\n{case}"
        );
    }
}

#[test]
fn repos_out_of_range_and_directory_checks() {
    let invalid_cases = [
        "[term.repos]\nmax_depth = 0\n",
        "[term.repos]\nmax_depth = 6\n",
        "[term.repos]\nparallel = 0\n",
        "[term.repos]\nparallel = 33\n",
        "[term.repos]\ntimeout_ms = 499\n",
        "[term.repos]\ntimeout_ms = 60001\n",
        "[term.repos]\nremote_timeout_ms = 999\n",
        "[term.repos]\nremote_timeout_ms = 60001\n",
        "[term.repos]\nskip = [\"foo/bar\"]\n",
        "[term.repos]\nskip = [\"foo\\\\bar\"]\n",
        "[term.repos]\nskip = [\"\"]\n",
        "[term.repos]\nskip = [\".\"]\n",
        "[term.repos]\nskip = [\"..\"]\n",
    ];

    for case in invalid_cases {
        assert!(
            load_from_str(case).is_err(),
            "expected repos validation error for:\n{case}"
        );
    }

    let temp = TempDirGuard::new("repos-dir");
    let missing = temp.path.join("does-not-exist");
    let file_path = temp.path.join("a-file.txt");
    fs::write(&file_path, "hi").expect("create file");

    let missing_toml = format!(
        "[term.repos]\nenabled = true\nroots = [\"{}\"]\n",
        missing.display().to_string().replace('\\', "/")
    );
    let err = load_from_str(&missing_toml).expect_err("missing root dir must error when enabled");
    assert!(matches!(err, ConfigError::MissingDirectory { .. }));

    let file_toml = format!(
        "[term.repos]\nenabled = true\nroots = [\"{}\"]\n",
        file_path.display().to_string().replace('\\', "/")
    );
    let err = load_from_str(&file_toml).expect_err("non-dir root must error when enabled");
    assert!(matches!(err, ConfigError::MissingDirectory { .. }));
}

#[test]
fn sql_ranges_prod_rejection_unique_names_and_colors() {
    let invalid_cases = [
        "[term.sql]\nrow_limit = 0\n",
        "[term.sql]\nrow_limit = 100001\n",
        "[term.sql]\nstatement_timeout_s = 0\n",
        "[term.sql]\nstatement_timeout_s = 601\n",
        "[term.sql]\nidle_tx_timeout_s = 9\n",
        "[term.sql]\nidle_tx_timeout_s = 601\n",
        "[term.sql]\n[[term.sql.connections]]\nname = \"\"\nenv = \"local\"\n",
        "[term.sql]\n[[term.sql.connections]]\nname = \"BadName\"\nenv = \"local\"\n",
        "[term.sql]\n[[term.sql.connections]]\nname = \"bad_name\"\nenv = \"local\"\n",
        "[term.sql]\n[[term.sql.connections]]\nname = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\nenv = \"local\"\n",
        "[term.sql]\n[[term.sql.connections]]\nname = \"staging-db\"\nenv = \"staging\"\n",
        "[term.sql]\n[[term.sql.connections]]\nname = \"bad-color\"\nenv = \"local\"\ncolor = \"red\"\n",
    ];

    for case in invalid_cases {
        assert!(
            load_from_str(case).is_err(),
            "expected sql validation error for:\n{case}"
        );
    }

    assert!(is_valid_connection_name("a"));
    assert!(is_valid_connection_name(&"a".repeat(40)));
    assert!(!is_valid_connection_name(&"a".repeat(41)));
    assert!(!is_valid_connection_name("db_1"));

    let prod_toml = r#"
[term.sql]
enabled = true
[[term.sql.connections]]
name = "main-prod"
env = "prod"
"#;
    let err = load_from_str(prod_toml).expect_err("prod env must be rejected");
    assert_eq!(
        err,
        ConfigError::ProdEnvForbidden {
            connection: String::from("main-prod")
        }
    );

    let dup_toml = r#"
[term.sql]
[[term.sql.connections]]
name = "app-db"
env = "local"

[[term.sql.connections]]
name = "app-db"
env = "dev"
"#;
    let err = load_from_str(dup_toml).expect_err("duplicate connection name must be rejected");
    assert_eq!(
        err,
        ConfigError::DuplicateConnectionName {
            name: String::from("app-db")
        }
    );
}

#[test]
fn claude_ranges_prices_and_directory_checks() {
    let invalid_cases = [
        "[term.claude]\ndays = 0\n",
        "[term.claude]\ndays = 366\n",
        "[term.claude]\ncontext_alert_tokens = 999\n",
        "[term.claude.prices.\"m1\"]\ninput = -1.0\noutput = 1.0\ncache_write_5m = 1.0\ncache_write_1h = 1.0\ncache_read = 1.0\n",
        "[term.claude.prices.\"m1\"]\ninput = 1.0\noutput = 1.0\ncache_write_5m = 1.0\ncache_write_1h = 1.0\n",
        "[term.claude.prices.\"   \"]\ninput = 1.0\noutput = 1.0\ncache_write_5m = 1.0\ncache_write_1h = 1.0\ncache_read = 1.0\n",
    ];

    for case in invalid_cases {
        assert!(
            load_from_str(case).is_err(),
            "expected claude validation error for:\n{case}"
        );
    }

    let temp = TempDirGuard::new("claude-dir");
    let missing = temp.path.join("no-claude");
    let missing_toml = format!(
        "[term.claude]\nenabled = true\nprojects_dir = \"{}\"\n",
        missing.display().to_string().replace('\\', "/")
    );
    let err =
        load_from_str(&missing_toml).expect_err("missing projects_dir must error when enabled");
    assert!(matches!(err, ConfigError::MissingDirectory { .. }));
}

#[test]
fn ai_url_validation_and_normalization_cases() {
    let valid_cases = [
        ("http://localhost:1234", "http://localhost:1234/v1"),
        ("http://localhost:1234/", "http://localhost:1234/v1"),
        ("http://localhost:1234/v1", "http://localhost:1234/v1"),
        ("http://localhost:1234/v1/", "http://localhost:1234/v1"),
        ("http://localhost:1234/v1///", "http://localhost:1234/v1"),
        ("http://localhost", "http://localhost/v1"),
        ("HTTP://LOCALHOST:11434", "http://localhost:11434/v1"),
        ("http://127.0.0.1:1234", "http://127.0.0.1:1234/v1"),
        ("http://127.0.0.1:11434/v1/", "http://127.0.0.1:11434/v1"),
        (
            "http://127.0.1.1:8080/proxy/v1/",
            "http://127.0.1.1:8080/proxy/v1",
        ),
        ("http://[::1]:1234", "http://[::1]:1234/v1"),
        ("http://[::1]:1234/v1/", "http://[::1]:1234/v1"),
        ("http://[::1]", "http://[::1]/v1"),
        ("http://::1/v1", "http://[::1]/v1"),
    ];

    for (input, expected) in valid_cases {
        let normalized = normalize_ai_url(input).expect("valid local AI URL");
        assert_eq!(normalized, expected, "input: {input}");

        let toml = format!("[term.ai]\nurl = \"{input}\"\n");
        let loaded = load_from_str(&toml).expect("valid AI URL in config");
        assert_eq!(loaded.config.ai.url, expected);
    }

    let invalid_urls = [
        "",
        "https://localhost:1234/v1",
        "https://127.0.0.1:1234/v1",
        "ftp://localhost:1234/v1",
        "localhost:1234/v1",
        "http://10.0.0.1/v1",
        "http://192.168.1.5:1234/v1",
        "http://0.0.0.0:1234/v1",
        "http://example.com/v1",
        "http://localhost.evil.com/v1",
        "http://127.0.0.1.nip.io/v1",
        "http://0177.0.0.1/v1",
        "http://[fe80::1]:1234/v1",
        "http://[::]:1234/v1",
        "http://localhost:1234/v2",
        "http://localhost:1234/api",
        "http://localhost:0/v1",
        "http://localhost:99999/v1",
        "http://localhost:/v1",
        "http://user:pass@localhost:1234/v1",
        "http://localhost:1234/v1?foo=bar",
        "http://localhost:1234/v1#frag",
        "http://localhost:1234/v1 extra",
    ];

    for bad in invalid_urls {
        assert!(
            normalize_ai_url(bad).is_err(),
            "expected normalize_ai_url to reject '{bad}'"
        );
        if !bad.is_empty() {
            let toml = format!("[term.ai]\nurl = \"{bad}\"\n");
            let err = load_from_str(&toml).expect_err("bad AI URL must fail config validation");
            assert!(matches!(err, ConfigError::InvalidAiUrl { .. }));
        }
    }

    assert!(is_local_ai_host("localhost"));
    assert!(is_local_ai_host("127.0.0.1"));
    assert!(is_local_ai_host("::1"));
    assert!(is_local_ai_host("[::1]"));
    assert!(!is_local_ai_host("10.0.0.1"));
    assert!(!is_local_ai_host("example.com"));
}

#[test]
fn unknown_keys_inside_term_collect_warnings_without_failing() {
    let toml = r#"
[keys]
extra_outside_term = true

[term]
future_root_flag = 123

[term.future_panel]
enabled = true
nested = "ignored_as_one_warning"

[term.ai]
enabled = true
future_ai_opt = "hello"

[term.repos]
future_repos_opt = 5

[term.sql]
future_sql_opt = true

[[term.sql.connections]]
name = "local-ok"
env = "local"
unexpected_conn_key = "warn-me"

[term.claude]
future_claude_opt = false

[term.claude.prices."claude-opus-5-5"]
input = 15.0
output = 75.0
cache_write_5m = 18.75
cache_write_1h = 30.0
cache_read = 1.5
future_price_tier = 2.0
"#;

    let loaded = load_from_str(toml).expect("unknown keys inside [term] must warn, not fail");
    assert!(loaded.config.ai.enabled);
    assert_eq!(loaded.config.sql.connections.len(), 1);

    let paths: Vec<&str> = loaded.warnings.iter().map(|w| w.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "term.ai.future_ai_opt",
            "term.claude.future_claude_opt",
            "term.claude.prices.claude-opus-5-5.future_price_tier",
            "term.future_panel",
            "term.future_root_flag",
            "term.repos.future_repos_opt",
            "term.sql.connections[0].unexpected_conn_key",
            "term.sql.future_sql_opt",
        ]
    );
    for w in &loaded.warnings {
        assert!(!w.to_string().is_empty());
    }
}
