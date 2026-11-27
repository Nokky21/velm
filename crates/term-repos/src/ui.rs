use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{Frame, Terminal};
use term_config::ReposConfig;

use crate::git::{cached_origin_head, git_diff_stat, inspect_repos_streaming};
use crate::parse::RepoStatus;
use crate::remote::{query_remote_default_branch, RemoteResult};
use crate::scan::{discover_repos, DiscoveredRepo};

/// Display row for a repository in the TUI (discovered immediately, filled progressively).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRow {
    pub path: String,
    pub name: String,
    pub worktree: bool,
    pub loading: bool,
    pub status: Option<RepoStatus>,
}

impl RepoRow {
    fn from_discovered(repo: &DiscoveredRepo) -> Self {
        Self {
            path: repo.path.to_string_lossy().into_owned(),
            name: repo.name.clone(),
            worktree: repo.worktree,
            loading: true,
            status: None,
        }
    }
}

/// Interactive state for the `term-repos` TUI (pure state separated for testing).
#[derive(Debug, Clone)]
pub struct AppState {
    pub repos: Vec<RepoRow>,
    pub dirty_only: bool,
    pub search_mode: bool,
    pub search_query: String,
    pub selected: usize,
    pub remote_in_flight: Option<String>,
    pub remote_results: HashMap<String, String>,
    pub diff_cache: HashMap<String, Result<String, String>>,
    pub origin_head_cache: HashMap<String, Option<String>>,
    pub status_message: Option<String>,
    pub should_quit: bool,
}

impl AppState {
    /// Builds initial state immediately from discovered repositories before `git` finishes.
    pub fn from_discovered(discovered: &[DiscoveredRepo]) -> Self {
        let repos = discovered.iter().map(RepoRow::from_discovered).collect();
        Self {
            repos,
            dirty_only: false,
            search_mode: false,
            search_query: String::new(),
            selected: 0,
            remote_in_flight: None,
            remote_results: HashMap::new(),
            diff_cache: HashMap::new(),
            origin_head_cache: HashMap::new(),
            status_message: None,
            should_quit: false,
        }
    }

    /// Updates a repository row when its `git` status completes.
    pub fn apply_status(&mut self, status: RepoStatus) {
        if let Some(row) = self.repos.iter_mut().find(|r| r.path == status.path) {
            row.loading = false;
            row.status = Some(status);
        }
        self.clamp_selection();
    }

    /// Returns indices into `self.repos` matching the current `dirty_only` and search filters.
    pub fn visible_indices(&self) -> Vec<usize> {
        let query = self.search_query.trim().to_ascii_lowercase();
        self.repos
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                if self.dirty_only {
                    let is_dirty = row.status.as_ref().is_some_and(RepoStatus::is_dirty);
                    if !is_dirty {
                        return false;
                    }
                }
                if !query.is_empty() {
                    let name_match = row.name.to_ascii_lowercase().contains(&query);
                    let path_match = row.path.to_ascii_lowercase().contains(&query);
                    if !name_match && !path_match {
                        return false;
                    }
                }
                true
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Toggles the dirty-only filter (`d`) and keeps selection valid.
    pub fn toggle_dirty_filter(&mut self) {
        self.dirty_only = !self.dirty_only;
        self.clamp_selection();
    }

    /// Updates the search query and keeps selection valid.
    pub fn set_search_query(&mut self, query: String) {
        self.search_query = query;
        self.clamp_selection();
    }

    /// Moves the table selection by `delta` within visible rows.
    pub fn move_selection(&mut self, delta: isize) {
        let count = self.visible_indices().len();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let max_idx = count - 1;
        let next = (self.selected as isize + delta).clamp(0, max_idx as isize);
        self.selected = next as usize;
    }

    /// Returns the currently selected visible repository row.
    pub fn selected_row(&self) -> Option<&RepoRow> {
        let visible = self.visible_indices();
        let idx = *visible.get(self.selected)?;
        self.repos.get(idx)
    }

    fn clamp_selection(&mut self) {
        let count = self.visible_indices().len();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }
}

enum DetailMsg {
    Diff {
        path: String,
        result: Result<String, String>,
    },
    OriginHead {
        path: String,
        branch: Option<String>,
    },
    Remote {
        path: String,
        result: RemoteResult,
    },
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        Terminal::new(backend)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

/// Runs the interactive ratatui TUI with progressive git status loading.
pub fn run_tui(roots: Vec<PathBuf>, config: &ReposConfig) -> Result<(), String> {
    let discovered = discover_repos(
        &roots,
        config.max_depth,
        config.include_worktrees,
        &config.skip,
    );

    let mut terminal =
        TerminalGuard::enter().map_err(|err| format!("no se pudo iniciar la terminal: {err}"))?;
    let mut state = AppState::from_discovered(&discovered);

    let (status_tx, mut status_rx) = mpsc::channel();
    spawn_scan_worker(discovered, config, status_tx);

    let (detail_tx, detail_rx) = mpsc::channel::<DetailMsg>();
    let mut requested_details: HashMap<String, bool> = HashMap::new();

    while !state.should_quit {
        while let Ok(status) = status_rx.try_recv() {
            state.apply_status(status);
        }
        while let Ok(msg) = detail_rx.try_recv() {
            match msg {
                DetailMsg::Diff { path, result } => {
                    state.diff_cache.insert(path, result);
                }
                DetailMsg::OriginHead { path, branch } => {
                    state.origin_head_cache.insert(path, branch);
                }
                DetailMsg::Remote { path, result } => {
                    if state.remote_in_flight.as_deref() == Some(path.as_str()) {
                        state.remote_in_flight = None;
                    }
                    state.remote_results.insert(path, result.display_text());
                }
            }
        }

        if let Some(row) = state.selected_row().cloned() {
            if !row.loading
                && !requested_details.contains_key(&row.path)
                && row
                    .status
                    .as_ref()
                    .is_some_and(|s| !s.timed_out && s.error.is_none())
            {
                requested_details.insert(row.path.clone(), true);
                let empty = row.status.as_ref().and_then(|s| s.empty).unwrap_or(false);
                spawn_detail_worker(row.path, config.timeout_ms, empty, detail_tx.clone());
            }
        }

        let now_secs = current_unix_secs();
        terminal
            .draw(|frame| draw_ui(frame, &state, now_secs))
            .map_err(|err| format!("error pintando la interfaz: {err}"))?;

        if event::poll(Duration::from_millis(50))
            .map_err(|err| format!("error leyendo teclado: {err}"))?
        {
            if let Event::Key(key) =
                event::read().map_err(|err| format!("error leyendo evento: {err}"))?
            {
                handle_key(
                    key,
                    &mut state,
                    &roots,
                    config,
                    &mut status_rx,
                    &detail_tx,
                    &mut requested_details,
                );
            }
        }
    }

    Ok(())
}

fn spawn_scan_worker(
    discovered: Vec<DiscoveredRepo>,
    config: &ReposConfig,
    tx: mpsc::Sender<RepoStatus>,
) {
    let parallel = config.parallel;
    let timeout_ms = config.timeout_ms;
    thread::spawn(move || {
        inspect_repos_streaming(&discovered, parallel, timeout_ms, tx);
    });
}

fn spawn_detail_worker(path: String, timeout_ms: u64, empty: bool, tx: mpsc::Sender<DetailMsg>) {
    thread::spawn(move || {
        let repo_path = Path::new(&path);
        let diff_res = git_diff_stat(repo_path, timeout_ms, empty);
        let _ = tx.send(DetailMsg::Diff {
            path: path.clone(),
            result: diff_res,
        });
        let origin_head = cached_origin_head(repo_path, timeout_ms);
        let _ = tx.send(DetailMsg::OriginHead {
            path,
            branch: origin_head,
        });
    });
}

fn spawn_remote_worker(path: String, remote_timeout_ms: u64, tx: mpsc::Sender<DetailMsg>) {
    thread::spawn(move || {
        let result = query_remote_default_branch(Path::new(&path), remote_timeout_ms);
        let _ = tx.send(DetailMsg::Remote { path, result });
    });
}

fn handle_key(
    key: KeyEvent,
    state: &mut AppState,
    roots: &[PathBuf],
    config: &ReposConfig,
    status_rx: &mut mpsc::Receiver<RepoStatus>,
    detail_tx: &mpsc::Sender<DetailMsg>,
    requested_details: &mut HashMap<String, bool>,
) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.should_quit = true;
        return;
    }

    if state.search_mode {
        match key.code {
            KeyCode::Esc => {
                state.search_mode = false;
                state.set_search_query(String::new());
            }
            KeyCode::Enter => {
                state.search_mode = false;
            }
            KeyCode::Backspace => {
                let mut q = state.search_query.clone();
                q.pop();
                state.set_search_query(q);
            }
            KeyCode::Char(c) => {
                let mut q = state.search_query.clone();
                q.push(c);
                state.set_search_query(q);
            }
            _ => {}
        }
        return;
    }

    state.status_message = None;

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => state.should_quit = true,
        KeyCode::Down | KeyCode::Char('j') => state.move_selection(1),
        KeyCode::Up | KeyCode::Char('k') => state.move_selection(-1),
        KeyCode::PageDown => state.move_selection(10),
        KeyCode::PageUp => state.move_selection(-10),
        KeyCode::Home | KeyCode::Char('g') => state.selected = 0,
        KeyCode::End | KeyCode::Char('G') => {
            let count = state.visible_indices().len();
            state.selected = count.saturating_sub(1);
        }
        KeyCode::Char('d') => state.toggle_dirty_filter(),
        KeyCode::Char('/') => state.search_mode = true,
        KeyCode::Char('r') => {
            if state.remote_in_flight.is_some() {
                state.status_message = Some(String::from("ya hay una consulta remota en curso"));
                return;
            }
            if let Some(row) = state.selected_row().cloned() {
                state.remote_in_flight = Some(row.path.clone());
                spawn_remote_worker(row.path, config.remote_timeout_ms, detail_tx.clone());
            }
        }
        KeyCode::Char('o') => {
            if let Some(row) = state.selected_row() {
                let path = row.path.clone();
                match open_repo_in_velm(Path::new(&path)) {
                    Ok(msg) => state.status_message = Some(msg),
                    Err(err) => state.status_message = Some(err),
                }
            }
        }
        KeyCode::Char('R') | KeyCode::F(5) => {
            let discovered = discover_repos(
                roots,
                config.max_depth,
                config.include_worktrees,
                &config.skip,
            );
            let mut next = AppState::from_discovered(&discovered);
            next.dirty_only = state.dirty_only;
            next.search_query = state.search_query.clone();
            *state = next;
            requested_details.clear();
            let (tx, rx) = mpsc::channel();
            *status_rx = rx;
            spawn_scan_worker(discovered, config, tx);
        }
        _ => {}
    }
}

fn open_repo_in_velm(repo_path: &Path) -> Result<String, String> {
    let has_socket = std::env::var_os("HERDR_SOCKET_PATH").is_some_and(|v| !v.is_empty());
    let has_pane = std::env::var_os("HERDR_PANE_ID").is_some_and(|v| !v.is_empty());

    let mut cmd = Command::new("velm");
    if has_pane {
        cmd.args([
            "pane",
            "split",
            "--current",
            "--direction",
            "right",
            "--cwd",
        ]);
        cmd.arg(repo_path);
        cmd.arg("--focus");
    } else if has_socket {
        cmd.args(["tab", "create", "--cwd"]);
        cmd.arg(repo_path);
        cmd.arg("--focus");
    } else {
        return Err(String::from(
            "HERDR_SOCKET_PATH no definido; ejecuta term-repos dentro de velm",
        ));
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let output = cmd
        .output()
        .map_err(|err| format!("no se pudo ejecutar velm: {err}"))?;
    if output.status.success() {
        Ok(format!("abierto en velm: {}", repo_path.display()))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if stderr.is_empty() {
            Err(String::from("velm devolvió un error al abrir el panel"))
        } else {
            Err(format!("velm: {stderr}"))
        }
    }
}

fn draw_ui(frame: &mut Frame, state: &AppState, now_secs: i64) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(8),
            Constraint::Length(9),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, chunks[0], state);
    draw_table(frame, chunks[1], state, now_secs);
    draw_detail(frame, chunks[2], state);
    draw_footer(frame, chunks[3], state);
}

fn draw_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let total = state.repos.len();
    let loaded = state.repos.iter().filter(|r| !r.loading).count();
    let visible = state.visible_indices().len();

    let mut spans = vec![
        Span::styled(
            " term-repos ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  repos: {visible}/{total}")),
    ];

    if loaded < total {
        spans.push(Span::styled(
            format!("  (cargando {loaded}/{total})"),
            Style::default().fg(Color::Yellow),
        ));
    }
    if state.dirty_only {
        spans.push(Span::styled(
            "  [filtro: pendientes]",
            Style::default().fg(Color::Yellow),
        ));
    }
    if !state.search_query.is_empty() || state.search_mode {
        spans.push(Span::styled(
            format!("  [buscar: {}]", state.search_query),
            Style::default().fg(Color::Green),
        ));
    }
    if state.remote_in_flight.is_some() {
        spans.push(Span::styled(
            "  [red]",
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let header = Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header, area);
}

fn draw_table(frame: &mut Frame, area: Rect, state: &AppState, now_secs: i64) {
    let visible = state.visible_indices();
    let header_cells = [
        "Repo",
        "Rama",
        "Remoto",
        "Mod",
        "Untracked",
        "Conflicto",
        "Stash",
        "Último commit",
    ]
    .into_iter()
    .map(|h| Cell::from(h).style(Style::default().add_modifier(Modifier::BOLD)));
    let header = Row::new(header_cells).height(1);

    let rows: Vec<Row> = visible
        .iter()
        .map(|&idx| {
            let row = &state.repos[idx];
            build_table_row(row, now_secs)
        })
        .collect();

    let widths = [
        Constraint::Percentage(22),
        Constraint::Percentage(18),
        Constraint::Percentage(16),
        Constraint::Length(6),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(7),
        Constraint::Percentage(15),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Repositorios"))
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    let mut table_state = TableState::default();
    if !visible.is_empty() {
        table_state.select(Some(state.selected.min(visible.len() - 1)));
    }
    frame.render_stateful_widget(table, area, &mut table_state);
}

fn build_table_row(row: &RepoRow, now_secs: i64) -> Row<'static> {
    let repo_label = if row.worktree {
        format!("{} [worktree]", row.name)
    } else {
        row.name.clone()
    };

    if row.loading {
        return Row::new([
            Cell::from(repo_label),
            Cell::from("cargando…").style(Style::default().fg(Color::DarkGray)),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
        ]);
    }

    let Some(st) = &row.status else {
        return Row::new([Cell::from(repo_label)]);
    };

    if st.timed_out {
        return Row::new([
            Cell::from(repo_label),
            Cell::from("lento").style(Style::default().fg(Color::Yellow)),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("timeout").style(Style::default().fg(Color::Yellow)),
        ]);
    }

    if st.error.is_some() {
        return Row::new([
            Cell::from(repo_label),
            Cell::from("error").style(Style::default().fg(Color::Red)),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
            Cell::from("-"),
        ]);
    }

    let branch_cell = if st.empty == Some(true) {
        let b = st.branch.as_deref().unwrap_or("sin commits");
        Cell::from(format!("{b} (sin commits)")).style(Style::default().fg(Color::Yellow))
    } else if st.detached == Some(true) {
        let short = st.oid.as_deref().unwrap_or("???????");
        Cell::from(format!("(detached) {short}")).style(Style::default().fg(Color::Magenta))
    } else {
        Cell::from(st.branch.clone().unwrap_or_default()).style(Style::default().fg(Color::Cyan))
    };

    let remote_cell = if st.upstream.is_none() {
        Cell::from("sin remoto").style(Style::default().fg(Color::DarkGray))
    } else if st.upstream_gone == Some(true) {
        Cell::from("remoto borrado").style(Style::default().fg(Color::Yellow))
    } else {
        let a = st.ahead.unwrap_or(0);
        let b = st.behind.unwrap_or(0);
        let style = if a > 0 || b > 0 {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::Green)
        };
        Cell::from(format!("+{a} -{b}")).style(style)
    };

    let mod_n = st.modified.unwrap_or(0);
    let unt_n = st.untracked.unwrap_or(0);
    let conf_n = st.conflicted.unwrap_or(0);
    let stash_n = st.stashes.unwrap_or(0);

    let mod_cell = if mod_n > 0 {
        Cell::from(mod_n.to_string()).style(Style::default().fg(Color::Yellow))
    } else {
        Cell::from("0")
    };
    let unt_cell = if unt_n > 0 {
        Cell::from(unt_n.to_string()).style(Style::default().fg(Color::Yellow))
    } else {
        Cell::from("0")
    };
    let conf_cell = if conf_n > 0 {
        Cell::from(conf_n.to_string())
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
    } else {
        Cell::from("0")
    };
    let stash_cell = if stash_n > 0 {
        Cell::from(stash_n.to_string()).style(Style::default().fg(Color::Blue))
    } else {
        Cell::from("0")
    };

    let age_text = match (&st.empty, &st.last_commit_at) {
        (Some(true), _) => String::from("sin commits"),
        (_, Some(iso)) => format_relative_age(iso, now_secs),
        _ => String::from("-"),
    };

    Row::new([
        Cell::from(repo_label),
        branch_cell,
        remote_cell,
        mod_cell,
        unt_cell,
        conf_cell,
        stash_cell,
        Cell::from(age_text),
    ])
}

fn draw_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut lines: Vec<Line> = Vec::new();
    if let Some(row) = state.selected_row() {
        lines.push(Line::from(vec![
            Span::styled("Ruta: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(row.path.clone()),
            if row.worktree {
                Span::styled("  [worktree]", Style::default().fg(Color::Magenta))
            } else {
                Span::raw("")
            },
        ]));

        if let Some(st) = &row.status {
            if st.timed_out {
                lines.push(Line::from(Span::styled(
                    "Estado: lento (tiempo agotado)",
                    Style::default().fg(Color::Yellow),
                )));
            } else if let Some(err) = &st.error {
                lines.push(Line::from(Span::styled(
                    format!("Error: {err}"),
                    Style::default().fg(Color::Red),
                )));
            } else {
                let upstream = st.upstream.as_deref().unwrap_or("sin remoto");
                let cached = state
                    .origin_head_cache
                    .get(&row.path)
                    .and_then(|opt| opt.as_deref())
                    .map(|b| format!(" | origin/HEAD cacheado: {b}"))
                    .unwrap_or_default();
                lines.push(Line::from(format!("Upstream: {upstream}{cached}")));

                if let Some(remote_res) = state.remote_results.get(&row.path) {
                    lines.push(Line::from(Span::styled(
                        format!("Remoto (r): {remote_res}"),
                        Style::default().fg(Color::Cyan),
                    )));
                }

                match state.diff_cache.get(&row.path) {
                    Some(Ok(diff)) if diff.trim().is_empty() => {
                        lines.push(Line::from("git diff --stat: sin cambios"));
                    }
                    Some(Ok(diff)) => {
                        for diff_line in diff.lines().take(4) {
                            lines.push(Line::from(diff_line.to_owned()));
                        }
                    }
                    Some(Err(err)) => {
                        lines.push(Line::from(Span::styled(
                            format!("diff --stat error: {err}"),
                            Style::default().fg(Color::Red),
                        )));
                    }
                    None => {
                        lines.push(Line::from("git diff --stat: cargando…"));
                    }
                }
            }
        } else {
            lines.push(Line::from("Cargando estado…"));
        }
    } else {
        lines.push(Line::from(
            "No hay repositorios que coincidan con el filtro.",
        ));
    }

    let detail = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Detalle"))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

fn draw_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    let text = if state.search_mode {
        format!(
            "Buscar: {}_  (Enter confirmar, Esc cancelar)",
            state.search_query
        )
    } else if let Some(msg) = &state.status_message {
        msg.clone()
    } else {
        String::from(
            "j/k mover · d pendientes · / buscar · r remoto (HEAD) · o abrir en velm · R recargar · q salir",
        )
    };
    let footer = Paragraph::new(text).style(Style::default().fg(Color::Gray));
    frame.render_widget(footer, area);
}

fn current_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Formats an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) as a concise relative age.
pub fn format_relative_age(iso_utc: &str, now_secs: i64) -> String {
    let Some(ts) = parse_iso8601_utc_secs(iso_utc) else {
        return iso_utc.to_owned();
    };
    let delta = (now_secs - ts).max(0);
    if delta < 60 {
        format!("{delta}s")
    } else if delta < 3_600 {
        format!("{}m", delta / 60)
    } else if delta < 86_400 {
        format!("{}h", delta / 3_600)
    } else if delta < 86_400 * 30 {
        format!("{}d", delta / 86_400)
    } else {
        iso_utc.get(..10).unwrap_or(iso_utc).to_owned()
    }
}

fn parse_iso8601_utc_secs(iso: &str) -> Option<i64> {
    if iso.len() != 20 || !iso.ends_with('Z') {
        return None;
    }
    let y: i64 = iso.get(0..4)?.parse().ok()?;
    let m: i64 = iso.get(5..7)?.parse().ok()?;
    let d: i64 = iso.get(8..10)?.parse().ok()?;
    let hh: i64 = iso.get(11..13)?.parse().ok()?;
    let mm: i64 = iso.get(14..16)?.parse().ok()?;
    let ss: i64 = iso.get(17..19)?.parse().ok()?;

    // Howard Hinnant's days-from-civil algorithm.
    let y_adj = y - i64::from(m <= 2);
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let m_adj = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_adj + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3_600 + mm * 60 + ss)
}
