use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothProfile {
    Media,
    Call,
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
    pub a2dp_sink_enabled: bool,
    pub hfp_hf_enabled: bool,
    pub le_audio_call_enabled: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionConfigStatus {
    pub path: String,
    pub changed: bool,
}
