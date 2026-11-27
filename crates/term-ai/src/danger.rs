/// Analyzes a shell command by segments and reports whether any segment is dangerous
/// according to CONTRATO §B.1.
pub fn is_dangerous(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    let pipelines = parse_pipelines(trimmed);
    for pipeline in &pipelines {
        if pipeline_is_dangerous(pipeline) {
            return true;
        }
    }

    // Check any nested subshell / command substitution fragments extracted during parsing.
    for sub in extract_command_substitutions(trimmed) {
        if is_dangerous(&sub) {
            return true;
        }
    }

    false
}

/// Comments every line of a command with `# ` for dangerous suggestions (CONTRATO §B.1).
pub fn comment_command(command: &str) -> String {
    command
        .lines()
        .map(|line| format!("# {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone)]
struct Word {
    text: String,
    quoted: bool,
}

#[derive(Debug, Clone, Default)]
struct Segment {
    words: Vec<Word>,
    redirects_to_dev_non_null: bool,
}

#[derive(Debug, Clone)]
struct Pipeline {
    segments: Vec<Segment>,
}

fn pipeline_is_dangerous(pipeline: &Pipeline) -> bool {
    // Check cross-segment pipe rule: curl/wget | ... | sh/bash/zsh
    let mut saw_downloader = false;
    for seg in &pipeline.segments {
        if seg.redirects_to_dev_non_null {
            return true;
        }
        if segment_is_dangerous(seg) {
            return true;
        }
        if segment_invokes_downloader(seg) {
            saw_downloader = true;
        } else if saw_downloader && segment_invokes_shell(seg) {
            return true;
        }
    }

    // Check piped SQL: echo/printf "DROP ..." | psql ...
    let pipes_to_sql_cli = pipeline.segments.iter().skip(1).any(segment_is_sql_cli);
    if pipes_to_sql_cli {
        for seg in &pipeline.segments {
            for w in &seg.words {
                if sql_text_is_dangerous(&w.text) {
                    return true;
                }
            }
        }
    }

    false
}

fn segment_is_dangerous(seg: &Segment) -> bool {
    let Some((cmd_name, args)) = unwrap_command(&seg.words) else {
        return false;
    };

    check_command_and_args(&cmd_name, args, &seg.words)
}

fn unwrap_command(words: &[Word]) -> Option<(String, &[Word])> {
    let mut idx = 0;
    while idx < words.len() {
        let w = &words[idx];
        // Skip leading VAR=value environment assignments (unquoted).
        if !w.quoted && is_env_assignment(&w.text) {
            idx += 1;
            continue;
        }
        let base = command_basename(&w.text);
        match base.as_str() {
            "command" | "builtin" | "nohup" | "time" => {
                idx += 1;
                // Skip simple flags like `command -p`
                while idx < words.len() && words[idx].text.starts_with('-') {
                    idx += 1;
                }
                continue;
            }
            "env" => {
                idx += 1;
                while idx < words.len() {
                    let arg = &words[idx].text;
                    if arg == "-i" || arg == "-0" || arg == "--ignore-environment" {
                        idx += 1;
                    } else if arg == "-u" || arg == "-C" || arg == "-S" {
                        idx += 2;
                    } else if arg.starts_with('-') || is_env_assignment(arg) {
                        idx += 1;
                    } else {
                        break;
                    }
                }
                continue;
            }
            _ => return Some((base, &words[idx + 1..])),
        }
    }
    None
}

fn check_command_and_args(cmd: &str, args: &[Word], _full_words: &[Word]) -> bool {
    match cmd {
        "sudo" | "dd" | "killall" => true,
        "mkfs" => true,
        c if c.starts_with("mkfs.") => true,
        "rm" => rm_args_dangerous(args),
        "xargs" => xargs_is_dangerous(args),
        "find" => find_is_dangerous(args),
        "git" => git_is_dangerous(args),
        "diskutil" => diskutil_is_dangerous(args),
        "chmod" | "chown" => has_recursive_flag(args),
        "kill" => kill_is_dangerous(args),
        "launchctl" => args
            .first()
            .is_some_and(|w| w.text.eq_ignore_ascii_case("unload")),
        "drop" | "truncate" => true,
        "delete" | "update" => !args.iter().any(|w| w.text.eq_ignore_ascii_case("where")),
        c if is_sql_cli_name(c) => sql_cli_args_dangerous(args),
        _ => false,
    }
}

fn rm_args_dangerous(args: &[Word]) -> bool {
    for arg in args {
        if arg.text == "--" {
            break;
        }
        let lower = arg.text.to_ascii_lowercase();
        if lower == "--recursive" || lower == "--force" {
            return true;
        }
        if lower.starts_with('-')
            && !lower.starts_with("--")
            && lower[1..].chars().any(|c| c == 'r' || c == 'f')
        {
            return true;
        }
    }
    false
}

fn xargs_is_dangerous(args: &[Word]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let t = &args[i].text;
        if t == "--" {
            i += 1;
            break;
        }
        if !t.starts_with('-') {
            break;
        }
        // Options that consume a separate argument in xargs
        if matches!(
            t.as_str(),
            "-I" | "-i" | "-L" | "-l" | "-n" | "-P" | "-s" | "-E" | "-e" | "-d" | "-a" | "-J"
        ) {
            i += 2;
        } else {
            i += 1;
        }
    }

    if i >= args.len() {
        return false;
    }

    let wrapped_cmd = command_basename(&args[i].text);
    check_command_and_args(&wrapped_cmd, &args[i + 1..], &args[i..])
}

fn find_is_dangerous(args: &[Word]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let lower = args[i].text.to_ascii_lowercase();
        if lower == "-delete" {
            return true;
        }
        if matches!(lower.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir") {
            let start = i + 1;
            let mut end = start;
            while end < args.len() && args[end].text != ";" && args[end].text != "+" {
                end += 1;
            }
            if start < end {
                let exec_cmd = command_basename(&args[start].text);
                if exec_cmd == "rm" && rm_args_dangerous(&args[start + 1..end]) {
                    return true;
                }
                if exec_cmd == "sudo" {
                    return true;
                }
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    false
}

fn git_is_dangerous(args: &[Word]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let t = &args[i].text;
        if !t.starts_with('-') {
            break;
        }
        if matches!(
            t.as_str(),
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--super-prefix"
        ) {
            i += 2;
        } else {
            i += 1;
        }
    }

    if i >= args.len() {
        return false;
    }

    let sub = args[i].text.to_ascii_lowercase();
    let rest = &args[i + 1..];
    match sub.as_str() {
        "push" => git_push_dangerous(rest),
        "reset" => rest.iter().any(|w| w.text.eq_ignore_ascii_case("--hard")),
        "clean" => git_clean_dangerous(rest),
        "checkout" => rest.iter().any(|w| w.text == "--"),
        "restore" => git_restore_dangerous(rest),
        _ => false,
    }
}

fn git_push_dangerous(args: &[Word]) -> bool {
    let mut i = 0;
    let mut after_double_dash = false;
    while i < args.len() {
        let t = &args[i].text;
        let lower = t.to_ascii_lowercase();
        if !after_double_dash && t == "--" {
            after_double_dash = true;
            i += 1;
            continue;
        }
        if !after_double_dash && t.starts_with('-') {
            if lower == "--force"
                || lower == "--force-with-lease"
                || lower.starts_with("--force-with-lease=")
            {
                return true;
            }
            if !lower.starts_with("--") && lower[1..].contains('f') {
                return true;
            }
            if matches!(
                lower.as_str(),
                "-o" | "--push-option" | "--repo" | "--receive-pack" | "--exec"
            ) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        // Positional arg (remote or refspec)
        if t.starts_with('+') && t.len() > 1 {
            return true;
        }
        i += 1;
    }
    false
}

fn git_clean_dangerous(args: &[Word]) -> bool {
    for w in args {
        if w.text == "--" {
            break;
        }
        let lower = w.text.to_ascii_lowercase();
        if lower == "--force" {
            return true;
        }
        if lower.starts_with('-') && !lower.starts_with("--") && lower[1..].contains('f') {
            return true;
        }
    }
    false
}

fn git_restore_dangerous(args: &[Word]) -> bool {
    let mut has_staged = false;
    let mut has_worktree = false;
    for w in args {
        if w.text == "--" {
            break;
        }
        let lower = w.text.to_ascii_lowercase();
        if lower == "--staged"
            || (lower.starts_with('-') && !lower.starts_with("--") && w.text[1..].contains('S'))
        {
            has_staged = true;
        }
        if lower == "--worktree"
            || (lower.starts_with('-') && !lower.starts_with("--") && w.text[1..].contains('W'))
        {
            has_worktree = true;
        }
    }
    !has_staged || has_worktree
}

fn diskutil_is_dangerous(args: &[Word]) -> bool {
    let Some(sub) = args.first() else {
        return false;
    };
    let lower = sub.text.to_ascii_lowercase();
    lower.starts_with("erase") || lower.starts_with("partition")
}

fn has_recursive_flag(args: &[Word]) -> bool {
    for w in args {
        if w.text == "--" {
            break;
        }
        let lower = w.text.to_ascii_lowercase();
        if lower == "--recursive" {
            return true;
        }
        if w.text.starts_with('-') && !w.text.starts_with("--") && w.text[1..].contains('R') {
            return true;
        }
    }
    false
}

fn kill_is_dangerous(args: &[Word]) -> bool {
    let mut has_sigkill = false;
    let mut has_minus_one = false;
    let mut i = 0;
    while i < args.len() {
        let lower = args[i].text.to_ascii_lowercase();
        if matches!(lower.as_str(), "-9" | "-kill" | "-sigkill") {
            has_sigkill = true;
        } else if (lower == "-s" || lower == "--signal") && i + 1 < args.len() {
            let sig = args[i + 1].text.to_ascii_lowercase();
            if matches!(sig.as_str(), "9" | "kill" | "sigkill") {
                has_sigkill = true;
            }
            i += 2;
            continue;
        } else if lower == "-1" {
            has_minus_one = true;
        }
        i += 1;
    }
    has_sigkill && has_minus_one
}

fn segment_invokes_downloader(seg: &Segment) -> bool {
    let Some((cmd, _)) = unwrap_command(&seg.words) else {
        return false;
    };
    matches!(cmd.as_str(), "curl" | "wget")
}

fn segment_invokes_shell(seg: &Segment) -> bool {
    let Some((cmd, args)) = unwrap_command(&seg.words) else {
        return false;
    };
    if matches!(cmd.as_str(), "sh" | "bash" | "zsh") {
        return true;
    }
    if cmd == "sudo" {
        if let Some((inner, _)) = unwrap_command(args) {
            return matches!(inner.as_str(), "sh" | "bash" | "zsh");
        }
    }
    false
}

fn segment_is_sql_cli(seg: &Segment) -> bool {
    let Some((cmd, _)) = unwrap_command(&seg.words) else {
        return false;
    };
    is_sql_cli_name(&cmd)
}

fn is_sql_cli_name(cmd: &str) -> bool {
    matches!(
        cmd,
        "psql" | "sqlite3" | "mysql" | "mariadb" | "duckdb" | "pgcli" | "usql"
    )
}

fn sql_cli_args_dangerous(args: &[Word]) -> bool {
    for w in args {
        if sql_text_is_dangerous(&w.text) {
            return true;
        }
    }
    false
}

fn sql_text_is_dangerous(sql: &str) -> bool {
    for stmt in sql.split(';') {
        let tokens: Vec<String> = stmt
            .split_whitespace()
            .map(|t| {
                t.trim_matches(|c: char| c == '(' || c == ')')
                    .to_ascii_lowercase()
            })
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.is_empty() {
            continue;
        }
        // Check first keyword or after CTEs / EXPLAIN
        if tokens.iter().any(|t| t == "drop" || t == "truncate") {
            return true;
        }
        let has_delete_or_update = tokens.iter().any(|t| t == "delete" || t == "update");
        if has_delete_or_update && !tokens.iter().any(|t| t == "where") {
            return true;
        }
    }
    false
}

fn command_basename(raw: &str) -> String {
    let base = raw.rsplit('/').next().unwrap_or(raw);
    base.to_ascii_lowercase()
}

fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .bytes()
            .enumerate()
            .all(|(idx, b)| b == b'_' || b.is_ascii_alphabetic() || (idx > 0 && b.is_ascii_digit()))
}

fn parse_pipelines(input: &str) -> Vec<Pipeline> {
    let mut pipelines = Vec::new();
    let mut current_pipeline = Pipeline {
        segments: Vec::new(),
    };
    let mut current_seg = Segment::default();

    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];

        if ch.is_ascii_whitespace() && ch != '\n' {
            i += 1;
            continue;
        }

        // Unquoted comment
        if ch == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Command separators (outside quotes)
        if ch == '\n' || ch == ';' {
            finish_segment(&mut current_pipeline, &mut current_seg);
            finish_pipeline(&mut pipelines, &mut current_pipeline);
            i += 1;
            continue;
        }

        if ch == '&' {
            if i + 1 < chars.len() && chars[i + 1] == '&' {
                finish_segment(&mut current_pipeline, &mut current_seg);
                finish_pipeline(&mut pipelines, &mut current_pipeline);
                i += 2;
                continue;
            }
            if i + 1 < chars.len() && chars[i + 1] == '>' {
                i += 2;
                if i < chars.len() && chars[i] == '>' {
                    i += 1;
                }
                while i < chars.len() && chars[i].is_ascii_whitespace() && chars[i] != '\n' {
                    i += 1;
                }
                let (target, next_i) = read_word(&chars, i);
                i = next_i;
                if is_dangerous_dev_target(&target.text) {
                    current_seg.redirects_to_dev_non_null = true;
                }
                continue;
            }
            finish_segment(&mut current_pipeline, &mut current_seg);
            finish_pipeline(&mut pipelines, &mut current_pipeline);
            i += 1;
            continue;
        }

        if ch == '|' {
            if i + 1 < chars.len() && chars[i + 1] == '|' {
                finish_segment(&mut current_pipeline, &mut current_seg);
                finish_pipeline(&mut pipelines, &mut current_pipeline);
                i += 2;
                continue;
            }
            finish_segment(&mut current_pipeline, &mut current_seg);
            if i + 1 < chars.len() && chars[i + 1] == '&' {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }

        // Subshell parens/braces as segment boundaries
        if matches!(ch, '(' | ')' | '{' | '}') {
            finish_segment(&mut current_pipeline, &mut current_seg);
            if ch == ')' || ch == '}' {
                finish_pipeline(&mut pipelines, &mut current_pipeline);
            }
            i += 1;
            continue;
        }

        // Output redirections: >, >>, >|, 1>, 2>, 2>&1, >&2, etc.
        if let Some((after_redir, is_fd_dup)) = match_output_redirect(&chars, i) {
            i = after_redir;
            while i < chars.len() && chars[i].is_ascii_whitespace() && chars[i] != '\n' {
                i += 1;
            }
            let (target, next_i) = read_word(&chars, i);
            i = next_i;
            if !is_fd_dup && is_dangerous_dev_target(&target.text) {
                current_seg.redirects_to_dev_non_null = true;
            }
            continue;
        }

        // Input fd duplication: <&N / 0<&1 (do not let `&` split pipeline)
        if let Some(after_in_dup) = match_input_fd_dup(&chars, i) {
            i = after_in_dup;
            while i < chars.len() && chars[i].is_ascii_whitespace() && chars[i] != '\n' {
                i += 1;
            }
            let (_, next_i) = read_word(&chars, i);
            i = next_i;
            continue;
        }

        let (word, next_i) = read_word(&chars, i);
        i = next_i;
        if !word.text.is_empty() {
            current_seg.words.push(word);
        }
    }

    finish_segment(&mut current_pipeline, &mut current_seg);
    finish_pipeline(&mut pipelines, &mut current_pipeline);
    pipelines
}

fn finish_segment(pipeline: &mut Pipeline, seg: &mut Segment) {
    if !seg.words.is_empty() || seg.redirects_to_dev_non_null {
        pipeline.segments.push(std::mem::take(seg));
    }
}

fn finish_pipeline(pipelines: &mut Vec<Pipeline>, pipeline: &mut Pipeline) {
    if !pipeline.segments.is_empty() {
        pipelines.push(std::mem::replace(
            pipeline,
            Pipeline {
                segments: Vec::new(),
            },
        ));
    }
}

fn match_output_redirect(chars: &[char], i: usize) -> Option<(usize, bool)> {
    let mut pos = i;
    if chars[pos].is_ascii_digit() && pos + 1 < chars.len() && chars[pos + 1] == '>' {
        pos += 1;
    }
    if chars.get(pos) != Some(&'>') {
        return None;
    }
    pos += 1;
    if chars.get(pos) == Some(&'&') {
        return Some((pos + 1, true));
    }
    if matches!(chars.get(pos), Some(&'>') | Some(&'|')) {
        pos += 1;
    }
    Some((pos, false))
}

fn match_input_fd_dup(chars: &[char], i: usize) -> Option<usize> {
    let mut pos = i;
    if chars[pos].is_ascii_digit() && pos + 1 < chars.len() && chars[pos + 1] == '<' {
        pos += 1;
    }
    if chars.get(pos) == Some(&'<') && chars.get(pos + 1) == Some(&'&') {
        return Some(pos + 2);
    }
    None
}

fn is_dangerous_dev_target(target: &str) -> bool {
    let lower = target.to_ascii_lowercase();
    lower.starts_with("/dev/") && lower != "/dev/null"
}

fn read_word(chars: &[char], mut i: usize) -> (Word, usize) {
    let mut text = String::new();
    let mut quoted = false;

    while i < chars.len() {
        let ch = chars[i];
        if ch.is_ascii_whitespace() || matches!(ch, ';' | '&' | '|' | '(' | ')' | '>') {
            break;
        }
        match ch {
            '\'' => {
                quoted = true;
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    text.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() && chars[i] == '\'' {
                    i += 1;
                }
            }
            '"' => {
                quoted = true;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        let next = chars[i + 1];
                        if matches!(next, '"' | '\\' | '$' | '`' | '\n') {
                            if next != '\n' {
                                text.push(next);
                            }
                            i += 2;
                            continue;
                        }
                    }
                    text.push(chars[i]);
                    i += 1;
                }
                if i < chars.len() && chars[i] == '"' {
                    i += 1;
                }
            }
            '\\' => {
                if i + 1 < chars.len() {
                    let next = chars[i + 1];
                    if next != '\n' {
                        text.push(next);
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            _ => {
                text.push(ch);
                i += 1;
            }
        }
    }

    (Word { text, quoted }, i)
}

fn extract_command_substitutions(input: &str) -> Vec<String> {
    let mut subs = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < chars.len() {
        let ch = chars[i];
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if ch == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = true;
            i += 1;
            continue;
        }
        if ch == '"' {
            in_double = !in_double;
            i += 1;
            continue;
        }
        if ch == '$' && i + 1 < chars.len() && chars[i + 1] == '(' {
            let start = i + 2;
            if let Some((inner, end)) = scan_balanced_parens(&chars, start) {
                subs.push(inner);
                i = end;
                continue;
            }
        }
        if ch == '`' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '`' {
                if chars[j] == '\\' && j + 1 < chars.len() {
                    j += 2;
                } else {
                    j += 1;
                }
            }
            if j < chars.len() {
                subs.push(chars[start..j].iter().collect());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }

    subs
}

fn scan_balanced_parens(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut depth = 1usize;
    let mut i = start;
    let mut in_single = false;
    let mut in_double = false;

    while i < chars.len() {
        let ch = chars[i];
        if in_single {
            if ch == '\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if ch == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = true;
            i += 1;
            continue;
        }
        if ch == '"' {
            in_double = !in_double;
            i += 1;
            continue;
        }
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth -= 1;
            if depth == 0 {
                return Some((chars[start..i].iter().collect(), i + 1));
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_all_contract_dangerous_patterns() {
        let positives = [
            "rm -rf /tmp/test",
            "rm -rfv ./dir",
            "rm -R dir",
            "rm -f file",
            "find . -name '*.tmp' | xargs rm -rf",
            "find . -print0 | xargs -0 rm -f",
            "find . -name '*.log' -delete",
            "find . -exec rm -rf {} +",
            "git push --force origin main",
            "git push -f",
            "git push --force-with-lease origin feat",
            "git push origin +main",
            "git -C repo push +HEAD:main",
            "git reset --hard HEAD~1",
            "git clean -fd",
            "git checkout -- src/main.rs",
            "git restore src/main.rs",
            "dd if=/dev/zero of=/dev/disk2",
            "mkfs.ext4 /dev/sdb1",
            "mkfs /dev/sdb1",
            "diskutil eraseDisk APFS Test disk2",
            "diskutil partitionDisk disk2 GPT APFS Test 100%",
            "chmod -R 777 /var",
            "chown -R root:wheel /tmp/a",
            "sudo ls",
            "echo x > /dev/disk0",
            "echo x >/dev/sda",
            "curl -sSL http://127.0.0.1/install.sh | sh",
            "curl -s http://127.0.0.1/x 2>&1 | sh",
            "wget -qO- http://127.0.0.1/x 2>/dev/null | bash",
            "wget -qO- http://127.0.0.1/x | bash",
            "curl http://127.0.0.1/x | tee a | zsh",
            "kill -9 -1",
            "killall Finder",
            "launchctl unload ~/Library/LaunchAgents/foo.plist",
            "DROP TABLE users;",
            "TRUNCATE TABLE logs;",
            "DELETE FROM users;",
            "UPDATE users SET admin = true;",
            "psql mydb -c \"DELETE FROM users\"",
            "ls; rm -rf /tmp/x",
            "echo $(rm -rf /tmp/x)",
            "echo `sudo id`",
            "(cd /tmp && rm -rf foo)",
        ];

        for cmd in positives {
            assert!(is_dangerous(cmd), "expected dangerous: {cmd}");
        }
    }

    #[test]
    fn avoids_false_positives_on_safe_commands_and_quoted_literals() {
        let negatives = [
            "rm file.txt",
            "rm -i file.txt",
            "find . -type f -size +100M",
            "find . -name '*.tmp' -exec rm {} \\;",
            "git push origin main",
            "git reset --soft HEAD~1",
            "git clean -nd",
            "git checkout -b feat/new",
            "git restore --staged src/main.rs",
            "chmod +x script.sh",
            "chown user:staff file.txt",
            "cmd > /dev/null",
            "cmd 2>/dev/null",
            "cmd >&2",
            "echo ok >&2 | grep -q ok",
            "cmd > output.txt",
            "curl -s http://127.0.0.1:8080/health | jq .",
            "kill -9 12345",
            "launchctl list",
            "grep \"DELETE\" log.txt",
            "grep -i \"DROP TABLE\" dump.sql",
            "echo 'rm -rf /'",
            "echo \"sudo make me a sandwich\"",
            "DELETE FROM users WHERE id = 42;",
            "UPDATE users SET active = false WHERE id = 42;",
            "psql mydb -c \"DELETE FROM users WHERE id = 42\"",
        ];

        for cmd in negatives {
            assert!(!is_dangerous(cmd), "expected safe: {cmd}");
        }
    }

    #[test]
    fn comments_each_line_of_multiline_command() {
        let commented = comment_command("cd /tmp\nrm -rf foo");
        assert_eq!(commented, "# cd /tmp\n# rm -rf foo");
    }

    #[test]
    fn fd_redirection_does_not_split_pipeline() {
        let pipes = parse_pipelines("curl -s http://127.0.0.1/x 2>&1 | sh");
        assert_eq!(pipes.len(), 1);
        assert_eq!(pipes[0].segments.len(), 2);

        let pipes2 = parse_pipelines("cmd >&2 | cat");
        assert_eq!(pipes2.len(), 1);
        assert_eq!(pipes2[0].segments.len(), 2);
    }
}
