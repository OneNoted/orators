# Orators

`orators` turns a Linux desktop into a Bluetooth audio target by coordinating BlueZ, PipeWire, and WirePlumber from a user-session daemon.

## Scope

- Linux-first implementation
- Rust workspace with a long-lived daemon and a CLI client
- D-Bus control API on the user session bus
- BlueZ pairing and trusted-device control
- WirePlumber configuration management for `a2dp_sink` and `hfp_ag`

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
- the daemon writes a per-user WirePlumber fragment to enable `a2dp_sink` and `hfp_ag`

