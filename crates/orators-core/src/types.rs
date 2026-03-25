use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaCodec {
    Sbc,
    Aac,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothProfile {
    Media,
    Call,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaBackendStatus {
    pub endpoints_registered: bool,
    pub active_codec: Option<MediaCodec>,
    pub transport_acquired: bool,
    pub playback_connected: bool,
    pub last_error: Option<String>,
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
    pub media_backend: MediaBackendStatus,
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
}
