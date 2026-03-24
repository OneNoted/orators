use std::{env, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand};
use orators_core::{
    BluetoothProfile, DiagnosticCheck, DiagnosticsReport, RuntimeStatus, Severity, dbus,
};
use orators_linux::{systemd::SystemdUserRuntime, wireplumber::WirePlumberRuntime};
use serde_json::Value;
use zbus::{Connection, Proxy};

use crate::daemon::{RuntimePaths, default_config_path, ensure_config_exists};

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
    Connect { mac: String },
    Disconnect,
    Doctor(DoctorArgs),
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
    Trust { mac: String },
    Forget { mac: String },
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[arg(long)]
    pub apply: bool,
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::InstallUserService => install_user_service(cli.json).await,
        Command::Doctor(args) => run_doctor(args, cli.json).await,
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
            DeviceCommand::Trust { mac } => {
                let output = client.trust_device(&mac).await?;
                render_status_output(output, json, "Device trusted.")?;
            }
            DeviceCommand::Forget { mac } => {
                let output = client.forget_device(&mac).await?;
                render_status_output(output, json, "Device removed.")?;
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
        Command::Doctor(_) | Command::InstallUserService => unreachable!(),
    }
    Ok(())
}

async fn run_doctor(args: DoctorArgs, json: bool) -> Result<()> {
    let client = ControllerClient::connect().await?;
    if args.apply {
        let applied = client.apply_session_config().await?;
        if json {
            print_jsonish(&applied)?;
        } else {
            let report: orators_core::SessionConfigStatus = serde_json::from_str(&applied)?;
            println!(
                "WirePlumber config {} at {}.",
                if report.changed {
                    "updated"
                } else {
                    "already matched"
                },
                report.path
            );
        }
    }

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
    let config = ensure_config_exists(&config_path)?;
    let paths = RuntimePaths::discover(config_path.clone(), &config)?;
    let daemon_path = resolve_daemon_path()?;
    let wireplumber = WirePlumberRuntime;
    let systemd = SystemdUserRuntime;

    let config_report = wireplumber
        .ensure_fragment(&paths.fragment_path, &config)
        .await?;
    let unit_path = systemd.install_user_service(&daemon_path).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&config_report)?);
        println!("{}", unit_path.display());
    } else {
        println!(
            "WirePlumber config {} at {}.",
            if config_report.changed {
                "updated"
            } else {
                "already matched"
            },
            config_report.path
        );
        println!("Installed user service at {}.", unit_path.display());
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

    render_audio_summary(status);
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
    render_audio_summary(status);
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

fn render_audio_summary(status: &RuntimeStatus) {
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
        "Bluetooth roles: a2dp_sink={}, hfp_hf={}",
        yes_no(status.audio.a2dp_sink_enabled),
        yes_no(status.audio.hfp_hf_enabled),
    );
    println!(
        "Call audio: {}",
        if status.audio.hfp_hf_enabled {
            "available by default; media stays on A2DP until a voice app uses the microphone"
        } else {
            "disabled; Discord and other VoIP apps will not see a Bluetooth microphone path"
        }
    );
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
    report
        .checks
        .iter()
        .find(|check| check.code == "bluez.adapter")
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

struct ControllerClient {
    connection: Connection,
}

impl ControllerClient {
    async fn connect() -> Result<Self> {
        Ok(Self {
            connection: Connection::session()
                .await
                .context("failed to connect to session bus")?,
        })
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

    async fn apply_session_config(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("ApplySessionConfig", &())
            .await
            .map_err(|error| explain_dbus_error("apply session config", error))
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

fn explain_dbus_error(action: &str, error: zbus::Error) -> anyhow::Error {
    let detail = error.to_string();
    let next_step = if detail.contains("pairing window is closed") {
        "Run `oratorsctl pair start --timeout 120` before pairing a new device."
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
