use std::{future::Future, pin::Pin};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use orators_core::{
    AdapterMode, BluetoothProfile, DiagnosticCheck, DiagnosticsReport, OratorsConfig, PlayerState,
    RuntimeStatus, Severity, normalize_device_address,
};
use orators_linux::systemd::SystemBackendAdapterMode;
use serde_json::Value;

use crate::control::{
    ControllerClient, install_system_backend, install_user_service, uninstall_system_backend,
};

#[derive(Debug, Parser)]
#[command(name = "oratorsctl", about = "Control the Orators daemon")]
pub struct Cli {
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Status,
    Pair(PairArgs),
    Devices(DeviceArgs),
    Config(ConfigArgs),
    Connect {
        mac: String,
    },
    Disconnect,
    Doctor,
    InstallSystemBackend {
        #[arg(long)]
        adapter: Option<String>,
    },
    UninstallSystemBackend,
    InstallUserService,
}

#[derive(Debug, Args)]
pub struct PairArgs {
    #[command(subcommand)]
    pub command: PairCommand,
}

#[derive(Debug, Subcommand)]
pub enum PairCommand {
    Start {
        #[arg(long)]
        timeout: Option<u64>,
    },
    Stop,
}

#[derive(Debug, Args)]
pub struct DeviceArgs {
    #[command(subcommand)]
    pub command: DeviceCommand,
}

#[derive(Debug, Subcommand)]
pub enum DeviceCommand {
    List,
    Allow {
        mac: String,
    },
    Disallow {
        mac: String,
    },
    Trust {
        mac: String,
    },
    Forget {
        mac: String,
    },
    Reset {
        mac: String,
        #[arg(long)]
        drop_allowlist: bool,
    },
    Alias {
        mac: String,
        name: String,
    },
    Unalias {
        mac: String,
    },
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Show,
    Set {
        #[command(subcommand)]
        setting: ConfigSetCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigSetCommand {
    PairingTimeout { seconds: u64 },
    AutoReconnect { enabled: bool },
    SingleActiveDevice { enabled: bool },
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::InstallUserService => install_user_service_command(cli.json).await,
        Command::InstallSystemBackend { adapter } => {
            install_system_backend_command(cli.json, adapter).await
        }
        Command::UninstallSystemBackend => uninstall_system_backend_command(cli.json).await,
        Command::Doctor => run_doctor(cli.json).await,
        command => run_daemon_command(command, cli.json).await,
    }
}

trait DeviceResetClient {
    fn disallow_device<'a>(
        &'a self,
        address: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>>;
    fn status<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>>;
    fn disconnect_active<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>>;
    fn forget_device<'a>(
        &'a self,
        address: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>>;
}

impl DeviceResetClient for ControllerClient {
    fn disallow_device<'a>(
        &'a self,
        address: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
        Box::pin(async move { ControllerClient::disallow_device(self, address).await })
    }

    fn status<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
        Box::pin(async move { ControllerClient::status(self).await })
    }

    fn disconnect_active<'a>(&'a self) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
        Box::pin(async move { ControllerClient::disconnect_active(self).await })
    }

    fn forget_device<'a>(
        &'a self,
        address: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String>> + 'a>> {
        Box::pin(async move { ControllerClient::forget_device(self, address).await })
    }
}

async fn run_daemon_command(command: Command, json: bool) -> Result<()> {
    let client = ControllerClient::connect().await?;
    match command {
        Command::Status => {
            let status = client.status().await?;
            if json {
                print_jsonish(&status)?;
            } else {
                let status: RuntimeStatus = serde_json::from_str(&status)?;
                let diagnostics = parse_optional_diagnostics(client.get_diagnostics().await.ok());
                render_status(&status, diagnostics.as_ref());
            }
        }
        Command::Pair(args) => match args.command {
            PairCommand::Start { timeout } => {
                let output = client.start_pairing(timeout.unwrap_or(120)).await?;
                if json {
                    print_jsonish(&output)?;
                } else {
                    let status: RuntimeStatus = serde_json::from_str(&output)?;
                    let diagnostics =
                        parse_optional_diagnostics(client.get_diagnostics().await.ok());
                    render_pairing_started(&status, diagnostics.as_ref());
                }
            }
            PairCommand::Stop => {
                let output = client.stop_pairing().await?;
                if json {
                    print_jsonish(&output)?;
                } else {
                    let status: RuntimeStatus = serde_json::from_str(&output)?;
                    println!("Pairing mode disabled.");
                    render_status(&status, None);
                }
            }
        },
        Command::Devices(args) => match args.command {
            DeviceCommand::List => {
                let output = client.list_devices().await?;
                if json {
                    print_jsonish(&output)?;
                } else {
                    let devices: Vec<orators_core::DeviceInfo> = serde_json::from_str(&output)?;
                    render_devices(&devices);
                }
            }
            DeviceCommand::Allow { mac } => {
                let mac = normalize_device_address(&mac);
                let output = client.allow_device(&mac).await?;
                render_status_output(output, json, "Device added to allowlist.")?;
            }
            DeviceCommand::Disallow { mac } => {
                let mac = normalize_device_address(&mac);
                let output = client.disallow_device(&mac).await?;
                render_status_output(output, json, "Device removed from allowlist.")?;
            }
            DeviceCommand::Trust { mac } => {
                let output = client.trust_device(&mac).await?;
                render_status_output(output, json, "Device trusted.")?;
            }
            DeviceCommand::Forget { mac } => {
                let output = client.forget_device(&mac).await?;
                render_status_output(output, json, "Device removed.")?;
            }
            DeviceCommand::Reset {
                mac,
                drop_allowlist,
            } => {
                let mac = normalize_device_address(&mac);
                let output = reset_device(&client, &mac, drop_allowlist).await?;
                if json {
                    print_jsonish(&output)?;
                } else {
                    let status: RuntimeStatus = serde_json::from_str(&output)?;
                    println!("Device reset on the host.");
                    if drop_allowlist {
                        println!("Allowlist entry removed.");
                    } else {
                        println!("Allowlist entry preserved.");
                    }
                    println!(
                        "Next: If you also removed it on the phone, run `oratorsctl pair start --timeout 120` and pair again."
                    );
                    render_status(&status, None);
                }
            }
            DeviceCommand::Alias { mac, name } => {
                let mac = normalize_device_address(&mac);
                let output = client.set_device_alias(&mac, &name).await?;
                render_status_output(output, json, "Local device alias updated.")?;
            }
            DeviceCommand::Unalias { mac } => {
                let mac = normalize_device_address(&mac);
                let output = client.clear_device_alias(&mac).await?;
                render_status_output(output, json, "Local device alias cleared.")?;
            }
        },
        Command::Config(args) => match args.command {
            ConfigCommand::Show => {
                let config = client.get_config_or_local().await?;
                if json {
                    print_jsonish(&config.json)?;
                } else {
                    let config_value: OratorsConfig = serde_json::from_str(&config.json)?;
                    if !config.daemon_backed {
                        println!(
                            "Daemon does not expose GetConfig yet; showing the local config file instead."
                        );
                    }
                    render_config(&config_value);
                }
            }
            ConfigCommand::Set { setting } => {
                let output = match setting {
                    ConfigSetCommand::PairingTimeout { seconds } => {
                        client.set_pairing_timeout(seconds).await?
                    }
                    ConfigSetCommand::AutoReconnect { enabled } => {
                        client.set_auto_reconnect(enabled).await?
                    }
                    ConfigSetCommand::SingleActiveDevice { enabled } => {
                        client.set_single_active_device(enabled).await?
                    }
                };
                if json {
                    print_jsonish(&output)?;
                } else {
                    let config: OratorsConfig = serde_json::from_str(&output)?;
                    println!("Configuration updated.");
                    render_config(&config);
                }
            }
        },
        Command::Connect { mac } => {
            let output = client.connect_device(&mac).await?;
            render_status_output(output, json, "Connect request sent.")?;
        }
        Command::Disconnect => {
            let output = client.disconnect_active().await?;
            render_status_output(output, json, "Disconnect request sent.")?;
        }
        Command::Doctor
        | Command::InstallUserService
        | Command::InstallSystemBackend { .. }
        | Command::UninstallSystemBackend => unreachable!(),
    }
    Ok(())
}

async fn reset_device<C: DeviceResetClient + ?Sized>(
    client: &C,
    mac: &str,
    drop_allowlist: bool,
) -> Result<String> {
    let status = if drop_allowlist {
        client.disallow_device(mac).await?
    } else {
        client.status().await?
    };
    let status: RuntimeStatus = serde_json::from_str(&status)?;
    if status.active_device.as_deref() == Some(mac) {
        let _ = client.disconnect_active().await;
    }
    client.forget_device(mac).await
}

async fn run_doctor(json: bool) -> Result<()> {
    let client = ControllerClient::connect().await?;
    let diagnostics = client.get_diagnostics().await?;
    if json {
        print_jsonish(&diagnostics)?;
    } else {
        let report: DiagnosticsReport = serde_json::from_str(&diagnostics)?;
        render_doctor(&report);
    }
    Ok(())
}

async fn install_user_service_command(json: bool) -> Result<()> {
    let unit_path = install_user_service().await?;

    if json {
        println!("{}", unit_path.display());
    } else {
        println!("Installed user service at {}.", unit_path.display());
        println!("This only installs the daemon unit.");
    }
    Ok(())
}

async fn install_system_backend_command(json: bool, adapter: Option<String>) -> Result<()> {
    let (user_unit_path, install) = install_system_backend(adapter).await?;

    if json {
        let payload = serde_json::json!({
            "user_service_path": user_unit_path,
            "wireplumber_fragment_path": install.wireplumber_fragment_path,
            "dbus_policy_path": install.dbus_policy_path,
            "system_unit_path": install.system_unit_path,
            "adapter_mode": match install.adapter_mode {
                SystemBackendAdapterMode::Auto => "auto",
                SystemBackendAdapterMode::Explicit => "explicit",
            },
            "resolved_adapter": install.resolved_adapter,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("Installed user service at {}.", user_unit_path.display());
        println!(
            "Installed WirePlumber Bluetooth-disable fragment at {}.",
            install.wireplumber_fragment_path.display()
        );
        println!(
            "Installed BlueALSA D-Bus policy at {}.",
            install.dbus_policy_path.display()
        );
        println!(
            "Installed BlueALSA system backend at {}.",
            install.system_unit_path.display()
        );
        println!(
            "Backend adapter mode: {}.",
            match install.adapter_mode {
                SystemBackendAdapterMode::Auto => "auto",
                SystemBackendAdapterMode::Explicit => "explicit",
            }
        );
        println!("Resolved Bluetooth adapter: {}.", install.resolved_adapter);
        println!("Restart WirePlumber and the Orators daemon to activate the backend.");
    }

    Ok(())
}

async fn uninstall_system_backend_command(json: bool) -> Result<()> {
    uninstall_system_backend().await?;

    if json {
        println!("{{\"removed\":true}}");
    } else {
        println!("Removed the Orators system backend.");
        println!("WirePlumber Bluetooth ownership was restored.");
    }

    Ok(())
}

fn print_jsonish(value: &str) -> Result<()> {
    match serde_json::from_str::<Value>(value) {
        Ok(json) => println!("{}", serde_json::to_string_pretty(&json)?),
        Err(_) => println!("{value}"),
    }
    Ok(())
}

fn render_status_output(output: String, json: bool, prefix: &str) -> Result<()> {
    if json {
        print_jsonish(&output)?;
    } else {
        let status: RuntimeStatus = serde_json::from_str(&output)?;
        println!("{prefix}");
        render_status(&status, None);
    }
    Ok(())
}

fn render_config(config: &OratorsConfig) {
    println!("Pairing timeout: {}s", config.pairing_timeout_secs);
    println!("Auto reconnect: {}", yes_no(config.auto_reconnect));
    println!(
        "Single active device: {}",
        yes_no(config.single_active_device)
    );
    println!(
        "Configured adapter: {}",
        config.adapter.as_deref().unwrap_or("auto")
    );
    println!("Allowed devices: {}", config.allowed_devices.len());
    if !config.device_aliases.is_empty() {
        println!("Local aliases:");
        for (address, alias) in &config.device_aliases {
            println!("- {address}: {alias}");
        }
    }
}

fn render_pairing_started(status: &RuntimeStatus, diagnostics: Option<&DiagnosticsReport>) {
    println!(
        "Pairing mode enabled for {} seconds.",
        status.pairing.timeout_secs
    );

    if let Some(check) = diagnostics.and_then(find_adapter_check) {
        println!("{}", check.summary);
        if let Some(detail) = &check.detail {
            println!("{detail}");
        }
    } else {
        println!("Open Bluetooth settings on the phone and look for this computer.");
    }

    render_audio_summary(status, diagnostics);
    render_devices_summary(&status.devices);
}

fn render_status(status: &RuntimeStatus, diagnostics: Option<&DiagnosticsReport>) {
    if let Some(check) = diagnostics.and_then(find_adapter_check) {
        println!("Bluetooth: {}", check.summary);
        if let Some(detail) = &check.detail {
            println!("{detail}");
        }
    }

    println!(
        "Pairing: {}",
        if status.pairing.enabled {
            format!(
                "enabled (expires at epoch {})",
                status.pairing.expires_at_epoch_secs.unwrap_or_default()
            )
        } else {
            "disabled".to_string()
        }
    );

    println!(
        "Active device: {}",
        status.active_device.as_deref().unwrap_or("none")
    );
    render_audio_summary(status, diagnostics);
    render_devices_summary(&status.devices);
}

fn render_devices(devices: &[orators_core::DeviceInfo]) {
    if devices.is_empty() {
        println!("No known Bluetooth devices yet.");
        return;
    }

    println!("Known Bluetooth devices:");
    for device in devices {
        let profile = device
            .active_profile
            .as_ref()
            .map(profile_label)
            .unwrap_or("unknown");
        println!(
            "- {} [{}] paired={}, trusted={}, connected={}, profile={}",
            device.alias.as_deref().unwrap_or("unnamed"),
            device.address,
            yes_no(device.paired),
            yes_no(device.trusted),
            yes_no(device.connected),
            profile,
        );
    }
}

fn render_devices_summary(devices: &[orators_core::DeviceInfo]) {
    println!("Known devices: {}", devices.len());
    let connected = devices.iter().filter(|device| device.connected).count();
    let trusted = devices.iter().filter(|device| device.trusted).count();
    println!("Connected: {connected}. Trusted: {trusted}.");
}

fn render_audio_summary(status: &RuntimeStatus, diagnostics: Option<&DiagnosticsReport>) {
    println!(
        "Audio output: {}",
        status
            .audio
            .output_device
            .as_deref()
            .unwrap_or("not detected")
    );
    println!(
        "Audio input: {}",
        status
            .audio
            .input_device
            .as_deref()
            .unwrap_or("not detected")
    );
    println!(
        "Local playback output: {}",
        yes_no(status.audio.local_output_available)
    );
    println!(
        "Backend: {}",
        match status.backend.backend {
            orators_core::MediaBackendKind::Bluealsa => "bluealsa",
        }
    );
    println!("Backend installed: {}", yes_no(status.backend.installed),);
    println!(
        "Backend adapter mode: {}",
        match status.backend.adapter_mode {
            AdapterMode::Auto => "auto",
            AdapterMode::Explicit => "explicit",
        }
    );
    if let Some(adapter) = status.backend.resolved_adapter.as_deref() {
        println!("Resolved adapter: {adapter}");
    }
    if let Some(adapter) = status.backend.configured_adapter.as_deref() {
        println!("Configured adapter override: {adapter}");
    }
    println!(
        "Backend service ready: {}",
        yes_no(status.backend.system_service_ready),
    );
    println!(
        "Player state: {}",
        player_state_label(&status.backend.player_state),
    );
    println!("Player running: {}", yes_no(status.backend.player_running));
    if let Some(address) = status.backend.active_device_address.as_deref() {
        println!("Backend active device: {address}");
    }
    if let Some(error) = status.backend.last_error.as_deref() {
        println!("Last backend error: {error}");
    }
    if let Some(summary) = diagnostics.and_then(host_support_summary) {
        println!("Host readiness: {summary}");
    }
}

fn render_doctor(report: &DiagnosticsReport) {
    let worst = report
        .checks
        .iter()
        .map(|check| &check.severity)
        .max_by_key(|severity| severity_rank(severity))
        .map(|severity| match severity {
            Severity::Info => "ready",
            Severity::Warn => "needs attention",
            Severity::Error => "blocked",
        })
        .unwrap_or("unknown");
    println!("Doctor summary: {worst}.");

    for check in &report.checks {
        println!("[{}] {}", severity_label(&check.severity), check.summary);
        if let Some(detail) = &check.detail {
            println!("  {}", detail);
        }
        if let Some(remediation) = &check.remediation {
            println!("  Next step: {}", remediation);
        }
    }
}

fn parse_optional_diagnostics(report: Option<String>) -> Option<DiagnosticsReport> {
    report.and_then(|report| serde_json::from_str(&report).ok())
}

fn find_adapter_check(report: &DiagnosticsReport) -> Option<&DiagnosticCheck> {
    find_check(report, "bluez.adapter")
}

fn find_check<'a>(report: &'a DiagnosticsReport, code: &str) -> Option<&'a DiagnosticCheck> {
    report.checks.iter().find(|check| check.code == code)
}

fn host_support_summary(report: &DiagnosticsReport) -> Option<&str> {
    let check = find_check(report, "host.media_support")?;
    check.detail.as_deref().or(Some(check.summary.as_str()))
}

fn severity_label(severity: &Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}

fn severity_rank(severity: &Severity) -> usize {
    match severity {
        Severity::Info => 0,
        Severity::Warn => 1,
        Severity::Error => 2,
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn profile_label(profile: &BluetoothProfile) -> &'static str {
    match profile {
        BluetoothProfile::Media => "media",
        BluetoothProfile::Call => "call",
    }
}

fn player_state_label(state: &PlayerState) -> &'static str {
    match state {
        PlayerState::Waiting => "waiting",
        PlayerState::Starting => "starting",
        PlayerState::Playing => "playing",
        PlayerState::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin, sync::Mutex};

    use anyhow::anyhow;
    use orators_core::{AudioDefaults, MediaBackendStatus, PairingWindow};

    use super::{DeviceResetClient, RuntimeStatus, reset_device};

    type StubResult = std::result::Result<String, &'static str>;

    struct FakeDeviceResetClient {
        calls: Mutex<Vec<String>>,
        disallow_result: StubResult,
        status_result: StubResult,
        disconnect_result: StubResult,
        forget_result: StubResult,
    }

    impl DeviceResetClient for FakeDeviceResetClient {
        fn disallow_device<'a>(
            &'a self,
            address: &'a str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + 'a>> {
            let result = self.disallow_result.clone().map_err(|error| anyhow!(error));
            let address = address.to_string();
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("disallow:{address}"));
                result
            })
        }

        fn status<'a>(&'a self) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + 'a>> {
            let result = self.status_result.clone().map_err(|error| anyhow!(error));
            Box::pin(async move {
                self.calls.lock().unwrap().push("status".to_string());
                result
            })
        }

        fn disconnect_active<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + 'a>> {
            let result = self
                .disconnect_result
                .clone()
                .map_err(|error| anyhow!(error));
            Box::pin(async move {
                self.calls.lock().unwrap().push("disconnect".to_string());
                result
            })
        }

        fn forget_device<'a>(
            &'a self,
            address: &'a str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + 'a>> {
            let result = self.forget_result.clone().map_err(|error| anyhow!(error));
            let address = address.to_string();
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("forget:{address}"));
                result
            })
        }
    }

    fn status_json(active_device: Option<&str>) -> String {
        serde_json::to_string(&RuntimeStatus {
            pairing: PairingWindow {
                enabled: false,
                timeout_secs: 60,
                expires_at_epoch_secs: None,
            },
            active_device: active_device.map(str::to_string),
            devices: vec![],
            audio: AudioDefaults::default(),
            backend: MediaBackendStatus::default(),
        })
        .unwrap()
    }

    #[tokio::test]
    async fn reset_with_drop_allowlist_propagates_disallow_errors() {
        let client = FakeDeviceResetClient {
            calls: Mutex::new(vec![]),
            disallow_result: Err("disallow failed"),
            status_result: Ok(status_json(Some("AA"))),
            disconnect_result: Ok(status_json(None)),
            forget_result: Ok(status_json(None)),
        };

        let error = reset_device(&client, "AA", true).await.unwrap_err();

        assert!(error.to_string().contains("disallow failed"));
        assert_eq!(client.calls.lock().unwrap().as_slice(), ["disallow:AA"]);
    }

    #[tokio::test]
    async fn reset_with_drop_allowlist_uses_disallow_status_and_forgets_device() {
        let client = FakeDeviceResetClient {
            calls: Mutex::new(vec![]),
            disallow_result: Ok(status_json(Some("AA"))),
            status_result: Ok(status_json(Some("AA"))),
            disconnect_result: Ok(status_json(None)),
            forget_result: Ok(status_json(None)),
        };

        let output = reset_device(&client, "AA", true).await.unwrap();
        let status: RuntimeStatus = serde_json::from_str(&output).unwrap();

        assert!(status.active_device.is_none());
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            ["disallow:AA", "disconnect", "forget:AA"]
        );
    }
}
