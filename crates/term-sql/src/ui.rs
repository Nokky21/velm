use std::io;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::Terminal;
use term_config::sql::{SqlColor, SqlConfig, SqlConnection, SqlEnv};

use crate::keychain::KeychainStore;
use crate::session::{QueryResult, ReadOnlySession};
use crate::write::{CommitOutcome, WriteConfirmation, WriteExecuteOutcome, WriteSession};

const PAGE_SIZE: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusArea {
    Editor,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionMode {
    ReadOnly,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfirmPurpose {
    SinWhere { sql: String, reason: String },
    ConnectionNameBeforeExec { sql: String, reason: String },
    DevCommit,
}

#[derive(Debug, Clone)]
struct ConfirmModal {
    purpose: ConfirmPurpose,
    expected: String,
    input: String,
    error: Option<String>,
}

enum Screen {
    Picker {
        selected: usize,
        error: Option<String>,
    },
    Workspace(Box<WorkspaceState>),
}

struct WorkspaceState {
    conn: SqlConnection,
    mode: SessionMode,
    focus: FocusArea,
    read_session: ReadOnlySession,
    write_session: Option<WriteSession>,
    editor: TextEditor,
    history: Vec<String>,
    history_idx: Option<usize>,
    result: Option<QueryResult>,
    page: usize,
    status: String,
    status_is_error: bool,
    modal: Option<ConfirmModal>,
}

#[derive(Default)]
struct TextEditor {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

impl TextEditor {
    fn new() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
        }
    }

    fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn set_text(&mut self, s: &str) {
        self.lines = if s.is_empty() {
            vec![String::new()]
        } else {
            s.lines().map(ToOwned::to_owned).collect()
        };
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].len();
    }

    fn insert_char(&mut self, ch: char) {
        let line = &mut self.lines[self.row];
        let byte_idx = char_to_byte_idx(line, self.col);
        line.insert(byte_idx, ch);
        self.col += 1;
    }

    fn insert_newline(&mut self) {
        let line = &mut self.lines[self.row];
        let byte_idx = char_to_byte_idx(line, self.col);
        let tail = line.split_off(byte_idx);
        self.row += 1;
        self.lines.insert(self.row, tail);
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            let line = &mut self.lines[self.row];
            let start = char_to_byte_idx(line, self.col - 1);
            let end = char_to_byte_idx(line, self.col);
            line.replace_range(start..end, "");
            self.col -= 1;
        } else if self.row > 0 {
            let current = self.lines.remove(self.row);
            self.row -= 1;
            let prev_len = self.lines[self.row].chars().count();
            self.lines[self.row].push_str(&current);
            self.col = prev_len;
        }
    }

    fn delete_char(&mut self) {
        let line_len = self.lines[self.row].chars().count();
        if self.col < line_len {
            let line = &mut self.lines[self.row];
            let start = char_to_byte_idx(line, self.col);
            let end = char_to_byte_idx(line, self.col + 1);
            line.replace_range(start..end, "");
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
        }
    }

    fn move_right(&mut self) {
        let line_len = self.lines[self.row].chars().count();
        if self.col < line_len {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            let max_col = self.lines[self.row].chars().count();
            self.col = self.col.min(max_col);
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            let max_col = self.lines[self.row].chars().count();
            self.col = self.col.min(max_col);
        }
    }

    fn move_home(&mut self) {
        self.col = 0;
    }

    fn move_end(&mut self) {
        self.col = self.lines[self.row].chars().count();
    }
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map_or(s.len(), |(idx, _)| idx)
}

/// Runs the interactive ratatui SQL panel.
pub fn run_tui(sql_cfg: &SqlConfig, keychain: &dyn KeychainStore) -> Result<(), String> {
    enable_raw_mode().map_err(|e| format!("no se pudo activar el modo terminal raw: {e}"))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)
        .map_err(|e| format!("no se pudo entrar en pantalla alternativa: {e}"))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|e| format!("no se pudo inicializar la interfaz TUI: {e}"))?;

    let result = event_loop(&mut terminal, sql_cfg, keychain);

    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = terminal.show_cursor();

    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    sql_cfg: &SqlConfig,
    keychain: &dyn KeychainStore,
) -> Result<(), String> {
    let mut screen = Screen::Picker {
        selected: 0,
        error: None,
    };

    loop {
        if let Screen::Workspace(ws) = &mut screen {
            if let Some(write_sess) = &mut ws.write_session {
                if write_sess.expire_if_elapsed() {
                    ws.status = String::from(
                        "la transacción se deshizo por inactividad (cuenta atrás agotada)",
                    );
                    ws.status_is_error = true;
                    ws.modal = None;
                }
            }
        }

        terminal
            .draw(|frame| draw_screen(frame, &screen, sql_cfg))
            .map_err(|e| format!("error al dibujar la interfaz: {e}"))?;

        if !event::poll(Duration::from_millis(200))
            .map_err(|e| format!("error leyendo eventos de teclado: {e}"))?
        {
            continue;
        }

        let Event::Key(key) = event::read().map_err(|e| format!("error leyendo tecla: {e}"))?
        else {
            continue;
        };

        if key.kind != KeyEventKind::Press {
            continue;
        }

        match &mut screen {
            Screen::Picker { selected, error } => {
                match handle_picker_key(key, *selected, sql_cfg, keychain) {
                    PickerAction::None => {}
                    PickerAction::Quit => return Ok(()),
                    PickerAction::Select(idx) => *selected = idx,
                    PickerAction::Error(msg) => *error = Some(msg),
                    PickerAction::Connected(ws) => screen = Screen::Workspace(ws),
                }
            }
            Screen::Workspace(ws) => match handle_workspace_key(key, ws, sql_cfg, keychain) {
                WorkspaceAction::None => {}
                WorkspaceAction::BackToPicker => {
                    screen = Screen::Picker {
                        selected: 0,
                        error: None,
                    };
                }
                WorkspaceAction::Quit => return Ok(()),
            },
        }
    }
}

enum PickerAction {
    None,
    Quit,
    Select(usize),
    Error(String),
    Connected(Box<WorkspaceState>),
}

fn handle_picker_key(
    key: KeyEvent,
    selected: usize,
    sql_cfg: &SqlConfig,
    keychain: &dyn KeychainStore,
) -> PickerAction {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => PickerAction::Quit,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => PickerAction::Quit,
        KeyCode::Up | KeyCode::Char('k') => PickerAction::Select(selected.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => {
            if sql_cfg.connections.is_empty() {
                PickerAction::None
            } else {
                PickerAction::Select((selected + 1).min(sql_cfg.connections.len() - 1))
            }
        }
        KeyCode::Enter => {
            let Some(conn) = sql_cfg.connections.get(selected) else {
                return PickerAction::Error(String::from(
                    "no hay conexiones configuradas en [[term.sql.connections]]",
                ));
            };
            let secret = match keychain.get(&conn.name) {
                Ok(url) => url,
                Err(err) => return PickerAction::Error(err.to_string()),
            };
            match ReadOnlySession::connect(&secret, sql_cfg.statement_timeout_s, sql_cfg.row_limit)
            {
                Ok(read_session) => PickerAction::Connected(Box::new(WorkspaceState {
                    conn: conn.clone(),
                    mode: SessionMode::ReadOnly,
                    focus: FocusArea::Editor,
                    read_session,
                    write_session: None,
                    editor: TextEditor::new(),
                    history: Vec::new(),
                    history_idx: None,
                    result: None,
                    page: 0,
                    status: format!("Conectado a {} ({}) en modo LECTURA", conn.name, conn.env),
                    status_is_error: false,
                    modal: None,
                })),
                Err(err) => PickerAction::Error(err),
            }
        }
        _ => PickerAction::None,
    }
}

enum WorkspaceAction {
    None,
    BackToPicker,
    Quit,
}

fn handle_workspace_key(
    key: KeyEvent,
    ws: &mut WorkspaceState,
    sql_cfg: &SqlConfig,
    keychain: &dyn KeychainStore,
) -> WorkspaceAction {
    if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return WorkspaceAction::Quit;
    }

    if let Some(mut modal) = ws.modal.take() {
        if handle_modal_key(key, &mut modal, ws) {
            ws.modal = Some(modal);
        }
        return WorkspaceAction::None;
    }

    // If a write transaction is pending, wait strictly for `c` (commit) or `r` (rollback).
    if ws
        .write_session
        .as_ref()
        .is_some_and(WriteSession::has_pending)
    {
        match key.code {
            KeyCode::Char('c') => {
                if let Some(write_sess) = &mut ws.write_session {
                    match write_sess.commit(None) {
                        Ok(CommitOutcome::Committed) => {
                            ws.status = String::from("COMMIT realizado");
                            ws.status_is_error = false;
                        }
                        Ok(CommitOutcome::NeedsDevConfirmation) => {
                            ws.modal = Some(ConfirmModal {
                                purpose: ConfirmPurpose::DevCommit,
                                expected: ws.conn.name.clone(),
                                input: String::new(),
                                error: None,
                            });
                        }
                        Err(err) => {
                            ws.status = err;
                            ws.status_is_error = true;
                        }
                    }
                }
            }
            KeyCode::Char('r') => {
                if let Some(write_sess) = &mut ws.write_session {
                    match write_sess.rollback() {
                        Ok(()) => {
                            ws.status = String::from("ROLLBACK realizado; cambios deshechos");
                            ws.status_is_error = false;
                        }
                        Err(err) => {
                            ws.status = err;
                            ws.status_is_error = true;
                        }
                    }
                }
            }
            _ => {}
        }
        return WorkspaceAction::None;
    }

    match key.code {
        KeyCode::Esc => return WorkspaceAction::BackToPicker,
        KeyCode::Tab => {
            ws.focus = match ws.focus {
                FocusArea::Editor => FocusArea::Results,
                FocusArea::Results => FocusArea::Editor,
            };
        }
        KeyCode::F(2) => {
            toggle_mode(ws, sql_cfg, keychain);
        }
        KeyCode::F(5) => {
            execute_editor_query(ws, sql_cfg, keychain);
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            execute_editor_query(ws, sql_cfg, keychain);
        }
        KeyCode::PageDown | KeyCode::Char(']') if ws.focus == FocusArea::Results => {
            let total = total_pages(ws.result.as_ref());
            if ws.page + 1 < total {
                ws.page += 1;
            }
        }
        KeyCode::PageUp | KeyCode::Char('[') if ws.focus == FocusArea::Results => {
            ws.page = ws.page.saturating_sub(1);
        }
        KeyCode::Char('q') if ws.focus == FocusArea::Results => {
            return WorkspaceAction::Quit;
        }
        _ if ws.focus == FocusArea::Editor => handle_editor_key(key, ws),
        _ => {}
    }

    WorkspaceAction::None
}

/// Returns `true` if the modal should remain open, `false` if it was closed/consumed.
fn handle_modal_key(key: KeyEvent, modal: &mut ConfirmModal, ws: &mut WorkspaceState) -> bool {
    match key.code {
        KeyCode::Esc => {
            ws.status = String::from("operación cancelada por el usuario");
            ws.status_is_error = false;
            false
        }
        KeyCode::Backspace => {
            modal.input.pop();
            modal.error = None;
            true
        }
        KeyCode::Enter => {
            if modal.input != modal.expected {
                modal.error = Some(format!(
                    "debe coincidir exactamente con '{}'",
                    modal.expected
                ));
                return true;
            }
            match &modal.purpose {
                ConfirmPurpose::SinWhere { sql, .. } => {
                    run_write_with_confirmation(ws, sql, Some("SIN WHERE"));
                }
                ConfirmPurpose::ConnectionNameBeforeExec { sql, .. } => {
                    let conn_name = ws.conn.name.clone();
                    run_write_with_confirmation(ws, sql, Some(&conn_name));
                }
                ConfirmPurpose::DevCommit => {
                    let conn_name = ws.conn.name.clone();
                    if let Some(write_sess) = &mut ws.write_session {
                        match write_sess.commit(Some(&conn_name)) {
                            Ok(CommitOutcome::Committed) => {
                                ws.status = String::from("COMMIT confirmado en dev");
                                ws.status_is_error = false;
                            }
                            Ok(CommitOutcome::NeedsDevConfirmation) => {
                                ws.status = String::from("confirmación de dev no válida");
                                ws.status_is_error = true;
                            }
                            Err(err) => {
                                ws.status = err;
                                ws.status_is_error = true;
                            }
                        }
                    }
                }
            }
            false
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            modal.input.push(c);
            modal.error = None;
            true
        }
        _ => true,
    }
}

fn handle_editor_key(key: KeyEvent, ws: &mut WorkspaceState) {
    match key.code {
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            navigate_history(ws, -1);
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            navigate_history(ws, 1);
        }
        KeyCode::Enter => ws.editor.insert_newline(),
        KeyCode::Backspace => ws.editor.backspace(),
        KeyCode::Delete => ws.editor.delete_char(),
        KeyCode::Left => ws.editor.move_left(),
        KeyCode::Right => ws.editor.move_right(),
        KeyCode::Up => ws.editor.move_up(),
        KeyCode::Down => ws.editor.move_down(),
        KeyCode::Home => ws.editor.move_home(),
        KeyCode::End => ws.editor.move_end(),
        KeyCode::PageDown => {
            let total = total_pages(ws.result.as_ref());
            if ws.page + 1 < total {
                ws.page += 1;
            }
        }
        KeyCode::PageUp => {
            ws.page = ws.page.saturating_sub(1);
        }
        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            ws.editor.insert_char(ch);
        }
        _ => {}
    }
}

fn navigate_history(ws: &mut WorkspaceState, delta: isize) {
    if ws.history.is_empty() {
        return;
    }
    let next_idx = match ws.history_idx {
        None if delta < 0 => Some(ws.history.len() - 1),
        None => None,
        Some(idx) => {
            let candidate = idx as isize + delta;
            if candidate < 0 {
                Some(0)
            } else if candidate as usize >= ws.history.len() {
                None
            } else {
                Some(candidate as usize)
            }
        }
    };
    ws.history_idx = next_idx;
    match next_idx {
        Some(i) => ws.editor.set_text(&ws.history[i]),
        None => ws.editor.set_text(""),
    }
}

fn toggle_mode(ws: &mut WorkspaceState, sql_cfg: &SqlConfig, keychain: &dyn KeychainStore) {
    match ws.mode {
        SessionMode::ReadOnly => {
            if ws
                .write_session
                .as_ref()
                .is_none_or(WriteSession::is_closed)
            {
                let secret = match keychain.get(&ws.conn.name) {
                    Ok(url) => url,
                    Err(err) => {
                        ws.status = err.to_string();
                        ws.status_is_error = true;
                        return;
                    }
                };
                match WriteSession::connect(&ws.conn, &secret, sql_cfg) {
                    Ok(session) => ws.write_session = Some(session),
                    Err(err) => {
                        ws.status = err;
                        ws.status_is_error = true;
                        return;
                    }
                }
            }
            ws.mode = SessionMode::Write;
            ws.status = String::from("MODO ESCRITURA activado");
            ws.status_is_error = false;
        }
        SessionMode::Write => {
            ws.mode = SessionMode::ReadOnly;
            ws.status = String::from("MODO LECTURA activado");
            ws.status_is_error = false;
        }
    }
}

fn execute_editor_query(
    ws: &mut WorkspaceState,
    sql_cfg: &SqlConfig,
    keychain: &dyn KeychainStore,
) {
    let sql = ws.editor.text();
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        ws.status = String::from("escribe una sentencia SQL primero");
        ws.status_is_error = true;
        return;
    }

    if ws.history.last().map(String::as_str) != Some(trimmed) {
        ws.history.push(trimmed.to_owned());
    }
    ws.history_idx = None;
    ws.page = 0;

    match ws.mode {
        SessionMode::ReadOnly => {
            if ws.read_session.is_closed() {
                if let Ok(secret) = keychain.get(&ws.conn.name) {
                    if let Ok(reconnected) = ReadOnlySession::connect(
                        &secret,
                        sql_cfg.statement_timeout_s,
                        sql_cfg.row_limit,
                    ) {
                        ws.read_session = reconnected;
                    }
                }
            }
            match ws.read_session.execute(trimmed) {
                Ok(res) => {
                    ws.status = format_result_summary(&res);
                    ws.status_is_error = false;
                    ws.result = Some(res);
                }
                Err(err) => {
                    ws.status = err;
                    ws.status_is_error = true;
                }
            }
        }
        SessionMode::Write => {
            if ws
                .write_session
                .as_ref()
                .is_none_or(WriteSession::is_closed)
            {
                let Ok(secret) = keychain.get(&ws.conn.name) else {
                    ws.status =
                        format!("no encuentro la conexión '{}' en el Llavero", ws.conn.name);
                    ws.status_is_error = true;
                    return;
                };
                match WriteSession::connect(&ws.conn, &secret, sql_cfg) {
                    Ok(session) => ws.write_session = Some(session),
                    Err(err) => {
                        ws.status = err;
                        ws.status_is_error = true;
                        return;
                    }
                }
            }
            run_write_with_confirmation(ws, trimmed, None);
        }
    }
}

fn run_write_with_confirmation(ws: &mut WorkspaceState, sql: &str, confirmation: Option<&str>) {
    let Some(write_sess) = &mut ws.write_session else {
        ws.status = String::from("no hay sesión de escritura activa");
        ws.status_is_error = true;
        return;
    };

    match write_sess.execute(sql, confirmation) {
        Ok(WriteExecuteOutcome::PendingTx {
            result,
            idle_timeout_s,
        }) => {
            let summary = format_result_summary(&result);
            ws.result = Some(result);
            ws.status = format!(
                "{summary} — transacción abierta ({idle_timeout_s}s): pulsa [c] COMMIT o [r] ROLLBACK"
            );
            ws.status_is_error = false;
        }
        Ok(WriteExecuteOutcome::NeedsConfirmation(WriteConfirmation::SinWhere { reason })) => {
            ws.modal = Some(ConfirmModal {
                purpose: ConfirmPurpose::SinWhere {
                    sql: sql.to_owned(),
                    reason,
                },
                expected: String::from("SIN WHERE"),
                input: String::new(),
                error: None,
            });
        }
        Ok(WriteExecuteOutcome::NeedsConfirmation(WriteConfirmation::ConnectionName {
            reason,
        })) => {
            ws.modal = Some(ConfirmModal {
                purpose: ConfirmPurpose::ConnectionNameBeforeExec {
                    sql: sql.to_owned(),
                    reason,
                },
                expected: ws.conn.name.clone(),
                input: String::new(),
                error: None,
            });
        }
        Ok(WriteExecuteOutcome::Rejected(msg)) => {
            ws.status = msg;
            ws.status_is_error = true;
        }
        Err(err) => {
            ws.status = err;
            ws.status_is_error = true;
        }
    }
}

fn format_result_summary(res: &QueryResult) -> String {
    let mut parts = Vec::new();
    if let Some(affected) = res.rows_affected {
        parts.push(format!("{affected} filas afectadas"));
    }
    if !res.columns.is_empty() {
        if res.has_more {
            parts.push(format!("{} filas (hay más filas)", res.rows.len()));
        } else {
            parts.push(format!("{} filas", res.rows.len()));
        }
    }
    if parts.is_empty() {
        String::from("sentencia ejecutada")
    } else {
        parts.join(" · ")
    }
}

fn total_pages(res: Option<&QueryResult>) -> usize {
    let len = res.map_or(0, |r| r.rows.len());
    len.div_ceil(PAGE_SIZE).max(1)
}

fn draw_screen(frame: &mut ratatui::Frame<'_>, screen: &Screen, sql_cfg: &SqlConfig) {
    match screen {
        Screen::Picker { selected, error } => {
            draw_picker(frame, *selected, error.as_deref(), sql_cfg);
        }
        Screen::Workspace(ws) => draw_workspace(frame, ws),
    }
}

fn sql_color_to_ratatui(color: SqlColor) -> Color {
    match color {
        SqlColor::Green => Color::Green,
        SqlColor::Yellow => Color::Yellow,
        SqlColor::Blue => Color::Blue,
        SqlColor::Magenta => Color::Magenta,
    }
}

fn draw_picker(
    frame: &mut ratatui::Frame<'_>,
    selected: usize,
    error: Option<&str>,
    sql_cfg: &SqlConfig,
) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " term-sql ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  Elige una conexión (solo local/dev; credenciales en Llavero)"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, chunks[0]);

    if sql_cfg.connections.is_empty() {
        let empty = Paragraph::new(
            "No hay conexiones configuradas en [[term.sql.connections]].\nAñade una en ~/.config/velm/config.toml y guarda la URL con `term-sql conn add <nombre>`.",
        )
        .block(Block::default().borders(Borders::ALL).title("Conexiones"))
        .wrap(Wrap { trim: false });
        frame.render_widget(empty, chunks[1]);
    } else {
        let items: Vec<ListItem<'_>> = sql_cfg
            .connections
            .iter()
            .enumerate()
            .map(|(idx, conn)| {
                let prefix = if idx == selected { "▶ " } else { "  " };
                let badge_color = sql_color_to_ratatui(conn.color);
                let line = Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(
                        format!("[{}]", conn.env),
                        Style::default()
                            .fg(badge_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        &conn.name,
                        if idx == selected {
                            Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                        } else {
                            Style::default()
                        },
                    ),
                ]);
                ListItem::new(line)
            })
            .collect();
        let list =
            List::new(items).block(Block::default().borders(Borders::ALL).title("Conexiones"));
        frame.render_widget(list, chunks[1]);
    }

    let footer_text = match error {
        Some(err) => Line::from(Span::styled(
            format!("Error: {err}"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        None => Line::from("↑/↓ mover · Enter conectar · q salir"),
    };
    let footer = Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);
}

fn draw_workspace(frame: &mut ratatui::Frame<'_>, ws: &WorkspaceState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(8),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(area);

    draw_status_banner(frame, chunks[0], ws);
    draw_editor(frame, chunks[1], ws);
    draw_results(frame, chunks[2], ws);
    draw_footer(frame, chunks[3], ws);

    if let Some(modal) = &ws.modal {
        draw_confirm_modal(frame, area, modal);
    }
}

fn draw_status_banner(frame: &mut ratatui::Frame<'_>, area: Rect, ws: &WorkspaceState) {
    let env_color = sql_color_to_ratatui(ws.conn.color);
    let banner = match ws.mode {
        SessionMode::ReadOnly => Paragraph::new(Line::from(vec![
            Span::styled(
                " LECTURA ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                format!("{} [{}]", ws.conn.name, ws.conn.env),
                Style::default().fg(env_color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" · F2 modo escritura · F5/Ctrl+E ejecutar"),
        ])),
        SessionMode::Write => {
            let pending_text = ws
                .write_session
                .as_ref()
                .and_then(|s| s.pending())
                .map(|p| {
                    let rem = p.remaining_seconds();
                    format!(" · TRANSACCIÓN ABIERTA: {rem}s restantes · [c] COMMIT · [r] ROLLBACK")
                });
            let mut spans = vec![
                Span::styled(
                    " ESCRITURA ",
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" {} [{}]", ws.conn.name, ws.conn.env),
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            if let Some(extra) = pending_text {
                spans.push(Span::styled(
                    extra,
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(Color::Red)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    " · F2 volver a lectura · F5/Ctrl+E ejecutar",
                    Style::default().fg(Color::White).bg(Color::Red),
                ));
            }
            Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Red))
        }
    };
    frame.render_widget(banner, area);
}

fn draw_editor(frame: &mut ratatui::Frame<'_>, area: Rect, ws: &WorkspaceState) {
    let border_style = if ws.focus == FocusArea::Editor {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let title = if ws.mode == SessionMode::Write {
        "Sentencia SQL (una sola sentencia en modo ESCRITURA)"
    } else {
        "Sentencia SQL (modo LECTURA)"
    };
    let text = ws.editor.text();
    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(title),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);

    if ws.focus == FocusArea::Editor && ws.modal.is_none() {
        let cursor_x = area.x + 1 + ws.editor.col as u16;
        let cursor_y = area.y + 1 + ws.editor.row as u16;
        if cursor_x < area.x + area.width.saturating_sub(1)
            && cursor_y < area.y + area.height.saturating_sub(1)
        {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn draw_results(frame: &mut ratatui::Frame<'_>, area: Rect, ws: &WorkspaceState) {
    let border_style = if ws.focus == FocusArea::Results {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let Some(res) = &ws.result else {
        let empty = Paragraph::new("Sin resultados todavía.").block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title("Resultados"),
        );
        frame.render_widget(empty, area);
        return;
    };

    if res.columns.is_empty() {
        let info = match res.rows_affected {
            Some(n) => format!("Filas afectadas: {n}"),
            None => String::from("Sentencia completada sin filas."),
        };
        let p = Paragraph::new(info).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title("Resultados"),
        );
        frame.render_widget(p, area);
        return;
    }

    let total_p = total_pages(Some(res));
    let page = ws.page.min(total_p - 1);
    let start = page * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(res.rows.len());

    let header = Row::new(res.columns.iter().map(|c| Cell::from(c.as_str())))
        .style(Style::default().add_modifier(Modifier::BOLD));

    let rows = res.rows[start..end]
        .iter()
        .map(|r| Row::new(r.iter().map(|val| Cell::from(val.as_str()))));

    let widths: Vec<Constraint> = res.columns.iter().map(|_| Constraint::Min(10)).collect();

    let more_tag = if res.has_more {
        " · hay más filas"
    } else {
        ""
    };
    let title = format!(
        "Resultados ({} filas, pág. {}/{}{more_tag})",
        res.rows.len(),
        page + 1,
        total_p
    );

    let table = Table::new(rows, widths).header(header).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(title),
    );
    frame.render_widget(table, area);
}

fn draw_footer(frame: &mut ratatui::Frame<'_>, area: Rect, ws: &WorkspaceState) {
    let status_style = if ws.status_is_error {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let lines = vec![
        Line::from(Span::styled(&ws.status, status_style)),
        Line::from(
            "Tab foco · F5/Ctrl+E ejecutar · F2 lectura/escritura · PgUp/PgDn paginar · Ctrl+P/N historial · Esc conexiones · Ctrl+Q salir",
        ),
    ];
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_confirm_modal(frame: &mut ratatui::Frame<'_>, area: Rect, modal: &ConfirmModal) {
    let min_w = area.width.min(30);
    let width = area.width.clamp(min_w, 68);
    let height = 9u16.min(area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let heading = match &modal.purpose {
        ConfirmPurpose::SinWhere { reason, .. } => format!("ATENCIÓN: {reason}"),
        ConfirmPurpose::ConnectionNameBeforeExec { reason, .. } => format!("PELIGRO: {reason}"),
        ConfirmPurpose::DevCommit => {
            String::from("CONFIRMACIÓN REFORZADA EN DEV: escribe el nombre de la conexión")
        }
    };

    let mut lines = vec![
        Line::from(Span::styled(
            heading,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!("Escribe exactamente: {}", modal.expected)),
        Line::from(format!("> {}", modal.input)),
    ];
    if let Some(err) = &modal.error {
        lines.push(Line::from(Span::styled(
            err.as_str(),
            Style::default().fg(Color::Red),
        )));
    } else {
        lines.push(Line::from("Enter confirmar · Esc cancelar"));
    }

    frame.render_widget(Clear, rect);
    let popup = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title("Confirmación requerida"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(popup, rect);
}

#[allow(dead_code)]
fn _env_label(env: SqlEnv) -> &'static str {
    env.as_str()
}
