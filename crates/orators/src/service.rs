use std::{path::Path, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use async_trait::async_trait;
use orators_core::{
    AudioDefaults, BluetoothProfile, DeviceInfo, DiagnosticsReport, OratorsConfig, OratorsState,
    SessionConfigStatus,
};
use tokio::sync::Mutex;

#[async_trait]
pub trait PlatformRuntime: Send + Sync {
    async fn list_devices(&self) -> Result<Vec<DeviceInfo>>;
    async fn start_pairing(&self, timeout_secs: u64) -> Result<()>;
    async fn stop_pairing(&self) -> Result<()>;
    async fn trust_device(&self, address: &str) -> Result<()>;
    async fn forget_device(&self, address: &str) -> Result<()>;
    async fn connect_device(&self, address: &str) -> Result<()>;
    async fn disconnect_device(&self, address: &str) -> Result<()>;
    async fn current_audio_defaults(&self) -> Result<AudioDefaults>;
    async fn apply_session_config(&self) -> Result<SessionConfigStatus>;
    async fn diagnostics(&self) -> Result<DiagnosticsReport>;
    async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf>;
}

#[async_trait]
impl PlatformRuntime for orators_linux::LinuxPlatform {
    async fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        self.list_devices().await
    }

    async fn start_pairing(&self, timeout_secs: u64) -> Result<()> {
        self.start_pairing(timeout_secs).await
    }

    async fn stop_pairing(&self) -> Result<()> {
        self.stop_pairing().await
    }

    async fn trust_device(&self, address: &str) -> Result<()> {
        self.trust_device(address).await
    }

    async fn forget_device(&self, address: &str) -> Result<()> {
        self.forget_device(address).await
    }

    async fn connect_device(&self, address: &str) -> Result<()> {
        self.connect_device(address).await
    }

    async fn disconnect_device(&self, address: &str) -> Result<()> {
        self.disconnect_device(address).await
    }

    async fn current_audio_defaults(&self) -> Result<AudioDefaults> {
        self.current_audio_defaults().await
    }

    async fn apply_session_config(&self) -> Result<SessionConfigStatus> {
        self.apply_session_config().await
    }

    async fn diagnostics(&self) -> Result<DiagnosticsReport> {
        self.diagnostics().await
    }

    async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf> {
        self.install_user_service(daemon_path).await
    }
}

pub struct OratorsService<R> {
    config: OratorsConfig,
    runtime: Arc<R>,
    state: Mutex<OratorsState>,
}

impl<R: PlatformRuntime> OratorsService<R> {
    pub fn new(runtime: Arc<R>, config: OratorsConfig) -> Self {
        Self {
            config: config.clone(),
            runtime,
            state: Mutex::new(OratorsState::new(config)),
        }
    }

    pub async fn status_json(&self) -> Result<String> {
        let status = self.refresh_status().await?;
        serialize(&status)
    }

    pub async fn pairing_json(&self) -> Result<String> {
        let now = now_epoch_secs();
        let state = self.state.lock().await;
        let pairing = state.pairing_window(now);
        serialize(&pairing)
    }

    pub async fn active_device(&self) -> Option<String> {
        let status = self.refresh_status().await.ok()?;
        status.active_device
    }

    pub async fn list_devices_json(&self) -> Result<String> {
        let status = self.refresh_status().await?;
        serialize(&status.devices)
    }

    pub async fn start_pairing(&self, timeout_secs: Option<u64>) -> Result<String> {
        let timeout_secs = timeout_secs.unwrap_or_else(|| self.default_timeout());
        self.runtime.start_pairing(timeout_secs).await?;

        let now = now_epoch_secs();
        {
            let mut state = self.state.lock().await;
            state.start_pairing(now, Some(timeout_secs));
        }
        self.status_json().await
    }

    pub async fn stop_pairing(&self) -> Result<String> {
        self.runtime.stop_pairing().await?;
        {
            let mut state = self.state.lock().await;
            state.stop_pairing();
        }
        self.status_json().await
    }

    pub async fn expire_pairing_if_needed(&self) -> Result<Option<String>> {
        let now = now_epoch_secs();
        let expired = {
            let mut state = self.state.lock().await;
            state.tick(now)
        };

        if expired {
            tracing::info!("pairing window expired; keeping existing device connections intact");
            self.runtime.stop_pairing().await?;
            return Ok(Some(self.status_json().await?));
        }

        Ok(None)
    }

    pub async fn trust_device(&self, address: &str) -> Result<String> {
        self.refresh_status().await?;
        self.runtime.trust_device(address).await?;

        let now = now_epoch_secs();
        let mut state = self.state.lock().await;
        state.trust_device(address)?;
        serialize(&state.status(now))
    }

    pub async fn forget_device(&self, address: &str) -> Result<String> {
        self.refresh_status().await?;
        self.runtime.forget_device(address).await?;

        let now = now_epoch_secs();
        let mut state = self.state.lock().await;
        state.forget_device(address)?;
        serialize(&state.status(now))
    }

    pub async fn connect_device(&self, address: &str) -> Result<String> {
        self.refresh_status().await?;
        self.runtime.connect_device(address).await?;

        let now = now_epoch_secs();
        let mut state = self.state.lock().await;
        state.connect_device(address, BluetoothProfile::Media)?;
        serialize(&state.status(now))
    }

    pub async fn disconnect_active(&self) -> Result<String> {
        let active = self.active_device().await;
        if let Some(active) = active.as_deref() {
            self.runtime.disconnect_device(active).await?;
            let mut state = self.state.lock().await;
            state.disconnect_active();
        }

        self.status_json().await
    }

    pub async fn apply_session_config(&self) -> Result<String> {
        let report = self.runtime.apply_session_config().await?;
        serialize(&report)
    }

    pub async fn diagnostics_json(&self) -> Result<String> {
        let report = self.runtime.diagnostics().await?;
        serialize(&report)
    }

    pub async fn install_user_service(&self, daemon_path: &Path) -> Result<String> {
        let unit_path = self.runtime.install_user_service(daemon_path).await?;
        Ok(unit_path.display().to_string())
    }

    async fn refresh_status(&self) -> Result<orators_core::RuntimeStatus> {
        let devices = self.runtime.list_devices().await?;
        let audio = self.runtime.current_audio_defaults().await?;
        let now = now_epoch_secs();
        let mut state = self.state.lock().await;
        state.sync_devices(devices);
        state.update_audio(audio);
        Ok(state.status(now))
    }

    fn default_timeout(&self) -> u64 {
        self.config.pairing_timeout_secs
    }
}

fn serialize<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string_pretty(value).context("failed to serialize response")
}

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::{path::Path, path::PathBuf, sync::Arc};

    use anyhow::Result;
    use async_trait::async_trait;
    use orators_core::{
        AudioDefaults, DeviceInfo, DiagnosticCheck, DiagnosticsReport, OratorsConfig,
        SessionConfigStatus, Severity,
    };

    use super::{OratorsService, PlatformRuntime};

    struct MockRuntime {
        devices: tokio::sync::Mutex<Vec<DeviceInfo>>,
        disconnects: tokio::sync::Mutex<Vec<String>>,
        stop_pairing_calls: tokio::sync::Mutex<usize>,
    }

    impl MockRuntime {
        fn new(devices: Vec<DeviceInfo>) -> Self {
            Self {
                devices: tokio::sync::Mutex::new(devices),
                disconnects: tokio::sync::Mutex::new(Vec::new()),
                stop_pairing_calls: tokio::sync::Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl PlatformRuntime for MockRuntime {
        async fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
            Ok(self.devices.lock().await.clone())
        }

        async fn start_pairing(&self, _timeout_secs: u64) -> Result<()> {
            Ok(())
        }

        async fn stop_pairing(&self) -> Result<()> {
            *self.stop_pairing_calls.lock().await += 1;
            Ok(())
        }

        async fn trust_device(&self, address: &str) -> Result<()> {
            if let Some(device) = self
                .devices
                .lock()
                .await
                .iter_mut()
                .find(|device| device.address == address)
            {
                device.trusted = true;
            }
            Ok(())
        }

        async fn forget_device(&self, address: &str) -> Result<()> {
            self.devices
                .lock()
                .await
                .retain(|device| device.address != address);
            Ok(())
        }

        async fn connect_device(&self, address: &str) -> Result<()> {
            if let Some(device) = self
                .devices
                .lock()
                .await
                .iter_mut()
                .find(|device| device.address == address)
            {
                device.connected = true;
            }
            Ok(())
        }

        async fn disconnect_device(&self, address: &str) -> Result<()> {
            self.disconnects.lock().await.push(address.to_string());
            Ok(())
        }

        async fn current_audio_defaults(&self) -> Result<AudioDefaults> {
            Ok(AudioDefaults {
                output_device: Some("Speakers".to_string()),
                input_device: Some("Microphone".to_string()),
                a2dp_sink_enabled: true,
                hfp_hf_enabled: false,
            })
        }

        async fn apply_session_config(&self) -> Result<SessionConfigStatus> {
            Ok(SessionConfigStatus {
                path: "/tmp/test.conf".to_string(),
                changed: false,
            })
        }

        async fn diagnostics(&self) -> Result<DiagnosticsReport> {
            Ok(DiagnosticsReport {
                generated_at_epoch_secs: 1,
                checks: vec![DiagnosticCheck {
                    code: "bluez.adapter".to_string(),
                    severity: Severity::Info,
                    summary: "BlueZ adapter is ready for pairing".to_string(),
                    detail: Some(
                        "Look for Bluetooth device 'aeolus' (04:7F:0E:02:13:3C). powered=yes, discoverable=yes, pairable=yes, scanning=no.".to_string(),
                    ),
                    remediation: None,
                }],
            })
        }

        async fn install_user_service(&self, _daemon_path: &Path) -> Result<PathBuf> {
            Ok(PathBuf::from("/tmp/oratorsd.service"))
        }
    }

    fn sample_device(address: &str) -> DeviceInfo {
        DeviceInfo {
            address: address.to_string(),
            alias: Some("Phone".to_string()),
            trusted: false,
            paired: true,
            connected: false,
            active_profile: None,
            auto_reconnect: false,
        }
    }

    #[tokio::test]
    async fn start_pairing_updates_status() {
        let runtime = Arc::new(MockRuntime::new(vec![sample_device("AA")]));
        let service = OratorsService::new(runtime, OratorsConfig::default());

        let status = service.start_pairing(Some(30)).await.unwrap();

        assert!(status.contains("\"enabled\": true"));
        assert!(status.contains("Speakers"));
    }

    #[tokio::test]
    async fn disconnect_active_uses_runtime() {
        let runtime = Arc::new(MockRuntime::new(vec![sample_device("AA")]));
        let service = OratorsService::new(runtime.clone(), OratorsConfig::default());
        service.connect_device("AA").await.unwrap();

        service.disconnect_active().await.unwrap();

        let disconnects = runtime.disconnects.lock().await.clone();
        assert_eq!(disconnects, vec!["AA".to_string()]);
    }

    #[tokio::test]
    async fn expiring_pairing_keeps_active_device_connected() {
        let runtime = Arc::new(MockRuntime::new(vec![sample_device("AA")]));
        let service = OratorsService::new(runtime.clone(), OratorsConfig::default());

        service.connect_device("AA").await.unwrap();
        service.start_pairing(Some(1)).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let expired = service.expire_pairing_if_needed().await.unwrap().unwrap();
        let expired: orators_core::RuntimeStatus = serde_json::from_str(&expired).unwrap();

        assert_eq!(expired.active_device.as_deref(), Some("AA"));
        assert_eq!(*runtime.stop_pairing_calls.lock().await, 1);
    }
}
