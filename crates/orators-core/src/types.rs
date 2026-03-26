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
