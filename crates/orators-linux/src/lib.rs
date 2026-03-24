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
    wireplumber::{WirePlumberRoles, WirePlumberRuntime, generic_restart_hint},
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
                classic_call_enabled: false,
                le_audio_enabled: false,
                autoswitch_to_headset_profile: None,
            });
        self.audio
            .current_defaults(
                roles.a2dp_sink_enabled,
                roles.classic_call_enabled,
                roles.le_audio_enabled,
            )
            .await
    }

    pub async fn apply_session_config(&self) -> Result<SessionConfigStatus> {
        let mut report = self
            .wireplumber
            .ensure_fragment(&self.fragment_path, &self.config)
            .await?;
        if report.changed {
            report.restart_required = true;
            report.restart_hint = Some(restart_hint_for_devices(
                self.bluez
                    .list_devices(self.config.auto_reconnect)
                    .await
                    .ok(),
            ));
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

fn restart_hint_for_devices(devices: Option<Vec<DeviceInfo>>) -> String {
    let Some(devices) = devices else {
        return format!(
            "{} BlueZ could not be queried, so verify Bluetooth is idle before restarting user services.",
            generic_restart_hint()
        );
    };

    let connected_devices = devices
        .into_iter()
        .filter(|device| device.connected)
        .map(|device| device.alias.unwrap_or(device.address))
        .collect::<Vec<_>>();

    if connected_devices.is_empty() {
        return generic_restart_hint();
    }

    format!(
        "WirePlumber was not restarted automatically because connected Bluetooth devices were detected: {}. Disconnect them first, then run `systemctl --user restart wireplumber.service oratorsd.service`.",
        connected_devices.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use orators_core::DeviceInfo;

    use super::restart_hint_for_devices;

    fn device(address: &str, alias: Option<&str>, connected: bool) -> DeviceInfo {
        DeviceInfo {
            address: address.to_string(),
            alias: alias.map(str::to_string),
            trusted: false,
            paired: false,
            connected,
            active_profile: None,
            auto_reconnect: false,
        }
    }

    #[test]
    fn restart_hint_warns_about_connected_devices() {
        let hint = restart_hint_for_devices(Some(vec![
            device("AA", Some("Phone"), true),
            device("BB", None, false),
        ]));

        assert!(hint.contains("Phone"));
        assert!(hint.contains("Disconnect them first"));
    }

    #[test]
    fn restart_hint_falls_back_when_no_devices_connected() {
        let hint = restart_hint_for_devices(Some(vec![device("AA", Some("Phone"), false)]));

        assert!(hint.contains("WirePlumber was not restarted automatically"));
        assert!(!hint.contains("Disconnect them first"));
    }
}
