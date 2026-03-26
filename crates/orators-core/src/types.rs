use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothProfile {
    Media,
    Call,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MediaBackendKind {
    #[default]
    Bluealsa,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AdapterMode {
    #[default]
    Auto,
    Explicit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlayerState {
    #[default]
    Waiting,
    Starting,
    Playing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaBackendStatus {
    pub backend: MediaBackendKind,
    pub installed: bool,
    pub system_service_ready: bool,
    #[serde(default)]
    pub adapter_mode: AdapterMode,
    pub resolved_adapter: Option<String>,
    pub configured_adapter: Option<String>,
    pub player_state: PlayerState,
    pub player_running: bool,
    pub active_device_address: Option<String>,
    pub last_error: Option<String>,
}

impl Default for MediaBackendStatus {
    fn default() -> Self {
        Self {
            backend: MediaBackendKind::Bluealsa,
            installed: false,
            system_service_ready: false,
            adapter_mode: AdapterMode::Auto,
            resolved_adapter: None,
            configured_adapter: None,
            player_state: PlayerState::Waiting,
            player_running: false,
            active_device_address: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceInfo {
    pub address: String,
    pub alias: Option<String>,
    pub trusted: bool,
    pub paired: bool,
    pub connected: bool,
    pub active_profile: Option<BluetoothProfile>,
    pub auto_reconnect: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioDefaults {
    pub output_device: Option<String>,
    pub input_device: Option<String>,
    pub local_output_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingWindow {
    pub enabled: bool,
    pub timeout_secs: u64,
    pub expires_at_epoch_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeStatus {
    pub pairing: PairingWindow,
    pub active_device: Option<String>,
    pub devices: Vec<DeviceInfo>,
    pub audio: AudioDefaults,
    pub backend: MediaBackendStatus,
}

#[cfg(test)]
mod tests {
    use super::{AdapterMode, RuntimeStatus};

    #[test]
    fn runtime_status_accepts_missing_backend_adapter_mode() {
        let payload = r#"{
            "pairing": {
                "enabled": false,
                "timeout_secs": 60,
                "expires_at_epoch_secs": null
            },
            "active_device": null,
            "devices": [],
            "audio": {
                "output_device": null,
                "input_device": null,
                "local_output_available": false
            },
            "backend": {
                "backend": "bluealsa",
                "installed": true,
                "system_service_ready": true,
                "resolved_adapter": "hci0",
                "configured_adapter": null,
                "player_state": "waiting",
                "player_running": false,
                "active_device_address": null,
                "last_error": null
            }
        }"#;

        let status: RuntimeStatus = serde_json::from_str(payload).unwrap();

        assert_eq!(status.backend.adapter_mode, AdapterMode::Auto);
        assert_eq!(status.backend.resolved_adapter.as_deref(), Some("hci0"));
    }
}
