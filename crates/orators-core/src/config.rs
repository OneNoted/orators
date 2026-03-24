use std::{fs, path::Path};

use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    error::{OratorsError, Result},
    types::SessionConfigStatus,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothMode {
    #[default]
    ClassicMedia,
    #[serde(alias = "classic_call")]
    ClassicCallCompat,
    #[serde(alias = "experimental_le_audio")]
    LeAudioCall,
}

impl BluetoothMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ClassicMedia => "classic_media",
            Self::ClassicCallCompat => "classic_call_compat",
            Self::LeAudioCall => "le_audio_call",
        }
    }

    pub fn classic_call_compat_enabled(self) -> bool {
        matches!(self, Self::ClassicCallCompat)
    }

    pub fn le_audio_call_enabled(self) -> bool {
        matches!(self, Self::LeAudioCall)
    }

    pub fn headset_autoswitch_enabled(self) -> bool {
        matches!(self, Self::ClassicCallCompat)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OratorsConfig {
    pub pairing_timeout_secs: u64,
    pub auto_reconnect: bool,
    pub single_active_device: bool,
    pub bluetooth_mode: BluetoothMode,
    pub wireplumber_fragment_name: String,
}

impl Default for OratorsConfig {
    fn default() -> Self {
        Self {
            pairing_timeout_secs: 120,
            auto_reconnect: true,
            single_active_device: true,
            bluetooth_mode: BluetoothMode::ClassicMedia,
            wireplumber_fragment_name: "90-orators-bluetooth.conf".to_string(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawOratorsConfig {
    pairing_timeout_secs: u64,
    auto_reconnect: bool,
    single_active_device: bool,
    bluetooth_mode: Option<BluetoothMode>,
    call_audio_enabled: Option<bool>,
    wireplumber_fragment_name: String,
}

impl Default for RawOratorsConfig {
    fn default() -> Self {
        let defaults = OratorsConfig::default();
        Self {
            pairing_timeout_secs: defaults.pairing_timeout_secs,
            auto_reconnect: defaults.auto_reconnect,
            single_active_device: defaults.single_active_device,
            bluetooth_mode: None,
            call_audio_enabled: None,
            wireplumber_fragment_name: defaults.wireplumber_fragment_name,
        }
    }
}

impl<'de> Deserialize<'de> for OratorsConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawOratorsConfig::deserialize(deserializer)?;
        let bluetooth_mode = raw.bluetooth_mode.unwrap_or(match raw.call_audio_enabled {
            Some(true) => BluetoothMode::ClassicCallCompat,
            Some(false) | None => BluetoothMode::ClassicMedia,
        });

        Ok(Self {
            pairing_timeout_secs: raw.pairing_timeout_secs,
            auto_reconnect: raw.auto_reconnect,
            single_active_device: raw.single_active_device,
            bluetooth_mode,
            wireplumber_fragment_name: raw.wireplumber_fragment_name,
        })
    }
}

impl OratorsConfig {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = fs::read_to_string(path).map_err(|source| OratorsError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(toml::from_str(&contents)?)
    }

    pub fn save(&self, path: &Path) -> Result<SessionConfigStatus> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| OratorsError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let serialized = toml::to_string_pretty(self)?;
        fs::write(path, serialized).map_err(|source| OratorsError::Io {
            path: path.to_path_buf(),
            source,
        })?;

        Ok(SessionConfigStatus {
            path: path.display().to_string(),
            changed: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{BluetoothMode, OratorsConfig};

    #[test]
    fn missing_new_fields_use_defaults() {
        let parsed: OratorsConfig = toml::from_str(
            r#"
pairing_timeout_secs = 45
auto_reconnect = true
single_active_device = true
wireplumber_fragment_name = "90-orators-bluetooth.conf"
"#,
        )
        .unwrap();

        assert_eq!(parsed.bluetooth_mode, BluetoothMode::ClassicMedia);
        assert_eq!(parsed.pairing_timeout_secs, 45);
    }

    #[test]
    fn legacy_call_audio_enabled_maps_to_classic_call() {
        let parsed: OratorsConfig = toml::from_str(
            r#"
call_audio_enabled = true
"#,
        )
        .unwrap();

        assert_eq!(parsed.bluetooth_mode, BluetoothMode::ClassicCallCompat);
    }

    #[test]
    fn explicit_bluetooth_mode_wins_over_legacy_flag() {
        let parsed: OratorsConfig = toml::from_str(
            r#"
bluetooth_mode = "le_audio_call"
call_audio_enabled = false
"#,
        )
        .unwrap();

        assert_eq!(parsed.bluetooth_mode, BluetoothMode::LeAudioCall);
    }

    #[test]
    fn legacy_experimental_le_audio_value_maps_to_new_mode() {
        let parsed: OratorsConfig = toml::from_str(
            r#"
bluetooth_mode = "experimental_le_audio"
"#,
        )
        .unwrap();

        assert_eq!(parsed.bluetooth_mode, BluetoothMode::LeAudioCall);
    }

    #[test]
    fn save_writes_bluetooth_mode_not_legacy_flag() {
        let serialized = toml::to_string_pretty(&OratorsConfig::default()).unwrap();

        assert!(serialized.contains("bluetooth_mode = \"classic_media\""));
        assert!(!serialized.contains("call_audio_enabled"));
    }
}
