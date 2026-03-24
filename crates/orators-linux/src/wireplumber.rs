use std::path::Path;

use anyhow::{Context, Result};
use orators_core::SessionConfigStatus;
use tokio::fs;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WirePlumberRoles {
    pub a2dp_sink_enabled: bool,
    pub hfp_ag_enabled: bool,
}

pub struct WirePlumberRuntime;

impl WirePlumberRuntime {
    pub async fn ensure_fragment(&self, path: &Path) -> Result<SessionConfigStatus> {
        let desired = render_fragment();
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

    pub async fn inspect_fragment(&self, path: &Path) -> Result<SessionConfigStatus> {
        let current = fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(SessionConfigStatus {
            path: path.display().to_string(),
            changed: current != render_fragment(),
        })
    }

    pub async fn roles(&self, path: &Path) -> Result<WirePlumberRoles> {
        let current = fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        Ok(parse_roles(&current))
    }
}

pub fn render_fragment() -> String {
    r#"monitor.bluez.properties = {
  bluez5.roles = [ a2dp_sink hfp_ag ]
  bluez5.hfphsp-backend = "native"
  bluez5.enable-msbc = true
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
        bluez5.auto-connect = [ a2dp_sink hfp_ag ]
      }
    }
  }
]
"#
    .to_string()
}

pub fn parse_roles(contents: &str) -> WirePlumberRoles {
    WirePlumberRoles {
        a2dp_sink_enabled: contents.contains("a2dp_sink"),
        hfp_ag_enabled: contents.contains("hfp_ag"),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_roles, render_fragment};

    #[test]
    fn fragment_enables_expected_roles() {
        let roles = parse_roles(&render_fragment());
        assert!(roles.a2dp_sink_enabled);
        assert!(roles.hfp_ag_enabled);
    }
}
