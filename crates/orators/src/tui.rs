use std::{
    io::{self, Stdout},
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use orators_core::{DeviceInfo, DiagnosticsReport, OratorsConfig, RuntimeStatus, Severity};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};

use crate::control::{
    ControllerClient, install_system_backend, load_local_config, save_local_config,
    uninstall_system_backend,
};

type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Dashboard,
    Devices,
    Pairing,
    Settings,
    Setup,
    Logs,
}

impl View {
    const ALL: [View; 6] = [
        View::Dashboard,
        View::Devices,
        View::Pairing,
        View::Settings,
        View::Setup,
        View::Logs,
    ];

    fn title(self) -> &'static str {
        match self {
            View::Dashboard => "Dashboard",
            View::Devices => "Devices",
            View::Pairing => "Pairing",
            View::Settings => "Settings",
            View::Setup => "Setup",
            View::Logs => "Logs",
        }
    }
}

#[derive(Debug, Clone)]
enum InputMode {
    Normal,
    EditAlias { address: String, value: String },
    EditPairingTimeout { value: String },
    EditAdapter { value: String },
}

pub async fn run() -> Result<()> {
    let mut terminal = enter_terminal()?;
    let mut app = App::load().await?;
    let result = run_app(&mut terminal, &mut app).await;
    exit_terminal(&mut terminal)?;
    result
}

struct App {
    view: usize,
    selected_device: usize,
    selected_setting: usize,
    status: Option<RuntimeStatus>,
    diagnostics: Option<DiagnosticsReport>,
    config: OratorsConfig,
    connection_error: Option<String>,
    messages: Vec<String>,
    input_mode: InputMode,
    should_quit: bool,
}

impl App {
    async fn load() -> Result<Self> {
        let (_, config) = load_local_config()?;
        let mut app = Self {
            view: 0,
            selected_device: 0,
            selected_setting: 0,
            status: None,
            diagnostics: None,
            config,
            connection_error: None,
            messages: Vec::new(),
            input_mode: InputMode::Normal,
            should_quit: false,
        };
        app.refresh().await;
        Ok(app)
    }

    fn current_view(&self) -> View {
        View::ALL[self.view]
    }

    fn next_view(&mut self) {
        self.view = (self.view + 1) % View::ALL.len();
    }

    fn previous_view(&mut self) {
        self.view = if self.view == 0 {
            View::ALL.len() - 1
        } else {
            self.view - 1
        };
    }

    fn push_message(&mut self, message: impl Into<String>) {
        self.messages.push(message.into());
        if self.messages.len() > 20 {
            let drop_count = self.messages.len() - 20;
            self.messages.drain(0..drop_count);
        }
    }

    fn selected_device(&self) -> Option<&DeviceInfo> {
        self.status
            .as_ref()
            .and_then(|status| status.devices.get(self.selected_device))
    }

    fn settings_items(&self) -> Vec<(String, String)> {
        vec![
            (
                "Pairing timeout".to_string(),
                format!("{}s", self.config.pairing_timeout_secs),
            ),
            (
                "Auto reconnect".to_string(),
                yes_no(self.config.auto_reconnect).to_string(),
            ),
            (
                "Single active device".to_string(),
                yes_no(self.config.single_active_device).to_string(),
            ),
            (
                "Adapter".to_string(),
                self.config
                    .adapter
                    .clone()
                    .unwrap_or_else(|| "auto".to_string()),
            ),
        ]
    }

    async fn refresh(&mut self) {
        if let Ok((_, config)) = load_local_config() {
            self.config = config;
        }

        match ControllerClient::connect().await {
            Ok(client) => {
                match (
                    client.status().await,
                    client.get_diagnostics().await,
                    client.get_config().await,
                ) {
                    (Ok(status), Ok(diagnostics), Ok(config)) => {
                        self.status = serde_json::from_str(&status).ok();
                        self.diagnostics = serde_json::from_str(&diagnostics).ok();
                        if let Ok(config) = serde_json::from_str(&config) {
                            self.config = config;
                        }
                        self.connection_error = None;
                    }
                    _ => {
                        self.connection_error = Some("Failed to refresh daemon status".to_string());
                    }
                }
            }
            Err(error) => {
                self.connection_error = Some(error.to_string());
                self.status = None;
                self.diagnostics = None;
                self.view = View::Setup as usize;
            }
        }

        if let Some(status) = &self.status {
            if self.selected_device >= status.devices.len() && !status.devices.is_empty() {
                self.selected_device = status.devices.len() - 1;
            }
        } else {
            self.selected_device = 0;
        }
    }

    async fn handle_key(&mut self, terminal: &mut TuiTerminal, key: KeyEvent) -> Result<()> {
        match &mut self.input_mode {
            InputMode::Normal => self.handle_normal_key(terminal, key).await,
            InputMode::EditAlias { .. } => self.handle_alias_input(key).await,
            InputMode::EditPairingTimeout { .. } => self.handle_pairing_timeout_input(key).await,
            InputMode::EditAdapter { .. } => self.handle_adapter_input(key).await,
        }
    }

    async fn handle_alias_input(&mut self, key: KeyEvent) -> Result<()> {
        let (address, mut value) = match std::mem::replace(&mut self.input_mode, InputMode::Normal)
        {
            InputMode::EditAlias { address, value } => (address, value),
            other => {
                self.input_mode = other;
                return Ok(());
            }
        };

        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                if let Err(error) = async {
                    let client = ControllerClient::connect().await?;
                    client.set_device_alias(&address, &value).await?;
                    self.push_message("Local alias updated.");
                    self.refresh().await;
                    Result::<()>::Ok(())
                }
                .await
                {
                    self.push_message(format!("Error: {error}"));
                }
            }
            KeyCode::Backspace => {
                value.pop();
                self.input_mode = InputMode::EditAlias { address, value };
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                value.push(c);
                self.input_mode = InputMode::EditAlias { address, value };
            }
            _ => {
                self.input_mode = InputMode::EditAlias { address, value };
            }
        }
        Ok(())
    }

    async fn handle_pairing_timeout_input(&mut self, key: KeyEvent) -> Result<()> {
        let mut value = match std::mem::replace(&mut self.input_mode, InputMode::Normal) {
            InputMode::EditPairingTimeout { value } => value,
            other => {
                self.input_mode = other;
                return Ok(());
            }
        };

        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                if let Err(error) = async {
                    let timeout = value.parse::<u64>()?;
                    let client = ControllerClient::connect().await?;
                    client.set_pairing_timeout(timeout).await?;
                    self.push_message("Pairing timeout updated.");
                    self.refresh().await;
                    Result::<()>::Ok(())
                }
                .await
                {
                    self.push_message(format!("Error: {error}"));
                }
            }
            KeyCode::Backspace => {
                value.pop();
                self.input_mode = InputMode::EditPairingTimeout { value };
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                value.push(c);
                self.input_mode = InputMode::EditPairingTimeout { value };
            }
            _ => {
                self.input_mode = InputMode::EditPairingTimeout { value };
            }
        }
        Ok(())
    }

    async fn handle_adapter_input(&mut self, key: KeyEvent) -> Result<()> {
        let mut value = match std::mem::replace(&mut self.input_mode, InputMode::Normal) {
            InputMode::EditAdapter { value } => value,
            other => {
                self.input_mode = other;
                return Ok(());
            }
        };

        match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                if let Err(error) = (|| -> Result<()> {
                    let (_, mut config) = load_local_config()?;
                    let trimmed = value.trim();
                    config.adapter = if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_ascii_lowercase())
                    };
                    save_local_config(&config)?;
                    self.config = config;
                    self.push_message("Adapter preference saved. Reinstall backend to apply it.");
                    Ok(())
                })() {
                    self.push_message(format!("Error: {error}"));
                }
            }
            KeyCode::Backspace => {
                value.pop();
                self.input_mode = InputMode::EditAdapter { value };
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                value.push(c);
                self.input_mode = InputMode::EditAdapter { value };
            }
            _ => {
                self.input_mode = InputMode::EditAdapter { value };
            }
        }
        Ok(())
    }

    async fn handle_normal_key(&mut self, terminal: &mut TuiTerminal, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab => self.next_view(),
            KeyCode::BackTab => self.previous_view(),
            KeyCode::Char('r') => self.refresh().await,
            KeyCode::Down | KeyCode::Char('j') => match self.current_view() {
                View::Devices => {
                    if let Some(status) = &self.status {
                        if !status.devices.is_empty() {
                            self.selected_device =
                                (self.selected_device + 1).min(status.devices.len() - 1);
                        }
                    }
                }
                View::Settings => {
                    self.selected_setting = (self.selected_setting + 1)
                        .min(self.settings_items().len().saturating_sub(1));
                }
                _ => {}
            },
            KeyCode::Up | KeyCode::Char('k') => match self.current_view() {
                View::Devices => {
                    self.selected_device = self.selected_device.saturating_sub(1);
                }
                View::Settings => {
                    self.selected_setting = self.selected_setting.saturating_sub(1);
                }
                _ => {}
            },
            _ => match self.current_view() {
                View::Dashboard => self.handle_dashboard_key(terminal, key).await?,
                View::Devices => self.handle_devices_key(key).await?,
                View::Pairing => self.handle_pairing_key(key).await?,
                View::Settings => self.handle_settings_key(key).await?,
                View::Setup => self.handle_setup_key(terminal, key).await?,
                View::Logs => {}
            },
        }
        Ok(())
    }

    async fn handle_dashboard_key(
        &mut self,
        terminal: &mut TuiTerminal,
        key: KeyEvent,
    ) -> Result<()> {
        match key.code {
            KeyCode::Char('p') => self.toggle_pairing().await?,
            KeyCode::Char('i') => self.run_install_flow(terminal).await?,
            KeyCode::Char('u') => self.run_uninstall_flow(terminal).await?,
            _ => {}
        }
        Ok(())
    }

    async fn handle_pairing_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Char('p') | KeyCode::Enter) {
            self.toggle_pairing().await?;
        }
        Ok(())
    }

    async fn handle_devices_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(device) = self.selected_device().cloned() else {
            return Ok(());
        };

        let client = ControllerClient::connect().await?;
        match key.code {
            KeyCode::Char('a') => {
                if self.config.allows_device(&device.address) {
                    client.disallow_device(&device.address).await?;
                    self.push_message(format!("Removed {} from allowlist.", device.address));
                } else {
                    client.allow_device(&device.address).await?;
                    self.push_message(format!("Added {} to allowlist.", device.address));
                }
                self.refresh().await;
            }
            KeyCode::Char('t') => {
                if device.trusted {
                    client.untrust_device(&device.address).await?;
                    self.push_message(format!("Untrusted {}.", device.address));
                } else {
                    client.trust_device(&device.address).await?;
                    self.push_message(format!("Trusted {}.", device.address));
                }
                self.refresh().await;
            }
            KeyCode::Char('c') => {
                if device.connected {
                    client.disconnect_active().await?;
                    self.push_message(format!("Disconnected {}.", device.address));
                } else {
                    client.connect_device(&device.address).await?;
                    self.push_message(format!("Connect requested for {}.", device.address));
                }
                self.refresh().await;
            }
            KeyCode::Char('x') => {
                let _ = client.disconnect_active().await;
                client.forget_device(&device.address).await?;
                self.push_message(format!("Reset {} on the host.", device.address));
                self.refresh().await;
            }
            KeyCode::Char('f') => {
                client.forget_device(&device.address).await?;
                self.push_message(format!("Forgot {}.", device.address));
                self.refresh().await;
            }
            KeyCode::Char('n') => {
                self.input_mode = InputMode::EditAlias {
                    address: device.address,
                    value: device.alias.unwrap_or_default(),
                };
            }
            KeyCode::Char('N') => {
                client.clear_device_alias(&device.address).await?;
                self.push_message(format!("Cleared local alias for {}.", device.address));
                self.refresh().await;
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_settings_key(&mut self, key: KeyEvent) -> Result<()> {
        let items = self.settings_items();
        if items.is_empty() {
            return Ok(());
        }

        match (self.selected_setting, key.code) {
            (0, KeyCode::Enter) => {
                self.input_mode = InputMode::EditPairingTimeout {
                    value: self.config.pairing_timeout_secs.to_string(),
                };
            }
            (1, KeyCode::Enter | KeyCode::Char(' ')) => {
                let client = ControllerClient::connect().await?;
                client
                    .set_auto_reconnect(!self.config.auto_reconnect)
                    .await?;
                self.push_message("Auto reconnect updated.");
                self.refresh().await;
            }
            (2, KeyCode::Enter | KeyCode::Char(' ')) => {
                let client = ControllerClient::connect().await?;
                client
                    .set_single_active_device(!self.config.single_active_device)
                    .await?;
                self.push_message("Single active device setting updated.");
                self.refresh().await;
            }
            (3, KeyCode::Enter) => {
                self.input_mode = InputMode::EditAdapter {
                    value: self.config.adapter.clone().unwrap_or_default(),
                };
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_setup_key(&mut self, terminal: &mut TuiTerminal, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('i') => self.run_install_flow(terminal).await?,
            KeyCode::Char('u') => self.run_uninstall_flow(terminal).await?,
            _ => {}
        }
        Ok(())
    }

    async fn toggle_pairing(&mut self) -> Result<()> {
        let client = ControllerClient::connect().await?;
        if self
            .status
            .as_ref()
            .is_some_and(|status| status.pairing.enabled)
        {
            client.stop_pairing().await?;
            self.push_message("Pairing disabled.");
        } else {
            client
                .start_pairing(self.config.pairing_timeout_secs)
                .await?;
            self.push_message("Pairing enabled.");
        }
        self.refresh().await;
        Ok(())
    }

    async fn run_install_flow(&mut self, terminal: &mut TuiTerminal) -> Result<()> {
        self.push_message("Starting integrated backend install...");
        let adapter = self.config.adapter.clone();
        let result =
            run_with_terminal_suspended(
                terminal,
                async move { install_system_backend(adapter).await },
            )
            .await;
        match result {
            Ok((_, install)) => {
                self.push_message(format!(
                    "Installed backend for adapter {}.",
                    install.adapter
                ));
            }
            Err(error) => self.push_message(format!("Install failed: {error}")),
        }
        self.refresh().await;
        Ok(())
    }

    async fn run_uninstall_flow(&mut self, terminal: &mut TuiTerminal) -> Result<()> {
        self.push_message("Starting backend uninstall...");
        let result =
            run_with_terminal_suspended(terminal, async { uninstall_system_backend().await }).await;
        match result {
            Ok(()) => self.push_message("Backend removed."),
            Err(error) => self.push_message(format!("Uninstall failed: {error}")),
        }
        self.refresh().await;
        Ok(())
    }

    fn draw(&self, frame: &mut Frame<'_>) {
        let root = frame.area();
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(root);

        let titles = View::ALL
            .iter()
            .map(|view| Line::from(Span::raw(view.title())))
            .collect::<Vec<_>>();
        let tabs = Tabs::new(titles)
            .block(Block::default().borders(Borders::ALL).title("Orators"))
            .select(self.view)
            .highlight_style(Style::default().fg(Color::Yellow));
        frame.render_widget(tabs, layout[0]);

        match self.current_view() {
            View::Dashboard => self.draw_dashboard(frame, layout[1]),
            View::Devices => self.draw_devices(frame, layout[1]),
            View::Pairing => self.draw_pairing(frame, layout[1]),
            View::Settings => self.draw_settings(frame, layout[1]),
            View::Setup => self.draw_setup(frame, layout[1]),
            View::Logs => self.draw_logs(frame, layout[1]),
        }

        let footer = Paragraph::new(self.footer_text())
            .block(Block::default().borders(Borders::ALL).title("Keys"));
        frame.render_widget(footer, layout[2]);

        self.draw_modal(frame);
    }

    fn draw_dashboard(&self, frame: &mut Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Min(0),
            ])
            .split(area);

        let status_lines = if let Some(status) = &self.status {
            vec![
                Line::from(format!(
                    "Active device: {}",
                    status.active_device.as_deref().unwrap_or("none")
                )),
                Line::from(format!(
                    "Pairing: {}",
                    if status.pairing.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )),
                Line::from(format!(
                    "Player state: {}",
                    player_state_label(&status.backend.player_state)
                )),
                Line::from(format!(
                    "Backend service ready: {}",
                    yes_no(status.backend.system_service_ready)
                )),
                Line::from(format!(
                    "Local output: {}",
                    status
                        .audio
                        .output_device
                        .as_deref()
                        .unwrap_or("not detected")
                )),
            ]
        } else {
            vec![Line::from(
                self.connection_error
                    .as_deref()
                    .unwrap_or("Daemon not connected."),
            )]
        };
        frame.render_widget(
            Paragraph::new(status_lines)
                .block(Block::default().borders(Borders::ALL).title("Status"))
                .wrap(Wrap { trim: true }),
            chunks[0],
        );

        let doctor_lines = self
            .diagnostics
            .as_ref()
            .map(|report| {
                report
                    .checks
                    .iter()
                    .take(6)
                    .map(|check| {
                        Line::from(format!(
                            "[{}] {}",
                            severity_label(&check.severity),
                            check.summary
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![Line::from("No doctor report yet.")]);
        frame.render_widget(
            Paragraph::new(doctor_lines)
                .block(Block::default().borders(Borders::ALL).title("Doctor"))
                .wrap(Wrap { trim: true }),
            chunks[1],
        );

        self.draw_logs_panel(frame, chunks[2], "Recent Activity");
    }

    fn draw_devices(&self, frame: &mut Frame<'_>, area: Rect) {
        let items = self
            .status
            .as_ref()
            .map(|status| {
                status
                    .devices
                    .iter()
                    .map(|device| {
                        let allowed = if self.config.allows_device(&device.address) {
                            " allowed"
                        } else {
                            ""
                        };
                        let connected = if device.connected { " connected" } else { "" };
                        let trusted = if device.trusted { " trusted" } else { "" };
                        ListItem::new(format!(
                            "{} [{}]{}{}{}",
                            device.alias.as_deref().unwrap_or("unnamed"),
                            device.address,
                            allowed,
                            trusted,
                            connected
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(self.selected_device.min(items.len() - 1)));
        }
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Devices"))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_pairing(&self, frame: &mut Frame<'_>, area: Rect) {
        let lines = if let Some(status) = &self.status {
            vec![
                Line::from(format!(
                    "Pairing: {}",
                    if status.pairing.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )),
                Line::from(format!("Timeout: {}s", status.pairing.timeout_secs)),
                Line::from(format!(
                    "Expires at: {}",
                    status
                        .pairing
                        .expires_at_epoch_secs
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "n/a".to_string())
                )),
            ]
        } else {
            vec![Line::from("Daemon unavailable.")]
        };
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("Pairing"))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn draw_settings(&self, frame: &mut Frame<'_>, area: Rect) {
        let items = self
            .settings_items()
            .into_iter()
            .map(|(label, value)| ListItem::new(format!("{label}: {value}")))
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        if !items.is_empty() {
            state.select(Some(self.selected_setting.min(items.len() - 1)));
        }
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Settings"))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_setup(&self, frame: &mut Frame<'_>, area: Rect) {
        let lines = vec![
            Line::from("Use this view for first-run setup and backend repair."),
            Line::from(format!(
                "Configured adapter: {}",
                self.config.adapter.as_deref().unwrap_or("auto")
            )),
            Line::from(format!(
                "Backend installed: {}",
                self.status
                    .as_ref()
                    .map(|status| yes_no(status.backend.installed))
                    .unwrap_or("no")
            )),
            Line::from(format!(
                "Backend ready: {}",
                self.status
                    .as_ref()
                    .map(|status| yes_no(status.backend.system_service_ready))
                    .unwrap_or("no")
            )),
            Line::from(
                self.connection_error
                    .as_deref()
                    .unwrap_or("Press `i` to install or repair the backend."),
            ),
        ];
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("Setup"))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn draw_logs(&self, frame: &mut Frame<'_>, area: Rect) {
        self.draw_logs_panel(frame, area, "Logs");
    }

    fn draw_logs_panel(&self, frame: &mut Frame<'_>, area: Rect, title: &str) {
        let lines = if self.messages.is_empty() {
            vec![Line::from("No messages yet.")]
        } else {
            self.messages
                .iter()
                .rev()
                .map(|message| Line::from(message.clone()))
                .collect::<Vec<_>>()
        };
        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title(title))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn draw_modal(&self, frame: &mut Frame<'_>) {
        let (title, value, prompt) = match &self.input_mode {
            InputMode::Normal => return,
            InputMode::EditAlias { value, .. } => ("Edit Alias", value.as_str(), "Enter alias"),
            InputMode::EditPairingTimeout { value } => {
                ("Pairing Timeout", value.as_str(), "Enter seconds")
            }
            InputMode::EditAdapter { value } => {
                ("Adapter", value.as_str(), "Enter hciX or blank for auto")
            }
        };

        let area = centered_rect(60, 20, frame.area());
        frame.render_widget(Clear, area);
        let modal = Paragraph::new(vec![
            Line::from(prompt),
            Line::from(""),
            Line::from(value.to_string()),
            Line::from(""),
            Line::from("Enter to save, Esc to cancel."),
        ])
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: true });
        frame.render_widget(modal, area);
    }

    fn footer_text(&self) -> Line<'static> {
        match self.current_view() {
            View::Dashboard => Line::from(
                "Tab/Shift-Tab switch views, p pairing, i install, u uninstall, r refresh, q quit",
            ),
            View::Devices => Line::from(
                "j/k move, a allow, t trust, c connect, f forget, x reset, n alias, N clear alias",
            ),
            View::Pairing => Line::from("p toggle pairing, r refresh, q quit"),
            View::Settings => Line::from("j/k move, Enter edit/toggle setting, r refresh, q quit"),
            View::Setup => Line::from("i install backend, u uninstall backend, r refresh, q quit"),
            View::Logs => Line::from("Tab/Shift-Tab switch views, r refresh, q quit"),
        }
    }
}

async fn run_app(terminal: &mut TuiTerminal, app: &mut App) -> Result<()> {
    let mut last_refresh = Instant::now();

    loop {
        terminal.draw(|frame| app.draw(frame))?;

        if app.should_quit {
            break;
        }

        if event::poll(INPUT_POLL_INTERVAL)? {
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind == KeyEventKind::Press {
                app.handle_key(terminal, key).await?;
            }
        }

        if last_refresh.elapsed() >= REFRESH_INTERVAL {
            app.refresh().await;
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

fn enter_terminal() -> Result<TuiTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn exit_terminal(terminal: &mut TuiTerminal) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, cursor::Show)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn run_with_terminal_suspended<F, T>(terminal: &mut TuiTerminal, future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    exit_terminal(terminal)?;
    let result = future.await;
    *terminal = enter_terminal()?;
    result
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn player_state_label(state: &orators_core::PlayerState) -> &'static str {
    match state {
        orators_core::PlayerState::Waiting => "waiting",
        orators_core::PlayerState::Starting => "starting",
        orators_core::PlayerState::Playing => "playing",
        orators_core::PlayerState::Error => "error",
    }
}

fn severity_label(severity: &Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}
