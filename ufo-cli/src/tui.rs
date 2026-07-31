use crate::auth::{self, ProviderPolicy, ProviderSummary};
use crate::opencode_local_models::LocalModelEntry;
use crate::{
    load_mailbox, load_rovers, post_local_model_prompt, sanitize_terminal, Operation, RoverEntry,
};
use anyhow::{Context, Result};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, Tabs, Wrap};
use ratatui::Terminal;
use std::io::{self, Stdout};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::sync::mpsc;

const MIN_TICK_MS: u64 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Dashboard,
    Models,
    Auth,
    Chat,
}

impl Screen {
    fn index(self) -> usize {
        match self {
            Self::Dashboard => 0,
            Self::Models => 1,
            Self::Auth => 2,
            Self::Chat => 3,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Dashboard => Self::Models,
            Self::Models => Self::Auth,
            Self::Auth => Self::Chat,
            Self::Chat => Self::Dashboard,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Dashboard => Self::Chat,
            Self::Models => Self::Dashboard,
            Self::Auth => Self::Models,
            Self::Chat => Self::Auth,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Models => "Models",
            Self::Auth => "Auth",
            Self::Chat => "Chat",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RoverView {
    pub name: String,
    pub units: u32,
    pub tags: Vec<String>,
}

impl From<RoverEntry> for RoverView {
    fn from(value: RoverEntry) -> Self {
        Self {
            name: sanitize_terminal(&value.name),
            units: value.units,
            tags: value
                .tags
                .into_iter()
                .map(|tag| sanitize_terminal(&tag))
                .collect(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct JobView {
    pub id: String,
    pub title: String,
    pub pilot_cmd: String,
    pub status: String,
}

impl From<Operation> for JobView {
    fn from(value: Operation) -> Self {
        Self {
            id: value.id,
            title: sanitize_terminal(&value.title),
            pilot_cmd: sanitize_terminal(&value.pilot_cmd),
            status: sanitize_terminal(&value.status),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub rovers: Vec<RoverView>,
    pub jobs: Vec<JobView>,
    pub models: Vec<LocalModelEntry>,
    pub auth: Vec<ProviderSummary>,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    EditingPrompt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponseState {
    Idle,
    Pending,
    Ready(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub request_id: u64,
    pub endpoint: String,
    pub model: String,
    pub prompt: String,
}

#[derive(Clone, Debug)]
pub struct App {
    pub screen: Screen,
    pub rovers: Vec<RoverView>,
    pub jobs: Vec<JobView>,
    pub models: Vec<LocalModelEntry>,
    pub auth: Vec<ProviderSummary>,
    pub selected_job: usize,
    pub selected_model: usize,
    pub selected_auth: usize,
    pub prompt_lines: Vec<String>,
    pub input_mode: InputMode,
    pub response: ResponseState,
    pub status: String,
    pending_chat_request: Option<u64>,
    next_chat_request_id: u64,
    selected_job_id: Option<String>,
    selected_model_id: Option<String>,
    selected_auth_id: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            screen: Screen::Dashboard,
            rovers: vec![],
            jobs: vec![],
            models: vec![],
            auth: vec![],
            selected_job: 0,
            selected_model: 0,
            selected_auth: 0,
            prompt_lines: vec![String::new()],
            input_mode: InputMode::Normal,
            response: ResponseState::Idle,
            status: String::new(),
            pending_chat_request: None,
            next_chat_request_id: 1,
            selected_job_id: None,
            selected_model_id: None,
            selected_auth_id: None,
        }
    }
}

impl App {
    pub fn refresh(&mut self, snapshot: Snapshot) {
        let previous_job = self.selected_job_id.clone();
        let previous_model = self.selected_model_id.clone();
        let previous_auth = self.selected_auth_id.clone();

        self.rovers = snapshot.rovers;
        self.jobs = snapshot.jobs;
        self.models = snapshot.models;
        self.auth = snapshot.auth;
        self.status = snapshot.status;

        self.selected_job = selected_index(&self.jobs, previous_job, |job| job.id.clone());
        self.selected_model = selected_index(&self.models, previous_model, model_key);
        self.selected_auth =
            selected_index(&self.auth, previous_auth, |item| item.provider_id.clone());

        self.selected_job_id = self.jobs.get(self.selected_job).map(|job| job.id.clone());
        self.selected_model_id = self.models.get(self.selected_model).map(model_key);
        self.selected_auth_id = self
            .auth
            .get(self.selected_auth)
            .map(|item| item.provider_id.clone());

        if self.selected_model_id.is_none() && !self.models.is_empty() {
            self.selected_model_id = self.models.first().map(model_key);
        }
    }

    pub fn selected_job(&self) -> Option<&JobView> {
        self.jobs.get(self.selected_job)
    }

    pub fn selected_model(&self) -> Option<&LocalModelEntry> {
        self.models.get(self.selected_model)
    }

    pub fn selected_auth(&self) -> Option<&ProviderSummary> {
        self.auth.get(self.selected_auth)
    }

    pub fn selected_model_label(&self) -> String {
        self.selected_model()
            .map(model_key)
            .unwrap_or_else(|| "<no local model>".to_string())
    }

    pub fn move_selection(&mut self, delta: isize) {
        match self.screen {
            Screen::Dashboard => {
                self.selected_job = shift_index(self.selected_job, self.jobs.len(), delta)
            }
            Screen::Models => {
                self.selected_model = shift_index(self.selected_model, self.models.len(), delta)
            }
            Screen::Auth => {
                self.selected_auth = shift_index(self.selected_auth, self.auth.len(), delta)
            }
            Screen::Chat => {
                self.selected_model = shift_index(self.selected_model, self.models.len(), delta)
            }
        }
        self.sync_ids();
    }

    pub fn go_top(&mut self) {
        match self.screen {
            Screen::Dashboard => self.selected_job = 0,
            Screen::Models => self.selected_model = 0,
            Screen::Auth => self.selected_auth = 0,
            Screen::Chat => self.selected_model = 0,
        }
        self.sync_ids();
    }

    pub fn go_bottom(&mut self) {
        match self.screen {
            Screen::Dashboard => self.selected_job = self.jobs.len().saturating_sub(1),
            Screen::Models => self.selected_model = self.models.len().saturating_sub(1),
            Screen::Auth => self.selected_auth = self.auth.len().saturating_sub(1),
            Screen::Chat => self.selected_model = self.models.len().saturating_sub(1),
        }
        self.sync_ids();
    }

    pub fn next_screen(&mut self) {
        self.screen = self.screen.next();
    }

    pub fn prev_screen(&mut self) {
        self.screen = self.screen.prev();
    }

    pub fn enter_edit_mode(&mut self) {
        if matches!(self.screen, Screen::Chat) {
            self.input_mode = InputMode::EditingPrompt;
        }
    }

    pub fn exit_edit_mode(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn push_prompt_char(&mut self, ch: char) {
        let line = self
            .prompt_lines
            .last_mut()
            .expect("prompt always has a line");
        line.push(ch);
    }

    pub fn push_prompt_newline(&mut self) {
        self.prompt_lines.push(String::new());
    }

    pub fn backspace_prompt(&mut self) {
        let Some(last) = self.prompt_lines.last_mut() else {
            return;
        };
        if last.pop().is_none() && self.prompt_lines.len() > 1 {
            self.prompt_lines.pop();
        }
    }

    pub fn prompt_text(&self) -> String {
        self.prompt_lines.join("\n")
    }

    pub fn begin_chat_request(&mut self) -> Option<ChatRequest> {
        let model = self.selected_model()?.clone();
        let prompt = self.prompt_text();
        if prompt.trim().is_empty() {
            self.response = ResponseState::Error("prompt is empty".to_string());
            return None;
        }
        let request_id = self.next_chat_request_id;
        self.next_chat_request_id = self.next_chat_request_id.saturating_add(1);
        self.pending_chat_request = Some(request_id);
        self.response = ResponseState::Pending;
        Some(ChatRequest {
            request_id,
            endpoint: model.endpoint.clone(),
            model: model.model_id.clone(),
            prompt,
        })
    }

    pub fn complete_chat_request(
        &mut self,
        request_id: u64,
        result: std::result::Result<String, String>,
    ) {
        if self.pending_chat_request != Some(request_id) {
            return;
        }
        self.pending_chat_request = None;
        self.response = match result {
            Ok(text) => ResponseState::Ready(sanitize_terminal(&text)),
            Err(err) => ResponseState::Error(sanitize_terminal(&err)),
        };
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> UiAction {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return UiAction::Quit;
        }

        if matches!(self.screen, Screen::Chat)
            && matches!(self.input_mode, InputMode::EditingPrompt)
        {
            match key.code {
                KeyCode::Esc => {
                    self.exit_edit_mode();
                    return UiAction::Redraw;
                }
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.push_prompt_newline();
                    return UiAction::Redraw;
                }
                KeyCode::Enter => {
                    self.exit_edit_mode();
                    return self
                        .begin_chat_request()
                        .map(UiAction::StartChat)
                        .unwrap_or(UiAction::Redraw);
                }
                KeyCode::Backspace => {
                    self.backspace_prompt();
                    return UiAction::Redraw;
                }
                KeyCode::Tab | KeyCode::BackTab => return UiAction::Redraw,
                KeyCode::Char(ch) => {
                    if !key.modifiers.contains(KeyModifiers::CONTROL) {
                        self.push_prompt_char(ch);
                        return UiAction::Redraw;
                    }
                }
                _ => return UiAction::Redraw,
            }
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if matches!(self.screen, Screen::Dashboard) {
                    UiAction::Quit
                } else {
                    self.screen = Screen::Dashboard;
                    UiAction::Redraw
                }
            }
            KeyCode::Tab => {
                self.next_screen();
                UiAction::Redraw
            }
            KeyCode::BackTab => {
                self.prev_screen();
                UiAction::Redraw
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection(1);
                UiAction::Redraw
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection(-1);
                UiAction::Redraw
            }
            KeyCode::Char('g') => {
                self.go_top();
                UiAction::Redraw
            }
            KeyCode::Char('G') => {
                self.go_bottom();
                UiAction::Redraw
            }
            KeyCode::Char('r') => UiAction::Refresh,
            KeyCode::Char('i') => {
                self.enter_edit_mode();
                UiAction::Redraw
            }
            KeyCode::Enter => match self.screen {
                Screen::Models => {
                    self.screen = Screen::Chat;
                    UiAction::Redraw
                }
                Screen::Chat => self
                    .begin_chat_request()
                    .map(UiAction::StartChat)
                    .unwrap_or(UiAction::Redraw),
                Screen::Auth => {
                    if let Some(item) = self.selected_auth() {
                        if item.policy == ProviderPolicy::Usable {
                            self.status = format!(
                                "read-only: opencode auth login --pure --provider {} | logout: opencode auth logout --provider {}",
                                sanitize_terminal(&item.provider_id),
                                sanitize_terminal(&item.provider_id),
                            );
                        }
                    }
                    UiAction::Redraw
                }
                Screen::Dashboard => UiAction::Redraw,
            },
            _ => UiAction::Redraw,
        }
    }

    fn sync_ids(&mut self) {
        self.selected_job_id = self.jobs.get(self.selected_job).map(|job| job.id.clone());
        self.selected_model_id = self.models.get(self.selected_model).map(model_key);
        self.selected_auth_id = self
            .auth
            .get(self.selected_auth)
            .map(|item| item.provider_id.clone());
    }
}

#[derive(Debug)]
pub enum UiAction {
    Redraw,
    Refresh,
    Quit,
    StartChat(ChatRequest),
}

pub async fn run_tui(refresh_ms: u64) -> Result<()> {
    let mut terminal = install_terminal()?;
    let _guard = TerminalRestore::new();

    let (tx, mut rx) = mpsc::unbounded_channel::<UiEvent>();
    let stop = Arc::new(AtomicBool::new(false));
    spawn_input_pump(tx.clone(), stop.clone());

    let mut app = App::default();
    app.refresh(load_snapshot());
    terminal.draw(|frame| render(frame, &app))?;

    let mut interval = tokio::time::interval(Duration::from_millis(refresh_ms.max(MIN_TICK_MS)));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if !stop.load(Ordering::Relaxed) {
                    app.refresh(load_snapshot());
                    terminal.draw(|frame| render(frame, &app))?;
                }
            }
            Some(event) = rx.recv() => {
                match event {
                    UiEvent::Input(key) => {
                        let action = app.handle_key(key);
                        match action {
                            UiAction::Redraw => {
                                terminal.draw(|frame| render(frame, &app))?;
                            }
                            UiAction::Refresh => {
                                app.refresh(load_snapshot());
                                terminal.draw(|frame| render(frame, &app))?;
                            }
                            UiAction::Quit => break,
                            UiAction::StartChat(request) => {
                                terminal.draw(|frame| render(frame, &app))?;
                                let tx = tx.clone();
                                tokio::spawn(async move {
                                    let result = post_local_model_prompt(&request.endpoint, &request.model, &request.prompt)
                                        .await
                                        .map_err(|err| format!("{err:#}"));
                                    let _ = tx.send(UiEvent::ChatDone { request_id: request.request_id, result });
                                });
                            }
                        }
                    }
                    UiEvent::ChatDone { request_id, result } => {
                        app.complete_chat_request(request_id, result);
                        terminal.draw(|frame| render(frame, &app))?;
                    }
                }
            }
            else => break,
        }
    }

    stop.store(true, Ordering::Relaxed);
    Ok(())
}

enum UiEvent {
    Input(KeyEvent),
    ChatDone {
        request_id: u64,
        result: std::result::Result<String, String>,
    },
}

fn spawn_input_pump(tx: mpsc::UnboundedSender<UiEvent>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => match event::read() {
                    Ok(Event::Key(key)) => {
                        let _ = tx.send(UiEvent::Input(key));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });
}

fn install_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    if let Err(err) = execute!(stdout, EnterAlternateScreen, Hide).context("enter alternate screen")
    {
        let _ = disable_raw_mode();
        return Err(err);
    }
    let backend = CrosstermBackend::new(stdout);
    match Terminal::new(backend).context("create terminal") {
        Ok(terminal) => Ok(terminal),
        Err(err) => {
            let mut stdout = io::stdout();
            let _ = execute!(stdout, Show, LeaveAlternateScreen);
            let _ = disable_raw_mode();
            Err(err)
        }
    }
}

struct TerminalRestore;

impl TerminalRestore {
    fn new() -> Self {
        Self
    }
}

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Show, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

fn load_snapshot() -> Snapshot {
    let mut status = String::new();
    let rovers = match load_rovers() {
        Ok(items) => items.into_iter().map(RoverView::from).collect::<Vec<_>>(),
        Err(err) => {
            status = format!("rovers: {err:#}");
            vec![]
        }
    };
    let jobs = match load_mailbox() {
        Ok(items) => items.into_iter().map(JobView::from).collect::<Vec<_>>(),
        Err(err) => {
            status = join_status(&status, format!("mailbox: {err:#}"));
            vec![]
        }
    };
    let models = match crate::local_model_catalog() {
        Ok(catalog) => catalog.entries(),
        Err(err) => {
            status = join_status(&status, format!("models: {err:#}"));
            vec![]
        }
    };
    let auth = match auth::load_auth() {
        Ok(snapshot) => snapshot.store.summaries(snapshot.source.clone()),
        Err(err) => {
            status = join_status(&status, format!("auth: {err:#}"));
            vec![]
        }
    };
    Snapshot {
        rovers,
        jobs,
        models,
        auth,
        status: if status.is_empty() {
            "ready".to_string()
        } else {
            sanitize_terminal(&status)
        },
    }
}

fn join_status(left: &str, right: String) -> String {
    if left.is_empty() {
        right
    } else {
        format!("{left} | {right}")
    }
}

fn selected_index<T, F>(items: &[T], selected_id: Option<String>, key: F) -> usize
where
    F: Fn(&T) -> String,
{
    if items.is_empty() {
        return 0;
    }
    selected_id
        .as_deref()
        .and_then(|id| items.iter().position(|item| key(item) == id))
        .unwrap_or(0)
        .min(items.len().saturating_sub(1))
}

fn shift_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let next = current.saturating_add_signed(delta);
    next.min(len.saturating_sub(1))
}

fn model_key(entry: &LocalModelEntry) -> String {
    format!("{}/{}", entry.provider_id, entry.model_id)
}

pub fn render(frame: &mut ratatui::Frame<'_>, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(area);

    render_tabs(frame, chunks[0], app);
    render_body(frame, chunks[1], app);
    render_footer(frame, chunks[2], app);
}

fn render_tabs(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let titles = [
        Screen::Dashboard,
        Screen::Models,
        Screen::Auth,
        Screen::Chat,
    ]
    .into_iter()
    .map(|screen| {
        Line::from(Span::styled(
            screen.title(),
            Style::default().fg(Color::Cyan),
        ))
    })
    .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .select(app.screen.index())
        .block(
            Block::default()
                .title("xbrd-selector")
                .borders(Borders::ALL),
        )
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, area);
}

fn render_body(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    match app.screen {
        Screen::Dashboard => render_dashboard(frame, area, app),
        Screen::Models => render_models(frame, area, app),
        Screen::Auth => render_auth(frame, area, app),
        Screen::Chat => render_chat(frame, area, app),
    }
}

fn render_dashboard(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let top = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(6)])
        .split(area);

    let counts = queue_counts(&app.jobs);
    let summary = Paragraph::new(Text::from(vec![Line::from(vec![
        Span::raw(format!("rovers: {} ", app.rovers.len())),
        Span::raw(format!("queued: {} ", counts.0)),
        Span::raw(format!("running: {} ", counts.1)),
        Span::raw(format!("done: {} ", counts.2)),
        Span::raw(format!("failed: {}", counts.3)),
    ])]))
    .block(
        Block::default()
            .title(sanitize_terminal(&app.status))
            .borders(Borders::ALL),
    );
    frame.render_widget(summary, top[0]);

    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(34), Constraint::Min(45)])
        .split(top[1]);
    render_rovers(frame, split[0], app);
    render_jobs(frame, split[1], app);
}

fn render_rovers(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let items = if app.rovers.is_empty() {
        vec![ListItem::new("(none)")]
    } else {
        app.rovers
            .iter()
            .map(|rover| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        sanitize_terminal(&rover.name),
                        Style::default().fg(Color::Green),
                    ),
                    Span::raw(format!(
                        "  units={}  tags={}",
                        rover.units,
                        rover.tags.join(",")
                    )),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .title("Enrolled rovers")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(list, area);
}

fn render_jobs(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(6)])
        .split(area);

    let rows = if app.jobs.is_empty() {
        vec![Row::new(vec![
            Cell::from("(none)"),
            Cell::from("-"),
            Cell::from("-"),
        ])]
    } else {
        app.jobs
            .iter()
            .enumerate()
            .map(|(idx, job)| {
                let style = if idx == app.selected_job {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Row::new(vec![
                    Cell::from(job.status.clone()),
                    Cell::from(job.title.clone()),
                    Cell::from(shorten(&job.id, 12)),
                ])
                .style(style)
            })
            .collect()
    };
    let widths = [
        Constraint::Length(10),
        Constraint::Min(24),
        Constraint::Length(14),
    ];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["status", "title", "id"]).style(Style::default().fg(Color::Cyan)))
        .block(Block::default().title("Jobs").borders(Borders::ALL))
        .column_spacing(1);
    frame.render_widget(table, layout[0]);

    let detail = if let Some(job) = app.selected_job() {
        vec![
            Line::from(vec![
                Span::styled("selected: ", Style::default().fg(Color::Cyan)),
                Span::raw(shorten(&job.id, 24)),
            ]),
            Line::from(vec![
                Span::styled("status: ", Style::default().fg(Color::Cyan)),
                Span::raw(job.status.clone()),
            ]),
            Line::from(vec![
                Span::styled("title: ", Style::default().fg(Color::Cyan)),
                Span::raw(job.title.clone()),
            ]),
            Line::from(vec![
                Span::styled("pilot: ", Style::default().fg(Color::Cyan)),
                Span::raw(job.pilot_cmd.clone()),
            ]),
        ]
    } else {
        vec![Line::from("(no job selected)")]
    };
    frame.render_widget(
        Paragraph::new(Text::from(detail))
            .block(
                Block::default()
                    .title("Selected job detail")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true }),
        layout[1],
    );
}

fn render_models(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let rows = if app.models.is_empty() {
        vec![Row::new(vec![
            Cell::from("(none)"),
            Cell::from("-"),
            Cell::from("-"),
        ])]
    } else {
        app.models
            .iter()
            .enumerate()
            .map(|(idx, model)| {
                let style = if idx == app.selected_model {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Row::new(vec![
                    Cell::from(model.provider_id.clone()),
                    Cell::from(model.model_id.clone()),
                    Cell::from(model.endpoint.clone()),
                    Cell::from(model.origin),
                ])
                .style(style)
            })
            .collect()
    };
    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(20),
            Constraint::Min(24),
            Constraint::Length(8),
        ],
    )
    .header(
        Row::new(vec!["provider", "model", "endpoint", "source"])
            .style(Style::default().fg(Color::Cyan)),
    )
    .block(
        Block::default()
            .title("Safe local OpenCode models")
            .borders(Borders::ALL),
    )
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn render_auth(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(5)])
        .split(area);
    let rows = if app.auth.is_empty() {
        vec![Row::new(vec![
            Cell::from("(none)"),
            Cell::from("-"),
            Cell::from("-"),
        ])]
    } else {
        app.auth
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let style = if idx == app.selected_auth {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let oauth = item
                    .oauth
                    .as_ref()
                    .map(|oauth| oauth.expiry_state.to_string())
                    .unwrap_or_else(|| "-".to_string());
                Row::new(vec![
                    Cell::from(item.provider_id.clone()),
                    Cell::from(item.kind.to_string()),
                    Cell::from(item.policy.to_string()),
                    Cell::from(oauth),
                    Cell::from(if item.metadata_present {
                        "present"
                    } else {
                        "-"
                    }),
                ])
                .style(style)
            })
            .collect()
    };
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Length(10),
            ],
        )
        .header(
            Row::new(vec!["provider", "kind", "policy", "oauth", "meta"])
                .style(Style::default().fg(Color::Cyan)),
        )
        .block(
            Block::default()
                .title("OpenCode auth summaries")
                .borders(Borders::ALL),
        )
        .column_spacing(1),
        layout[0],
    );

    let detail = if let Some(item) = app.selected_auth() {
        let provider = sanitize_terminal(&item.provider_id);
        let command = if item.policy == ProviderPolicy::Usable {
            format!(
                "login: opencode auth login --pure --provider {provider}\nlogout: opencode auth logout --provider {provider}\nmutation: deferred (read-only in M05)"
            )
        } else {
            "ignored credential family; mutation commands hidden".to_string()
        };
        vec![
            Line::from(vec![
                Span::styled("source: ", Style::default().fg(Color::Cyan)),
                Span::raw(item.source.to_string()),
            ]),
            Line::from(vec![
                Span::styled("policy: ", Style::default().fg(Color::Cyan)),
                Span::raw(item.policy.to_string()),
            ]),
            Line::from(command),
        ]
    } else {
        vec![Line::from("(no auth entries)")]
    };
    frame.render_widget(
        Paragraph::new(Text::from(detail))
            .block(
                Block::default()
                    .title("Command/status preview")
                    .borders(Borders::ALL),
            )
            .wrap(Wrap { trim: true }),
        layout[1],
    );
}

fn render_chat(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(8),
            Constraint::Min(5),
        ])
        .split(area);
    render_model_picker(frame, layout[0], app);
    render_prompt(frame, layout[1], app);
    render_response(frame, layout[2], app);
}

fn render_model_picker(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let rows = if app.models.is_empty() {
        vec![Row::new(vec![Cell::from("(no local models)")])]
    } else {
        app.models
            .iter()
            .enumerate()
            .map(|(idx, model)| {
                let style = if idx == app.selected_model {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Row::new(vec![
                    Cell::from(model_key(model).to_string()),
                    Cell::from(model.model_id.clone()),
                    Cell::from(model.endpoint.clone()),
                ])
                .style(style)
            })
            .collect()
    };
    frame.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(16),
                Constraint::Length(20),
                Constraint::Min(24),
            ],
        )
        .header(
            Row::new(vec!["provider", "model", "endpoint"]).style(Style::default().fg(Color::Cyan)),
        )
        .block(
            Block::default()
                .title(format!(
                    "Selected local model: {}",
                    app.selected_model_label()
                ))
                .borders(Borders::ALL),
        )
        .column_spacing(1),
        area,
    );
}

fn render_prompt(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let mut lines = app
        .prompt_lines
        .iter()
        .map(|line| Line::from(sanitize_terminal(line)))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    let title = match app.input_mode {
        InputMode::EditingPrompt => "Prompt (editing)",
        InputMode::Normal => "Prompt",
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_response(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let (title, body) = match &app.response {
        ResponseState::Idle => ("Response", vec![Line::from("(idle)")]),
        ResponseState::Pending => ("Response", vec![Line::from("(sending...)")]),
        ResponseState::Ready(text) => ("Response", split_lines(text)),
        ResponseState::Error(err) => ("Error", split_lines(err)),
    };
    frame.render_widget(
        Paragraph::new(Text::from(body))
            .block(Block::default().title(title).borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_footer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let help = format!(
        "q/Esc quit/back • Tab/Shift-Tab screen switch • j/k or ↑↓ navigate • g/G bounds • r refresh • Enter select/send • i edit prompt • Ctrl-C safe quit • screen={}",
        app.screen.title()
    );
    let footer = Paragraph::new(help)
        .block(Block::default().borders(Borders::ALL).title("Help"))
        .wrap(Wrap { trim: true });
    frame.render_widget(footer, area);
}

fn queue_counts(jobs: &[JobView]) -> (usize, usize, usize, usize) {
    let mut queued = 0;
    let mut running = 0;
    let mut done = 0;
    let mut failed = 0;
    for job in jobs {
        match job.status.as_str() {
            "queued" => queued += 1,
            "running" => running += 1,
            "done" => done += 1,
            "failed" => failed += 1,
            _ => queued += 1,
        }
    }
    (queued, running, done, failed)
}

fn split_lines(text: &str) -> Vec<Line<'_>> {
    let sanitized = sanitize_terminal(text);
    let mut out = Vec::new();
    for line in sanitized.lines() {
        out.push(Line::from(line.to_string()));
    }
    if out.is_empty() {
        out.push(Line::from(String::new()));
    }
    out
}

fn shorten(text: &str, max: usize) -> String {
    let text = sanitize_terminal(text);
    if text.chars().count() <= max {
        return text;
    }
    let mut out = String::new();
    for ch in text.chars().take(max.saturating_sub(1)) {
        out.push(ch);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            rovers: vec![RoverView {
                name: "alpha".into(),
                units: 2,
                tags: vec!["fast".into()],
            }],
            jobs: vec![
                JobView {
                    id: "job-1".into(),
                    title: "ping".into(),
                    pilot_cmd: "echo ping".into(),
                    status: "queued".into(),
                },
                JobView {
                    id: "job-2".into(),
                    title: "pong".into(),
                    pilot_cmd: "echo pong".into(),
                    status: "running".into(),
                },
            ],
            models: vec![LocalModelEntry {
                provider_id: "ollama".into(),
                model_id: "llama3.2".into(),
                endpoint: "http://127.0.0.1:11434/v1".into(),
                origin: "builtin",
            }],
            auth: vec![ProviderSummary {
                provider_id: "openai".into(),
                kind: auth::ProviderKind::Oauth,
                source: auth::AuthSource::Env,
                policy: ProviderPolicy::Usable,
                oauth: Some(auth::OauthSummary {
                    expiry_state: auth::ExpiryState::Valid,
                    account_id: Some("acct".into()),
                    enterprise_url: None,
                }),
                metadata_present: false,
            }],
            status: "ready".into(),
        }
    }

    #[test]
    fn empty_dashboard_renders() {
        let app = App::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let view = format!("{}", terminal.backend());
        assert!(view.contains("Dashboard"));
        assert!(view.contains("(none)"));
    }

    #[test]
    fn status_counts_render() {
        let mut app = App::default();
        app.refresh(sample_snapshot());
        let mut terminal = Terminal::new(TestBackend::new(96, 28)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let view = format!("{}", terminal.backend());
        assert!(view.contains("queued: 1"));
        assert!(view.contains("running: 1"));
    }

    #[test]
    fn navigation_boundaries_clamp() {
        let mut app = App::default();
        app.refresh(sample_snapshot());
        app.screen = Screen::Dashboard;
        app.go_bottom();
        app.move_selection(10);
        assert_eq!(app.selected_job, 1);
        app.go_top();
        app.move_selection(-10);
        assert_eq!(app.selected_job, 0);
    }

    #[test]
    fn selection_preservation_keeps_job() {
        let mut app = App::default();
        app.refresh(sample_snapshot());
        app.selected_job = 1;
        app.selected_job_id = Some("job-2".into());
        let mut snapshot = sample_snapshot();
        snapshot.jobs.reverse();
        app.refresh(snapshot);
        assert_eq!(app.selected_job_id.as_deref(), Some("job-2"));
    }

    #[test]
    fn tiny_terminal_renders() {
        let mut app = App::default();
        app.refresh(sample_snapshot());
        let mut terminal = Terminal::new(TestBackend::new(20, 8)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
    }

    #[test]
    fn auth_redaction_hides_secrets() {
        let mut app = App::default();
        let mut snapshot = sample_snapshot();
        snapshot.auth.push(ProviderSummary {
            provider_id: "github-copilot".into(),
            kind: auth::ProviderKind::Api,
            source: auth::AuthSource::File,
            policy: ProviderPolicy::UnsupportedCredential,
            oauth: None,
            metadata_present: true,
        });
        app.refresh(snapshot);
        app.screen = Screen::Auth;
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let view = format!("{}", terminal.backend());
        assert!(!view.contains("secret"));
        assert!(view.contains("ignored"));
    }

    #[test]
    fn local_models_only_render() {
        let mut app = App::default();
        let mut snapshot = sample_snapshot();
        snapshot.models.push(LocalModelEntry {
            provider_id: "remote".into(),
            model_id: "bad".into(),
            endpoint: "http://127.0.0.1:9999/v1".into(),
            origin: "config",
        });
        app.refresh(snapshot);
        app.screen = Screen::Models;
        let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let view = format!("{}", terminal.backend());
        assert!(view.contains("http://127.0.0.1:11434/v1"));
    }

    #[test]
    fn tui_one_exchange_renders_ping_then_pong() {
        let mut app = App::default();
        app.refresh(sample_snapshot());
        app.screen = Screen::Chat;
        app.selected_model = 0;
        app.selected_model_id = Some("ollama".into());
        app.prompt_lines = vec!["ping".into()];
        let request = app.begin_chat_request().unwrap();
        assert_eq!(request.prompt, "ping");

        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let pending = format!("{}", terminal.backend());
        assert!(pending.contains("ping"));
        assert!(pending.contains("sending"));

        app.complete_chat_request(request.request_id, Ok("pong".into()));
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let done = format!("{}", terminal.backend());
        assert!(done.contains("ping"));
        assert!(done.contains("pong"));
    }
}
