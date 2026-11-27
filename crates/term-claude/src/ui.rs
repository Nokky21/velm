use std::io::{self, Stdout};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
use term_config::ClaudeConfig;

use crate::cost::CostSource;
use crate::model::{SessionSummary, TokenCounts, TranscriptStore};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const POLL_TIMEOUT: Duration = Duration::from_millis(200);

/// Interactive UI state for `term-claude`, decoupled from terminal I/O for testability.
#[derive(Debug, Clone)]
pub struct AppState {
    pub sessions: Vec<SessionSummary>,
    pub days: u32,
    pub context_alert_tokens: u64,
    pub current_cwd: Option<String>,
    pub filter_cwd: bool,
    pub search_query: String,
    pub search_mode: bool,
    pub selected: usize,
    pub should_quit: bool,
    pub last_error: Option<String>,
}

impl AppState {
    /// Creates an initial state with `sessions` and optional `current_cwd`.
    pub fn new(
        sessions: Vec<SessionSummary>,
        days: u32,
        context_alert_tokens: u64,
        current_cwd: Option<String>,
    ) -> Self {
        Self {
            sessions,
            days,
            context_alert_tokens,
            current_cwd,
            filter_cwd: false,
            search_query: String::new(),
            search_mode: false,
            selected: 0,
            should_quit: false,
            last_error: None,
        }
    }

    /// Replaces the session list and clamps the selection index.
    pub fn set_sessions(&mut self, sessions: Vec<SessionSummary>) {
        self.sessions = sessions;
        self.clamp_selection();
    }

    /// Returns references to sessions matching active CWD and text filters.
    pub fn visible_sessions(&self) -> Vec<&SessionSummary> {
        let query = self.search_query.trim().to_ascii_lowercase();
        self.sessions
            .iter()
            .filter(|s| {
                if self.filter_cwd {
                    let Some(cwd) = self.current_cwd.as_deref() else {
                        return false;
                    };
                    if !session_matches_cwd(s, cwd) {
                        return false;
                    }
                }
                if !query.is_empty() && !session_matches_query(s, &query) {
                    return false;
                }
                true
            })
            .collect()
    }

    /// Returns the currently selected visible session, if any.
    pub fn selected_session(&self) -> Option<&SessionSummary> {
        let visible = self.visible_sessions();
        visible.get(self.selected).copied()
    }

    /// Handles a key press; returns `true` if a manual refresh was requested.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return false;
        }

        if self.search_mode {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.search_mode = false;
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.clamp_selection();
                }
                KeyCode::Char(ch) => {
                    self.search_query.push(ch);
                    self.clamp_selection();
                }
                _ => {}
            }
            return false;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Char('c') => {
                self.filter_cwd = !self.filter_cwd;
                self.clamp_selection();
            }
            KeyCode::Char('/') => {
                self.search_mode = true;
            }
            KeyCode::Char('r') => {
                return true;
            }
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Home | KeyCode::Char('g') => {
                self.selected = 0;
            }
            KeyCode::End | KeyCode::Char('G') => {
                let len = self.visible_sessions().len();
                self.selected = len.saturating_sub(1);
            }
            _ => {}
        }

        false
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.visible_sessions().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, (len - 1) as isize);
        self.selected = next as usize;
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_sessions().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }
}

/// Checks whether `session` matches `current_cwd` by path hierarchy or encoded project name.
pub fn session_matches_cwd(session: &SessionSummary, current_cwd: &str) -> bool {
    let norm_current = normalize_path_str(current_cwd);
    if norm_current.is_empty() {
        return false;
    }

    if let Some(session_cwd) = &session.cwd {
        let norm_session = normalize_path_str(session_cwd);
        if !norm_session.is_empty() && paths_related(&norm_session, &norm_current) {
            return true;
        }
    }

    let encoded_current = encode_claude_project_name(&norm_current);
    !encoded_current.is_empty() && session.project.eq_ignore_ascii_case(&encoded_current)
}

fn session_matches_query(session: &SessionSummary, lower_query: &str) -> bool {
    session.project.to_ascii_lowercase().contains(lower_query)
        || session
            .session_id
            .to_ascii_lowercase()
            .contains(lower_query)
        || session
            .cwd
            .as_deref()
            .is_some_and(|c| c.to_ascii_lowercase().contains(lower_query))
        || session
            .git_branch
            .as_deref()
            .is_some_and(|b| b.to_ascii_lowercase().contains(lower_query))
        || session
            .models
            .iter()
            .any(|m| m.to_ascii_lowercase().contains(lower_query))
}

fn normalize_path_str(raw: &str) -> String {
    let replaced = raw.replace('\\', "/");
    let trimmed = replaced.trim_end_matches('/');
    if trimmed.is_empty() && replaced.starts_with('/') {
        String::from("/")
    } else {
        trimmed.to_owned()
    }
}

fn paths_related(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        let al = a.to_ascii_lowercase();
        let bl = b.to_ascii_lowercase();
        al == bl || al.starts_with(&format!("{bl}/")) || bl.starts_with(&format!("{al}/"))
    } else {
        a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
    }
}

fn encode_claude_project_name(normalized_path: &str) -> String {
    normalized_path
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '.' | ' ' => '-',
            other => other,
        })
        .collect()
}

/// Launches the interactive ratatui TUI until the user presses `q` or `Esc`.
pub fn run_tui(config: &ClaudeConfig, days: u32) -> io::Result<()> {
    let mut store = TranscriptStore::new();
    let initial_sessions = store.refresh(config, days, SystemTime::now())?;
    let current_cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    let mut state = AppState::new(
        initial_sessions,
        days,
        config.context_alert_tokens,
        current_cwd,
    );
    let mut guard = TerminalGuard::enter()?;
    let mut last_refresh = Instant::now();

    while !state.should_quit {
        guard.terminal.draw(|frame| render(frame, &state))?;

        let mut do_refresh = false;
        if event::poll(POLL_TIMEOUT)? {
            if let Event::Key(key) = event::read()? {
                do_refresh = state.handle_key(key);
            }
        }

        if do_refresh || last_refresh.elapsed() >= REFRESH_INTERVAL {
            match store.refresh(config, days, SystemTime::now()) {
                Ok(updated) => {
                    state.set_sessions(updated);
                    state.last_error = None;
                }
                Err(err) => {
                    state.last_error = Some(err.to_string());
                }
            }
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

/// Renders the complete `term-claude` UI into `frame`.
pub fn render(frame: &mut Frame, state: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(7),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], state);
    render_table(frame, chunks[1], state);
    render_detail(frame, chunks[2], state);
    render_footer(frame, chunks[3], state);
}

fn render_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let visible = state.visible_sessions();
    let alert_count = visible.iter().filter(|s| s.context_alert).count();

    let mut spans = vec![
        Span::styled(
            " term-claude ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "│ {} sesiones (últimos {} d) ",
            visible.len(),
            state.days
        )),
    ];

    if alert_count > 0 {
        spans.push(Span::styled(
            format!(
                "│ ⚠ {alert_count} alerta(s) contexto ≥ {} ",
                state.context_alert_tokens
            ),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }

    if state.filter_cwd {
        spans.push(Span::styled(
            "│ [Filtro CWD: ON] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    if state.search_mode {
        spans.push(Span::styled(
            format!("│ Buscar: {}_ ", state.search_query),
            Style::default().fg(Color::Green),
        ));
    } else if !state.search_query.is_empty() {
        spans.push(Span::styled(
            format!("│ Filtro: \"{}\" ", state.search_query),
            Style::default().fg(Color::Green),
        ));
    }

    let block = Block::default().borders(Borders::ALL).title("Claude Code");
    let header = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(header, area);
}

fn render_table(frame: &mut Frame, area: Rect, state: &AppState) {
    let visible = state.visible_sessions();
    let block = Block::default().borders(Borders::ALL).title("Sesiones");

    if visible.is_empty() {
        let msg = if state.sessions.is_empty() {
            format!(
                "No se encontraron sesiones modificadas en los últimos {} días.",
                state.days
            )
        } else {
            String::from("Ninguna sesión coincide con el filtro activo.")
        };
        let empty = Paragraph::new(msg)
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let header = Row::new([
        "Proyecto",
        "Rama",
        "Modelos",
        "Contexto",
        "Tokens",
        "Sidechain",
        "Coste",
        "Última actividad",
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows = visible.iter().map(|s| {
        let project_label = s
            .cwd
            .as_deref()
            .and_then(|c| Path::new(c).file_name().and_then(|n| n.to_str()))
            .filter(|n| !n.is_empty())
            .unwrap_or(&s.project);
        let branch = s.git_branch.as_deref().unwrap_or("—");
        let models = if s.models.is_empty() {
            String::from("—")
        } else {
            s.models.join(", ")
        };

        let ctx_text = if s.context_alert {
            format!("⚠ {}", format_tokens(s.context_tokens))
        } else {
            format_tokens(s.context_tokens)
        };
        let ctx_style = if s.context_alert {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let main_total = sum_tokens(&s.tokens);
        let side_total = sum_tokens(&s.sidechain_tokens);
        let cost_cell = format_cost(s);
        let row_style = if s.context_alert {
            Style::default().bg(Color::Rgb(50, 15, 15))
        } else {
            Style::default()
        };

        Row::new([
            Cell::from(project_label.to_owned()),
            Cell::from(branch.to_owned()),
            Cell::from(models),
            Cell::from(ctx_text).style(ctx_style),
            Cell::from(format_tokens(main_total)),
            Cell::from(format_tokens(side_total)),
            Cell::from(cost_cell),
            Cell::from(s.last_activity_at.clone()),
        ])
        .style(row_style)
    });

    let widths = [
        Constraint::Percentage(18),
        Constraint::Percentage(14),
        Constraint::Percentage(18),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(14),
        Constraint::Min(20),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    let mut table_state = TableState::default().with_selected(Some(state.selected));
    frame.render_stateful_widget(table, area, &mut table_state);
}

fn render_detail(frame: &mut Frame, area: Rect, state: &AppState) {
    let block = Block::default().borders(Borders::ALL).title("Detalle");
    let Some(s) = state.selected_session() else {
        let p = Paragraph::new("Selecciona una sesión para ver el desglose.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(p, area);
        return;
    };

    let cwd_str = s.cwd.as_deref().unwrap_or("—");
    let branch_str = s.git_branch.as_deref().unwrap_or("—");
    let models_str = if s.models.is_empty() {
        String::from("—")
    } else {
        s.models.join(", ")
    };

    let alert_badge = if s.context_alert {
        Span::styled(
            format!(
                "  [⚠ CONTEXTO ALTO ≥ {}]",
                format_tokens(state.context_alert_tokens)
            ),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("")
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("Sesión: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(&s.session_id),
            Span::raw("   "),
            Span::styled("CWD: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(cwd_str),
        ]),
        Line::from(vec![
            Span::styled("Rama: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(branch_str),
            Span::raw("   "),
            Span::styled("Modelos: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(models_str),
        ]),
        Line::from(vec![
            Span::styled(
                "Contexto actual: ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format_tokens(s.context_tokens)),
            alert_badge,
            Span::raw("   "),
            Span::styled("Coste: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format_cost_detail(s)),
        ]),
        Line::from(format!(
            "Principal → in: {} · out: {} · cache_read: {} · cache_write: {}",
            format_tokens(s.tokens.input),
            format_tokens(s.tokens.output),
            format_tokens(s.tokens.cache_read),
            format_tokens(s.tokens.cache_write),
        )),
        Line::from(format!(
            "Sidechain → in: {} · out: {} · cache_read: {} · cache_write: {}",
            format_tokens(s.sidechain_tokens.input),
            format_tokens(s.sidechain_tokens.output),
            format_tokens(s.sidechain_tokens.cache_read),
            format_tokens(s.sidechain_tokens.cache_write),
        )),
    ];

    let detail = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    frame.render_widget(detail, area);
}

fn render_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    let text = if let Some(err) = &state.last_error {
        Line::from(Span::styled(
            format!("Error al actualizar: {err}   (q salir · r reintentar)"),
            Style::default().fg(Color::Red),
        ))
    } else if state.search_mode {
        Line::from(Span::styled(
            " Escribe para filtrar · Enter/Esc confirmar filtro · Backspace borrar",
            Style::default().fg(Color::Green),
        ))
    } else {
        Line::from(Span::styled(
            " ↑↓/jk mover · c filtro CWD · / buscar · r refrescar · q salir",
            Style::default().fg(Color::DarkGray),
        ))
    };
    frame.render_widget(Paragraph::new(text), area);
}

fn sum_tokens(t: &TokenCounts) -> u64 {
    t.input
        .saturating_add(t.output)
        .saturating_add(t.cache_read)
        .saturating_add(t.cache_write)
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_cost(s: &SessionSummary) -> String {
    let inc = if s.cost_incomplete { " !" } else { "" };
    match s.cost_source {
        CostSource::CostState => format!("${:.4}{inc}", s.cost_usd),
        CostSource::Estimated => format!("~${:.4}{inc}", s.cost_usd),
        CostSource::None => {
            if s.cost_incomplete {
                String::from("incompleto")
            } else {
                String::from("—")
            }
        }
    }
}

fn format_cost_detail(s: &SessionSummary) -> String {
    let source_label = match s.cost_source {
        CostSource::CostState => "cost-state",
        CostSource::Estimated => "estimado",
        CostSource::None => "sin datos",
    };
    let inc_label = if s.cost_incomplete {
        ", incompleto"
    } else {
        ""
    };
    match s.cost_source {
        CostSource::None => format!("{source_label}{inc_label}"),
        CostSource::CostState | CostSource::Estimated => {
            format!("${:.4} ({source_label}{inc_label})", s.cost_usd)
        }
    }
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(err);
        }
        let backend = CrosstermBackend::new(stdout);
        match Terminal::new(backend) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(err) => {
                let mut out = io::stdout();
                let _ = execute!(out, LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(err)
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}
