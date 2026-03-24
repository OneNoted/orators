# Orators

`orators` turns a Linux desktop into a Bluetooth audio target. The MVP direction is an app-owned Bluetooth media backend: BlueZ for pairing and media transport, PipeWire for local playback, and WirePlumber kept in its non-Bluetooth `audio` profile.

## Scope

- Linux-first implementation
- Rust workspace with a long-lived daemon and a CLI client
- D-Bus control API on the user session bus
- BlueZ pairing and trusted-device control
- Managed host-backend install that moves `wireplumber.service` to the official `audio` profile
- No WirePlumber fragment writes or saved `wpctl` settings

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
- `oratorsctl install-host-backend` installs the daemon unit and a reversible `wireplumber.service` override that runs `wireplumber -p audio`
- The long-term MVP path is that Orators owns Bluetooth media transport instead of sharing ownership with WirePlumber's Bluetooth monitor
- Orators does not write WirePlumber fragments or saved `wpctl` settings

## Host Model

- The user session must have a healthy PipeWire setup with a real default sink
- The stock BlueZ system service must be healthy
- For the app-owned Bluetooth path, `wireplumber.service` should be running in the official `audio` profile instead of owning Bluetooth itself
- Orators will not write ad hoc WirePlumber or PipeWire policy files to repair the desktop

## Configuration

Example `~/.config/orators/config.toml`:

```toml
pairing_timeout_secs = 120
auto_reconnect = true
single_active_device = true
```

Legacy Bluetooth-mode fields are still ignored on load so existing configs remain readable, but Orators no longer uses or saves them.

## Operational Notes

1. Run `oratorsctl install-host-backend` once to install the daemon unit and the `wireplumber -p audio` override.
2. Restart `wireplumber.service` and `oratorsd.service` only when no Bluetooth audio devices are connected.
3. Run `oratorsctl doctor` before pairing or connecting a phone.
4. If doctor reports that the host audio stack is unsupported or unhealthy, fix the host outside Orators first.
5. Orators will not write WirePlumber fragments, saved `wpctl` settings, or other PipeWire session policy files.
