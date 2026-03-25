pub mod audio;
pub mod bluez;
pub mod diagnostics;
pub mod owned_backend;
pub mod systemd;

use std::path::{Path, PathBuf};

use anyhow::Result;
use orators_core::{AudioDefaults, BluetoothProfile, DeviceInfo, DiagnosticsReport, OratorsConfig};

use crate::{
    audio::WpctlAudioRuntime, bluez::BluetoothCtlBluez, diagnostics::collect_report,
    owned_backend::OwnedBluetoothMediaBackend, systemd::SystemdUserRuntime,
    systemd::service_uses_audio_profile,
};

pub struct LinuxPlatform {
    bluez: BluetoothCtlBluez,
    audio: WpctlAudioRuntime,
    owned_backend: OwnedBluetoothMediaBackend,
    systemd: SystemdUserRuntime,
    config: OratorsConfig,
}

impl LinuxPlatform {
    pub async fn new(config: OratorsConfig) -> Result<Self> {
        let systemd = SystemdUserRuntime;
        Ok(Self {
            bluez: BluetoothCtlBluez::new().await?,
            audio: WpctlAudioRuntime,
            owned_backend: OwnedBluetoothMediaBackend::new().await?,
            systemd,
            config,
        })
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let mut devices = self.bluez.list_devices(self.config.auto_reconnect).await?;
        let backend = self.owned_backend.snapshot_status().await;
        if backend.playback_connected {
            for device in devices.iter_mut().filter(|device| device.connected) {
                device.active_profile = Some(BluetoothProfile::Media);
            }
        }
        Ok(devices)
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
        self.bluez.connect_device(address).await
    }

    pub async fn disconnect_device(&self, address: &str) -> Result<()> {
        self.bluez.disconnect_device(address).await
    }

    pub async fn current_audio_defaults(&self) -> Result<AudioDefaults> {
        let backend = self.owned_backend.snapshot_status().await;
        let defaults = self.audio.current_defaults().await?;
        Ok(AudioDefaults {
            output_device: defaults.output_device,
            input_device: defaults.input_device,
            media_backend: backend,
        })
    }

    pub async fn ensure_host_media_ready(&self) -> Result<()> {
        let adapter = self.bluez.adapter_info().await?;
        if !adapter_supports_media(&adapter) {
            anyhow::bail!(
                "the Bluetooth controller does not advertise Audio Sink / A2DP media support"
            );
        }

        if !self.systemd.host_backend_installed().await? {
            anyhow::bail!(
                "Orators' managed host backend is not installed; run `oratorsctl install-host-backend` first"
            );
        }

        let wireplumber = self.systemd.service_status("wireplumber.service").await?;
        if wireplumber.active_state != "active"
            || wireplumber.sub_state != "running"
            || !service_uses_audio_profile(&wireplumber)
        {
            anyhow::bail!(
                "wireplumber.service is not running the managed audio profile: ActiveState={}, SubState={}, ExecStart={}",
                wireplumber.active_state,
                wireplumber.sub_state,
                wireplumber.exec_start.as_deref().unwrap_or("not detected")
            );
        }

        let defaults = self.audio.pipewire_defaults().await?;
        if defaults.output_device.is_none() || defaults.output_is_dummy {
            anyhow::bail!("PipeWire does not currently have a usable default sink");
        }

        let backend = self.owned_backend.snapshot_status().await;
        if !backend.endpoints_registered {
            anyhow::bail!("Orators' BlueZ media endpoints are not registered");
        }

        Ok(())
    }

    pub async fn guard_active_audio(&self, active_device: Option<&str>) -> Result<()> {
        if active_device.is_some() {
            let backend = self.owned_backend.snapshot_status().await;
            if !backend.endpoints_registered {
                anyhow::bail!("Orators' BlueZ media endpoints are not registered");
            }
            if backend.last_error.is_some() {
                anyhow::bail!(
                    "Orators' owned Bluetooth backend reported an error while audio was active"
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
        collect_report(&self.bluez, &self.audio, &self.systemd, &self.owned_backend).await
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
