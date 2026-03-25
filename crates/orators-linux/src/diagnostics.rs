use anyhow::Result;
use orators_core::{DiagnosticCheck, DiagnosticsReport, Severity};

use crate::{
    audio::{BluetoothCard, BluetoothRuntimeSettings, PipeWireDefaults, WpctlAudioRuntime},
    bluez::{AdapterInfo, BluetoothCtlBluez},
    systemd::{SystemdUserRuntime, UserServiceStatus},
};

pub async fn collect_report(
    bluez: &BluetoothCtlBluez,
    audio: &WpctlAudioRuntime,
    systemd: &SystemdUserRuntime,
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
    checks.push(call_roles_check(adapter_info.as_ref()));

    let wireplumber = systemd.service_status("wireplumber.service").await.ok();
    checks.push(wireplumber_check(wireplumber.as_ref()));

    let defaults = audio.pipewire_defaults().await.ok();
    checks.push(pipewire_defaults_check(defaults.as_ref()));

    let runtime_settings = audio.bluetooth_runtime_settings().await.ok();
    checks.push(headset_autoswitch_check(runtime_settings.as_ref()));
    checks.push(device_restore_profile_check(runtime_settings.as_ref()));
    checks.push(bluetooth_persistent_storage_check(
        runtime_settings.as_ref(),
    ));

    let bluetooth_cards = audio.bluetooth_cards().await.ok();
    let active_device = bluez
        .list_devices(true)
        .await
        .ok()
        .and_then(|devices| devices.into_iter().find(|device| device.connected));
    checks.push(active_profile_check(
        active_device.as_ref().map(|device| device.address.as_str()),
        bluetooth_cards.as_deref(),
    ));

    checks.push(host_support_check(
        adapter_info.as_ref(),
        defaults.as_ref(),
        wireplumber.as_ref(),
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
            "The stock Bluetooth stack advertises Audio Sink / A2DP media support".to_string()
        } else {
            "The stock Bluetooth stack does not advertise Audio Sink / A2DP media support"
                .to_string()
        },
        detail: adapter.map(|adapter| format!("Advertised adapter UUIDs: {}.", adapter.uuids.join(", "))),
        remediation: (!supported).then_some(
            "Fix the host Bluetooth audio stack outside Orators before pairing or connecting media devices."
                .to_string(),
        ),
    }
}

fn call_roles_check(adapter: Option<&AdapterInfo>) -> DiagnosticCheck {
    let exposed = adapter.is_some_and(adapter_exposes_call_roles);
    DiagnosticCheck {
        code: "bluez.call_roles".to_string(),
        severity: Severity::Info,
        summary: if exposed {
            "The stock Bluetooth stack also advertises classic call roles".to_string()
        } else {
            "The stock Bluetooth stack is effectively media-only".to_string()
        },
        detail: Some(
            "Orators does not manage or optimize call profiles. It only pins connected audio devices back to A2DP for speaker playback."
                .to_string(),
        ),
        remediation: None,
    }
}

fn wireplumber_check(status: Option<&UserServiceStatus>) -> DiagnosticCheck {
    match status {
        Some(status) if status.active_state == "active" && status.sub_state == "running" => {
            let detail = if status.restart_count > 0 {
                format!(
                    "wireplumber.service is active and running. Restart count since boot: {}.",
                    status.restart_count
                )
            } else {
                "wireplumber.service is active and running.".to_string()
            };
            DiagnosticCheck {
                code: "wireplumber.service".to_string(),
                severity: Severity::Info,
                summary: "WirePlumber is healthy".to_string(),
                detail: Some(detail),
                remediation: None,
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

fn headset_autoswitch_check(settings: Option<&BluetoothRuntimeSettings>) -> DiagnosticCheck {
    match settings.and_then(|settings| settings.headset_autoswitch) {
        Some(false) => DiagnosticCheck {
            code: "wireplumber.bluetooth_autoswitch".to_string(),
            severity: Severity::Info,
            summary: "Bluetooth headset autoswitch is disabled at runtime".to_string(),
            detail: Some(
                "This host stays on the high-quality A2DP media profile instead of switching Bluetooth devices into headset mode when a recording stream appears."
                    .to_string(),
            ),
            remediation: None,
        },
        Some(true) => DiagnosticCheck {
            code: "wireplumber.bluetooth_autoswitch".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth headset autoswitch is enabled at runtime".to_string(),
            detail: Some(
                "On WirePlumber 0.5.x, automatic headset-profile switching can destabilize media-only Bluetooth speaker use."
                    .to_string(),
            ),
            remediation: Some(
                "If this host starts switching profiles on its own, disable the setting in WirePlumber outside Orators."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "wireplumber.bluetooth_autoswitch".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth headset autoswitch state could not be inspected".to_string(),
            detail: Some(
                "wpctl could not read the `bluetooth.autoswitch-to-headset-profile` runtime setting."
                    .to_string(),
            ),
            remediation: Some(
                "Make sure WirePlumber is healthy before using Orators with Bluetooth audio."
                    .to_string(),
            ),
        },
    }
}

fn device_restore_profile_check(settings: Option<&BluetoothRuntimeSettings>) -> DiagnosticCheck {
    match settings.and_then(|settings| settings.device_restore_profile) {
        Some(false) => DiagnosticCheck {
            code: "wireplumber.device_restore_profile".to_string(),
            severity: Severity::Info,
            summary: "Bluetooth profile restore is disabled at runtime".to_string(),
            detail: Some(
                "WirePlumber will not restore a previous headset profile for Bluetooth devices, which keeps the Fold7 on the stable media profile instead of drifting back to audio-gateway."
                    .to_string(),
            ),
            remediation: None,
        },
        Some(true) => DiagnosticCheck {
            code: "wireplumber.device_restore_profile".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth profile restore is enabled at runtime".to_string(),
            detail: Some(
                "WirePlumber may restore a previously selected headset profile on reconnect, which can drag media-only devices back into the wrong Bluetooth mode."
                    .to_string(),
            ),
            remediation: Some(
                "If Bluetooth devices keep reconnecting in the wrong profile, disable profile restore at runtime."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "wireplumber.device_restore_profile".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth profile restore state could not be inspected".to_string(),
            detail: Some(
                "wpctl could not read the `device.restore-profile` runtime setting.".to_string(),
            ),
            remediation: Some(
                "Make sure WirePlumber is healthy before using Orators with Bluetooth audio."
                    .to_string(),
            ),
        },
    }
}

fn bluetooth_persistent_storage_check(
    settings: Option<&BluetoothRuntimeSettings>,
) -> DiagnosticCheck {
    match settings.and_then(|settings| settings.bluetooth_persistent_storage) {
        Some(false) => DiagnosticCheck {
            code: "wireplumber.bluetooth_persistent_storage".to_string(),
            severity: Severity::Info,
            summary: "Bluetooth persistent storage is disabled at runtime".to_string(),
            detail: Some(
                "WirePlumber will not reuse saved Bluetooth state for reconnects, which avoids replaying stale headset selections on this host."
                    .to_string(),
            ),
            remediation: None,
        },
        Some(true) => DiagnosticCheck {
            code: "wireplumber.bluetooth_persistent_storage".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth persistent storage is enabled at runtime".to_string(),
            detail: Some(
                "Saved Bluetooth state can bring back a previously unstable profile choice when the device reconnects."
                    .to_string(),
            ),
            remediation: Some(
                "If the Bluetooth profile keeps drifting after reconnects, disable persistent storage at runtime."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "wireplumber.bluetooth_persistent_storage".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth persistent storage state could not be inspected".to_string(),
            detail: Some(
                "wpctl could not read the `bluetooth.use-persistent-storage` runtime setting."
                    .to_string(),
            ),
            remediation: Some(
                "Make sure WirePlumber is healthy before using Orators with Bluetooth audio."
                    .to_string(),
            ),
        },
    }
}

fn active_profile_check(
    active_device_address: Option<&str>,
    cards: Option<&[BluetoothCard]>,
) -> DiagnosticCheck {
    let Some(address) = active_device_address else {
        return DiagnosticCheck {
            code: "bluetooth.a2dp_pin".to_string(),
            severity: Severity::Info,
            summary: "No active Bluetooth audio device is connected".to_string(),
            detail: Some(
                "Orators will pin the active Bluetooth card to A2DP only while a device is connected."
                    .to_string(),
            ),
            remediation: None,
        };
    };

    let Some(card) = cards.and_then(|cards| {
        cards.iter().find(|card| {
            card.address
                .as_deref()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(address))
        })
    }) else {
        return DiagnosticCheck {
            code: "bluetooth.a2dp_pin".to_string(),
            severity: Severity::Warn,
            summary:
                "A Bluetooth device is connected, but PipeWire has no matching Bluetooth audio card yet"
                    .to_string(),
            detail: Some(format!("Connected device address: {address}.")),
            remediation: Some(
                "If playback does not appear within a few seconds, disconnect and reconnect the device."
                    .to_string(),
            ),
        };
    };

    match card.active_profile_name.as_deref() {
        Some("a2dp-sink") => DiagnosticCheck {
            code: "bluetooth.a2dp_pin".to_string(),
            severity: Severity::Info,
            summary: "The active Bluetooth audio card is pinned to A2DP".to_string(),
            detail: Some(format!(
                "Connected device {address} maps to PipeWire card {}.",
                card.name
            )),
            remediation: None,
        },
        Some(profile) => DiagnosticCheck {
            code: "bluetooth.a2dp_pin".to_string(),
            severity: Severity::Warn,
            summary: "The active Bluetooth audio card drifted away from A2DP".to_string(),
            detail: Some(format!(
                "Connected device {address} is currently on profile `{profile}`."
            )),
            remediation: Some(
                "Disconnect and reconnect the device if media playback does not return to A2DP on its own."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "bluetooth.a2dp_pin".to_string(),
            severity: Severity::Warn,
            summary: "The active Bluetooth audio card has no explicit profile selected".to_string(),
            detail: Some(format!(
                "Connected device {address} maps to PipeWire card {}.",
                card.name
            )),
            remediation: Some(
                "Reconnect the device if playback does not settle on A2DP within a few seconds."
                    .to_string(),
            ),
        },
    }
}

fn host_support_check(
    adapter: Option<&AdapterInfo>,
    defaults: Option<&PipeWireDefaults>,
    wireplumber: Option<&UserServiceStatus>,
) -> DiagnosticCheck {
    let ready = adapter.is_some_and(adapter_supports_media)
        && defaults
            .is_some_and(|defaults| defaults.output_device.is_some() && !defaults.output_is_dummy)
        && wireplumber
            .is_some_and(|status| status.active_state == "active" && status.sub_state == "running");

    DiagnosticCheck {
        code: "host.media_support".to_string(),
        severity: if ready {
            Severity::Info
        } else {
            Severity::Error
        },
        summary: if ready {
            "This host is ready for config-free Bluetooth speaker playback".to_string()
        } else {
            "This host is not ready for config-free Bluetooth speaker playback".to_string()
        },
        detail: Some(
            "Orators now stays out of PipeWire/WirePlumber Bluetooth ownership and only checks the host audio session read-only."
                .to_string(),
        ),
        remediation: (!ready).then_some(
            "Fix the host audio stack first. Orators will refuse pairing or connection attempts on unsupported or unhealthy hosts."
                .to_string(),
        ),
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

fn adapter_exposes_call_roles(adapter: &AdapterInfo) -> bool {
    adapter.uuids.iter().any(|uuid| {
        matches!(
            uuid.to_ascii_lowercase().as_str(),
            "00001108-0000-1000-8000-00805f9b34fb"
                | "00001112-0000-1000-8000-00805f9b34fb"
                | "0000111e-0000-1000-8000-00805f9b34fb"
                | "0000111f-0000-1000-8000-00805f9b34fb"
        )
    })
}
