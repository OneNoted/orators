# Orators

`orators` turns a Linux desktop into a Bluetooth speaker. The primary interface is now a Ratatui dashboard that manages pairing, trusted/allowed devices, setup, and backend health. `oratorsctl` remains available for advanced and scriptable control.

## Scope

- Linux-first implementation
- Rust workspace with a long-lived daemon, a Ratatui app, and an advanced CLI client
- D-Bus control API on the user session bus
- BlueZ pairing and trusted-device control
- Media-only MVP: A2DP sink playback, no call or mic support
- Managed host install: one BlueALSA system service plus one Bluetooth-only WirePlumber fragment

## Workspace

- `crates/orators-core`: domain types, config, diagnostics, state machine
- `crates/orators-linux`: Linux integrations for BlueZ, BlueALSA, local audio inspection, and systemd
- `crates/orators`: publishable crate with:
  - `orators` for the TUI
  - `oratorsd` for the daemon
  - `oratorsctl` for advanced/compat CLI usage

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

- `orators` opens the TUI
- `oratorsd` runs under `systemd --user`
- `oratorsctl` talks to the daemon over the session bus for pairing and device control
- `oratorsctl install-user-service` installs only the user daemon unit
- `oratorsctl install-system-backend` installs the supported media backend:
  - root `orators-bluealsad.service`
  - user `~/.config/wireplumber/wireplumber.conf.d/90-orators-disable-bluez.conf`
- `oratorsctl uninstall-system-backend` removes that backend and restores stock WirePlumber Bluetooth ownership

## Host Model

- The system must provide trusted BlueALSA binaries in the expected system locations:
  - `bluealsad`
  - `bluealsa-aplay`
  - `bluealsactl`
- The host must have a usable local playback output
- WirePlumber stays responsible for the rest of the desktop audio graph, but not Bluetooth media ownership

## Configuration

Example `~/.config/orators/config.toml`:

```toml
pairing_timeout_secs = 120
auto_reconnect = true
single_active_device = true
adapter = "hci1"
allowed_devices = []

[device_aliases]
"5C:DC:49:92:D0:D8" = "Fold 7"
```

- `adapter` is optional. If omitted, Orators auto-selects the only powered adapter. If multiple powered adapters exist, the managed backend install requires `--adapter hciX`.
- `device_aliases` are local-only display names. Orators never writes them back into BlueZ.
- Legacy Bluetooth-mode fields are still ignored on load so existing configs remain readable.

## TUI

Run:

```bash
orators
```

Key workflows:

- `Tab` / `Shift-Tab`: switch views
- `q`: quit
- Dashboard:
  - `p` toggle pairing
  - `i` install/repair backend
  - `u` uninstall backend
- Devices:
  - `j` / `k` move
  - `a` allow/disallow
  - `t` trust/untrust
  - `c` connect/disconnect
  - `f` forget
  - `x` reset on host
  - `n` set local alias
  - `N` clear local alias
- Settings:
  - `Enter` edits pairing timeout or adapter
  - `Enter` / `Space` toggles boolean settings

## Advanced CLI

Examples:

```bash
oratorsctl doctor
oratorsctl pair start --timeout 120
oratorsctl devices list
oratorsctl devices allow 5C:DC:49:92:D0:D8
oratorsctl devices alias 5C:DC:49:92:D0:D8 "Fold 7"
oratorsctl config show
oratorsctl config set pairing-timeout 180
```

## Operational Notes

1. Install the daemon unit once with `oratorsctl install-user-service`.
2. Install the managed media backend once with `oratorsctl install-system-backend [--adapter hciX]`.
3. Run `orators` for the normal control flow.
4. Pair a new phone from the Pairing view, then allow it from the Devices view if you want stable reconnect behavior.
5. Disconnect Bluetooth audio devices before uninstalling or changing the managed backend.

## License

MIT. See [LICENSE](/home/notes/Projects/orators/LICENSE).
