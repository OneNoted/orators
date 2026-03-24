use std::{process::Output, time::Duration};

use anyhow::{Context, Result, anyhow};
use orators_core::{AudioDefaults, BluetoothProfile};
use serde_json::Value;
use tokio::process::Command;

pub struct WpctlAudioRuntime;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const HEADSET_AUTOSWITCH_SETTING: &str = "bluetooth.autoswitch-to-headset-profile";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeWireDefaults {
    pub output_device: Option<String>,
    pub input_device: Option<String>,
    pub output_is_dummy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothRuntimeSettings {
    pub headset_autoswitch: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothProfileOption {
    pub index: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothCard {
    pub id: u32,
    pub address: Option<String>,
    pub name: String,
    pub active_profile_name: Option<String>,
    pub available_profiles: Vec<BluetoothProfileOption>,
}

impl WpctlAudioRuntime {
    pub async fn current_defaults(
        &self,
        bluetooth_audio_supported: bool,
        call_roles_detected: bool,
        active_device_address: Option<&str>,
    ) -> Result<AudioDefaults> {
        let defaults = self.pipewire_defaults().await?;
        let bluetooth_cards = self.bluetooth_cards().await.unwrap_or_default();
        let active_card = active_device_address.and_then(|address| {
            bluetooth_cards
                .iter()
                .find(|card| card_matches_address(card, address))
        });
        let active_bluetooth_profile = active_card
            .and_then(|card| card.active_profile_name.as_deref())
            .and_then(profile_name_to_kind);

        let output_device = inspect_wpctl("@DEFAULT_AUDIO_SINK@")
            .await
            .ok()
            .or(defaults.output_device.clone());
        let input_device = inspect_wpctl("@DEFAULT_AUDIO_SOURCE@")
            .await
            .ok()
            .or(defaults.input_device.clone());

        Ok(AudioDefaults {
            output_device,
            input_device,
            bluetooth_audio_supported,
            call_roles_detected,
            active_bluetooth_profile,
            a2dp_pinned: active_card
                .and_then(|card| card.active_profile_name.as_deref())
                .is_none_or(is_a2dp_profile_name),
        })
    }

    pub async fn pipewire_defaults(&self) -> Result<PipeWireDefaults> {
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

        Ok(PipeWireDefaults {
            output_is_dummy: output_device.as_deref().is_some_and(is_dummy_output_name),
            output_device,
            input_device,
        })
    }

    pub async fn bluetooth_cards(&self) -> Result<Vec<BluetoothCard>> {
        let output = run_command("pw-dump", &[]).await?;

        if !output.status.success() {
            anyhow::bail!(
                "pw-dump failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        parse_pw_dump_bluetooth_cards(&String::from_utf8_lossy(&output.stdout))
            .context("failed to parse pw-dump Bluetooth cards")
    }

    pub async fn pin_device_to_a2dp(&self, address: &str) -> Result<()> {
        let card = self
            .wait_for_bluetooth_card(address, 20, Duration::from_millis(500))
            .await?;
        self.pin_card_to_a2dp(&card).await
    }

    pub async fn guard_active_device_audio(&self, address: &str) -> Result<()> {
        let defaults = self.pipewire_defaults().await?;
        if defaults.output_device.is_none() || defaults.output_is_dummy {
            anyhow::bail!("PipeWire default sink is unavailable");
        }

        let card = self.find_bluetooth_card(address).await?;
        if card
            .active_profile_name
            .as_deref()
            .is_some_and(is_a2dp_profile_name)
        {
            return Ok(());
        }

        self.pin_card_to_a2dp(&card).await?;
        let refreshed = self.find_bluetooth_card(address).await?;
        if refreshed
            .active_profile_name
            .as_deref()
            .is_some_and(is_a2dp_profile_name)
        {
            Ok(())
        } else {
            anyhow::bail!(
                "Bluetooth audio profile drifted away from A2DP and could not be restored"
            )
        }
    }

    pub async fn bluetooth_runtime_settings(&self) -> Result<BluetoothRuntimeSettings> {
        let output = run_command("wpctl", &["settings", HEADSET_AUTOSWITCH_SETTING]).await?;
        if !output.status.success() {
            anyhow::bail!(
                "wpctl settings {HEADSET_AUTOSWITCH_SETTING} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        Ok(BluetoothRuntimeSettings {
            headset_autoswitch: parse_wpctl_setting_bool(&String::from_utf8_lossy(&output.stdout)),
        })
    }

    pub async fn disable_headset_autoswitch(&self) -> Result<()> {
        let output =
            run_command("wpctl", &["settings", HEADSET_AUTOSWITCH_SETTING, "false"]).await?;
        if !output.status.success() {
            anyhow::bail!(
                "wpctl settings {HEADSET_AUTOSWITCH_SETTING} false failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    async fn find_bluetooth_card(&self, address: &str) -> Result<BluetoothCard> {
        self.bluetooth_cards()
            .await?
            .into_iter()
            .find(|card| card_matches_address(card, address))
            .ok_or_else(|| anyhow!("no PipeWire Bluetooth audio card was found for {address}"))
    }

    async fn wait_for_bluetooth_card(
        &self,
        address: &str,
        attempts: usize,
        sleep: Duration,
    ) -> Result<BluetoothCard> {
        for _ in 0..attempts {
            if let Ok(card) = self.find_bluetooth_card(address).await {
                return Ok(card);
            }
            tokio::time::sleep(sleep).await;
        }

        Err(anyhow!(
            "BlueZ connected {address}, but no PipeWire Bluetooth audio card appeared"
        ))
    }

    async fn pin_card_to_a2dp(&self, card: &BluetoothCard) -> Result<()> {
        let profile = card
            .available_profiles
            .iter()
            .find(|profile| is_a2dp_profile_name(&profile.name))
            .ok_or_else(|| {
                anyhow!(
                    "no A2DP profile is available on Bluetooth card {}",
                    card.name
                )
            })?;

        let card_id = card.id.to_string();
        let profile_index = profile.index.to_string();
        let output = run_command("wpctl", &["set-profile", &card_id, &profile_index]).await?;

        if !output.status.success() {
            anyhow::bail!(
                "wpctl set-profile {} {} failed: {}",
                card.id,
                profile.index,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        Ok(())
    }
}

#[derive(Debug, Default)]
struct StatusDefaults {
    output_device: Option<String>,
    input_device: Option<String>,
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

async fn inspect_wpctl_status_defaults() -> Result<StatusDefaults> {
    let output = run_command("wpctl", &["status"]).await?;

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

fn parse_wpctl_setting_bool(output: &str) -> Option<bool> {
    output.lines().map(str::trim).find_map(|line| {
        let value = line.strip_prefix("Value:")?.trim();
        match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        }
    })
}

async fn run_command(program: &str, args: &[&str]) -> Result<Output> {
    run_spawned_command(
        {
            let mut command = Command::new(program);
            command.args(args);
            command
        },
        format!("{program} {}", args.join(" ")).trim().to_string(),
    )
    .await
}

async fn run_spawned_command(mut command: Command, label: String) -> Result<Output> {
    tokio::time::timeout(COMMAND_TIMEOUT, command.output())
        .await
        .with_context(|| format!("{label} timed out after {}s", COMMAND_TIMEOUT.as_secs()))?
        .with_context(|| format!("failed to invoke {label}"))
}

fn parse_pw_dump_bluetooth_cards(output: &str) -> Option<Vec<BluetoothCard>> {
    let objects = serde_json::from_str::<Value>(output).ok()?;
    let objects = objects.as_array()?;

    Some(
        objects
            .iter()
            .filter_map(parse_pw_dump_bluetooth_card)
            .collect(),
    )
}

fn parse_pw_dump_bluetooth_card(object: &Value) -> Option<BluetoothCard> {
    if object.get("type")?.as_str()? != "PipeWire:Interface:Device" {
        return None;
    }

    let id = object.get("id")?.as_u64()? as u32;
    let props = object.pointer("/info/props")?.as_object()?;
    if props.get("device.api")?.as_str()? != "bluez5" {
        return None;
    }

    let name = props
        .get("device.description")
        .and_then(Value::as_str)
        .or_else(|| props.get("device.nick").and_then(Value::as_str))
        .or_else(|| props.get("device.name").and_then(Value::as_str))?
        .to_string();
    let address = props
        .get("api.bluez5.address")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            props
                .get("device.name")
                .and_then(Value::as_str)
                .and_then(extract_bluetooth_address)
        })
        .or_else(|| {
            props
                .get("object.path")
                .and_then(Value::as_str)
                .and_then(extract_bluetooth_address)
        });
    let active_profile_name = object
        .pointer("/info/params/Profile/0/name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let available_profiles = object
        .pointer("/info/params/EnumProfile")
        .and_then(Value::as_array)
        .map(|profiles| {
            profiles
                .iter()
                .filter_map(parse_profile_option)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(BluetoothCard {
        id,
        address,
        name,
        active_profile_name,
        available_profiles,
    })
}

fn parse_profile_option(value: &Value) -> Option<BluetoothProfileOption> {
    Some(BluetoothProfileOption {
        index: value.get("index")?.as_u64()? as u32,
        name: value.get("name")?.as_str()?.to_string(),
    })
}

pub fn extract_bluetooth_address(input: &str) -> Option<String> {
    let token = input
        .split(|ch: char| !(ch.is_ascii_hexdigit() || ch == '_' || ch == ':'))
        .find(|part| part.matches('_').count() == 5 || part.matches(':').count() == 5)?;
    let normalized = token.replace('_', ":").to_ascii_uppercase();
    if normalized.split(':').all(|part| part.len() == 2) {
        Some(normalized)
    } else {
        None
    }
}

fn card_matches_address(card: &BluetoothCard, address: &str) -> bool {
    card.address
        .as_deref()
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(address))
}

fn is_dummy_output_name(name: &str) -> bool {
    name.eq_ignore_ascii_case("dummy output")
        || name.eq_ignore_ascii_case("auto_null")
        || name.contains("auto_null")
}

fn is_a2dp_profile_name(name: &str) -> bool {
    matches!(name, "a2dp-sink" | "a2dp_sink")
}

fn profile_name_to_kind(name: &str) -> Option<BluetoothProfile> {
    if is_a2dp_profile_name(name) {
        Some(BluetoothProfile::Media)
    } else if matches!(
        name,
        "headset-head-unit" | "handsfree-head-unit" | "hfp_hf" | "hsp_hs"
    ) {
        Some(BluetoothProfile::Call)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_bluetooth_address, parse_pw_dump_bluetooth_cards, parse_wpctl_inspect,
        parse_wpctl_setting_bool, parse_wpctl_status_defaults,
    };

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

    #[test]
    fn parses_boolean_wpctl_setting() {
        assert_eq!(parse_wpctl_setting_bool("Value: true\n"), Some(true));
        assert_eq!(parse_wpctl_setting_bool("Value: false\n"), Some(false));
    }

    #[test]
    fn extracts_bluetooth_address_from_pipewire_names() {
        assert_eq!(
            extract_bluetooth_address("bluez_card.5C_DC_49_92_D0_D8"),
            Some("5C:DC:49:92:D0:D8".to_string())
        );
    }

    #[test]
    fn parses_bluetooth_cards_from_pw_dump() {
        let output = r#"
[
  {
    "id": 91,
    "type": "PipeWire:Interface:Device",
    "info": {
      "props": {
        "device.api": "bluez5",
        "device.description": "Phone",
        "device.name": "bluez_card.5C_DC_49_92_D0_D8",
        "api.bluez5.address": "5C:DC:49:92:D0:D8"
      },
      "params": {
        "Profile": [
          { "index": 2, "name": "a2dp-sink" }
        ],
        "EnumProfile": [
          { "index": 1, "name": "off" },
          { "index": 2, "name": "a2dp-sink" },
          { "index": 3, "name": "headset-head-unit" }
        ]
      }
    }
  }
]
"#;

        let cards = parse_pw_dump_bluetooth_cards(output).unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].id, 91);
        assert_eq!(cards[0].address.as_deref(), Some("5C:DC:49:92:D0:D8"));
        assert_eq!(cards[0].active_profile_name.as_deref(), Some("a2dp-sink"));
        assert_eq!(cards[0].available_profiles.len(), 3);
    }
}
