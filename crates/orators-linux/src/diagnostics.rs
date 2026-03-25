use anyhow::Result;
use orators_core::{DiagnosticCheck, DiagnosticsReport, Severity};

use crate::{
    audio::LocalAudioRuntime,
    bluealsa::{BluealsaAssets, SYSTEM_BACKEND_UNIT},
    bluez::{AdapterInfo, BluetoothCtlBluez, remote_device_supports_media},
    systemd::{ServiceStatus, SystemdUserRuntime},
};

pub async fn collect_report(
    bluez: &BluetoothCtlBluez,
    audio: &LocalAudioRuntime,
    systemd: &SystemdUserRuntime,
    configured_adapter: Option<&str>,
    backend_service: Option<ServiceStatus>,
) -> Result<DiagnosticsReport> {
    let mut checks = Vec::new();

    let adapter_info = match bluez.adapter_info().await {
        Ok(info) => {
            checks.push(adapter_check(&info, configured_adapter));
            Some(info)
        }
        Err(error) if error.to_string().contains("no BlueZ adapter found") => {
            checks.push(DiagnosticCheck {
                code: "bluez.adapter".to_string(),
                severity: Severity::Error,
                summary: "No BlueZ adapter was detected".to_string(),
                detail: Some(
                    "No `org.bluez.Adapter1` object was found on the system bus.".to_string(),
                ),
                remediation: Some(
                    "Make sure the Bluetooth controller is present and powered on.".to_string(),
                ),
            });
            None
        }
        Err(error) => {
            checks.push(DiagnosticCheck {
                code: "bluez.adapter".to_string(),
                severity: Severity::Error,
                summary: "Failed to inspect the BlueZ adapter".to_string(),
                detail: Some(error.to_string()),
                remediation: Some(
                    "Make sure the BlueZ system service is running and reachable on D-Bus."
                        .to_string(),
                ),
            });
            None
        }
    };

    checks.push(assets_check(BluealsaAssets::discover().ok().as_ref()));
    checks.push(fragment_check(systemd.wireplumber_fragment_installed()?));
    checks.push(system_backend_check(backend_service.as_ref()));
    checks.push(aplay_check(audio.aplay_available().await.ok()));

    let defaults = audio.current_defaults().await.ok();
    checks.push(alsa_default_check(defaults.as_ref()));
    checks.push(local_output_check(defaults.as_ref()));

    let active_device = bluez.remote_devices().await.ok().and_then(|devices| {
        devices
            .into_iter()
            .find(|device| device.connected && remote_device_supports_media(device))
    });
    checks.push(active_device_check(active_device.as_ref()));

    checks.push(backend_ready_check(
        adapter_info.as_ref(),
        BluealsaAssets::discover().ok().as_ref(),
        systemd.wireplumber_fragment_installed()?,
        backend_service.as_ref(),
        defaults.as_ref(),
    ));

    Ok(DiagnosticsReport {
        generated_at_epoch_secs: crate::now_epoch_secs(),
        checks,
    })
}

fn adapter_check(adapter: &AdapterInfo, configured_adapter: Option<&str>) -> DiagnosticCheck {
    let configured = configured_adapter
        .map(|adapter_id| format!("Configured adapter: {adapter_id}. "))
        .unwrap_or_default();
    DiagnosticCheck {
        code: "bluez.adapter".to_string(),
        severity: Severity::Info,
        summary: "BlueZ adapter is ready".to_string(),
        detail: Some(format!(
            "{configured}Look for Bluetooth device '{}' ({}). powered={}, discoverable={}, pairable={}, scanning={}.",
            adapter.alias.as_deref().unwrap_or("orators"),
            adapter.address.as_deref().unwrap_or("unknown"),
            adapter.powered,
            adapter.discoverable,
            adapter.pairable,
            adapter.discovering
        )),
        remediation: None,
    }
}

fn assets_check(assets: Option<&BluealsaAssets>) -> DiagnosticCheck {
    match assets {
        Some(assets) => DiagnosticCheck {
            code: "bluealsa.assets".to_string(),
            severity: Severity::Info,
            summary: "BlueALSA binaries are available".to_string(),
            detail: Some(format!(
                "bluealsad={}, bluealsa-aplay={}, bluealsactl={}.",
                assets.bluealsad.display(),
                assets.bluealsa_aplay.display(),
                assets.bluealsactl.display()
            )),
            remediation: None,
        },
        None => DiagnosticCheck {
            code: "bluealsa.assets".to_string(),
            severity: Severity::Error,
            summary: "BlueALSA binaries are missing".to_string(),
            detail: Some(
                "The host must provide `bluealsad`, `bluealsa-aplay`, and `bluealsactl`."
                    .to_string(),
            ),
            remediation: Some("Install `bluez-alsa` on the host before using Orators.".to_string()),
        },
    }
}

fn fragment_check(installed: bool) -> DiagnosticCheck {
    if installed {
        DiagnosticCheck {
            code: "backend.fragment".to_string(),
            severity: Severity::Info,
            summary: "WirePlumber Bluetooth ownership is disabled for Orators".to_string(),
            detail: Some(
                "The Orators-managed WirePlumber fragment is installed and disables only the Bluetooth monitor."
                    .to_string(),
            ),
            remediation: None,
        }
    } else {
        DiagnosticCheck {
            code: "backend.fragment".to_string(),
            severity: Severity::Error,
            summary: "The Orators WirePlumber Bluetooth-disable fragment is not installed".to_string(),
            detail: Some(
                "Orators requires WirePlumber to stay out of Bluetooth ownership while BlueALSA owns the A2DP sink."
                    .to_string(),
            ),
            remediation: Some(
                "Run `oratorsctl install-system-backend` to install the managed backend."
                    .to_string(),
            ),
        }
    }
}

fn system_backend_check(status: Option<&ServiceStatus>) -> DiagnosticCheck {
    match status {
        Some(status) if status.active_state == "active" && status.sub_state == "running" => {
            DiagnosticCheck {
                code: "bluealsa.service".to_string(),
                severity: Severity::Info,
                summary: "The Orators BlueALSA system backend is healthy".to_string(),
                detail: Some(format!(
                    "{} is active and running. Restart count since boot: {}.",
                    SYSTEM_BACKEND_UNIT, status.restart_count
                )),
                remediation: None,
            }
        }
        Some(status) => DiagnosticCheck {
            code: "bluealsa.service".to_string(),
            severity: Severity::Error,
            summary: "The Orators BlueALSA system backend is not healthy".to_string(),
            detail: Some(format!(
                "ActiveState={}, SubState={}, Result={}, RestartCount={}.",
                status.active_state,
                status.sub_state,
                status.result.as_deref().unwrap_or("unknown"),
                status.restart_count
            )),
            remediation: Some(
                "Run `oratorsctl install-system-backend` and make sure the system backend starts successfully."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "bluealsa.service".to_string(),
            severity: Severity::Error,
            summary: "The Orators BlueALSA system backend is not installed".to_string(),
            detail: Some(
                "The root-owned `orators-bluealsad.service` unit is missing or inactive."
                    .to_string(),
            ),
            remediation: Some(
                "Run `oratorsctl install-system-backend` to install the managed backend."
                    .to_string(),
            ),
        },
    }
}

fn aplay_check(aplay_available: Option<bool>) -> DiagnosticCheck {
    match aplay_available {
        Some(true) => DiagnosticCheck {
            code: "alsa.aplay".to_string(),
            severity: Severity::Info,
            summary: "ALSA playback tools are available".to_string(),
            detail: Some("`aplay` is available for local ALSA playback checks.".to_string()),
            remediation: None,
        },
        Some(false) | None => DiagnosticCheck {
            code: "alsa.aplay".to_string(),
            severity: Severity::Error,
            summary: "ALSA playback tools are missing".to_string(),
            detail: Some("`aplay` could not be executed on this host.".to_string()),
            remediation: Some(
                "Install `alsa-utils` so `bluealsa-aplay` has a usable ALSA playback target."
                    .to_string(),
            ),
        },
    }
}

fn alsa_default_check(defaults: Option<&orators_core::AudioDefaults>) -> DiagnosticCheck {
    match defaults {
        Some(defaults) if defaults.alsa_default_output_available => DiagnosticCheck {
            code: "alsa.default".to_string(),
            severity: Severity::Info,
            summary: "ALSA default output is available".to_string(),
            detail: Some(
                "The host exposes an ALSA `default` output path for `bluealsa-aplay`."
                    .to_string(),
            ),
            remediation: None,
        },
        Some(_) => DiagnosticCheck {
            code: "alsa.default".to_string(),
            severity: Severity::Error,
            summary: "ALSA default output is not available".to_string(),
            detail: Some(
                "`aplay -L` did not expose an ALSA `default` playback target.".to_string(),
            ),
            remediation: Some(
                "Fix the local ALSA/PipeWire playback stack so `default` exists before using Orators."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "alsa.default".to_string(),
            severity: Severity::Warn,
            summary: "ALSA default output could not be inspected".to_string(),
            detail: Some("Local audio defaults could not be queried.".to_string()),
            remediation: Some(
                "Make sure ALSA and PipeWire are healthy before using Orators.".to_string(),
            ),
        },
    }
}

fn local_output_check(defaults: Option<&orators_core::AudioDefaults>) -> DiagnosticCheck {
    let Some(defaults) = defaults else {
        return DiagnosticCheck {
            code: "local.output".to_string(),
            severity: Severity::Warn,
            summary: "Current desktop output could not be detected".to_string(),
            detail: Some("`wpctl inspect` did not return a current sink/source.".to_string()),
            remediation: None,
        };
    };

    DiagnosticCheck {
        code: "local.output".to_string(),
        severity: Severity::Info,
        summary: "Current desktop output devices were discovered".to_string(),
        detail: Some(format!(
            "Output: {}. Input: {}.",
            defaults.output_device.as_deref().unwrap_or("not detected"),
            defaults.input_device.as_deref().unwrap_or("not detected")
        )),
        remediation: None,
    }
}

fn active_device_check(device: Option<&crate::bluez::RemoteDeviceInfo>) -> DiagnosticCheck {
    match device {
        Some(device) => DiagnosticCheck {
            code: "active.audio_device".to_string(),
            severity: Severity::Info,
            summary: "A connected Bluetooth audio device was detected".to_string(),
            detail: Some(format!(
                "{} [{}] is connected and eligible for media playback.",
                device.alias.as_deref().unwrap_or("unnamed"),
                device.address
            )),
            remediation: None,
        },
        None => DiagnosticCheck {
            code: "active.audio_device".to_string(),
            severity: Severity::Info,
            summary: "No connected Bluetooth audio device is active".to_string(),
            detail: Some(
                "Orators will start `bluealsa-aplay` automatically when a trusted media device connects."
                    .to_string(),
            ),
            remediation: None,
        },
    }
}

fn backend_ready_check(
    adapter: Option<&AdapterInfo>,
    assets: Option<&BluealsaAssets>,
    fragment_installed: bool,
    backend_service: Option<&ServiceStatus>,
    defaults: Option<&orators_core::AudioDefaults>,
) -> DiagnosticCheck {
    let ready = adapter.is_some()
        && assets.is_some()
        && fragment_installed
        && backend_service
            .is_some_and(|status| status.active_state == "active" && status.sub_state == "running")
        && defaults.is_some_and(|defaults| defaults.alsa_default_output_available);

    if ready {
        DiagnosticCheck {
            code: "host.media_support".to_string(),
            severity: Severity::Info,
            summary: "This host is ready for BlueALSA-backed Bluetooth speaker playback".to_string(),
            detail: Some(
                "Orators uses BlueALSA for Bluetooth media and leaves the rest of the desktop audio session alone."
                    .to_string(),
            ),
            remediation: None,
        }
    } else {
        DiagnosticCheck {
            code: "host.media_support".to_string(),
            severity: Severity::Error,
            summary: "This host is not ready for the BlueALSA Bluetooth backend".to_string(),
            detail: Some(
                "Orators needs a BlueZ adapter, BlueALSA binaries, the managed WirePlumber fragment, a healthy `orators-bluealsad.service`, and a usable ALSA `default` output."
                    .to_string(),
            ),
            remediation: Some(
                "Run `oratorsctl install-system-backend`, verify the system backend is healthy, and make sure ALSA default output is available."
                    .to_string(),
            ),
        }
    }
}
