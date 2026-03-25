use std::{env, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use orators_core::{
    BluetoothProfile, DiagnosticCheck, DiagnosticsReport, OratorsConfig, PlayerState,
    RuntimeStatus, Severity, dbus, normalize_device_address,
};
use orators_linux::LinuxPlatform;
use orators_linux::systemd::SystemdUserRuntime;
use serde_json::Value;
use zbus::{Connection, Proxy, fdo::DBusProxy, names::BusName};

use crate::daemon::{default_config_path, ensure_config_exists};

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
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::InstallUserService => install_user_service(cli.json).await,
        Command::InstallSystemBackend { adapter } => {
            install_system_backend(cli.json, adapter).await
        }
        Command::UninstallSystemBackend => uninstall_system_backend(cli.json).await,
        Command::Doctor => run_doctor(cli.json).await,
        command => run_daemon_command(command, cli.json).await,
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
                update_allowlist(&mac, true)?;
                let output = client.trust_device(&mac).await?;
                render_status_output(output, json, "Device added to allowlist.")?;
            }
            DeviceCommand::Disallow { mac } => {
                let mac = normalize_device_address(&mac);
                update_allowlist(&mac, false)?;
                let output = client.untrust_device(&mac).await?;
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
                if drop_allowlist {
                    update_allowlist(&mac, false)?;
                }
                let status = client.status().await?;
                let status: RuntimeStatus = serde_json::from_str(&status)?;
                if status.active_device.as_deref() == Some(mac.as_str()) {
                    let _ = client.disconnect_active().await;
                }
                let output = client.forget_device(&mac).await?;
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

async fn install_user_service(json: bool) -> Result<()> {
    let config_path = default_config_path()?;
    ensure_config_exists(&config_path)?;
    let daemon_path = resolve_daemon_path()?;
    let systemd = SystemdUserRuntime;
    let unit_path = systemd.install_user_service(&daemon_path).await?;

    if json {
        println!("{}", unit_path.display());
    } else {
        println!("Installed user service at {}.", unit_path.display());
        println!("This only installs the daemon unit.");
    }
    Ok(())
}

async fn install_system_backend(json: bool, adapter: Option<String>) -> Result<()> {
    let config_path = default_config_path()?;
    let mut config = ensure_config_exists(&config_path)?;
    if let Some(adapter) = adapter.clone() {
        config.adapter = Some(adapter.to_ascii_lowercase());
        config.save(&config_path)?;
    }

    let daemon_path = resolve_daemon_path()?;
    let runtime = LinuxPlatform::new(config.clone()).await?;
    let user_unit_path = runtime.install_user_service(&daemon_path).await?;
    let install = runtime.install_system_backend(adapter.as_deref()).await?;

    config.adapter = Some(install.adapter.clone());
    config.save(&config_path)?;

    if json {
        let payload = serde_json::json!({
            "user_service_path": user_unit_path,
            "wireplumber_fragment_path": install.wireplumber_fragment_path,
            "system_unit_path": install.system_unit_path,
            "adapter": install.adapter,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("Installed user service at {}.", user_unit_path.display());
        println!(
            "Installed WirePlumber Bluetooth-disable fragment at {}.",
            install.wireplumber_fragment_path.display()
        );
        println!(
            "Installed BlueALSA system backend at {}.",
            install.system_unit_path.display()
        );
        println!("Selected Bluetooth adapter: {}.", install.adapter);
        println!("Restart WirePlumber and the Orators daemon to activate the backend.");
    }

    Ok(())
}

async fn uninstall_system_backend(json: bool) -> Result<()> {
    let config_path = default_config_path()?;
    let config = ensure_config_exists(&config_path)?;
    let runtime = LinuxPlatform::new(config).await?;
    runtime.uninstall_system_backend().await?;

    if json {
        println!("{{\"removed\":true}}");
    } else {
        println!("Removed the Orators system backend.");
        println!("WirePlumber Bluetooth ownership was restored.");
    }

    Ok(())
}

fn resolve_daemon_path() -> Result<PathBuf> {
    let current_exe = env::current_exe().context("failed to resolve current executable path")?;
    let daemon_path = current_exe
        .parent()
        .context("current executable has no parent directory")?
        .join("oratorsd");
    Ok(daemon_path)
}

fn update_allowlist(address: &str, allow: bool) -> Result<()> {
    let config_path = default_config_path()?;
    let mut config = OratorsConfig::load_or_default(&config_path)?;
    if allow {
        config.allow_device(address);
    } else {
        config.disallow_device(address);
    }
    config.save(&config_path)?;
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
        "ALSA default output: {}",
        yes_no(status.audio.alsa_default_output_available)
    );
    println!(
        "Backend: {}",
        match status.backend.backend {
            orators_core::MediaBackendKind::Bluealsa => "bluealsa",
        }
    );
    println!("Backend installed: {}", yes_no(status.backend.installed),);
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

struct ControllerClient {
    connection: Connection,
}

impl ControllerClient {
    async fn connect() -> Result<Self> {
        let connection = Connection::session()
            .await
            .context("failed to connect to session bus")?;
        ensure_daemon_running(&connection).await?;
        Ok(Self { connection })
    }

    async fn status(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("GetStatus", &())
            .await
            .map_err(|error| explain_dbus_error("get status", error))
    }

    async fn start_pairing(&self, timeout: u64) -> Result<String> {
        self.proxy()
            .await?
            .call("StartPairing", &(timeout,))
            .await
            .map_err(|error| explain_dbus_error("start pairing", error))
    }

    async fn stop_pairing(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("StopPairing", &())
            .await
            .map_err(|error| explain_dbus_error("stop pairing", error))
    }

    async fn list_devices(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("ListDevices", &())
            .await
            .map_err(|error| explain_dbus_error("list devices", error))
    }

    async fn trust_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("TrustDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("trust device", error))
    }

    async fn untrust_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("UntrustDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("untrust device", error))
    }

    async fn forget_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("ForgetDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("forget device", error))
    }

    async fn connect_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("ConnectDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("connect device", error))
    }

    async fn disconnect_active(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("DisconnectActive", &())
            .await
            .map_err(|error| explain_dbus_error("disconnect active device", error))
    }

    async fn get_diagnostics(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("GetDiagnostics", &())
            .await
            .map_err(|error| explain_dbus_error("get diagnostics", error))
    }

    async fn proxy(&self) -> Result<Proxy<'_>> {
        Proxy::new(
            &self.connection,
            dbus::BUS_NAME,
            dbus::OBJECT_PATH,
            dbus::CONTROL_INTERFACE,
        )
        .await
        .context("failed to connect to Orators D-Bus interface")
    }
}

async fn ensure_daemon_running(connection: &Connection) -> Result<()> {
    let bus_name = BusName::try_from(dbus::BUS_NAME).context("invalid Orators D-Bus bus name")?;
    let bus = DBusProxy::new(connection)
        .await
        .context("failed to connect to session D-Bus daemon")?;
    if bus
        .name_has_owner(bus_name.clone())
        .await
        .context("failed to query Orators D-Bus ownership")?
    {
        return Ok(());
    }

    SystemdUserRuntime
        .start_orators_service()
        .await
        .context("failed to start oratorsd.service; run `oratorsctl install-user-service` first")?;

    for _ in 0..12 {
        if bus
            .name_has_owner(bus_name.clone())
            .await
            .context("failed to re-check Orators D-Bus ownership")?
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    anyhow::bail!("oratorsd.service started, but the D-Bus control interface never appeared")
}

fn explain_dbus_error(action: &str, error: zbus::Error) -> anyhow::Error {
    let detail = error.to_string();
    let next_step = if detail.contains("pairing window is closed") {
        "Run `oratorsctl pair start --timeout 120` before pairing a new device."
    } else if detail.contains("BlueALSA")
        || detail.contains("bluealsad")
        || detail.contains("WirePlumber Bluetooth-disable fragment")
    {
        "Run `oratorsctl doctor`, then install or repair the managed backend with `oratorsctl install-system-backend`."
    } else if detail.contains("ALSA does not currently expose") || detail.contains("`aplay`") {
        "Run `oratorsctl doctor` and repair the host ALSA/PipeWire playback output before retrying."
    } else if detail.contains("classic A2DP audio-source profile") {
        "This device does not advertise classic Bluetooth media-source support, so it is not a supported speaker source."
    } else if detail.contains("RequestDefaultAgent") {
        "Close other Bluetooth pairing dialogs, then rerun `oratorsctl pair start`."
    } else if detail.contains("no BlueZ adapter found") {
        "Check that Bluetooth is present, powered on, and visible in `bluetoothctl show`."
    } else if detail.contains("org.bluez.Error") {
        "Run `oratorsctl doctor` and review the BlueZ service logs for the exact failure."
    } else {
        "Run `oratorsctl doctor` for more detail."
    };

    anyhow!("{action} failed: {detail}\nNext step: {next_step}")
}
