use std::{
    io::{self, Stdout},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use orators_core::{
    AdapterMode, DeviceInfo, DiagnosticsReport, OratorsConfig, RuntimeStatus, Severity,
};
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

    fn from_shortcut(shortcut: char) -> Option<usize> {
        match shortcut {
            '1' => Some(View::Dashboard as usize),
            '2' => Some(View::Devices as usize),
            '3' => Some(View::Pairing as usize),
            '4' => Some(View::Settings as usize),
            '5' => Some(View::Setup as usize),
            '6' => Some(View::Logs as usize),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
enum InputMode {
    Normal,
    EditAlias { address: String, value: String },
    EditPairingTimeout { value: String },
    EditAdapter { value: String },
    Confirm(ConfirmAction),
}

#[derive(Debug, Clone)]
enum ConfirmAction {
    ForgetDevice { address: String, label: String },
    ResetDevice { address: String, label: String },
    UninstallBackend,
}

#[derive(Clone, Copy)]
enum SettingItem {
    PairingTimeout,
    AutoReconnect,
    SingleActiveDevice,
    Adapter,
}

pub async fn run() -> Result<()> {
    let mut terminal = enter_terminal()?;
    let result = async {
        let mut app = App::load().await?;
        run_app(&mut terminal, &mut app).await
    }
    .await;
    if let Err(exit_error) = exit_terminal(&mut terminal) {
        return match result {
            Ok(()) => Err(exit_error),
            Err(run_error) => Err(run_error.context(format!(
                "failed to restore terminal after TUI error: {exit_error}"
            ))),
        };
    }
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

    fn jump_to_view(&mut self, index: usize) {
        if index < View::ALL.len() {
            self.view = index;
        }
    }

    fn push_message(&mut self, message: impl Into<String>) {
        let message = message.into();
        if self
            .messages
            .last()
            .is_some_and(|existing| existing == &message)
        {
            return;
        }
        self.messages.push(message);
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

    fn selected_setting_item(&self) -> Option<SettingItem> {
        let items = self.settings_items();
        items.get(self.selected_setting).map(|(item, _, _)| *item)
    }

    fn settings_items(&self) -> Vec<(SettingItem, String, String)> {
        let mut items = vec![
            (
                SettingItem::PairingTimeout,
                "Pairing timeout".to_string(),
                format!("{}s", self.config.pairing_timeout_secs),
            ),
            (
                SettingItem::AutoReconnect,
                "Auto reconnect".to_string(),
                yes_no(self.config.auto_reconnect).to_string(),
            ),
            (
                SettingItem::SingleActiveDevice,
                "Single active device".to_string(),
                yes_no(self.config.single_active_device).to_string(),
            ),
        ];

        if self.adapter_setting_visible() {
            items.push((
                SettingItem::Adapter,
                "Adapter".to_string(),
                self.config
                    .adapter
                    .clone()
                    .unwrap_or_else(|| "auto".to_string()),
            ));
        }

        items
    }

    fn adapter_setting_visible(&self) -> bool {
        !self.status.as_ref().is_some_and(|status| {
            status.backend.adapter_mode == AdapterMode::Auto
                && status.backend.resolved_adapter.is_some()
        })
    }

    async fn refresh(&mut self) {
        if let Ok((_, config)) = load_local_config() {
            self.config = config;
        }

        match ControllerClient::connect().await {
            Ok(client) => match self.refresh_from_client(&client).await {
                Ok(()) => {
                    self.connection_error = None;
                }
                Err(error) => {
                    self.connection_error =
                        Some(format!("Failed to refresh daemon status: {error}"));
                }
            },
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

    async fn refresh_from_client(&mut self, client: &ControllerClient) -> Result<()> {
        let status_json = client.status().await?;
        let diagnostics_json = client.get_diagnostics().await?;
        let config = client.get_config_or_local().await?;

        let status: RuntimeStatus =
            serde_json::from_str(&status_json).context("failed to decode daemon status payload")?;
        let diagnostics: DiagnosticsReport = serde_json::from_str(&diagnostics_json)
            .context("failed to decode daemon diagnostics payload")?;
        let config_value: OratorsConfig =
            serde_json::from_str(&config.json).context("failed to decode daemon config payload")?;

        self.status = Some(status);
        self.diagnostics = Some(diagnostics);
        self.config = config_value;

        if !config.daemon_backed {
            self.push_message(
                "Running daemon does not expose GetConfig yet; using local config fallback.",
            );
        }

        Ok(())
    }

    async fn handle_key(&mut self, terminal: &mut TuiTerminal, key: KeyEvent) -> Result<()> {
        match &mut self.input_mode {
            InputMode::Normal => self.handle_normal_key(terminal, key).await,
            InputMode::EditAlias { .. } => self.handle_alias_input(key).await,
            InputMode::EditPairingTimeout { .. } => self.handle_pairing_timeout_input(key).await,
            InputMode::EditAdapter { .. } => self.handle_adapter_input(key).await,
            InputMode::Confirm(_) => self.handle_confirm_input(terminal, key).await,
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

    async fn handle_confirm_input(
        &mut self,
        terminal: &mut TuiTerminal,
        key: KeyEvent,
    ) -> Result<()> {
        let action = match std::mem::replace(&mut self.input_mode, InputMode::Normal) {
            InputMode::Confirm(action) => action,
            other => {
                self.input_mode = other;
                return Ok(());
            }
        };

        match key.code {
            KeyCode::Esc | KeyCode::Char('n') => {}
            KeyCode::Enter | KeyCode::Char('y') => {
                self.execute_confirm_action(terminal, action).await?;
            }
            _ => {
                self.input_mode = InputMode::Confirm(action);
            }
        }

        Ok(())
    }

    async fn execute_confirm_action(
        &mut self,
        terminal: &mut TuiTerminal,
        action: ConfirmAction,
    ) -> Result<()> {
        match action {
            ConfirmAction::ForgetDevice { address, label } => {
                let client = ControllerClient::connect().await?;
                client.forget_device(&address).await?;
                self.push_message(format!("Forgot {label}."));
                self.refresh().await;
            }
            ConfirmAction::ResetDevice { address, label } => {
                let client = ControllerClient::connect().await?;
                if self
                    .status
                    .as_ref()
                    .and_then(|status| {
                        status
                            .devices
                            .iter()
                            .find(|device| device.address == address)
                            .map(|device| device.connected)
                    })
                    .unwrap_or(false)
                {
                    client.disconnect_device(&address).await?;
                }
                client.forget_device(&address).await?;
                self.push_message(format!("Reset {label} on the host."));
                self.refresh().await;
            }
            ConfirmAction::UninstallBackend => {
                self.run_uninstall_flow(terminal).await?;
            }
        }

        Ok(())
    }

    async fn handle_normal_key(&mut self, terminal: &mut TuiTerminal, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => self.next_view(),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => self.previous_view(),
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(index) = View::from_shortcut(c) {
                    self.jump_to_view(index);
                }
            }
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
            KeyCode::Char('u') => {
                self.input_mode = InputMode::Confirm(ConfirmAction::UninstallBackend)
            }
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
            KeyCode::Enter | KeyCode::Char('c') => {
                if device.connected {
                    client.disconnect_device(&device.address).await?;
                    self.push_message(format!(
                        "Disconnected {}.",
                        device_label(device.alias.as_deref(), &device.address)
                    ));
                } else {
                    client.connect_device(&device.address).await?;
                    self.push_message(format!(
                        "Connect requested for {}.",
                        device_label(device.alias.as_deref(), &device.address)
                    ));
                }
                self.refresh().await;
            }
            KeyCode::Char('x') => {
                self.input_mode = InputMode::Confirm(ConfirmAction::ResetDevice {
                    address: device.address.clone(),
                    label: device_label(device.alias.as_deref(), &device.address),
                });
            }
            KeyCode::Char('f') => {
                self.input_mode = InputMode::Confirm(ConfirmAction::ForgetDevice {
                    address: device.address.clone(),
                    label: device_label(device.alias.as_deref(), &device.address),
                });
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
        if self.selected_setting >= items.len() {
            self.selected_setting = items.len() - 1;
        }

        match (items[self.selected_setting].0, key.code) {
            (SettingItem::PairingTimeout, KeyCode::Enter) => {
                self.input_mode = InputMode::EditPairingTimeout {
                    value: self.config.pairing_timeout_secs.to_string(),
                };
            }
            (SettingItem::AutoReconnect, KeyCode::Enter | KeyCode::Char(' ')) => {
                let client = ControllerClient::connect().await?;
                client
                    .set_auto_reconnect(!self.config.auto_reconnect)
                    .await?;
                self.push_message("Auto reconnect updated.");
                self.refresh().await;
            }
            (SettingItem::SingleActiveDevice, KeyCode::Enter | KeyCode::Char(' ')) => {
                let client = ControllerClient::connect().await?;
                client
                    .set_single_active_device(!self.config.single_active_device)
                    .await?;
                self.push_message("Single active device setting updated.");
                self.refresh().await;
            }
            (SettingItem::Adapter, KeyCode::Enter) => {
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
            KeyCode::Char('u') => {
                self.input_mode = InputMode::Confirm(ConfirmAction::UninstallBackend)
            }
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
                let mode = match install.adapter_mode {
                    orators_linux::systemd::SystemBackendAdapterMode::Auto => "auto",
                    orators_linux::systemd::SystemBackendAdapterMode::Explicit => "explicit",
                };
                self.push_message(format!(
                    "Installed backend in {mode} mode on {}.",
                    install.resolved_adapter
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
                Constraint::Length(4),
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

        let banner = Paragraph::new(self.banner_lines())
            .block(Block::default().borders(Borders::ALL).title("Overview"))
            .wrap(Wrap { trim: true });
        frame.render_widget(banner, layout[1]);

        match self.current_view() {
            View::Dashboard => self.draw_dashboard(frame, layout[2]),
            View::Devices => self.draw_devices(frame, layout[2]),
            View::Pairing => self.draw_pairing(frame, layout[2]),
            View::Settings => self.draw_settings(frame, layout[2]),
            View::Setup => self.draw_setup(frame, layout[2]),
            View::Logs => self.draw_logs(frame, layout[2]),
        }

        let footer = Paragraph::new(self.footer_text())
            .block(Block::default().borders(Borders::ALL).title("Keys"));
        frame.render_widget(footer, layout[3]);

        self.draw_modal(frame);
    }

    fn banner_lines(&self) -> Vec<Line<'static>> {
        let mut line_one = vec![
            Span::styled(
                if self.connection_error.is_some() {
                    "Daemon offline"
                } else {
                    "Daemon online"
                },
                Style::default()
                    .fg(if self.connection_error.is_some() {
                        Color::Red
                    } else {
                        Color::Green
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw(format!("View {} of {}", self.view + 1, View::ALL.len())),
        ];

        if let Some(status) = &self.status {
            line_one.extend([
                Span::raw("  "),
                Span::raw(format!(
                    "Active {}",
                    status.active_device.as_deref().unwrap_or("none")
                )),
                Span::raw("  "),
                Span::raw(format!(
                    "Pairing {}",
                    if status.pairing.enabled { "on" } else { "off" }
                )),
                Span::raw("  "),
                Span::raw(format!(
                    "Backend {}",
                    if status.backend.system_service_ready {
                        "ready"
                    } else {
                        "needs repair"
                    }
                )),
            ]);
        }

        vec![Line::from(line_one), Line::from(self.banner_hint_text())]
    }

    fn banner_hint_text(&self) -> String {
        if let Some(error) = &self.connection_error {
            return format!("Connection problem: {error}");
        }

        if self
            .status
            .as_ref()
            .is_some_and(|status| !status.backend.system_service_ready)
        {
            return "Use Setup (5) to install or repair the managed backend.".to_string();
        }

        if self
            .status
            .as_ref()
            .is_some_and(|status| status.devices.is_empty())
        {
            return "No devices yet. Start pairing from Dashboard (1) or Pairing (3).".to_string();
        }

        "Use 1-6 to jump views, Left/Right to switch tabs, r to refresh, q to quit.".to_string()
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

        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(chunks[0]);

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
                    "Adapter: {}",
                    status
                        .backend
                        .resolved_adapter
                        .as_deref()
                        .unwrap_or("not resolved")
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
            top[0],
        );

        frame.render_widget(
            Paragraph::new(self.dashboard_action_lines())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Quick Actions"),
                )
                .wrap(Wrap { trim: true }),
            top[1],
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
        let layout = if area.width >= 100 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
                .split(area)
        };

        let items = self
            .status
            .as_ref()
            .map(|status| {
                status
                    .devices
                    .iter()
                    .map(|device| {
                        let badges = device_badges(self.status.as_ref(), &self.config, device);
                        let text = if badges.is_empty() {
                            device_label(device.alias.as_deref(), &device.address)
                        } else {
                            format!(
                                "{}  {}",
                                device_label(device.alias.as_deref(), &device.address),
                                badges
                            )
                        };
                        ListItem::new(text)
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
        frame.render_stateful_widget(list, layout[0], &mut state);

        frame.render_widget(
            Paragraph::new(self.device_detail_lines())
                .block(Block::default().borders(Borders::ALL).title("Selection"))
                .wrap(Wrap { trim: true }),
            layout[1],
        );
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
        let layout = if area.width >= 100 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area)
        };

        let items = self
            .settings_items()
            .into_iter()
            .map(|(_, label, value)| ListItem::new(format!("{label}: {value}")))
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
        frame.render_stateful_widget(list, layout[0], &mut state);
        frame.render_widget(
            Paragraph::new(self.setting_detail_lines())
                .block(Block::default().borders(Borders::ALL).title("Details"))
                .wrap(Wrap { trim: true }),
            layout[1],
        );
    }

    fn draw_setup(&self, frame: &mut Frame<'_>, area: Rect) {
        let lines = vec![
            Line::from("Use this view for first-run setup and backend repair."),
            Line::from(format!(
                "Adapter mode: {}",
                self.status
                    .as_ref()
                    .map(|status| match status.backend.adapter_mode {
                        AdapterMode::Auto => "auto",
                        AdapterMode::Explicit => "explicit",
                    })
                    .unwrap_or("unknown")
            )),
            Line::from(format!(
                "Resolved adapter: {}",
                self.status
                    .as_ref()
                    .and_then(|status| status.backend.resolved_adapter.as_deref())
                    .unwrap_or("not resolved")
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
            Line::from(""),
            Line::from("Suggested flow:"),
            Line::from("1. Install or repair the backend with `i`."),
            Line::from("2. Return to Pairing or Devices once the backend is ready."),
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

    fn dashboard_action_lines(&self) -> Vec<Line<'static>> {
        let pairing_action = if self
            .status
            .as_ref()
            .is_some_and(|status| status.pairing.enabled)
        {
            "`p` stop pairing mode".to_string()
        } else {
            format!(
                "`p` start pairing for {} seconds",
                self.config.pairing_timeout_secs
            )
        };

        vec![
            Line::from(pairing_action),
            Line::from("`i` install or repair the managed backend"),
            Line::from("`u` uninstall the backend (confirmation required)"),
            Line::from("`2` jump straight to Devices after pairing"),
            Line::from("`4` review settings like auto reconnect"),
        ]
    }

    fn device_detail_lines(&self) -> Vec<Line<'static>> {
        let Some(status) = &self.status else {
            return vec![
                Line::from("No live device data yet."),
                Line::from(
                    self.connection_error
                        .clone()
                        .unwrap_or_else(|| "Connect to the daemon to inspect devices.".to_string()),
                ),
            ];
        };

        let Some(device) = self.selected_device() else {
            return if status.devices.is_empty() {
                vec![
                    Line::from("No Bluetooth devices are known yet."),
                    Line::from("Start pairing from Dashboard (1) or Pairing (3)."),
                    Line::from(
                        "After a phone appears, return here to connect, trust, and rename it.",
                    ),
                ]
            } else {
                vec![Line::from("Select a device to inspect it.")]
            };
        };

        let is_active = status.active_device.as_deref() == Some(device.address.as_str());
        let mut lines = vec![
            Line::from(device_label(device.alias.as_deref(), &device.address)),
            Line::from(format!("Address: {}", device.address)),
            Line::from(format!(
                "State: paired={}, trusted={}, connected={}, active={}",
                yes_no(device.paired),
                yes_no(device.trusted),
                yes_no(device.connected),
                yes_no(is_active)
            )),
            Line::from(format!(
                "Allowlisted: {}",
                yes_no(self.config.allows_device(&device.address))
            )),
            Line::from(format!("Auto reconnect: {}", yes_no(device.auto_reconnect))),
            Line::from(format!(
                "Profile: {}",
                device
                    .active_profile
                    .as_ref()
                    .map(profile_label)
                    .unwrap_or("none")
            )),
            Line::from(""),
        ];

        if device.connected {
            lines.push(Line::from(
                "Primary action: Enter or `c` disconnects this device.",
            ));
        } else {
            lines.push(Line::from(
                "Primary action: Enter or `c` connects this device.",
            ));
        }
        lines.push(Line::from(
            "`a` toggles the allowlist entry used for auto-trust.",
        ));
        lines.push(Line::from("`t` toggles trust immediately on the host."));
        lines.push(Line::from(
            "`n` renames the local alias. `N` clears that alias.",
        ));
        lines.push(Line::from(
            "`f` forgets host pairing only. `x` disconnects if needed, then forgets.",
        ));
        lines.push(Line::from(
            "Forget/reset keep the allowlist entry in place.",
        ));
        lines
    }

    fn setting_detail_lines(&self) -> Vec<Line<'static>> {
        let Some(setting) = self.selected_setting_item() else {
            return vec![Line::from("Select a setting to inspect it.")];
        };

        match setting {
            SettingItem::PairingTimeout => vec![
                Line::from(format!(
                    "Current timeout: {} seconds",
                    self.config.pairing_timeout_secs
                )),
                Line::from(
                    "Used when you start pairing from the Dashboard, Pairing, or Setup views.",
                ),
                Line::from("Press Enter to type a new timeout."),
            ],
            SettingItem::AutoReconnect => vec![
                Line::from(format!(
                    "Current value: {}",
                    yes_no(self.config.auto_reconnect)
                )),
                Line::from("Controls whether trusted devices are marked for automatic reconnect."),
                Line::from("Press Enter or Space to toggle it immediately."),
            ],
            SettingItem::SingleActiveDevice => vec![
                Line::from(format!(
                    "Current value: {}",
                    yes_no(self.config.single_active_device)
                )),
                Line::from(
                    "When enabled, a second connect request is rejected while another device is active.",
                ),
                Line::from("Press Enter or Space to toggle it."),
            ],
            SettingItem::Adapter => vec![
                Line::from(format!(
                    "Current override: {}",
                    self.config.adapter.as_deref().unwrap_or("auto")
                )),
                Line::from("Only needed on hosts with multiple Bluetooth adapters."),
                Line::from("Press Enter to edit it. Reinstall the backend after changing it."),
            ],
        }
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
        let (title, lines) = match &self.input_mode {
            InputMode::Normal => return,
            InputMode::EditAlias { value, .. } => (
                "Edit Alias".to_string(),
                vec![
                    Line::from("Enter a friendly local name for this device."),
                    Line::from(""),
                    Line::from(value.to_string()),
                    Line::from(""),
                    Line::from("Enter to save, Esc to cancel."),
                ],
            ),
            InputMode::EditPairingTimeout { value } => (
                "Pairing Timeout".to_string(),
                vec![
                    Line::from("Enter the default pairing window in seconds."),
                    Line::from(""),
                    Line::from(value.to_string()),
                    Line::from(""),
                    Line::from("Enter to save, Esc to cancel."),
                ],
            ),
            InputMode::EditAdapter { value } => (
                "Adapter".to_string(),
                vec![
                    Line::from("Enter hciX only when you need a specific Bluetooth adapter."),
                    Line::from("Leave it blank to use auto mode."),
                    Line::from(""),
                    Line::from(value.to_string()),
                    Line::from(""),
                    Line::from("Enter to save, Esc to cancel."),
                ],
            ),
            InputMode::Confirm(action) => match action {
                ConfirmAction::ForgetDevice { label, .. } => (
                    "Forget Device".to_string(),
                    vec![
                        Line::from(format!("Forget {label}?")),
                        Line::from("This removes host-side pairing state only."),
                        Line::from("The allowlist entry stays in place."),
                        Line::from(""),
                        Line::from("Enter or y to confirm. Esc or n to cancel."),
                    ],
                ),
                ConfirmAction::ResetDevice { label, .. } => (
                    "Reset Device".to_string(),
                    vec![
                        Line::from(format!("Reset {label}?")),
                        Line::from(
                            "This disconnects the device if needed, then forgets it on the host.",
                        ),
                        Line::from("The allowlist entry stays in place."),
                        Line::from(""),
                        Line::from("Enter or y to confirm. Esc or n to cancel."),
                    ],
                ),
                ConfirmAction::UninstallBackend => (
                    "Uninstall Backend".to_string(),
                    vec![
                        Line::from("Remove the managed backend?"),
                        Line::from(
                            "Bluetooth audio from Orators will stop until you reinstall it.",
                        ),
                        Line::from(""),
                        Line::from("Enter or y to confirm. Esc or n to cancel."),
                    ],
                ),
            },
        };

        let area = centered_rect(64, 28, frame.area());
        frame.render_widget(Clear, area);
        let modal = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: true });
        frame.render_widget(modal, area);
    }

    fn footer_text(&self) -> Line<'static> {
        match self.current_view() {
            View::Dashboard => Line::from(
                "1-6 views, Left/Right switch, p pairing, i install, u uninstall, r refresh, q quit",
            ),
            View::Devices => Line::from(
                "1-6 views, j/k move, Enter/c connect, a allow, t trust, f forget, x reset, n alias",
            ),
            View::Pairing => Line::from("p toggle pairing, r refresh, q quit"),
            View::Settings => {
                Line::from("1-6 views, j/k move, Enter or Space edits/toggles, r refresh, q quit")
            }
            View::Setup => {
                Line::from("1-6 views, i install backend, u uninstall backend, r refresh, q quit")
            }
            View::Logs => Line::from("1-6 views, Left/Right switch, r refresh, q quit"),
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
                if let Err(error) = app.handle_key(terminal, key).await {
                    app.push_message(format!("Error: {error}"));
                    app.refresh().await;
                }
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

fn device_label(alias: Option<&str>, address: &str) -> String {
    match alias {
        Some(alias) if alias != address => format!("{alias} [{address}]"),
        _ => address.to_string(),
    }
}

fn device_badges(
    status: Option<&RuntimeStatus>,
    config: &OratorsConfig,
    device: &DeviceInfo,
) -> String {
    let mut badges = Vec::new();
    if status.is_some_and(|status| status.active_device.as_deref() == Some(device.address.as_str()))
    {
        badges.push("active");
    }
    if device.connected {
        badges.push("connected");
    }
    if device.trusted {
        badges.push("trusted");
    }
    if config.allows_device(&device.address) {
        badges.push("allowed");
    }
    if badges.is_empty() {
        String::new()
    } else {
        format!("[{}]", badges.join("] ["))
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn profile_label(profile: &orators_core::BluetoothProfile) -> &'static str {
    match profile {
        orators_core::BluetoothProfile::Media => "media",
        orators_core::BluetoothProfile::Call => "call",
    }
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
