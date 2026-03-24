use std::path::Path;

use anyhow::{Context, Result};
use orators_core::{BluetoothMode, OratorsConfig, SessionConfigStatus};
use tokio::fs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WirePlumberRoles {
    pub a2dp_sink_enabled: bool,
    pub classic_call_enabled: bool,
    pub le_audio_enabled: bool,
    pub autoswitch_to_headset_profile: Option<bool>,
}

pub struct WirePlumberRuntime;

impl WirePlumberRuntime {
    pub async fn ensure_fragment(
        &self,
        path: &Path,
        config: &OratorsConfig,
    ) -> Result<SessionConfigStatus> {
        let desired = render_fragment(config);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }

        let changed = match fs::read_to_string(path).await {
            Ok(current) => current != desired,
            Err(_) => true,
        };

        if changed {
            fs::write(path, desired).await?;
        }

        Ok(SessionConfigStatus {
            path: path.display().to_string(),
            changed,
        })
    }

    pub async fn inspect_fragment(
        &self,
        path: &Path,
        config: &OratorsConfig,
    ) -> Result<SessionConfigStatus> {
        let current = fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(SessionConfigStatus {
            path: path.display().to_string(),
            changed: current != render_fragment(config),
        })
    }

    pub async fn roles(&self, path: &Path) -> Result<WirePlumberRoles> {
        let current = fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(parse_roles(&current))
    }
}

pub fn render_fragment(config: &OratorsConfig) -> String {
    let roles = match config.bluetooth_mode {
        BluetoothMode::ClassicMedia => "a2dp_sink",
        BluetoothMode::ClassicCallCompat => "a2dp_sink hsp_hs hfp_hf",
        BluetoothMode::LeAudioCall => "a2dp_sink bap_sink bap_source",
    };
    let mut fragment = format!(
        r#"wireplumber.settings = {{
  bluetooth.autoswitch-to-headset-profile = {}
}}

monitor.bluez.properties = {{
  bluez5.roles = [ {roles} ]
"#,
        bool_literal(config.bluetooth_mode.headset_autoswitch_enabled())
    );

    if config.bluetooth_mode.classic_call_compat_enabled() {
        fragment.push_str(
            r#"  bluez5.hfphsp-backend = "native"
  bluez5.enable-msbc = true
"#,
        );
    }

    fragment.push_str(
        r#"
}

monitor.bluez.rules = [
  {
    matches = [
      {
        device.name = "~bluez_card.*"
      }
    ]
    actions = {
      update-props = {
        device.profile = "a2dp-sink"
        bluez5.auto-connect = [ a2dp_sink ]
      }
    }
  }
]
"#,
    );

    fragment
}

pub fn parse_roles(contents: &str) -> WirePlumberRoles {
    WirePlumberRoles {
        a2dp_sink_enabled: contents.contains("a2dp_sink"),
        classic_call_enabled: contents.contains("hfp_hf") || contents.contains("hsp_hs"),
        le_audio_enabled: contents.contains("bap_sink") || contents.contains("bap_source"),
        autoswitch_to_headset_profile: parse_autoswitch_setting(contents),
    }
}

fn parse_autoswitch_setting(contents: &str) -> Option<bool> {
    contents.lines().map(str::trim).find_map(|line| {
        let value = line.strip_prefix("bluetooth.autoswitch-to-headset-profile = ")?;
        match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    })
}

fn bool_literal(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    use orators_core::{BluetoothMode, OratorsConfig};

    use super::{parse_roles, render_fragment};

    #[test]
    fn default_fragment_is_media_first() {
        let roles = parse_roles(&render_fragment(&OratorsConfig::default()));
        assert!(roles.a2dp_sink_enabled);
        assert!(!roles.classic_call_enabled);
        assert!(!roles.le_audio_enabled);
        assert_eq!(roles.autoswitch_to_headset_profile, Some(false));
    }

    #[test]
    fn classic_call_fragment_enables_hfp_without_auto_connecting_it() {
        let fragment = render_fragment(&OratorsConfig {
            bluetooth_mode: BluetoothMode::ClassicCallCompat,
            ..OratorsConfig::default()
        });
        let roles = parse_roles(&fragment);

        assert!(roles.a2dp_sink_enabled);
        assert!(roles.classic_call_enabled);
        assert!(!roles.le_audio_enabled);
        assert_eq!(roles.autoswitch_to_headset_profile, Some(true));
        assert!(fragment.contains("device.profile = \"a2dp-sink\""));
        assert!(fragment.contains("bluez5.auto-connect = [ a2dp_sink ]"));
        assert!(fragment.contains("hsp_hs"));
    }

    #[test]
    fn media_only_fragment_disables_hfp_explicitly() {
        let fragment = render_fragment(&OratorsConfig {
            bluetooth_mode: BluetoothMode::ClassicMedia,
            ..OratorsConfig::default()
        });
        let roles = parse_roles(&fragment);

        assert!(roles.a2dp_sink_enabled);
        assert!(!roles.classic_call_enabled);
        assert_eq!(roles.autoswitch_to_headset_profile, Some(false));
    }

    #[test]
    fn experimental_le_audio_fragment_enables_bap_roles_with_a2dp_fallback() {
        let fragment = render_fragment(&OratorsConfig {
            bluetooth_mode: BluetoothMode::LeAudioCall,
            ..OratorsConfig::default()
        });
        let roles = parse_roles(&fragment);

        assert!(roles.a2dp_sink_enabled);
        assert!(!roles.classic_call_enabled);
        assert!(roles.le_audio_enabled);
        assert_eq!(roles.autoswitch_to_headset_profile, Some(false));
        assert!(fragment.contains("bap_sink"));
        assert!(fragment.contains("bap_source"));
    }
}
