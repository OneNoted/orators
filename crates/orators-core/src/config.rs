use std::{fs, path::Path};

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{OratorsError, Result};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OratorsConfig {
    pub pairing_timeout_secs: u64,
    pub auto_reconnect: bool,
    pub single_active_device: bool,
}

impl Default for OratorsConfig {
    fn default() -> Self {
        Self {
            pairing_timeout_secs: 120,
            auto_reconnect: true,
            single_active_device: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawOratorsConfig {
    pairing_timeout_secs: u64,
    auto_reconnect: bool,
    single_active_device: bool,
    bluetooth_mode: Option<String>,
    call_audio_enabled: Option<bool>,
    wireplumber_fragment_name: Option<String>,
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
            wireplumber_fragment_name: None,
        }
    }
}

impl<'de> Deserialize<'de> for OratorsConfig {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawOratorsConfig::deserialize(deserializer)?;
        let _legacy_mode = raw.bluetooth_mode;
        let _legacy_call_audio = raw.call_audio_enabled;
        let _legacy_fragment_name = raw.wireplumber_fragment_name;

        Ok(Self {
            pairing_timeout_secs: raw.pairing_timeout_secs,
            auto_reconnect: raw.auto_reconnect,
            single_active_device: raw.single_active_device,
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

    pub fn save(&self, path: &Path) -> Result<()> {
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

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::OratorsConfig;

    #[test]
    fn missing_legacy_fields_use_defaults() {
        let parsed: OratorsConfig = toml::from_str(
            r#"
pairing_timeout_secs = 45
auto_reconnect = true
single_active_device = true
"#,
        )
        .unwrap();

        assert_eq!(parsed.pairing_timeout_secs, 45);
    }

    #[test]
    fn legacy_bluetooth_fields_are_ignored() {
        let parsed: OratorsConfig = toml::from_str(
            r#"
call_audio_enabled = true
bluetooth_mode = "le_audio_call"
wireplumber_fragment_name = "90-orators-bluetooth.conf"
"#,
        )
        .unwrap();

        assert_eq!(parsed, OratorsConfig::default());
    }

    #[test]
    fn save_writes_only_media_safe_fields() {
        let serialized = toml::to_string_pretty(&OratorsConfig::default()).unwrap();

        assert!(serialized.contains("pairing_timeout_secs = 120"));
        assert!(!serialized.contains("call_audio_enabled"));
        assert!(!serialized.contains("bluetooth_mode"));
    }
}
