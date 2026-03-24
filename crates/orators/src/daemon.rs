use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use clap::Parser;
use orators_core::{OratorsConfig, dbus};
use orators_linux::LinuxPlatform;
use zbus::{ConnectionBuilder, SignalContext, fdo};

use crate::service::OratorsService;

#[derive(Debug, Parser, Clone)]
pub struct DaemonArgs {
    #[arg(long)]
    pub config: Option<PathBuf>,
}

pub async fn run(args: DaemonArgs) -> Result<()> {
    let config_path = args.config.unwrap_or(default_config_path()?);
    let config = ensure_config_exists(&config_path)?;
    let paths = RuntimePaths::discover()?;
    tokio::fs::create_dir_all(&paths.state_dir).await?;

    let runtime = Arc::new(LinuxPlatform::new(config.clone()).await?);
    let service = Arc::new(OratorsService::new(runtime, config));

    let monitor_service = Arc::clone(&service);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Err(error) = monitor_service.expire_pairing_if_needed().await {
                tracing::warn!(?error, "failed to expire pairing window");
            }
            if let Err(error) = monitor_service.protect_active_audio_if_needed().await {
                tracing::warn!(?error, "failed to protect active bluetooth audio");
            }
        }
    });

    let _connection = ConnectionBuilder::session()?
        .name(dbus::BUS_NAME)?
        .serve_at(dbus::OBJECT_PATH, DbusApi { service })?
        .build()
        .await
        .context("failed to start D-Bus service")?;

    tracing::info!("oratorsd is running");
    tokio::signal::ctrl_c().await?;
    Ok(())
}

pub struct RuntimePaths {
    pub state_dir: PathBuf,
}

impl RuntimePaths {
    pub fn discover() -> Result<Self> {
        let state_dir = dirs::state_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/state")))
            .context("unable to determine state directory")?
            .join("orators");

        Ok(Self { state_dir })
    }
}

pub fn default_config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .context("unable to determine config directory")?;
    Ok(config_dir.join("orators/config.toml"))
}

pub fn ensure_config_exists(path: &Path) -> Result<OratorsConfig> {
    let config = OratorsConfig::load_or_default(path)?;
    if !path.exists() {
        config.save(path)?;
    }
    Ok(config)
}

struct DbusApi {
    service: Arc<OratorsService<LinuxPlatform>>,
}

#[zbus::interface(name = "dev.orators.Orators1.Control")]
impl DbusApi {
    #[zbus(name = "GetStatus")]
    async fn get_status(&self) -> fdo::Result<String> {
        self.service.status_json().await.map_err(to_fdo)
    }

    #[zbus(name = "StartPairing")]
    async fn start_pairing(
        &self,
        timeout_sec: u64,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<String> {
        let status = self
            .service
            .start_pairing(Some(timeout_sec))
            .await
            .map_err(to_fdo)?;
        let pairing = self.service.pairing_json().await.map_err(to_fdo)?;
        Self::pairing_window_changed(&ctxt, &pairing)
            .await
            .map_err(to_fdo_zbus)?;
        Self::status_changed(&ctxt, &status)
            .await
            .map_err(to_fdo_zbus)?;
        Ok(status)
    }

    #[zbus(name = "StopPairing")]
    async fn stop_pairing(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<String> {
        let status = self.service.stop_pairing().await.map_err(to_fdo)?;
        let pairing = self.service.pairing_json().await.map_err(to_fdo)?;
        Self::pairing_window_changed(&ctxt, &pairing)
            .await
            .map_err(to_fdo_zbus)?;
        Self::status_changed(&ctxt, &status)
            .await
            .map_err(to_fdo_zbus)?;
        Ok(status)
    }

    #[zbus(name = "ListDevices")]
    async fn list_devices(&self) -> fdo::Result<String> {
        self.service.list_devices_json().await.map_err(to_fdo)
    }

    #[zbus(name = "TrustDevice")]
    async fn trust_device(
        &self,
        address: &str,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<String> {
        let status = self.service.trust_device(address).await.map_err(to_fdo)?;
        Self::status_changed(&ctxt, &status)
            .await
            .map_err(to_fdo_zbus)?;
        Ok(status)
    }

    #[zbus(name = "ForgetDevice")]
    async fn forget_device(
        &self,
        address: &str,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<String> {
        let status = self.service.forget_device(address).await.map_err(to_fdo)?;
        let active = self.service.active_device().await.unwrap_or_default();
        Self::active_device_changed(&ctxt, &active)
            .await
            .map_err(to_fdo_zbus)?;
        Self::status_changed(&ctxt, &status)
            .await
            .map_err(to_fdo_zbus)?;
        Ok(status)
    }

    #[zbus(name = "ConnectDevice")]
    async fn connect_device(
        &self,
        address: &str,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<String> {
        let status = self.service.connect_device(address).await.map_err(to_fdo)?;
        Self::active_device_changed(&ctxt, address)
            .await
            .map_err(to_fdo_zbus)?;
        Self::status_changed(&ctxt, &status)
            .await
            .map_err(to_fdo_zbus)?;
        Ok(status)
    }

    #[zbus(name = "DisconnectActive")]
    async fn disconnect_active(
        &self,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<String> {
        let status = self.service.disconnect_active().await.map_err(to_fdo)?;
        Self::active_device_changed(&ctxt, "")
            .await
            .map_err(to_fdo_zbus)?;
        Self::status_changed(&ctxt, &status)
            .await
            .map_err(to_fdo_zbus)?;
        Ok(status)
    }

    #[zbus(name = "GetDiagnostics")]
    async fn get_diagnostics(&self) -> fdo::Result<String> {
        self.service.diagnostics_json().await.map_err(to_fdo)
    }

    #[zbus(signal, name = "StatusChanged")]
    async fn status_changed(ctxt: &SignalContext<'_>, status_json: &str) -> zbus::Result<()>;

    #[zbus(signal, name = "PairingWindowChanged")]
    async fn pairing_window_changed(
        ctxt: &SignalContext<'_>,
        pairing_json: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "ActiveDeviceChanged")]
    async fn active_device_changed(ctxt: &SignalContext<'_>, address: &str) -> zbus::Result<()>;

    #[zbus(signal, name = "DiagnosticsChanged")]
    async fn diagnostics_changed(
        ctxt: &SignalContext<'_>,
        diagnostics_json: &str,
    ) -> zbus::Result<()>;
}

fn to_fdo(error: anyhow::Error) -> fdo::Error {
    fdo::Error::Failed(error.to_string())
}

fn to_fdo_zbus(error: zbus::Error) -> fdo::Error {
    fdo::Error::Failed(error.to_string())
}
