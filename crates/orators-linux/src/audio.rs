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
        let local_output_available = output_device
            .as_deref()
            .is_some_and(|device| !is_dummy_output_name(device));

        Ok(AudioDefaults {
            output_device,
            input_device,
            local_output_available,
        })
    }
}

fn is_dummy_output_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("Dummy Output")
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
            let line = line.strip_prefix("* ").unwrap_or(line);
            ["node.description = ", "node.nick = ", "node.name = "]
                .into_iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .map(|value| value.trim_matches('"').to_string())
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
    use super::{is_dummy_output_name, parse_wpctl_inspect};

    #[test]
    fn parses_wpctl_inspect_output() {
        let parsed = parse_wpctl_inspect(
            r#"
id 42, type PipeWire:Interface:Node
  * node.description = "RODECaster Pro II"
"#,
        );

        assert_eq!(parsed.as_deref(), Some("RODECaster Pro II"));
    }

    #[test]
    fn detects_dummy_output_name() {
        assert!(is_dummy_output_name("Dummy Output"));
        assert!(is_dummy_output_name("dummy output"));
        assert!(!is_dummy_output_name("RODECaster Pro II"));
    }
}
