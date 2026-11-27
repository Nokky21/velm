use std::path::Path;

use term_repos::parse::{
    format_unix_timestamp_utc, parse_porcelain_v2, parse_worktree_gitdir, truncate_chars,
    RepoStatus,
};
use term_repos::scan::DiscoveredRepo;
use term_repos::ui::{format_relative_age, AppState};

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read fixture {}: {err}", path.display()))
}

#[test]
fn parses_clean_fixture() {
    let raw = fixture("git.status.porcelain-v2.clean.txt");
    let st = parse_porcelain_v2(
        Path::new("/tmp/clean"),
        "clean",
        false,
        &raw,
        Some("2026-11-24T08:58:12Z".to_owned()),
    );

    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.detached, Some(false));
    assert_eq!(st.oid.as_deref(), Some("c92bca3"));
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!(st.upstream_gone, Some(false));
    assert_eq!(st.ahead, Some(0));
    assert_eq!(st.behind, Some(0));
    assert_eq!(st.modified, Some(0));
    assert_eq!(st.untracked, Some(0));
    assert_eq!(st.conflicted, Some(0));
    assert_eq!(st.stashes, Some(0));
    assert_eq!(st.empty, Some(false));
    assert_eq!(st.worktree, Some(false));
    assert!(!st.timed_out);
    assert_eq!(st.error, None);
    assert!(!st.is_dirty());
}

#[test]
fn parses_dirty_fixture() {
    let raw = fixture("git.status.porcelain-v2.dirty.txt");
    let st = parse_porcelain_v2(Path::new("/tmp/dirty"), "dirty", false, &raw, None);

    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.oid.as_deref(), Some("e846dd3"));
    assert_eq!(st.upstream.as_deref(), Some("origin/main"));
    assert_eq!(st.upstream_gone, Some(false));
    assert_eq!(st.ahead, Some(1));
    assert_eq!(st.behind, Some(0));
    assert_eq!(st.stashes, Some(1));
    assert_eq!(st.modified, Some(2));
    assert_eq!(st.untracked, Some(1));
    assert_eq!(st.conflicted, Some(0));
    assert!(st.is_dirty());
}

#[test]
fn parses_conflict_fixture() {
    let raw = fixture("git.status.porcelain-v2.conflict.txt");
    let st = parse_porcelain_v2(Path::new("/tmp/conflict"), "conflict", false, &raw, None);

    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.oid.as_deref(), Some("2b503ae"));
    assert_eq!(st.ahead, Some(1));
    assert_eq!(st.behind, Some(0));
    assert_eq!(st.conflicted, Some(1));
    assert_eq!(st.modified, Some(0));
    assert_eq!(st.untracked, Some(0));
    assert!(st.is_dirty());
}

#[test]
fn parses_detached_fixture() {
    let raw = fixture("git.status.porcelain-v2.detached.txt");
    let st = parse_porcelain_v2(Path::new("/tmp/detached"), "detached", false, &raw, None);

    assert_eq!(st.branch, None);
    assert_eq!(st.detached, Some(true));
    assert_eq!(st.oid.as_deref(), Some("c92bca3"));
    assert_eq!(st.upstream, None);
    assert_eq!(st.upstream_gone, Some(false));
    assert_eq!(st.ahead, None);
    assert_eq!(st.behind, None);
}

#[test]
fn parses_empty_fixture() {
    let raw = fixture("git.status.porcelain-v2.empty.txt");
    let st = parse_porcelain_v2(
        Path::new("/tmp/empty"),
        "empty",
        false,
        &raw,
        Some("2026-11-24T08:58:12Z".to_owned()),
    );

    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.empty, Some(true));
    assert_eq!(st.oid, None);
    assert_eq!(st.last_commit_at, None);
}

#[test]
fn parses_gone_fixture() {
    let raw = fixture("git.status.porcelain-v2.gone.txt");
    let st = parse_porcelain_v2(Path::new("/tmp/gone"), "gone", false, &raw, None);

    assert_eq!(st.branch.as_deref(), Some("feat/x"));
    assert_eq!(st.upstream.as_deref(), Some("origin/feat/x"));
    assert_eq!(st.upstream_gone, Some(true));
    assert_eq!(st.ahead, None);
    assert_eq!(st.behind, None);
    assert!(st.is_dirty());
}

#[test]
fn parses_noup_fixture() {
    let raw = fixture("git.status.porcelain-v2.noup.txt");
    let st = parse_porcelain_v2(Path::new("/tmp/noup"), "noup", false, &raw, None);

    assert_eq!(st.branch.as_deref(), Some("main"));
    assert_eq!(st.oid.as_deref(), Some("bdb037d"));
    assert_eq!(st.upstream, None);
    assert_eq!(st.upstream_gone, Some(false));
    assert_eq!(st.ahead, None);
    assert_eq!(st.behind, None);
    assert!(!st.is_dirty());
}

#[test]
fn parses_worktree_dotgit_file_fixture() {
    let raw = fixture("git.worktree-dotgit-file.txt");
    assert_eq!(
        parse_worktree_gitdir(&raw),
        Some("/Users/x/Desktop/WORK/ejemplo/.git/worktrees/wt")
    );
    assert_eq!(parse_worktree_gitdir("not a gitdir file\n"), None);
}

#[test]
fn formats_unix_timestamps_and_truncates_errors() {
    assert_eq!(format_unix_timestamp_utc(0), "1970-01-01T00:00:00Z");
    assert_eq!(
        format_unix_timestamp_utc(1_795_510_692),
        "2026-11-24T08:58:12Z"
    );

    let long = "x".repeat(250);
    assert_eq!(truncate_chars(&long, 200).chars().count(), 200);

    let timed_out = RepoStatus::timed_out(Path::new("/tmp/slow"), "slow");
    let json = serde_json::to_value(&timed_out).expect("serialize timedOut");
    assert_eq!(json["timedOut"], serde_json::Value::Bool(true));
    assert!(json["branch"].is_null());
    assert!(json["modified"].is_null());
    assert!(json["worktree"].is_null());
    assert!(json["error"].is_null());
}

#[test]
fn ui_state_filters_and_formats_ages() {
    let discovered = vec![
        DiscoveredRepo {
            path: "/tmp/alpha".into(),
            name: "alpha".to_owned(),
            worktree: false,
        },
        DiscoveredRepo {
            path: "/tmp/beta".into(),
            name: "beta".to_owned(),
            worktree: true,
        },
    ];
    let mut state = AppState::from_discovered(&discovered);
    assert_eq!(state.visible_indices(), vec![0, 1]);

    let clean = parse_porcelain_v2(
        Path::new("/tmp/alpha"),
        "alpha",
        false,
        &fixture("git.status.porcelain-v2.clean.txt"),
        None,
    );
    let dirty = parse_porcelain_v2(
        Path::new("/tmp/beta"),
        "beta",
        true,
        &fixture("git.status.porcelain-v2.dirty.txt"),
        None,
    );
    state.apply_status(clean);
    state.apply_status(dirty);

    state.toggle_dirty_filter();
    assert_eq!(state.visible_indices(), vec![1]);

    state.toggle_dirty_filter();
    state.set_search_query("alp".to_owned());
    assert_eq!(state.visible_indices(), vec![0]);

    assert_eq!(
        format_relative_age("2026-11-24T08:58:12Z", 1_795_510_692 + 180),
        "3m"
    );
}
