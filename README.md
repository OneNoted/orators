# Orators

`orators` turns a Linux desktop into a Bluetooth audio target by coordinating BlueZ, PipeWire, and WirePlumber from a user-session daemon.

## Scope

- Linux-first implementation
- Rust workspace with a long-lived daemon and a CLI client
- D-Bus control API on the user session bus
- BlueZ pairing and trusted-device control
- WirePlumber configuration management for A2DP media playback, explicit classic call mode, and an experimental LE Audio track

## Workspace

- `crates/orators-core`: domain types, config, diagnostics, state machine
- `crates/orators-linux`: Linux integrations for BlueZ, WirePlumber, PipeWire, and systemd user services
- `crates/orators`: publishable crate with `oratorsd` and `oratorsctl`

## Local Development

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Nix

```bash
nix develop
nix flake check
```

## Service Model

- `oratorsd` runs under `systemd --user`
- `oratorsctl` talks to the daemon over the session bus
- the daemon writes a per-user WirePlumber fragment under `~/.config/wireplumber/wireplumber.conf.d/`
- `classic_media` is the default mode and keeps the host in speaker-style `a2dp_sink` playback with headset autoswitch disabled
- `classic_call_compat` is an explicit opt-in compatibility mode that also exposes headset-side `hsp_hs` / `hfp_hf` roles and allows autoswitch into lower-fidelity bidirectional call audio
- `le_audio_call` requests `bap_sink` / `bap_source` while keeping A2DP fallback enabled, and is the only mode intended for first-class Bluetooth calls on devices that advertise modern LE Audio capability

## Bluetooth Modes

- `classic_media`
- Best speaker quality.
- A2DP only.
- No Bluetooth mic/call route is exposed to the phone.

- `classic_call_compat`
- Classic Bluetooth bidirectional call path.
- Starts in A2DP, but WirePlumber is allowed to autoswitch into headset mode when a voice app starts recording.
- Lower fidelity during calls is expected.

- `le_audio_call`
- Premium-call mode for newer Bluetooth stacks.
- Requests BAP roles and keeps A2DP fallback for media.
- Orators should only treat calls as first-class when both the Linux host and the connected phone advertise LE Audio capability.

## Configuration

Example `~/.config/orators/config.toml`:

```toml
pairing_timeout_secs = 120
auto_reconnect = true
single_active_device = true
bluetooth_mode = "classic_media"
wireplumber_fragment_name = "90-orators-bluetooth.conf"
```

To switch modes:

1. Edit `bluetooth_mode` in `~/.config/orators/config.toml`
2. Run `oratorsctl doctor --apply`
3. Disconnect any active Bluetooth devices
4. Restart `wireplumber.service` and `oratorsd.service`
5. Reconnect or re-pair the phone if the visible Bluetooth services changed

Legacy config values still load:

- `classic_call` maps to `classic_call_compat`
- `experimental_le_audio` maps to `le_audio_call`
