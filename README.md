# Orators

`orators` turns a Linux desktop into a Bluetooth audio target. The current MVP direction is an app-owned Bluetooth media backend: BlueZ for pairing and trusted-device management, an Orators-managed A2DP sink endpoint, and a managed host install that keeps the desktop audio session on the normal non-Bluetooth profile.

## Scope

- Linux-first implementation
- Rust workspace with a long-lived daemon and a CLI client
- D-Bus control API on the user session bus
- BlueZ pairing and trusted-device control
- Managed host backend install for the user-session WirePlumber audio profile
- No ad hoc PipeWire config writes or runtime `wpctl` Bluetooth hacks

## Workspace

- `crates/orators-core`: domain types, config, diagnostics, state machine
- `crates/orators-linux`: Linux integrations for BlueZ, PipeWire, the app-owned Bluetooth backend, and systemd user services
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
- `oratorsctl install-user-service` installs the daemon unit only
- `oratorsctl install-host-backend` installs the daemon unit and the managed host backend drop-in
- `oratorsctl uninstall-host-backend` removes the managed host backend drop-in

## Host Model

- The user session must have a healthy PipeWire setup with a real default sink
- The stock BlueZ system service must be healthy
- The managed host backend must be installed before pairing or connecting Bluetooth media devices

## Configuration

Example `~/.config/orators/config.toml`:

```toml
pairing_timeout_secs = 120
auto_reconnect = true
single_active_device = true
```

Legacy Bluetooth-mode fields are still ignored on load so existing configs remain readable, but Orators no longer uses or saves them.

## Operational Notes

1. Run `oratorsctl install-host-backend` once to install the daemon unit and managed host backend.
2. Restart `wireplumber.service` and `oratorsd.service` after installing the host backend.
3. Run `oratorsctl doctor` before pairing or connecting a phone.
4. If doctor reports that the host audio stack is unsupported or unhealthy, fix the host outside Orators first.
5. Use `oratorsctl uninstall-host-backend` to remove the managed host backend and restore the stock session-manager behavior.

## License

MIT. See [LICENSE](/home/notes/Projects/orators/LICENSE).
