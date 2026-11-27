use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use term_repos::git::{inspect_repos, inspect_single_repo};
use term_repos::parse::RepoStatus;
use term_repos::remote::{is_credential_prompt, query_remote_default_branch, RemoteResult};
use term_repos::scan::DiscoveredRepo;

static PATH_LOCK: Mutex<()> = Mutex::new(());

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
        let path = std::env::temp_dir().join(format!("term-repos-int-{label}-{pid}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct PathEnvGuard {
    prev: Option<OsString>,
}

impl PathEnvGuard {
    fn prepend(dir: &Path) -> Self {
        let prev = std::env::var_os("PATH");
        let mut paths = vec![dir.to_path_buf()];
        if let Some(ref existing) = prev {
            paths.extend(std::env::split_paths(existing));
        }
        let joined = std::env::join_paths(paths).expect("join PATH");
        std::env::set_var("PATH", joined);
        Self { prev }
    }
}

impl Drop for PathEnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(val) => std::env::set_var("PATH", val),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn run_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Test User")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test User")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {:?} in {} failed: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[cfg(windows)]
fn build_fake_git(fake_bin: &Path, rust_body: &str) {
    let src = fake_bin.join("fake_git.rs");
    let exe = fake_bin.join("git.exe");
    fs::write(&src, rust_body).expect("write fake_git.rs");
    let mut cmd = Command::new("rustc");
    cmd.args(["--edition", "2021", "-o"]).arg(&exe).arg(&src);
    for var in [
        "CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER",
        "CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER",
    ] {
        if let Ok(linker) = std::env::var(var) {
            if !linker.is_empty() {
                cmd.arg(format!("-Clinker={linker}"));
                break;
            }
        }
    }
    let out = cmd.output().expect("run rustc for fake git.exe");
    assert!(
        out.status.success(),
        "rustc failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn slow_fake_git_times_out_and_returns_null_fields() {
    let _lock = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = TempDirGuard::new("slow-git");
    let fake_bin = temp.path.join("bin");
    let repo_dir = temp.path.join("slow-repo");
    fs::create_dir_all(&fake_bin).unwrap();
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    #[cfg(windows)]
    {
        build_fake_git(
            &fake_bin,
            "fn main() { std::thread::sleep(std::time::Duration::from_secs(5)); }\n",
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = fake_bin.join("git");
        fs::write(&script, "#!/bin/sh\nsleep 5\n").unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
    }

    let _path_guard = PathEnvGuard::prepend(&fake_bin);
    let repo = DiscoveredRepo {
        path: repo_dir,
        name: String::from("slow-repo"),
        worktree: false,
    };

    let status = inspect_single_repo(&repo, Duration::from_millis(200));
    assert!(status.timed_out);
    assert_eq!(status.branch, None);
    assert_eq!(status.modified, None);
    assert_eq!(status.worktree, None);
    assert_eq!(status.error, None);
}

#[test]
fn error_fake_git_truncates_stderr_to_200_chars() {
    let _lock = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = TempDirGuard::new("err-git");
    let fake_bin = temp.path.join("bin");
    let repo_dir = temp.path.join("bad-repo");
    fs::create_dir_all(&fake_bin).unwrap();
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    let long_err = "E".repeat(260);
    #[cfg(windows)]
    {
        build_fake_git(
            &fake_bin,
            &format!("fn main() {{ eprintln!(\"{long_err}\"); std::process::exit(128); }}\n"),
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = fake_bin.join("git");
        fs::write(
            &script,
            format!("#!/bin/sh\necho '{long_err}' >&2\nexit 128\n"),
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
    }

    let _path_guard = PathEnvGuard::prepend(&fake_bin);
    let repos = vec![DiscoveredRepo {
        path: repo_dir,
        name: String::from("bad-repo"),
        worktree: false,
    }];

    let statuses = inspect_repos(&repos, 2, 2000);
    assert_eq!(statuses.len(), 1);
    assert!(!statuses[0].timed_out);
    let err = statuses[0].error.as_deref().expect("error present");
    assert_eq!(err.chars().count(), 200);
}

#[test]
fn json_cli_integration_covers_real_git_repos_and_exit_codes() {
    let _lock = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = TempDirGuard::new("cli-json");
    let root = temp.path.join("roots");
    fs::create_dir_all(&root).unwrap();

    // 1. Bare upstream + clean repo (repo_a)
    let bare = temp.path.join("origin.git");
    fs::create_dir_all(&bare).unwrap();
    run_git(&bare, &["init", "--bare", "-b", "main"]);

    let repo_a = root.join("a-clean");
    fs::create_dir_all(&repo_a).unwrap();
    run_git(&repo_a, &["init", "-b", "main"]);
    fs::write(repo_a.join("README.md"), "hello\n").unwrap();
    run_git(&repo_a, &["add", "README.md"]);
    run_git(&repo_a, &["commit", "-m", "init"]);
    let bare_url = format!(
        "file:///{}",
        bare.to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    run_git(&repo_a, &["remote", "add", "origin", &bare_url]);
    run_git(&repo_a, &["push", "-u", "origin", "main"]);

    // 2. Dirty repo without remote + detached HEAD check via another repo
    let repo_b = root.join("b-dirty");
    fs::create_dir_all(&repo_b).unwrap();
    run_git(&repo_b, &["init", "-b", "main"]);
    fs::write(repo_b.join("f.txt"), "one\n").unwrap();
    run_git(&repo_b, &["add", "f.txt"]);
    run_git(&repo_b, &["commit", "-m", "first"]);
    fs::write(repo_b.join("f.txt"), "two\n").unwrap();
    fs::write(repo_b.join("untracked.txt"), "new\n").unwrap();

    // 3. Detached HEAD repo
    let repo_c = root.join("c-detached");
    fs::create_dir_all(&repo_c).unwrap();
    run_git(&repo_c, &["init", "-b", "main"]);
    fs::write(repo_c.join("x.txt"), "x\n").unwrap();
    run_git(&repo_c, &["add", "x.txt"]);
    run_git(&repo_c, &["commit", "-m", "one"]);
    run_git(&repo_c, &["checkout", "--detach", "HEAD"]);

    // 4. Empty repo (no commits)
    let repo_d = root.join("d-empty");
    fs::create_dir_all(&repo_d).unwrap();
    run_git(&repo_d, &["init", "-b", "main"]);

    // 5. Worktree of repo_a
    let repo_e = root.join("e-worktree");
    run_git(
        &repo_a,
        &[
            "worktree",
            "add",
            repo_e.to_str().unwrap(),
            "-b",
            "wt-branch",
        ],
    );

    let cfg_path = temp.path.join("config.toml");
    fs::write(&cfg_path, "[term.repos]\nenabled = false\n").unwrap();

    let bin = env!("CARGO_BIN_EXE_term-repos");
    let out = Command::new(bin)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "--json",
            "--root",
            root.to_str().unwrap(),
        ])
        .output()
        .expect("run term-repos --json");

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let parsed: Vec<RepoStatus> = serde_json::from_slice(&out.stdout).expect("valid JSON output");
    assert_eq!(parsed.len(), 5);

    let names: Vec<&str> = parsed.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["a-clean", "b-dirty", "c-detached", "d-empty", "e-worktree"]
    );

    // Check sorted by path
    for win in parsed.windows(2) {
        assert!(win[0].path <= win[1].path);
    }

    // a-clean
    assert_eq!(parsed[0].branch.as_deref(), Some("main"));
    assert_eq!(parsed[0].upstream.as_deref(), Some("origin/main"));
    assert_eq!(parsed[0].ahead, Some(0));
    assert_eq!(parsed[0].behind, Some(0));
    assert_eq!(parsed[0].worktree, Some(false));
    assert_eq!(parsed[0].empty, Some(false));
    assert!(parsed[0].last_commit_at.is_some());

    // b-dirty
    assert_eq!(parsed[1].upstream, None);
    assert_eq!(parsed[1].modified, Some(1));
    assert_eq!(parsed[1].untracked, Some(1));

    // c-detached
    assert_eq!(parsed[2].branch, None);
    assert_eq!(parsed[2].detached, Some(true));
    assert_eq!(parsed[2].oid.as_ref().map(|s| s.len()), Some(7));

    // d-empty
    assert_eq!(parsed[3].empty, Some(true));
    assert_eq!(parsed[3].oid, None);
    assert_eq!(parsed[3].last_commit_at, None);

    // e-worktree
    assert_eq!(parsed[4].worktree, Some(true));
    assert_eq!(parsed[4].branch.as_deref(), Some("wt-branch"));

    // Remote query against local file:// bare repo
    let remote_res = query_remote_default_branch(&repo_a, 5000);
    assert_eq!(
        remote_res,
        RemoteResult::DefaultBranch(String::from("main"))
    );

    // Invalid config / missing root -> exit 2
    let bad_root = temp.path.join("does-not-exist");
    let bad_out = Command::new(bin)
        .args([
            "--config",
            cfg_path.to_str().unwrap(),
            "--json",
            "--root",
            bad_root.to_str().unwrap(),
        ])
        .output()
        .expect("run term-repos with missing root");
    assert_eq!(bad_out.status.code(), Some(2));
}

#[test]
fn detects_credential_prompt_on_remote_query() {
    assert!(is_credential_prompt(
        "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
    ));

    let _lock = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp = TempDirGuard::new("cred-remote");
    let fake_bin = temp.path.join("bin");
    let repo_dir = temp.path.join("repo");
    fs::create_dir_all(&fake_bin).unwrap();
    fs::create_dir_all(repo_dir.join(".git")).unwrap();

    #[cfg(windows)]
    {
        build_fake_git(
            &fake_bin,
            "fn main() { eprintln!(\"fatal: could not read Username: terminal prompts disabled\"); std::process::exit(128); }\n",
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = fake_bin.join("git");
        fs::write(
            &script,
            "#!/bin/sh\necho 'fatal: could not read Username: terminal prompts disabled' >&2\nexit 128\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();
    }

    let _path_guard = PathEnvGuard::prepend(&fake_bin);
    let res = query_remote_default_branch(&repo_dir, 3000);
    assert_eq!(res, RemoteResult::CredentialsRequired);
    assert_eq!(res.display_text(), "el remoto pide credenciales");
}
