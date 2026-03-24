# Orators

`orators` turns a Linux desktop into a Bluetooth audio target by coordinating BlueZ, PipeWire, and WirePlumber from a user-session daemon.

## Scope

- Linux-first implementation
- Rust workspace with a long-lived daemon and a CLI client
- D-Bus control API on the user session bus
- BlueZ pairing and trusted-device control
- Runtime-only Bluetooth media control with no PipeWire or WirePlumber config writes

## Workspace

- `crates/orators-core`: domain types, config, diagnostics, state machine
- `crates/orators-linux`: Linux integrations for BlueZ, PipeWire, and systemd user services
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
- `oratorsd` relies on the host's stock BlueZ, PipeWire, and WirePlumber setup
- Orators does not write WirePlumber fragments, saved `wpctl` settings, or other PipeWire session config
- Supported baseline is media-only Bluetooth speaker playback
- When a Bluetooth audio card appears, Orators pins it back to A2DP at runtime and disconnects the active Bluetooth audio device if the host audio session becomes unhealthy

## Host Model

- Orators only supports the host audio stack the machine already exposes
- The host must already advertise Bluetooth media support through BlueZ
- The user session must already have a healthy WirePlumber and PipeWire setup with a real default sink
- Orators will not try to repair the desktop by writing WirePlumber or PipeWire config files

## Configuration

Example `~/.config/orators/config.toml`:

```toml
pairing_timeout_secs = 120
auto_reconnect = true
single_active_device = true
```

Legacy Bluetooth-mode fields are still ignored on load so existing configs remain readable, but Orators no longer uses or saves them.

## Operational Notes

1. Run `oratorsctl doctor` before pairing or connecting a phone.
2. If doctor reports that the host audio stack is unsupported or unhealthy, fix the host outside Orators first.
3. Orators will not restart WirePlumber or PipeWire automatically.
4. If the active Bluetooth device drifts into a non-A2DP profile or the user-session audio stack collapses, Orators disconnects only the active Bluetooth audio device and leaves the rest of the desktop stack alone.
