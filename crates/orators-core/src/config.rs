use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{OratorsError, Result};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OratorsConfig {
    pub pairing_timeout_secs: u64,
    pub auto_reconnect: bool,
    pub single_active_device: bool,
    pub adapter: Option<String>,
    pub allowed_devices: Vec<String>,
    pub device_aliases: BTreeMap<String, String>,
}

impl Default for OratorsConfig {
    fn default() -> Self {
        Self {
            pairing_timeout_secs: 120,
            auto_reconnect: true,
            single_active_device: true,
            adapter: None,
            allowed_devices: Vec::new(),
            device_aliases: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
struct RawOratorsConfig {
    pairing_timeout_secs: u64,
    auto_reconnect: bool,
    single_active_device: bool,
    adapter: Option<String>,
    allowed_devices: Vec<String>,
    device_aliases: BTreeMap<String, String>,
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
            adapter: defaults.adapter,
            allowed_devices: defaults.allowed_devices,
            device_aliases: defaults.device_aliases,
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
            adapter: normalize_adapter(raw.adapter),
            allowed_devices: normalize_allowed_devices(raw.allowed_devices),
            device_aliases: normalize_device_aliases(raw.device_aliases),
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

    pub fn allows_device(&self, address: &str) -> bool {
        let normalized = normalize_device_address(address);
        self.allowed_devices.iter().any(|item| item == &normalized)
    }

    pub fn allow_device(&mut self, address: &str) -> bool {
        let normalized = normalize_device_address(address);
        if self.allowed_devices.iter().any(|item| item == &normalized) {
            return false;
        }
        self.allowed_devices.push(normalized);
        self.allowed_devices.sort();
        true
    }

    pub fn disallow_device(&mut self, address: &str) -> bool {
        let normalized = normalize_device_address(address);
        let original_len = self.allowed_devices.len();
        self.allowed_devices.retain(|item| item != &normalized);
        original_len != self.allowed_devices.len()
    }

    pub fn device_alias(&self, address: &str) -> Option<&str> {
        let normalized = normalize_device_address(address);
        self.device_aliases.get(&normalized).map(String::as_str)
    }

    pub fn set_device_alias(&mut self, address: &str, alias: &str) -> bool {
        let normalized = normalize_device_address(address);
        let alias = alias.trim();
        if alias.is_empty() {
            return false;
        }

        let changed = self
            .device_aliases
            .get(&normalized)
            .is_none_or(|existing| existing != alias);
        self.device_aliases.insert(normalized, alias.to_string());
        changed
    }

    pub fn clear_device_alias(&mut self, address: &str) -> bool {
        let normalized = normalize_device_address(address);
        self.device_aliases.remove(&normalized).is_some()
    }
}

pub fn normalize_device_address(address: &str) -> String {
    address.trim().replace('-', ":").to_ascii_uppercase()
}

fn normalize_allowed_devices(devices: Vec<String>) -> Vec<String> {
    let mut normalized = devices
        .into_iter()
        .map(|device| normalize_device_address(&device))
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalize_adapter(adapter: Option<String>) -> Option<String> {
    adapter
        .map(|adapter| adapter.trim().to_ascii_lowercase())
        .filter(|adapter| !adapter.is_empty())
}

fn normalize_device_aliases(aliases: BTreeMap<String, String>) -> BTreeMap<String, String> {
    aliases
        .into_iter()
        .filter_map(|(address, alias)| {
            let alias = alias.trim().to_string();
            if alias.is_empty() {
                None
            } else {
                Some((normalize_device_address(&address), alias))
            }
        })
        .collect()
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
adapter = "HCI1"
"#,
        )
        .unwrap();

        assert_eq!(parsed.pairing_timeout_secs, 45);
        assert_eq!(parsed.adapter.as_deref(), Some("hci1"));
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
    fn allowed_devices_are_normalized() {
        let parsed: OratorsConfig = toml::from_str(
            r#"
allowed_devices = ["5c-dc-49-92-d0-d8", "5C:DC:49:92:D0:D8"]
"#,
        )
        .unwrap();

        assert_eq!(
            parsed.allowed_devices,
            vec!["5C:DC:49:92:D0:D8".to_string()]
        );
        assert!(parsed.allows_device("5c:dc:49:92:d0:d8"));
    }

    #[test]
    fn save_writes_only_media_safe_fields() {
        let serialized = toml::to_string_pretty(&OratorsConfig {
            adapter: Some("hci1".to_string()),
            ..OratorsConfig::default()
        })
        .unwrap();

        assert!(serialized.contains("pairing_timeout_secs = 120"));
        assert!(serialized.contains("adapter ="));
        assert!(serialized.contains("allowed_devices = []"));
        assert!(!serialized.contains("call_audio_enabled"));
        assert!(!serialized.contains("bluetooth_mode"));
    }

    #[test]
    fn device_aliases_are_normalized() {
        let parsed: OratorsConfig = toml::from_str(
            r#"
[device_aliases]
"5c-dc-49-92-d0-d8" = "Fold"
"AA:BB:CC:DD:EE:FF" = "   "
"#,
        )
        .unwrap();

        assert_eq!(parsed.device_alias("5c:dc:49:92:d0:d8"), Some("Fold"));
        assert_eq!(parsed.device_aliases.len(), 1);
    }
}
