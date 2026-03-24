use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use orators_core::{BluetoothMode, DiagnosticCheck, DiagnosticsReport, OratorsConfig, Severity};

use crate::{
    audio::{BluetoothAudioSettings, WpctlAudioRuntime},
    bluez::{AdapterInfo, BluetoothCtlBluez, RemoteDeviceInfo},
    wireplumber::{WirePlumberRoles, WirePlumberRuntime},
};

const TMAP_UUID: &str = "00001855-0000-1000-8000-00805f9b34fb";
const LOCAL_LE_AUDIO_MARKER_ROOTS: &[&str] = &["/usr/share/wireplumber", "/usr/share/pipewire"];

#[derive(Debug, Clone)]
struct LeAudioSupport {
    local_capable: bool,
    remote_capable: bool,
    remote_device_label: Option<String>,
}

pub async fn collect_report(
    bluez: &BluetoothCtlBluez,
    audio: &WpctlAudioRuntime,
    wireplumber: &WirePlumberRuntime,
    fragment_path: &Path,
    config: &OratorsConfig,
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

    checks.push(mode_check(config.bluetooth_mode));

    checks.push(
        match wireplumber.inspect_fragment(fragment_path, config).await {
            Ok(report) if report.changed => DiagnosticCheck {
                code: "wireplumber.fragment".to_string(),
                severity: Severity::Warn,
                summary: "WirePlumber fragment does not match the configured bluetooth mode"
                    .to_string(),
                detail: Some(format!(
                    "Fragment path: {}. Expected mode: {}. Expected roles: {}.",
                    report.path,
                    config.bluetooth_mode.label(),
                    expected_roles_label(config.bluetooth_mode)
                )),
                remediation: Some(
                    "Run `oratorsctl doctor --apply` and restart WirePlumber if needed."
                        .to_string(),
                ),
            },
            Ok(report) => DiagnosticCheck {
                code: "wireplumber.fragment".to_string(),
                severity: Severity::Info,
                summary: "WirePlumber fragment matches the configured bluetooth mode".to_string(),
                detail: Some(format!(
                    "Fragment path: {}. Expected mode: {}. Expected roles: {}.",
                    report.path,
                    config.bluetooth_mode.label(),
                    expected_roles_label(config.bluetooth_mode)
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
    checks.push(policy_check(
        config.bluetooth_mode,
        roles.as_ref(),
        fragment_path,
    ));

    let bluetooth_settings = audio.bluetooth_settings().await.ok();
    checks.push(autoswitch_check(
        config.bluetooth_mode,
        bluetooth_settings,
        roles.as_ref(),
    ));

    checks.push(le_audio_local_check(
        config.bluetooth_mode,
        adapter_info.as_ref(),
        roles.as_ref(),
    ));

    let remote_devices = bluez.remote_devices().await.ok();
    checks.push(le_audio_remote_check(
        config.bluetooth_mode,
        remote_devices.as_deref(),
    ));

    let le_audio_support = assess_le_audio_support(
        adapter_info.as_ref(),
        roles.as_ref(),
        remote_devices.as_deref(),
    );
    checks.push(call_experience_check(
        config.bluetooth_mode,
        &le_audio_support,
    ));

    let defaults = audio
        .current_defaults(
            roles.as_ref().is_some_and(|roles| roles.a2dp_sink_enabled),
            roles
                .as_ref()
                .is_some_and(|roles| roles.classic_call_enabled),
            roles.as_ref().is_some_and(|roles| roles.le_audio_enabled),
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

fn mode_check(mode: BluetoothMode) -> DiagnosticCheck {
    let (summary, detail) = match mode {
        BluetoothMode::ClassicMedia => (
            "Bluetooth mode is classic_media",
            "Speaker-first mode. Orators exposes A2DP playback only and disables headset autoswitch so media stays in the high-quality speaker path.",
        ),
        BluetoothMode::ClassicCallCompat => (
            "Bluetooth mode is classic_call_compat",
            "Classic Bluetooth compatibility-call mode. Orators still prefers A2DP first, but it also exposes headset-side HSP/HFP roles and allows headset autoswitch for bidirectional call audio with a quality tradeoff.",
        ),
        BluetoothMode::LeAudioCall => (
            "Bluetooth mode is le_audio_call",
            "LE Audio call mode. Orators requests BAP roles for newer Bluetooth stacks and keeps A2DP fallback enabled for media, but premium call behavior depends on both the Linux host and the remote device advertising LE Audio capability.",
        ),
    };

    DiagnosticCheck {
        code: "bluetooth.mode".to_string(),
        severity: Severity::Info,
        summary: summary.to_string(),
        detail: Some(detail.to_string()),
        remediation: None,
    }
}

fn policy_check(
    mode: BluetoothMode,
    roles: Option<&WirePlumberRoles>,
    fragment_path: &Path,
) -> DiagnosticCheck {
    match roles {
        Some(roles)
            if roles.a2dp_sink_enabled
                && roles.classic_call_enabled == mode.classic_call_compat_enabled()
                && roles.le_audio_enabled == mode.le_audio_call_enabled() =>
        {
            DiagnosticCheck {
                code: "bluetooth.policy".to_string(),
                severity: Severity::Info,
                summary: "WirePlumber Bluetooth roles match the configured mode".to_string(),
                detail: Some(format!(
                    "Mode: {}. Observed roles: {}.",
                    mode.label(),
                    observed_roles_label(roles)
                )),
                remediation: None,
            }
        }
        Some(roles) => DiagnosticCheck {
            code: "bluetooth.policy".to_string(),
            severity: Severity::Warn,
            summary: "WirePlumber Bluetooth roles do not match the configured mode"
                .to_string(),
            detail: Some(format!(
                "Mode: {}. Expected roles: {}. Observed roles: {}.",
                mode.label(),
                expected_roles_label(mode),
                observed_roles_label(roles)
            )),
            remediation: Some(format!(
                "Run `oratorsctl doctor --apply` to rewrite {}.",
                fragment_path.display()
            )),
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
    }
}

fn autoswitch_check(
    mode: BluetoothMode,
    settings: Option<BluetoothAudioSettings>,
    roles: Option<&WirePlumberRoles>,
) -> DiagnosticCheck {
    let expected = mode.headset_autoswitch_enabled();

    let observed = settings
        .and_then(|settings| settings.autoswitch_to_headset_profile)
        .or_else(|| roles.and_then(|roles| roles.autoswitch_to_headset_profile));

    match observed {
        Some(value) if value == expected => DiagnosticCheck {
            code: "bluetooth.autoswitch".to_string(),
            severity: Severity::Info,
            summary: "Bluetooth headset autoswitch matches the configured mode".to_string(),
            detail: Some(format!(
                "Mode: {}. bluetooth.autoswitch-to-headset-profile={value}.",
                mode.label(),
            )),
            remediation: None,
        },
        Some(value) => DiagnosticCheck {
            code: "bluetooth.autoswitch".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth headset autoswitch does not match the configured mode"
                .to_string(),
            detail: Some(format!(
                "Mode: {} expects bluetooth.autoswitch-to-headset-profile={expected}, but the live setting is {value}.",
                mode.label(),
            )),
            remediation: Some(
                "Run `oratorsctl doctor --apply` and restart WirePlumber or the user session."
                    .to_string(),
            ),
        },
        None => DiagnosticCheck {
            code: "bluetooth.autoswitch".to_string(),
            severity: Severity::Warn,
            summary: "Bluetooth headset autoswitch could not be inspected".to_string(),
            detail: Some(
                "Neither `wpctl settings bluetooth.autoswitch-to-headset-profile` nor the fragment content provided a clear value."
                    .to_string(),
            ),
            remediation: Some(
                "Make sure WirePlumber and wpctl are installed, then rerun `oratorsctl doctor`."
                    .to_string(),
            ),
        },
    }
}

fn le_audio_local_check(
    mode: BluetoothMode,
    adapter_info: Option<&AdapterInfo>,
    roles: Option<&WirePlumberRoles>,
) -> DiagnosticCheck {
    let bap_marker_paths = detect_local_bap_role_markers();
    let fragment_requests_bap = roles.is_some_and(|roles| roles.le_audio_enabled);
    let adapter_hints = adapter_info
        .is_some_and(|adapter| adapter.uuids.iter().any(|uuid| is_le_audio_hint_uuid(uuid)));

    let detail = format!(
        "Fragment requests LE Audio roles: {}. Installed BAP role markers: {}. Adapter LE Audio hint UUIDs: {}.",
        yes_no(fragment_requests_bap),
        if bap_marker_paths.is_empty() {
            "none".to_string()
        } else {
            paths_label(&bap_marker_paths)
        },
        if adapter_hints {
            "present"
        } else {
            "not detected"
        }
    );

    if !bap_marker_paths.is_empty() || adapter_hints {
        DiagnosticCheck {
            code: "bluetooth.le_audio.local".to_string(),
            severity: Severity::Info,
            summary: "Local stack shows some LE Audio/BAP capability hints".to_string(),
            detail: Some(detail),
            remediation: None,
        }
    } else {
        DiagnosticCheck {
            code: "bluetooth.le_audio.local".to_string(),
            severity: if mode.le_audio_call_enabled() {
                Severity::Warn
            } else {
                Severity::Info
            },
            summary: "Local LE Audio/BAP capability could not be confirmed".to_string(),
            detail: Some(detail),
            remediation: mode.le_audio_call_enabled().then_some(
                "Stay on `classic_media` unless the host stack is upgraded to a PipeWire/WirePlumber/BlueZ combination that clearly exposes LE Audio roles."
                    .to_string(),
            ),
        }
    }
}

fn le_audio_remote_check(
    mode: BluetoothMode,
    remote_devices: Option<&[RemoteDeviceInfo]>,
) -> DiagnosticCheck {
    let Some(device) = remote_devices.and_then(select_remote_device_for_diagnostics) else {
        return DiagnosticCheck {
            code: "bluetooth.le_audio.remote".to_string(),
            severity: Severity::Info,
            summary: "No paired Bluetooth device is available for LE Audio hint inspection"
                .to_string(),
            detail: Some(
                "Pair or reconnect a phone, then rerun `oratorsctl doctor` to inspect remote-device service UUIDs."
                    .to_string(),
            ),
            remediation: None,
        };
    };

    let has_hint = device.uuids.iter().any(|uuid| is_le_audio_hint_uuid(uuid));
    if has_hint {
        DiagnosticCheck {
            code: "bluetooth.le_audio.remote".to_string(),
            severity: Severity::Info,
            summary: "Remote device advertises LE Audio-related UUID hints".to_string(),
            detail: Some(format!(
                "{} [{}] advertises Telephony and Media Audio (TMAP) or another LE Audio-related UUID.",
                device.alias.as_deref().unwrap_or("unnamed"),
                device.address
            )),
            remediation: None,
        }
    } else {
        DiagnosticCheck {
            code: "bluetooth.le_audio.remote".to_string(),
            severity: if mode.le_audio_call_enabled() {
                Severity::Warn
            } else {
                Severity::Info
            },
            summary: "Remote device does not advertise obvious LE Audio service hints"
                .to_string(),
            detail: Some(format!(
                "{} [{}] does not advertise TMAP in BlueZ-visible UUIDs, so LE Audio remains unconfirmed.",
                device.alias.as_deref().unwrap_or("unnamed"),
                device.address
            )),
            remediation: mode.le_audio_call_enabled().then_some(
                "Re-pair with a phone that advertises LE Audio services, or fall back to `classic_media`."
                    .to_string(),
            ),
        }
    }
}

fn call_experience_check(mode: BluetoothMode, support: &LeAudioSupport) -> DiagnosticCheck {
    match mode {
        BluetoothMode::ClassicMedia => DiagnosticCheck {
            code: "bluetooth.call_experience".to_string(),
            severity: Severity::Info,
            summary: "Bluetooth calls are intentionally disabled in speaker-first mode".to_string(),
            detail: Some(
                "This mode keeps the device on the high-quality A2DP speaker path. Discord and other call apps should stay on the phone unless you explicitly opt into another mode."
                    .to_string(),
            ),
            remediation: None,
        },
        BluetoothMode::ClassicCallCompat => DiagnosticCheck {
            code: "bluetooth.call_experience".to_string(),
            severity: Severity::Warn,
            summary: "Classic Bluetooth calls are compatibility-only".to_string(),
            detail: Some(
                "This mode exposes HSP/HFP so calls can work on older devices, but quality will drop when the microphone is active."
                    .to_string(),
            ),
            remediation: Some(
                "Use `bluetooth_mode = \"le_audio_call\"` only on devices that advertise LE Audio support if you want the premium call path."
                    .to_string(),
            ),
        },
        BluetoothMode::LeAudioCall if support.local_capable && support.remote_capable => {
            DiagnosticCheck {
                code: "bluetooth.call_experience".to_string(),
                severity: Severity::Info,
                summary: "Premium LE Audio call path is eligible on the current device pair"
                    .to_string(),
                detail: Some(format!(
                    "The local Linux stack exposes LE Audio hints and {} advertises LE Audio-related services. This is the only mode where Orators treats Bluetooth calls as first-class.",
                    support
                        .remote_device_label
                        .as_deref()
                        .unwrap_or("the remote device")
                )),
                remediation: None,
            }
        }
        BluetoothMode::LeAudioCall => DiagnosticCheck {
            code: "bluetooth.call_experience".to_string(),
            severity: Severity::Warn,
            summary: "Premium LE Audio calls are not yet eligible on the current device pair"
                .to_string(),
            detail: Some(format!(
                "Local LE Audio capable: {}. Remote LE Audio capable: {}. {}",
                yes_no(support.local_capable),
                yes_no(support.remote_capable),
                support
                    .remote_device_label
                    .as_deref()
                    .map(|label| format!("{label} does not currently qualify for the premium call path."))
                    .unwrap_or_else(|| "No paired phone currently qualifies for the premium call path.".to_string())
            )),
            remediation: Some(
                "Keep using `classic_media` for speaker use, or switch to `classic_call_compat` only if you explicitly want the lower-quality compatibility call path."
                    .to_string(),
            ),
        },
    }
}

fn assess_le_audio_support(
    adapter_info: Option<&AdapterInfo>,
    roles: Option<&WirePlumberRoles>,
    remote_devices: Option<&[RemoteDeviceInfo]>,
) -> LeAudioSupport {
    let local_marker_paths = detect_local_bap_role_markers();
    let local_capable = roles.is_some_and(|roles| roles.le_audio_enabled)
        && (!local_marker_paths.is_empty()
            || adapter_info.is_some_and(|adapter| {
                adapter.uuids.iter().any(|uuid| is_le_audio_hint_uuid(uuid))
            }));
    let remote = remote_devices.and_then(select_remote_device_for_diagnostics);
    let remote_capable =
        remote.is_some_and(|device| device.uuids.iter().any(|uuid| is_le_audio_hint_uuid(uuid)));

    LeAudioSupport {
        local_capable,
        remote_capable,
        remote_device_label: remote.map(|device| {
            format!(
                "{} [{}]",
                device.alias.as_deref().unwrap_or("unnamed"),
                device.address
            )
        }),
    }
}

fn detect_local_bap_role_markers() -> Vec<PathBuf> {
    let mut matches = Vec::new();
    for root in LOCAL_LE_AUDIO_MARKER_ROOTS {
        collect_bap_role_markers(Path::new(root), &mut matches, 4);
        if matches.len() >= 4 {
            break;
        }
    }
    matches
}

fn collect_bap_role_markers(path: &Path, matches: &mut Vec<PathBuf>, limit: usize) {
    if matches.len() >= limit {
        return;
    }

    let Ok(metadata) = fs::metadata(path) else {
        return;
    };

    if metadata.is_dir() {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };

        for entry in entries.flatten() {
            collect_bap_role_markers(&entry.path(), matches, limit);
            if matches.len() >= limit {
                return;
            }
        }
        return;
    }

    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    if contents.contains("bap_sink") || contents.contains("bap_source") {
        matches.push(path.to_path_buf());
    }
}

fn select_remote_device_for_diagnostics(devices: &[RemoteDeviceInfo]) -> Option<&RemoteDeviceInfo> {
    devices
        .iter()
        .find(|device| device.connected)
        .or_else(|| devices.iter().find(|device| device.paired))
}

fn is_le_audio_hint_uuid(uuid: &str) -> bool {
    uuid.eq_ignore_ascii_case(TMAP_UUID)
}

fn expected_roles_label(mode: BluetoothMode) -> &'static str {
    match mode {
        BluetoothMode::ClassicMedia => "a2dp_sink",
        BluetoothMode::ClassicCallCompat => "a2dp_sink, hsp_hs, hfp_hf",
        BluetoothMode::LeAudioCall => "a2dp_sink, bap_sink, bap_source",
    }
}

fn observed_roles_label(roles: &WirePlumberRoles) -> String {
    let mut labels = Vec::new();
    if roles.a2dp_sink_enabled {
        labels.push("a2dp_sink");
    }
    if roles.classic_call_enabled {
        labels.push("classic_call_compat");
    }
    if roles.le_audio_enabled {
        labels.push("le_audio_call");
    }

    if labels.is_empty() {
        "none".to_string()
    } else {
        labels.join(", ")
    }
}

fn paths_label(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
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
        "Look for Bluetooth device '{visible_name}' ({address}). powered={}, discoverable={}, pairable={}, scanning={}, service_uuids={}.",
        yes_no(info.powered),
        yes_no(info.discoverable),
        yes_no(info.pairable),
        yes_no(info.discovering),
        info.uuids.len(),
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
