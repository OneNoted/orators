# Orators

`orators` turns a Linux desktop into a Bluetooth speaker. The current MVP direction is a managed BlueALSA backend for Bluetooth media, plus a user daemon for pairing, trust, allowlist, reconnect policy, and diagnostics.

## Scope

- Linux-first implementation
- Rust workspace with a long-lived daemon and a CLI client
- D-Bus control API on the user session bus
- BlueZ pairing and trusted-device control
- Media-only MVP: A2DP sink playback, no call or mic support
- Managed host install: one BlueALSA system service plus one Bluetooth-only WirePlumber fragment

## Workspace

- `crates/orators-core`: domain types, config, diagnostics, state machine
- `crates/orators-linux`: Linux integrations for BlueZ, BlueALSA, local audio inspection, and systemd
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
- `oratorsctl` talks to the daemon over the session bus for pairing and device control
- `oratorsctl install-user-service` installs only the user daemon unit
- `oratorsctl install-system-backend` installs the supported media backend:
  - root `orators-bluealsad.service`
  - user `~/.config/wireplumber/wireplumber.conf.d/90-orators-disable-bluez.conf`
- `oratorsctl uninstall-system-backend` removes that backend and restores stock WirePlumber Bluetooth ownership

## Host Model

- The system must provide `bluez-alsa` binaries:
  - `bluealsad`
  - `bluealsa-aplay`
  - `bluealsactl`
- The host must have a usable ALSA `default` playback target
- WirePlumber stays responsible for the rest of the desktop audio graph, but not Bluetooth media ownership

## Configuration

Example `~/.config/orators/config.toml`:

```toml
pairing_timeout_secs = 120
auto_reconnect = true
single_active_device = true
adapter = "hci1"
allowed_devices = []
```

`adapter` is optional. If omitted, Orators auto-selects the only powered adapter. If multiple powered adapters exist, the managed backend install requires `--adapter hciX`.

Legacy Bluetooth-mode fields are still ignored on load so existing configs remain readable.

## Operational Notes

1. Install the daemon unit once with `oratorsctl install-user-service`.
2. Install the managed media backend once with `oratorsctl install-system-backend [--adapter hciX]`.
3. Run `oratorsctl doctor` before pairing or connecting a phone.
4. Pair a new phone with `oratorsctl pair start --timeout 120`, then add it to the allowlist with `oratorsctl devices allow <MAC>` if you want stable reconnect behavior.
5. Disconnect Bluetooth audio devices before uninstalling or changing the managed backend.

## License

MIT. See [LICENSE](/home/notes/Projects/orators/LICENSE).
