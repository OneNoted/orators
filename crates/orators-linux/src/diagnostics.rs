use anyhow::Result;
use orators_core::{DiagnosticCheck, DiagnosticsReport, MediaBackendStatus, MediaCodec, Severity};

use crate::{
    audio::{PipeWireDefaults, WpctlAudioRuntime},
    bluez::{AdapterInfo, BluetoothCtlBluez},
    owned_backend::OwnedBluetoothMediaBackend,
    systemd::{SystemdUserRuntime, UserServiceStatus, service_uses_audio_profile},
};

pub async fn collect_report(
    bluez: &BluetoothCtlBluez,
    audio: &WpctlAudioRuntime,
    systemd: &SystemdUserRuntime,
    owned_backend: &OwnedBluetoothMediaBackend,
) -> Result<DiagnosticsReport> {
    let mut checks = Vec::new();

    let adapter_info = match bluez.adapter_info().await {
        Ok(info) => {
            checks.push(adapter_check(info.clone()));
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

    checks.push(media_role_check(adapter_info.as_ref()));

    let wireplumber = systemd.service_status("wireplumber.service").await.ok();
    let host_backend_installed = systemd.host_backend_installed().await.unwrap_or(false);
    checks.push(wireplumber_check(wireplumber.as_ref()));

    let defaults = audio.pipewire_defaults().await.ok();
    checks.push(pipewire_defaults_check(defaults.as_ref()));

    let backend = owned_backend.snapshot_status().await;
    checks.push(owned_backend_check(&backend));

    checks.push(host_support_check(
        adapter_info.as_ref(),
        defaults.as_ref(),
        wireplumber.as_ref(),
        host_backend_installed,
        &backend,
    ));

    Ok(DiagnosticsReport {
        generated_at_epoch_secs: crate::now_epoch_secs(),
        checks,
    })
}

fn media_role_check(adapter: Option<&AdapterInfo>) -> DiagnosticCheck {
    let supported = adapter.is_some_and(adapter_supports_media);
    DiagnosticCheck {
        code: "bluez.media_roles".to_string(),
        severity: if supported {
            Severity::Info
        } else {
            Severity::Error
        },
        summary: if supported {
            "The Bluetooth controller advertises Audio Sink / A2DP media support".to_string()
        } else {
            "The Bluetooth controller does not advertise Audio Sink / A2DP media support"
                .to_string()
        },
        detail: adapter.map(|adapter| format!("Advertised adapter UUIDs: {}.", adapter.uuids.join(", "))),
        remediation: (!supported).then_some(
            "Fix the host Bluetooth media support outside Orators before pairing or connecting media devices."
                .to_string(),
        ),
    }
}

fn wireplumber_check(status: Option<&UserServiceStatus>) -> DiagnosticCheck {
    match status {
        Some(status) if status.active_state == "active" && status.sub_state == "running" => {
            DiagnosticCheck {
                code: "wireplumber.service".to_string(),
                severity: if service_uses_audio_profile(status) {
                    Severity::Info
                } else {
                    Severity::Warn
                },
                summary: if service_uses_audio_profile(status) {
                    "WirePlumber is healthy and running the managed audio profile".to_string()
                } else {
                    "WirePlumber is healthy but not running the managed audio profile".to_string()
                },
                detail: Some(format!(
                    "wireplumber.service is active and running. Restart count since boot: {}. ExecStart: {}.",
                    status.restart_count,
                    status.exec_start.as_deref().unwrap_or("not detected")
                )),
                remediation: (!service_uses_audio_profile(status)).then_some(
                    "Install the managed host backend with `oratorsctl install-host-backend` and restart the user audio services."
                        .to_string(),
                ),
            }
        }
        Some(status) => DiagnosticCheck {
            code: "wireplumber.service".to_string(),
            severity: Severity::Error,
            summary: "WirePlumber is not healthy".to_string(),
            detail: Some(format!(
                "ActiveState={}, SubState={}, Result={}, RestartCount={}.",
                status.active_state,
                status.sub_state,
                status.result.as_deref().unwrap_or("unknown"),
                status.restart_count
            )),
            remediation: Some(
                "Stabilize the user-session audio stack before using Orators. Orators will not rewrite WirePlumber config to recover it."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "wireplumber.service".to_string(),
            severity: Severity::Warn,
            summary: "WirePlumber health could not be inspected".to_string(),
            detail: Some(
                "systemctl --user show wireplumber.service did not return a parseable status."
                    .to_string(),
            ),
            remediation: Some(
                "Make sure WirePlumber is installed and running in the current user session."
                    .to_string(),
            ),
        },
    }
}

fn pipewire_defaults_check(defaults: Option<&PipeWireDefaults>) -> DiagnosticCheck {
    match defaults {
        Some(defaults) if defaults.output_device.is_some() && !defaults.output_is_dummy => {
            DiagnosticCheck {
                code: "pipewire.defaults".to_string(),
                severity: Severity::Info,
                summary: "PipeWire default devices were discovered".to_string(),
                detail: Some(format!(
                    "Output: {}. Input: {}.",
                    defaults.output_device.as_deref().unwrap_or("not detected"),
                    defaults.input_device.as_deref().unwrap_or("not detected")
                )),
                remediation: None,
            }
        }
        Some(defaults) => DiagnosticCheck {
            code: "pipewire.defaults".to_string(),
            severity: Severity::Error,
            summary: "PipeWire does not currently have a usable default sink".to_string(),
            detail: Some(format!(
                "Output: {}. Input: {}.",
                defaults.output_device.as_deref().unwrap_or("not detected"),
                defaults.input_device.as_deref().unwrap_or("not detected")
            )),
            remediation: Some(
                "Fix the host audio session so a real default sink exists before using Orators."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "pipewire.defaults".to_string(),
            severity: Severity::Warn,
            summary: "PipeWire defaults could not be inspected".to_string(),
            detail: Some("wpctl could not provide the current default sink/source.".to_string()),
            remediation: Some(
                "Make sure PipeWire and wpctl are installed and available in the user session."
                    .to_string(),
            ),
        },
    }
}

fn host_support_check(
    adapter: Option<&AdapterInfo>,
    defaults: Option<&PipeWireDefaults>,
    wireplumber: Option<&UserServiceStatus>,
    host_backend_installed: bool,
    backend: &MediaBackendStatus,
) -> DiagnosticCheck {
    let ready = adapter.is_some_and(adapter_supports_media)
        && defaults
            .is_some_and(|defaults| defaults.output_device.is_some() && !defaults.output_is_dummy)
        && wireplumber.is_some_and(|status| {
            status.active_state == "active"
                && status.sub_state == "running"
                && service_uses_audio_profile(status)
        })
        && host_backend_installed
        && backend.endpoints_registered;

    DiagnosticCheck {
        code: "host.media_support".to_string(),
        severity: if ready {
            Severity::Info
        } else {
            Severity::Error
        },
        summary: if ready {
            "This host is ready for the app-owned Bluetooth media backend".to_string()
        } else {
            "This host is not ready for the app-owned Bluetooth media backend".to_string()
        },
        detail: Some(
            "Orators owns the Bluetooth media endpoint itself and only depends on the host for a healthy PipeWire output path."
                .to_string(),
        ),
        remediation: (!ready).then_some(
            "Install the managed host backend with `oratorsctl install-host-backend`, make sure WirePlumber and PipeWire are healthy, and then retry pairing."
                .to_string(),
        ),
    }
}

fn owned_backend_check(backend: &MediaBackendStatus) -> DiagnosticCheck {
    match (backend.endpoints_registered, backend.last_error.as_ref()) {
        (false, _) => DiagnosticCheck {
            code: "orators.owned_backend".to_string(),
            severity: Severity::Error,
            summary: "Orators' BlueZ media endpoints are not registered".to_string(),
            detail: Some(
                "The app-owned Bluetooth backend has not finished registering its A2DP sink endpoints."
                    .to_string(),
            ),
            remediation: Some(
                "Restart oratorsd after installing the managed host backend, then retry pairing."
                    .to_string(),
            ),
        },
        (true, Some(error)) => DiagnosticCheck {
            code: "orators.owned_backend".to_string(),
            severity: Severity::Warn,
            summary: "Orators' owned Bluetooth backend recorded a transport error".to_string(),
            detail: Some(format!(
                "Active codec: {}. Transport acquired: {}. Playback connected: {}. Last error: {}.",
                media_codec_label(backend.active_codec.as_ref()),
                backend.transport_acquired,
                backend.playback_connected,
                error
            )),
            remediation: Some(
                "Disconnect the device, restart oratorsd, and retry pairing if the transport does not recover."
                    .to_string(),
            ),
        },
        (true, None) if backend.playback_connected => DiagnosticCheck {
            code: "orators.owned_backend".to_string(),
            severity: Severity::Info,
            summary: "Orators' owned Bluetooth media backend is active".to_string(),
            detail: Some(format!(
                "Active codec: {}. Transport acquired: {}. Playback connected: {}.",
                media_codec_label(backend.active_codec.as_ref()),
                backend.transport_acquired,
                backend.playback_connected
            )),
            remediation: None,
        },
        (true, None) => DiagnosticCheck {
            code: "orators.owned_backend".to_string(),
            severity: Severity::Info,
            summary: "Orators' BlueZ media endpoints are registered and waiting for a device".to_string(),
            detail: Some(format!(
                "Active codec: {}. Transport acquired: {}. Playback connected: {}.",
                media_codec_label(backend.active_codec.as_ref()),
                backend.transport_acquired,
                backend.playback_connected
            )),
            remediation: None,
        },
    }
}

fn adapter_check(info: AdapterInfo) -> DiagnosticCheck {
    let alias = info
        .alias
        .or(info.name)
        .unwrap_or_else(|| "unknown".to_string());
    let address = info.address.unwrap_or_else(|| "unknown".to_string());

    DiagnosticCheck {
        code: "bluez.adapter".to_string(),
        severity: Severity::Info,
        summary: "BlueZ adapter is ready for pairing".to_string(),
        detail: Some(format!(
            "Look for Bluetooth device '{alias}' ({address}). powered={}, discoverable={}, pairable={}, scanning={}.",
            info.powered, info.discoverable, info.pairable, info.discovering
        )),
        remediation: None,
    }
}

fn adapter_supports_media(adapter: &AdapterInfo) -> bool {
    adapter.uuids.iter().any(|uuid| {
        uuid.eq_ignore_ascii_case("0000110b-0000-1000-8000-00805f9b34fb")
            || uuid.eq_ignore_ascii_case("0000110d-0000-1000-8000-00805f9b34fb")
    })
}

fn media_codec_label(codec: Option<&MediaCodec>) -> &'static str {
    match codec {
        Some(MediaCodec::Sbc) => "sbc",
        Some(MediaCodec::Aac) => "aac",
        None => "none",
    }
}
