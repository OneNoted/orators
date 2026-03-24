pub mod audio;
pub mod bluez;
pub mod diagnostics;
pub mod systemd;
pub mod wireplumber;

use std::path::{Path, PathBuf};

use anyhow::Result;
use orators_core::{
    AudioDefaults, DeviceInfo, DiagnosticsReport, OratorsConfig, SessionConfigStatus,
};

use crate::{
    audio::WpctlAudioRuntime,
    bluez::BluetoothCtlBluez,
    diagnostics::collect_report,
    systemd::SystemdUserRuntime,
    wireplumber::{WirePlumberRoles, WirePlumberRuntime},
};

pub struct LinuxPlatform {
    bluez: BluetoothCtlBluez,
    audio: WpctlAudioRuntime,
    wireplumber: WirePlumberRuntime,
    systemd: SystemdUserRuntime,
    fragment_path: PathBuf,
    config: OratorsConfig,
}

impl LinuxPlatform {
    pub async fn new(fragment_path: PathBuf, config: OratorsConfig) -> Result<Self> {
        Ok(Self {
            bluez: BluetoothCtlBluez::new().await?,
            audio: WpctlAudioRuntime,
            wireplumber: WirePlumberRuntime,
            systemd: SystemdUserRuntime,
            fragment_path,
            config,
        })
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        self.bluez.list_devices(self.config.auto_reconnect).await
    }

    pub async fn start_pairing(&self, timeout_secs: u64) -> Result<()> {
        self.bluez.start_pairing(timeout_secs).await
    }

    pub async fn stop_pairing(&self) -> Result<()> {
        self.bluez.stop_pairing().await
    }

    pub async fn trust_device(&self, address: &str) -> Result<()> {
        self.bluez.trust_device(address).await
    }

    pub async fn forget_device(&self, address: &str) -> Result<()> {
        self.bluez.forget_device(address).await
    }

    pub async fn connect_device(&self, address: &str) -> Result<()> {
        self.bluez.connect_device(address).await
    }

    pub async fn disconnect_device(&self, address: &str) -> Result<()> {
        self.bluez.disconnect_device(address).await
    }

    pub async fn current_audio_defaults(&self) -> Result<AudioDefaults> {
        let roles = self
            .wireplumber
            .roles(&self.fragment_path)
            .await
            .unwrap_or(WirePlumberRoles {
                a2dp_sink_enabled: false,
                hfp_hf_enabled: false,
            });
        self.audio
            .current_defaults(roles.a2dp_sink_enabled, roles.hfp_hf_enabled)
            .await
    }

    pub async fn apply_session_config(&self) -> Result<SessionConfigStatus> {
        let report = self
            .wireplumber
            .ensure_fragment(&self.fragment_path, &self.config)
            .await?;
        if report.changed {
            let _ = self.systemd.try_restart("wireplumber.service").await;
        }
        Ok(report)
    }

    pub async fn diagnostics(&self) -> Result<DiagnosticsReport> {
        collect_report(
            &self.bluez,
            &self.audio,
            &self.wireplumber,
            &self.fragment_path,
            &self.config,
        )
        .await
    }

    pub async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf> {
        self.systemd.install_user_service(daemon_path).await
    }

    pub fn fragment_path(&self) -> &Path {
        &self.fragment_path
    }
}

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
