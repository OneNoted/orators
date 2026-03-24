pub mod audio;
pub mod bluez;
pub mod diagnostics;
pub mod systemd;

use std::path::{Path, PathBuf};

use anyhow::Result;
use orators_core::{AudioDefaults, DeviceInfo, DiagnosticsReport, OratorsConfig};

use crate::{
    audio::WpctlAudioRuntime, bluez::BluetoothCtlBluez, diagnostics::collect_report,
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
        Ok(Self {
            bluez: BluetoothCtlBluez::new().await?,
            audio: WpctlAudioRuntime,
            systemd: SystemdUserRuntime,
            config,
        })
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        self.bluez.list_devices(self.config.auto_reconnect).await
    }

    pub async fn start_pairing(&self, timeout_secs: u64) -> Result<()> {
        self.ensure_host_media_ready().await?;
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
        self.bluez.connect_device(address).await?;
        self.audio.pin_device_to_a2dp(address).await
    }

    pub async fn disconnect_device(&self, address: &str) -> Result<()> {
        self.bluez.disconnect_device(address).await
    }

    pub async fn current_audio_defaults(&self) -> Result<AudioDefaults> {
        let adapter = self.bluez.adapter_info().await.ok();
        let active_device = self
            .bluez
            .list_devices(self.config.auto_reconnect)
            .await
            .ok()
            .and_then(|devices| devices.into_iter().find(|device| device.connected))
            .map(|device| device.address);

        self.audio
            .current_defaults(
                adapter.as_ref().is_some_and(adapter_supports_media),
                adapter.as_ref().is_some_and(adapter_exposes_call_roles),
                active_device.as_deref(),
            )
            .await
    }

    pub async fn ensure_host_media_ready(&self) -> Result<()> {
        let adapter = self.bluez.adapter_info().await?;
        if !adapter_supports_media(&adapter) {
            anyhow::bail!(
                "the stock Bluetooth stack does not advertise Audio Sink / A2DP media support"
            );
        }

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

        self.audio.disable_headset_autoswitch().await?;

        Ok(())
    }

    pub async fn guard_active_audio(&self, active_device: Option<&str>) -> Result<()> {
        if let Some(address) = active_device {
            let wireplumber = self.systemd.service_status("wireplumber.service").await?;
            if wireplumber.active_state != "active" || wireplumber.sub_state != "running" {
                anyhow::bail!(
                    "wireplumber.service became unhealthy while Bluetooth audio was active"
                );
            }

            self.audio.guard_active_device_audio(address).await?;
        }

        Ok(())
    }

    pub async fn diagnostics(&self) -> Result<DiagnosticsReport> {
        collect_report(&self.bluez, &self.audio, &self.systemd).await
    }

    pub async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf> {
        self.systemd.install_user_service(daemon_path).await
    }
}

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn adapter_supports_media(adapter: &crate::bluez::AdapterInfo) -> bool {
    adapter.uuids.iter().any(|uuid| {
        uuid.eq_ignore_ascii_case("0000110b-0000-1000-8000-00805f9b34fb")
            || uuid.eq_ignore_ascii_case("0000110d-0000-1000-8000-00805f9b34fb")
    })
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
