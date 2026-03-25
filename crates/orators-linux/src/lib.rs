pub mod audio;
pub mod bluealsa;
pub mod bluez;
pub mod diagnostics;
pub mod systemd;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use orators_core::{
    AudioDefaults, DeviceInfo, DiagnosticsReport, MediaBackendStatus, OratorsConfig,
};

use crate::{
    audio::LocalAudioRuntime,
    bluealsa::{BluealsaAssets, BluealsaRuntime, SYSTEM_BACKEND_UNIT},
    bluez::{BluetoothCtlBluez, remote_device_supports_media},
    diagnostics::collect_report,
    systemd::{ServiceStatus, SystemBackendInstallResult, SystemdUserRuntime},
};

pub struct LinuxPlatform {
    bluez: BluetoothCtlBluez,
    audio: LocalAudioRuntime,
    bluealsa: BluealsaRuntime,
    systemd: SystemdUserRuntime,
    config: OratorsConfig,
}

impl LinuxPlatform {
    pub async fn new(config: OratorsConfig) -> Result<Self> {
        let systemd = SystemdUserRuntime;
        if let Err(error) = systemd.cleanup_legacy_wireplumber_dropin().await {
            tracing::warn!(?error, "failed to clean up legacy WirePlumber drop-in");
        }
        if let Err(error) = systemd.remove_legacy_fragment().await {
            tracing::warn!(?error, "failed to clean up legacy WirePlumber fragment");
        }

        Ok(Self {
            bluez: BluetoothCtlBluez::new(config.adapter.clone()).await?,
            audio: LocalAudioRuntime,
            bluealsa: BluealsaRuntime::new(),
            systemd,
            config,
        })
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        self.bluez.list_devices(self.config.auto_reconnect).await
    }

    pub async fn start_pairing(&self, timeout_secs: u64) -> Result<()> {
        self.ensure_backend_ready().await?;
        self.bluez.start_pairing(timeout_secs).await
    }

    pub async fn stop_pairing(&self) -> Result<()> {
        self.bluez.stop_pairing().await
    }

    pub async fn trust_device(&self, address: &str) -> Result<()> {
        self.bluez.trust_device(address).await
    }

    pub async fn untrust_device(&self, address: &str) -> Result<()> {
        self.bluez.untrust_device(address).await
    }

    pub async fn forget_device(&self, address: &str) -> Result<()> {
        self.bluez.forget_device(address).await
    }

    pub async fn connect_device(&self, address: &str) -> Result<()> {
        self.ensure_backend_ready().await?;
        self.ensure_audio_capable_device(address).await?;
        self.bluez
            .connect_device(address)
            .await
            .with_context(|| format!("failed to initiate Bluetooth connection for {address}"))?;

        self.bluez
            .wait_for_connected(address, std::time::Duration::from_secs(10))
            .await?
            .then_some(())
            .context("the phone did not reach a connected Bluetooth state within 10 seconds")
    }

    pub async fn disconnect_device(&self, address: &str) -> Result<()> {
        self.bluez.disconnect_device(address).await?;
        self.reconcile_runtime().await
    }

    pub async fn current_audio_defaults(&self) -> Result<AudioDefaults> {
        self.audio.current_defaults().await
    }

    pub async fn backend_status(&self) -> Result<MediaBackendStatus> {
        let installed = self.systemd.wireplumber_fragment_installed()?;
        let service_ready = self.system_backend_ready().await?.is_some();
        Ok(self.bluealsa.backend_status(installed, service_ready).await)
    }

    pub async fn ensure_host_media_ready(&self) -> Result<()> {
        self.ensure_backend_ready().await
    }

    pub async fn guard_active_audio(&self, _active_device: Option<&str>) -> Result<()> {
        self.reconcile_runtime().await
    }

    pub async fn reconcile_runtime(&self) -> Result<()> {
        let installed = self.systemd.wireplumber_fragment_installed()?;
        let Some(_service_status) = self.system_backend_ready().await? else {
            self.bluealsa.stop_player().await?;
            return Ok(());
        };

        if !installed {
            self.bluealsa.stop_player().await?;
            return Ok(());
        }

        let active_device = self
            .bluez
            .remote_devices()
            .await?
            .into_iter()
            .find(|device| device.connected && remote_device_supports_media(device))
            .map(|device| device.address);

        let assets = BluealsaAssets::discover()?;
        self.bluealsa
            .reconcile_player(&assets, active_device.as_deref())
            .await
    }

    pub async fn diagnostics(&self) -> Result<DiagnosticsReport> {
        collect_report(
            &self.bluez,
            &self.audio,
            &self.systemd,
            self.config.adapter.as_deref(),
            self.system_backend_ready().await?,
        )
        .await
    }

    pub async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf> {
        self.systemd.install_user_service(daemon_path).await
    }

    pub async fn install_system_backend(
        &self,
        adapter: Option<&str>,
    ) -> Result<SystemBackendInstallResult> {
        if self.any_connected_audio_devices().await? {
            anyhow::bail!(
                "disconnect connected Bluetooth audio devices before installing the Orators system backend"
            );
        }

        let assets = BluealsaAssets::discover()?;
        let adapter = self.resolve_adapter(adapter).await?;
        self.systemd.install_system_backend(&assets, &adapter).await
    }

    pub async fn uninstall_system_backend(&self) -> Result<()> {
        if self.any_connected_audio_devices().await? {
            anyhow::bail!(
                "disconnect connected Bluetooth audio devices before uninstalling the Orators system backend"
            );
        }

        self.systemd.uninstall_system_backend().await
    }

    async fn ensure_backend_ready(&self) -> Result<()> {
        self.bluez
            .adapter_info()
            .await
            .context("no BlueZ adapter is available for pairing")?;

        if !self.systemd.wireplumber_fragment_installed()? {
            anyhow::bail!("the managed WirePlumber Bluetooth-disable fragment is not installed");
        }

        BluealsaAssets::discover().context("BlueALSA binaries are not available")?;

        self.system_backend_ready()
            .await?
            .context("the Orators BlueALSA system service is not healthy")?;

        let defaults = self.audio.current_defaults().await?;
        if !defaults.local_output_available {
            anyhow::bail!("the host does not currently expose a usable local playback output");
        }

        Ok(())
    }

    async fn system_backend_ready(&self) -> Result<Option<ServiceStatus>> {
        match self
            .systemd
            .system_service_status(SYSTEM_BACKEND_UNIT)
            .await
        {
            Ok(status) if status.active_state == "active" && status.sub_state == "running" => {
                Ok(Some(status))
            }
            Ok(_) => Ok(None),
            Err(error) if error.to_string().contains("could not be found") => Ok(None),
            Err(error) if error.to_string().contains("not-found") => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn ensure_audio_capable_device(&self, address: &str) -> Result<()> {
        let device = self
            .bluez
            .remote_devices()
            .await?
            .into_iter()
            .find(|device| device.address == address)
            .with_context(|| format!("no BlueZ device found for {address}"))?;

        if remote_device_supports_media(&device) {
            Ok(())
        } else {
            anyhow::bail!("{address} does not advertise a classic A2DP audio-source profile");
        }
    }

    async fn any_connected_audio_devices(&self) -> Result<bool> {
        Ok(self
            .bluez
            .remote_devices()
            .await?
            .into_iter()
            .any(|device| device.connected && remote_device_supports_media(&device)))
    }

    async fn resolve_adapter(&self, requested: Option<&str>) -> Result<String> {
        if let Some(adapter) = requested.or(self.config.adapter.as_deref()) {
            return Ok(adapter.to_ascii_lowercase());
        }

        let powered = self.bluez.powered_adapter_ids().await?;
        match powered.as_slice() {
            [adapter] => Ok(adapter.clone()),
            [] => anyhow::bail!("no powered BlueZ adapter is available"),
            _ => anyhow::bail!(
                "multiple powered Bluetooth adapters were found; rerun install with --adapter hciX"
            ),
        }
    }
}

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
