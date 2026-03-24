use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    error::{OratorsError, Result},
    types::SessionConfigStatus,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OratorsConfig {
    pub pairing_timeout_secs: u64,
    pub auto_reconnect: bool,
    pub single_active_device: bool,
    pub wireplumber_fragment_name: String,
}

impl Default for OratorsConfig {
    fn default() -> Self {
        Self {
            pairing_timeout_secs: 120,
            auto_reconnect: true,
            single_active_device: true,
            wireplumber_fragment_name: "90-orators-bluetooth.conf".to_string(),
        }
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
