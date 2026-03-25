# Orators

`orators` turns a Linux desktop into a Bluetooth audio target. The current MVP direction is a safe control-only daemon: BlueZ for pairing and trusted-device management, PipeWire for local playback, and no WirePlumber or PipeWire policy mutation.

## Scope

- Linux-first implementation
- Rust workspace with a long-lived daemon and a CLI client
- D-Bus control API on the user session bus
- BlueZ pairing and trusted-device control
- No WirePlumber fragment writes, no saved `wpctl` settings, and no session-manager overrides

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
- `oratorsctl install-user-service` installs the daemon unit
- Orators does not write WirePlumber fragments or session-manager overrides. It does reapply runtime Bluetooth stability settings in the current session so Bluetooth devices stay on A2DP instead of drifting into headset mode.

## Host Model

- The user session must have a healthy PipeWire setup with a real default sink
- The stock BlueZ system service must be healthy
- Orators leaves the desktop audio session manager alone and only reads host state, but it does clear WirePlumber runtime Bluetooth settings that can drag devices back into headset mode

## Configuration

Example `~/.config/orators/config.toml`:

```toml
pairing_timeout_secs = 120
auto_reconnect = true
single_active_device = true
```

Legacy Bluetooth-mode fields are still ignored on load so existing configs remain readable, but Orators no longer uses or saves them.

## Operational Notes

1. Run `oratorsctl install-user-service` once to install the daemon unit.
2. Run `oratorsctl doctor` before pairing or connecting a phone.
3. If doctor reports that the host audio stack is unsupported or unhealthy, fix the host outside Orators first.
4. Orators will not write WirePlumber fragments or other PipeWire session policy files, but it will keep the Bluetooth runtime settings pinned to the media-safe values for the current session.

## License

MIT. See [LICENSE](/home/notes/Projects/orators/LICENSE).
