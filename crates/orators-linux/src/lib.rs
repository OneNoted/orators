pub mod audio;
pub mod bluez;
pub mod diagnostics;
pub mod systemd;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use orators_core::{AudioDefaults, DeviceInfo, DiagnosticsReport, OratorsConfig};

use crate::{
    audio::WpctlAudioRuntime,
    bluez::{BluetoothCtlBluez, remote_device_supports_media},
    diagnostics::collect_report,
    systemd::SystemdUserRuntime,
};

pub struct LinuxPlatform {
    bluez: BluetoothCtlBluez,
    audio: WpctlAudioRuntime,
    systemd: SystemdUserRuntime,
    config: OratorsConfig,
}

impl LinuxPlatform {
    pub async fn new(config: OratorsConfig) -> Result<Self> {
        let systemd = SystemdUserRuntime;
        if let Err(error) = systemd.cleanup_legacy_wireplumber_dropin().await {
            tracing::warn!(
                ?error,
                "failed to clean up legacy WirePlumber Bluetooth ownership state"
            );
        }
        Ok(Self {
            bluez: BluetoothCtlBluez::new().await?,
            audio: WpctlAudioRuntime,
            systemd,
            config,
        })
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        self.bluez.list_devices(self.config.auto_reconnect).await
    }

    pub async fn start_pairing(&self, timeout_secs: u64) -> Result<()> {
        self.ensure_pairing_ready().await?;
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
        self.ensure_host_media_ready().await?;
        self.bluez.connect_device(address).await
            .with_context(|| format!("failed to initiate Bluetooth connection for {address}"))?;

        if self
            .audio
            .wait_for_bluetooth_audio_card(address, std::time::Duration::from_secs(8))
            .await?
            .is_none()
        {
            let _ = self.bluez.disconnect_device(address).await;
            anyhow::bail!(
                "the phone connected over Bluetooth, but the host never surfaced a matching Bluetooth audio card"
            );
        }

        Ok(())
    }

    pub async fn disconnect_device(&self, address: &str) -> Result<()> {
        self.bluez.disconnect_device(address).await
    }

    pub async fn current_audio_defaults(&self) -> Result<AudioDefaults> {
        let adapter = self.bluez.adapter_info().await.ok();
        let active_device = self
            .bluez
            .remote_devices()
            .await
            .ok()
            .and_then(|devices| {
                devices
                    .into_iter()
                    .find(|device| device.connected && remote_device_supports_media(device))
            })
            .map(|device| device.address);

        let defaults = self
            .audio
            .current_defaults(
                adapter.is_some(),
                adapter.as_ref().is_some_and(adapter_exposes_call_roles),
                active_device.as_deref(),
            )
            .await?;
        Ok(defaults)
    }

    pub async fn ensure_host_media_ready(&self) -> Result<()> {
        self.ensure_pairing_ready().await?;

        let wireplumber = self.systemd.service_status("wireplumber.service").await?;
        if wireplumber.active_state != "active" || wireplumber.sub_state != "running" {
            anyhow::bail!(
                "wireplumber.service is not healthy: ActiveState={}, SubState={}",
                wireplumber.active_state,
                wireplumber.sub_state
            );
        }

        let defaults = self.audio.pipewire_defaults().await?;
        if defaults.output_device.is_none() || defaults.output_is_dummy {
            anyhow::bail!("PipeWire does not currently have a usable default sink");
        }

        self.audio.apply_media_stability_settings().await?;

        Ok(())
    }

    pub async fn guard_active_audio(&self, active_device: Option<&str>) -> Result<()> {
        if active_device.is_some() {
            let wireplumber = self.systemd.service_status("wireplumber.service").await?;
            if wireplumber.active_state != "active" || wireplumber.sub_state != "running" {
                anyhow::bail!(
                    "wireplumber.service became unhealthy while Bluetooth audio was active"
                );
            }
            let defaults = self.audio.pipewire_defaults().await?;
            if defaults.output_device.is_none() || defaults.output_is_dummy {
                anyhow::bail!(
                    "PipeWire default sink is unavailable while Bluetooth audio is active"
                );
            }
        }

        Ok(())
    }

    pub async fn diagnostics(&self) -> Result<DiagnosticsReport> {
        collect_report(&self.bluez, &self.audio, &self.systemd).await
    }

    pub async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf> {
        self.systemd.install_user_service(daemon_path).await
    }

    async fn ensure_pairing_ready(&self) -> Result<()> {
        self.bluez
            .adapter_info()
            .await
            .context("no BlueZ adapter is available for pairing")?;
        Ok(())
    }
}

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn adapter_exposes_call_roles(adapter: &crate::bluez::AdapterInfo) -> bool {
    adapter.uuids.iter().any(|uuid| {
        matches!(
            uuid.to_ascii_lowercase().as_str(),
            "00001108-0000-1000-8000-00805f9b34fb"
                | "00001112-0000-1000-8000-00805f9b34fb"
                | "0000111e-0000-1000-8000-00805f9b34fb"
                | "0000111f-0000-1000-8000-00805f9b34fb"
        )
    })
}
