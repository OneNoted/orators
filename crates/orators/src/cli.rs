use std::{env, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use orators_core::dbus;
use orators_linux::{systemd::SystemdUserRuntime, wireplumber::WirePlumberRuntime};
use serde_json::Value;
use zbus::{Connection, Proxy};

use crate::daemon::{RuntimePaths, default_config_path, ensure_config_exists};

#[derive(Debug, Parser)]
#[command(name = "oratorsctl", about = "Control the Orators daemon")]
pub struct Cli {
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
        Command::InstallUserService => install_user_service().await,
        Command::Doctor(args) => run_doctor(args).await,
        command => run_daemon_command(command).await,
    }
}

async fn run_daemon_command(command: Command) -> Result<()> {
    let client = ControllerClient::connect().await?;
    let output = match command {
        Command::Status => client.status().await?,
        Command::Pair(args) => match args.command {
            PairCommand::Start { timeout } => client.start_pairing(timeout.unwrap_or(120)).await?,
            PairCommand::Stop => client.stop_pairing().await?,
        },
        Command::Devices(args) => match args.command {
            DeviceCommand::List => client.list_devices().await?,
            DeviceCommand::Trust { mac } => client.trust_device(&mac).await?,
            DeviceCommand::Forget { mac } => client.forget_device(&mac).await?,
        },
        Command::Connect { mac } => client.connect_device(&mac).await?,
        Command::Disconnect => client.disconnect_active().await?,
        Command::Doctor(_) | Command::InstallUserService => unreachable!(),
    };

    print_jsonish(&output)?;
    Ok(())
}

async fn run_doctor(args: DoctorArgs) -> Result<()> {
    let client = ControllerClient::connect().await?;
    if args.apply {
        let applied = client.apply_session_config().await?;
        print_jsonish(&applied)?;
    }

    let diagnostics = client.get_diagnostics().await?;
    print_jsonish(&diagnostics)?;
    Ok(())
}

async fn install_user_service() -> Result<()> {
    let config_path = default_config_path()?;
    let config = ensure_config_exists(&config_path)?;
    let paths = RuntimePaths::discover(config_path.clone(), &config)?;
    let daemon_path = resolve_daemon_path()?;
    let wireplumber = WirePlumberRuntime;
    let systemd = SystemdUserRuntime;

    let config_report = wireplumber.ensure_fragment(&paths.fragment_path).await?;
    let unit_path = systemd.install_user_service(&daemon_path).await?;

    println!("{}", serde_json::to_string_pretty(&config_report)?);
    println!("{}", unit_path.display());
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
            .context("GetStatus failed")
    }

    async fn start_pairing(&self, timeout: u64) -> Result<String> {
        self.proxy()
            .await?
            .call("StartPairing", &(timeout,))
            .await
            .context("StartPairing failed")
    }

    async fn stop_pairing(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("StopPairing", &())
            .await
            .context("StopPairing failed")
    }

    async fn list_devices(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("ListDevices", &())
            .await
            .context("ListDevices failed")
    }

    async fn trust_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("TrustDevice", &(address,))
            .await
            .context("TrustDevice failed")
    }

    async fn forget_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("ForgetDevice", &(address,))
            .await
            .context("ForgetDevice failed")
    }

    async fn connect_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("ConnectDevice", &(address,))
            .await
            .context("ConnectDevice failed")
    }

    async fn disconnect_active(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("DisconnectActive", &())
            .await
            .context("DisconnectActive failed")
    }

    async fn apply_session_config(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("ApplySessionConfig", &())
            .await
            .context("ApplySessionConfig failed")
    }

    async fn get_diagnostics(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("GetDiagnostics", &())
            .await
            .context("GetDiagnostics failed")
    }

    async fn proxy(&self) -> Result<Proxy<'_>> {
        Proxy::new(
            &self.connection,
            dbus::BUS_NAME,
            dbus::OBJECT_PATH,
            dbus::CONTROL_INTERFACE,
        )
        .await
        .context("failed to create Orators proxy")
    }
}
