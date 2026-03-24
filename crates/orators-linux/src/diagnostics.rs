use std::path::Path;

use anyhow::Result;
use orators_core::{DiagnosticCheck, DiagnosticsReport, Severity};

use crate::{audio::WpctlAudioRuntime, bluez::BluetoothCtlBluez, wireplumber::WirePlumberRuntime};

pub async fn collect_report(
    bluez: &BluetoothCtlBluez,
    audio: &WpctlAudioRuntime,
    wireplumber: &WirePlumberRuntime,
    fragment_path: &Path,
) -> Result<DiagnosticsReport> {
    let mut checks = Vec::new();

    checks.push(match bluez.adapter_available().await {
        Ok(true) => DiagnosticCheck {
            code: "bluez.adapter".to_string(),
            severity: Severity::Info,
            summary: "BlueZ adapter is available".to_string(),
            detail: None,
            remediation: None,
        },
        Ok(false) => DiagnosticCheck {
            code: "bluez.adapter".to_string(),
            severity: Severity::Error,
            summary: "No BlueZ adapter was detected".to_string(),
            detail: Some("`bluetoothctl show` returned no active controller.".to_string()),
            remediation: Some(
                "Make sure the Bluetooth controller is present and powered on.".to_string(),
            ),
        },
        Err(error) => DiagnosticCheck {
            code: "bluez.command".to_string(),
            severity: Severity::Error,
            summary: "Failed to invoke bluetoothctl".to_string(),
            detail: Some(error.to_string()),
            remediation: Some(
                "Install BlueZ userspace tools and verify they are in PATH.".to_string(),
            ),
        },
    });

    let fragment_report = wireplumber.inspect_fragment(fragment_path).await;
    checks.push(match fragment_report {
        Ok(report) if report.changed => DiagnosticCheck {
            code: "wireplumber.fragment".to_string(),
            severity: Severity::Warn,
            summary: "WirePlumber fragment does not match Orators defaults".to_string(),
            detail: Some(format!("Fragment path: {}", report.path)),
            remediation: Some(
                "Run `oratorsctl doctor --apply` or `oratorsctl install-user-service`.".to_string(),
            ),
        },
        Ok(report) => DiagnosticCheck {
            code: "wireplumber.fragment".to_string(),
            severity: Severity::Info,
            summary: "WirePlumber fragment is present".to_string(),
            detail: Some(format!("Fragment path: {}", report.path)),
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
    });

    let defaults = audio.current_defaults(true, true).await;
    checks.push(match defaults {
        Ok(audio_defaults) => DiagnosticCheck {
            code: "pipewire.defaults".to_string(),
            severity: Severity::Info,
            summary: "PipeWire default devices were discovered".to_string(),
            detail: Some(format!(
                "output={:?}, input={:?}",
                audio_defaults.output_device, audio_defaults.input_device
            )),
            remediation: None,
        },
        Err(error) => DiagnosticCheck {
            code: "pipewire.defaults".to_string(),
            severity: Severity::Warn,
            summary: "Failed to inspect PipeWire default devices".to_string(),
            detail: Some(error.to_string()),
            remediation: Some(
                "Make sure PipeWire and wpctl are installed for the active user session."
                    .to_string(),
            ),
        },
    });

    Ok(DiagnosticsReport {
        generated_at_epoch_secs: crate::now_epoch_secs(),
        checks,
    })
}
