use anyhow::{Context, Result};
use orators_core::AudioDefaults;
use tokio::process::Command;

pub struct WpctlAudioRuntime;

impl WpctlAudioRuntime {
    pub async fn current_defaults(
        &self,
        a2dp_sink_enabled: bool,
        hfp_ag_enabled: bool,
    ) -> Result<AudioDefaults> {
        let fallback_defaults = inspect_wpctl_status_defaults().await.ok();
        let output_device = inspect_wpctl("@DEFAULT_AUDIO_SINK@")
            .await
            .ok()
            .or_else(|| {
                fallback_defaults
                    .as_ref()
                    .and_then(|defaults| defaults.output_device.clone())
            });
        let input_device = inspect_wpctl("@DEFAULT_AUDIO_SOURCE@")
            .await
            .ok()
            .or_else(|| {
                fallback_defaults
                    .as_ref()
                    .and_then(|defaults| defaults.input_device.clone())
            });

        Ok(AudioDefaults {
            output_device,
            input_device,
            a2dp_sink_enabled,
            hfp_ag_enabled,
        })
    }
}

#[derive(Debug, Default)]
struct StatusDefaults {
    output_device: Option<String>,
    input_device: Option<String>,
}

async fn inspect_wpctl(target: &str) -> Result<String> {
    let output = Command::new("wpctl")
        .args(["inspect", target])
        .output()
        .await
        .with_context(|| format!("failed to invoke wpctl inspect {target}"))?;

    if !output.status.success() {
        anyhow::bail!(
            "wpctl inspect {target} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_wpctl_inspect(&stdout).context("failed to parse wpctl inspect output")
}

pub fn parse_wpctl_inspect(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find_map(|line| {
            ["node.description = ", "node.nick = ", "node.name = "]
                .into_iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .map(|value| value.trim_matches('"').to_string())
}

async fn inspect_wpctl_status_defaults() -> Result<StatusDefaults> {
    let output = Command::new("wpctl")
        .arg("status")
        .output()
        .await
        .context("failed to invoke wpctl status")?;

    if !output.status.success() {
        anyhow::bail!(
            "wpctl status failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_wpctl_status_defaults(&stdout))
}

fn parse_wpctl_status_defaults(output: &str) -> StatusDefaults {
    let mut defaults = StatusDefaults::default();
    let mut in_settings = false;

    for line in output.lines().map(str::trim) {
        if line == "Settings" {
            in_settings = true;
            continue;
        }

        if !in_settings {
            continue;
        }

        if let Some(rest) = line.strip_prefix("0. Audio/Sink") {
            defaults.output_device = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("1. Audio/Source") {
            defaults.input_device = Some(rest.trim().to_string());
        }
    }

    defaults
}

#[cfg(test)]
mod tests {
    use super::{parse_wpctl_inspect, parse_wpctl_status_defaults};

    #[test]
    fn parses_best_available_pipewire_name() {
        let output = r#"
id 42, type PipeWire:Interface:Node
    node.description = "Laptop Speakers"
    node.name = "alsa_output.pci-0000"
"#;

        assert_eq!(
            parse_wpctl_inspect(output).as_deref(),
            Some("Laptop Speakers")
        );
    }

    #[test]
    fn parses_default_devices_from_wpctl_status() {
        let output = r#"
Settings
 └─ Default Configured Devices:
         0. Audio/Sink    alsa_output.test
         1. Audio/Source  alsa_input.test
"#;

        let defaults = parse_wpctl_status_defaults(output);
        assert_eq!(defaults.output_device.as_deref(), Some("alsa_output.test"));
        assert_eq!(defaults.input_device.as_deref(), Some("alsa_input.test"));
    }
}
