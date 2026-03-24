use std::path::Path;

use anyhow::{Context, Result};
use orators_core::{OratorsConfig, SessionConfigStatus};
use tokio::fs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WirePlumberRoles {
    pub a2dp_sink_enabled: bool,
    pub hfp_hf_enabled: bool,
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
    let roles = if config.call_audio_enabled {
        "a2dp_sink hfp_hf"
    } else {
        "a2dp_sink"
    };
    let mut fragment = format!(
        r#"monitor.bluez.properties = {{
  bluez5.roles = [ {roles} ]
"#
    );

    if config.call_audio_enabled {
        fragment.push_str(
            r#"  bluez5.hfphsp-backend = "native"
  bluez5.enable-msbc = true
"#,
        );
    }

    fragment.push_str(
        r#"}

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
        hfp_hf_enabled: contents.contains("hfp_hf"),
    }
}

#[cfg(test)]
mod tests {
    use orators_core::OratorsConfig;

    use super::{parse_roles, render_fragment};

    #[test]
    fn default_fragment_enables_dynamic_call_support() {
        let roles = parse_roles(&render_fragment(&OratorsConfig::default()));
        assert!(roles.a2dp_sink_enabled);
        assert!(roles.hfp_hf_enabled);
    }

    #[test]
    fn call_audio_fragment_enables_hfp_without_auto_connecting_it() {
        let fragment = render_fragment(&OratorsConfig {
            call_audio_enabled: true,
            ..OratorsConfig::default()
        });
        let roles = parse_roles(&fragment);

        assert!(roles.a2dp_sink_enabled);
        assert!(roles.hfp_hf_enabled);
        assert!(fragment.contains("device.profile = \"a2dp-sink\""));
        assert!(fragment.contains("bluez5.auto-connect = [ a2dp_sink ]"));
    }

    #[test]
    fn media_only_fragment_disables_hfp_explicitly() {
        let fragment = render_fragment(&OratorsConfig {
            call_audio_enabled: false,
            ..OratorsConfig::default()
        });
        let roles = parse_roles(&fragment);

        assert!(roles.a2dp_sink_enabled);
        assert!(!roles.hfp_hf_enabled);
    }
}
