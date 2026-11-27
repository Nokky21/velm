use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use term_repos::scan::discover_repos;

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("term-repos-scan-{label}-{pid}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn discovers_normal_nested_and_worktree_and_skips_ignored_dirs() {
    let temp = TempDirGuard::new("tree");
    let root = &temp.path;

    // Normal repo at depth 1
    let normal = root.join("alpha");
    fs::create_dir_all(normal.join(".git")).unwrap();

    // Nested repo inside normal at depth 2
    let nested = normal.join("nested-sub");
    fs::create_dir_all(nested.join(".git")).unwrap();

    // Too deep (depth 3 when max_depth = 2)
    let too_deep = nested.join("too-deep");
    fs::create_dir_all(too_deep.join(".git")).unwrap();

    // Sibling worktree with .git file at depth 1
    let sibling_wt = root.join("alpha-wt");
    fs::create_dir_all(&sibling_wt).unwrap();
    fs::write(
        sibling_wt.join(".git"),
        "gitdir: /tmp/alpha/.git/worktrees/wt\n",
    )
    .unwrap();

    // Repo inside node_modules (must be skipped)
    let nm_repo = root.join("node_modules").join("pkg");
    fs::create_dir_all(nm_repo.join(".git")).unwrap();

    // Repo inside target (must be skipped)
    let target_repo = root.join("target").join("dep");
    fs::create_dir_all(target_repo.join(".git")).unwrap();

    // Repo inside hidden dir (must be skipped)
    let hidden_repo = root.join(".hidden").join("secret");
    fs::create_dir_all(hidden_repo.join(".git")).unwrap();

    // Worktree inside .worktrees (depth 2)
    let dot_wt = root.join(".worktrees").join("feat-wt");
    fs::create_dir_all(&dot_wt).unwrap();
    fs::write(
        dot_wt.join(".git"),
        "gitdir: /tmp/alpha/.git/worktrees/feat-wt\n",
    )
    .unwrap();

    let skip = vec![
        String::from("node_modules"),
        String::from("target"),
        String::from(".worktrees"),
    ];

    let without_wt = discover_repos(std::slice::from_ref(root), 2, false, &skip);
    let names_without: Vec<(&str, bool)> = without_wt
        .iter()
        .map(|r| (r.name.as_str(), r.worktree))
        .collect();
    assert_eq!(
        names_without,
        vec![("alpha", false), ("alpha-wt", true), ("nested-sub", false)]
    );

    let with_wt = discover_repos(std::slice::from_ref(root), 2, true, &skip);
    let names_with: Vec<(&str, bool)> = with_wt
        .iter()
        .map(|r| (r.name.as_str(), r.worktree))
        .collect();
    assert_eq!(
        names_with,
        vec![
            ("feat-wt", true),
            ("alpha", false),
            ("alpha-wt", true),
            ("nested-sub", false),
        ]
    );
}
