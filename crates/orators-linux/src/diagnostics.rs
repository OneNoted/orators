use std::path::Path;

use anyhow::Result;
use orators_core::{DiagnosticCheck, DiagnosticsReport, OratorsConfig, Severity};

use crate::{
    audio::WpctlAudioRuntime,
    bluez::{AdapterInfo, BluetoothCtlBluez},
    wireplumber::WirePlumberRuntime,
};

pub async fn collect_report(
    bluez: &BluetoothCtlBluez,
    audio: &WpctlAudioRuntime,
    wireplumber: &WirePlumberRuntime,
    fragment_path: &Path,
    config: &OratorsConfig,
) -> Result<DiagnosticsReport> {
    let mut checks = Vec::new();

    checks.push(match bluez.adapter_info().await {
        Ok(info) => adapter_check(info),
        Err(error) if error.to_string().contains("no BlueZ adapter found") => DiagnosticCheck {
            code: "bluez.adapter".to_string(),
            severity: Severity::Error,
            summary: "No BlueZ adapter was detected".to_string(),
            detail: Some("No `org.bluez.Adapter1` object was found on the system bus.".to_string()),
            remediation: Some(
                "Make sure the Bluetooth controller is present and powered on.".to_string(),
            ),
        },
        Err(error) => DiagnosticCheck {
            code: "bluez.adapter".to_string(),
            severity: Severity::Error,
            summary: "Failed to inspect the BlueZ adapter".to_string(),
            detail: Some(error.to_string()),
            remediation: Some(
                "Make sure the BlueZ system service is running and reachable on D-Bus.".to_string(),
            ),
        },
    });

    checks.push(
        match wireplumber.inspect_fragment(fragment_path, config).await {
            Ok(report) if report.changed => DiagnosticCheck {
                code: "wireplumber.fragment".to_string(),
                severity: Severity::Warn,
                summary: "WirePlumber fragment does not match Orators defaults".to_string(),
                detail: Some(format!(
                    "Fragment path: {}. Expected roles: {}.",
                    report.path,
                    expected_roles_label(config.call_audio_enabled)
                )),
                remediation: Some(
                    "Run `oratorsctl doctor --apply` or `oratorsctl install-user-service`."
                        .to_string(),
                ),
            },
            Ok(report) => DiagnosticCheck {
                code: "wireplumber.fragment".to_string(),
                severity: Severity::Info,
                summary: "WirePlumber fragment is present".to_string(),
                detail: Some(format!(
                    "Fragment path: {}. Expected roles: {}.",
                    report.path,
                    expected_roles_label(config.call_audio_enabled)
                )),
                remediation: None,
            },
            Err(error) => DiagnosticCheck {
                code: "wireplumber.fragment".to_string(),
                severity: Severity::Warn,
                summary: "WirePlumber fragment is missing".to_string(),
                detail: Some(error.to_string()),
                remediation: Some(
                    "Run `oratorsctl doctor --apply` to install the fragment.".to_string(),
                ),
            },
        },
    );

    let roles = wireplumber.roles(fragment_path).await.ok();
    checks.push(match roles {
        Some(roles) if roles.hfp_hf_enabled => DiagnosticCheck {
            code: "bluetooth.policy".to_string(),
            severity: Severity::Info,
            summary: "Bluetooth media and hands-free call support are enabled".to_string(),
            detail: Some(
                "Orators will prefer A2DP for normal playback and expose headset-side HSP/HFP roles when a voice app opens the microphone. Call sessions will use lower-fidelity headset audio by design."
                    .to_string(),
            ),
            remediation: None,
        },
        Some(_) => DiagnosticCheck {
            code: "bluetooth.policy".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth call audio is disabled".to_string(),
            detail: Some(
                "The current policy exposes speaker-style A2DP playback only. Discord and other VoIP apps will not see a Bluetooth microphone/input path."
                    .to_string(),
            ),
            remediation: Some(
                "Set `call_audio_enabled = true` in `~/.config/orators/config.toml` and run `oratorsctl doctor --apply`."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "bluetooth.policy".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth audio policy could not be inspected".to_string(),
            detail: Some(format!(
                "Failed to parse WirePlumber roles from {}.",
                fragment_path.display()
            )),
            remediation: Some(
                "Run `oratorsctl doctor --apply` to rewrite the fragment, then rerun `oratorsctl doctor`."
                    .to_string(),
            ),
        },
    });

    let defaults = audio
        .current_defaults(
            roles.as_ref().is_some_and(|roles| roles.a2dp_sink_enabled),
            roles.as_ref().is_some_and(|roles| roles.hfp_hf_enabled),
        )
        .await;
    checks.push(match defaults {
        Ok(audio_defaults) if audio_defaults.output_device.is_some() => DiagnosticCheck {
            code: "pipewire.defaults".to_string(),
            severity: Severity::Info,
            summary: "PipeWire default devices were discovered".to_string(),
            detail: Some(format!(
                "Output: {}. Input: {}.",
                audio_defaults.output_device.as_deref().unwrap_or("not detected"),
                audio_defaults.input_device.as_deref().unwrap_or("not detected")
            )),
            remediation: None,
        },
        Ok(audio_defaults) => DiagnosticCheck {
            code: "pipewire.defaults".to_string(),
            severity: Severity::Warn,
            summary: "PipeWire defaults were not fully detected".to_string(),
            detail: Some(format!(
                "Output: {}. Input: {}. Media playback can still work, but diagnostics will be less precise.",
                audio_defaults.output_device.as_deref().unwrap_or("not detected"),
                audio_defaults.input_device.as_deref().unwrap_or("not detected")
            )),
            remediation: Some(
                "Make sure PipeWire is running in the user session and that a default sink and source are selected."
                    .to_string(),
            ),
        },
        Err(error) => DiagnosticCheck {
            code: "pipewire.defaults".to_string(),
            severity: Severity::Warn,
            summary: "PipeWire defaults could not be inspected".to_string(),
            detail: Some(error.to_string()),
            remediation: Some(
                "Make sure PipeWire and wpctl are installed and running.".to_string(),
            ),
        },
    });

    Ok(DiagnosticsReport {
        generated_at_epoch_secs: crate::now_epoch_secs(),
        checks,
    })
}

fn expected_roles_label(call_audio_enabled: bool) -> &'static str {
    if call_audio_enabled {
        "a2dp_sink, hsp_hs, hfp_hf"
    } else {
        "a2dp_sink"
    }
}

fn adapter_check(info: AdapterInfo) -> DiagnosticCheck {
    let summary = if info.powered && info.discoverable && info.pairable {
        "BlueZ adapter is ready for pairing"
    } else if info.powered {
        "BlueZ adapter is available but not fully ready for pairing"
    } else {
        "BlueZ adapter is present but powered off"
    };

    let severity = if info.powered && info.discoverable && info.pairable {
        Severity::Info
    } else {
        Severity::Warn
    };

    let visible_name = info
        .alias
        .clone()
        .or(info.name.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let address = info.address.unwrap_or_else(|| "unknown".to_string());
    let detail = format!(
        "Look for Bluetooth device '{visible_name}' ({address}). powered={}, discoverable={}, pairable={}, scanning={}.",
        yes_no(info.powered),
        yes_no(info.discoverable),
        yes_no(info.pairable),
        yes_no(info.discovering),
    );

    let remediation = (!info.discoverable || !info.pairable || !info.powered).then(|| {
        "Run `oratorsctl pair start --timeout 120` and then refresh Bluetooth devices on the phone."
            .to_string()
    });

    DiagnosticCheck {
        code: "bluez.adapter".to_string(),
        severity,
        summary: summary.to_string(),
        detail: Some(detail),
        remediation,
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
