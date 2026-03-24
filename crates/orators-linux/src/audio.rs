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
        let output_device = inspect_wpctl("@DEFAULT_AUDIO_SINK@").await.ok();
        let input_device = inspect_wpctl("@DEFAULT_AUDIO_SOURCE@").await.ok();

        Ok(AudioDefaults {
            output_device,
            input_device,
            a2dp_sink_enabled,
            hfp_ag_enabled,
        })
    }
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

#[cfg(test)]
mod tests {
    use super::parse_wpctl_inspect;

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
}
