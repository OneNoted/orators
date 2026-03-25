use std::{process::Output, time::Duration};

use anyhow::{Context, Result};
use orators_core::AudioDefaults;
use tokio::process::Command;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

pub struct LocalAudioRuntime;

impl LocalAudioRuntime {
    pub async fn current_defaults(&self) -> Result<AudioDefaults> {
        let output_device = inspect_wpctl("@DEFAULT_AUDIO_SINK@").await.ok();
        let input_device = inspect_wpctl("@DEFAULT_AUDIO_SOURCE@").await.ok();

        Ok(AudioDefaults {
            output_device,
            input_device,
            alsa_default_output_available: self
                .alsa_default_output_available()
                .await
                .unwrap_or(false),
        })
    }

    pub async fn alsa_default_output_available(&self) -> Result<bool> {
        let output = run_command("aplay", &["-L"]).await?;
        if !output.status.success() {
            anyhow::bail!(
                "aplay -L failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        Ok(parse_aplay_has_default(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    pub async fn aplay_available(&self) -> Result<bool> {
        let output = run_command("aplay", &["--version"]).await?;
        Ok(output.status.success())
    }
}

async fn inspect_wpctl(target: &str) -> Result<String> {
    let output = run_command("wpctl", &["inspect", target]).await?;

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

fn parse_aplay_has_default(output: &str) -> bool {
    output.lines().map(str::trim).any(|line| line == "default")
}

async fn run_command(program: &str, args: &[&str]) -> Result<Output> {
    let mut command = Command::new(program);
    command.args(args);
    tokio::time::timeout(COMMAND_TIMEOUT, command.output())
        .await
        .with_context(|| format!("{program} {} timed out", args.join(" ")))?
        .with_context(|| format!("failed to invoke {program} {}", args.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::{parse_aplay_has_default, parse_wpctl_inspect};

    #[test]
    fn parses_wpctl_inspect_output() {
        let parsed = parse_wpctl_inspect(
            r#"
id 42, type PipeWire:Interface:Node
    node.description = "RODECaster Pro II"
"#,
        );

        assert_eq!(parsed.as_deref(), Some("RODECaster Pro II"));
    }

    #[test]
    fn detects_alsa_default_output() {
        assert!(parse_aplay_has_default("default\nsysdefault:CARD=USB\n"));
        assert!(!parse_aplay_has_default("sysdefault:CARD=USB\n"));
    }
}
