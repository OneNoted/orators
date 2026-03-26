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
        let service_status = self.system_backend_status().await?;
        let service_ready = service_status
            .as_ref()
            .is_some_and(|status| status.active_state == "active" && status.sub_state == "running");
        let resolved_adapter = self.bluez.resolved_adapter().await.ok();
        let mut status = self.bluealsa.backend_status(installed, service_ready).await;
        status.configured_adapter = self.config.adapter.clone();
        if let Some(resolved) = resolved_adapter {
            status.adapter_mode = resolved.mode;
            status.resolved_adapter = Some(resolved.info.id);
        }
        Ok(status)
    }

    pub async fn ensure_host_media_ready(&self) -> Result<()> {
        self.ensure_backend_ready().await
    }

    pub async fn guard_active_audio(&self, _active_device: Option<&str>) -> Result<()> {
        self.reconcile_runtime().await
    }

    pub async fn reconcile_runtime(&self) -> Result<()> {
        let installed = self.systemd.wireplumber_fragment_installed()?;
        let Some(service_status) = self.system_backend_status().await? else {
            self.bluealsa.stop_player().await?;
            return Ok(());
        };

        if service_status.active_state != "active" || service_status.sub_state != "running" {
            self.bluealsa.stop_player().await?;
            return Ok(());
        }

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
            self.system_backend_status().await?,
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
        let adapter = self.resolve_install_adapter(adapter).await?;
        self.systemd
            .install_system_backend(
                &assets,
                adapter.system_backend_adapter(),
                adapter.resolved_adapter(),
            )
            .await
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
        self.bluez.resolved_adapter().await?;

        if !self.systemd.wireplumber_fragment_installed()? {
            anyhow::bail!("the managed WirePlumber Bluetooth-disable fragment is not installed");
        }

        BluealsaAssets::discover().context("BlueALSA binaries are not available")?;

        let backend_status = self
            .system_backend_status()
            .await?
            .context("the Orators BlueALSA system service is not installed")?;
        if backend_status.active_state != "active" || backend_status.sub_state != "running" {
            anyhow::bail!(
                "the Orators BlueALSA system service is not healthy: ActiveState={}, SubState={}",
                backend_status.active_state,
                backend_status.sub_state
            );
        }

        let defaults = self.audio.current_defaults().await?;
        if !defaults.local_output_available {
            anyhow::bail!("the host does not currently expose a usable local playback output");
        }

        Ok(())
    }

    async fn system_backend_status(&self) -> Result<Option<ServiceStatus>> {
        match self
            .systemd
            .system_service_status(SYSTEM_BACKEND_UNIT)
            .await
        {
            Ok(status) => Ok(Some(status)),
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
            .all_remote_devices()
            .await?
            .into_iter()
            .any(|device| device.connected && remote_device_supports_media(&device)))
    }

    async fn resolve_install_adapter(&self, requested: Option<&str>) -> Result<InstallAdapter> {
        let powered = self.bluez.powered_adapter_ids().await?;
        match powered.as_slice() {
            [adapter] => {
                if let Some(requested) = requested {
                    let requested = requested.to_ascii_lowercase();
                    if requested != *adapter {
                        anyhow::bail!(
                            "requested Bluetooth adapter {requested} is not available; the only powered adapter is {adapter}"
                        );
                    }
                }
                Ok(InstallAdapter::Auto {
                    resolved_adapter: adapter.clone(),
                })
            }
            [] => anyhow::bail!("no powered BlueZ adapter is available"),
            _ => {
                let available = powered.join(", ");
                let adapter = requested
                    .or(self.config.adapter.as_deref())
                    .map(|adapter| adapter.to_ascii_lowercase())
                    .context(
                        "multiple powered Bluetooth adapters were found; rerun install with --adapter hciX",
                    )?;

                if powered.iter().any(|candidate| candidate == &adapter) {
                    Ok(InstallAdapter::Explicit { adapter })
                } else {
                    anyhow::bail!(
                        "configured Bluetooth adapter {adapter} is not available; powered adapters: {available}"
                    )
                }
            }
        }
    }
}

enum InstallAdapter {
    Auto { resolved_adapter: String },
    Explicit { adapter: String },
}

impl InstallAdapter {
    fn system_backend_adapter(&self) -> Option<&str> {
        match self {
            Self::Auto { .. } => None,
            Self::Explicit { adapter } => Some(adapter.as_str()),
        }
    }

    fn resolved_adapter(&self) -> &str {
        match self {
            Self::Auto { resolved_adapter } => resolved_adapter.as_str(),
            Self::Explicit { adapter } => adapter.as_str(),
        }
    }
}

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
