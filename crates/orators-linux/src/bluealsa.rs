use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use orators_core::{MediaBackendKind, MediaBackendStatus, PlayerState};
use tokio::{process::Child, process::Command, sync::Mutex};

const PLAYER_RESTART_BACKOFF: Duration = Duration::from_secs(3);
const PLAYER_MAX_RESTARTS: u8 = 3;
pub const SYSTEM_BACKEND_UNIT: &str = "orators-bluealsad.service";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluealsaAssets {
    pub bluealsad: PathBuf,
    pub bluealsa_aplay: PathBuf,
    pub bluealsactl: PathBuf,
}

impl BluealsaAssets {
    pub fn discover() -> Result<Self> {
        Ok(Self {
            bluealsad: find_binary("bluealsad").context("failed to find `bluealsad` in PATH")?,
            bluealsa_aplay: find_binary("bluealsa-aplay")
                .context("failed to find `bluealsa-aplay` in PATH")?,
            bluealsactl: find_binary("bluealsactl")
                .context("failed to find `bluealsactl` in PATH")?,
        })
    }

    #[cfg(test)]
    fn discover_in_path(path: &OsStr) -> Result<Self> {
        Ok(Self {
            bluealsad: find_binary_in_path("bluealsad", path)
                .context("failed to find `bluealsad` in PATH")?,
            bluealsa_aplay: find_binary_in_path("bluealsa-aplay", path)
                .context("failed to find `bluealsa-aplay` in PATH")?,
            bluealsactl: find_binary_in_path("bluealsactl", path)
                .context("failed to find `bluealsactl` in PATH")?,
        })
    }
}

pub struct BluealsaRuntime {
    supervisor: Mutex<PlayerSupervisor>,
}

impl BluealsaRuntime {
    pub fn new() -> Self {
        Self {
            supervisor: Mutex::new(PlayerSupervisor::default()),
        }
    }

    pub async fn stop_player(&self) -> Result<()> {
        let mut supervisor = self.supervisor.lock().await;
        supervisor.stop_current().await?;
        supervisor.current_address = None;
        supervisor.player_state = PlayerState::Waiting;
        supervisor.last_error = None;
        supervisor.restart_attempts = 0;
        supervisor.next_restart_at = None;
        Ok(())
    }

    pub async fn reconcile_player(
        &self,
        assets: &BluealsaAssets,
        active_device_address: Option<&str>,
    ) -> Result<()> {
        let mut supervisor = self.supervisor.lock().await;
        supervisor.reap_current().await?;

        match active_device_address {
            None => {
                supervisor.stop_current().await?;
                supervisor.current_address = None;
                supervisor.player_state = PlayerState::Waiting;
                supervisor.last_error = None;
                supervisor.restart_attempts = 0;
                supervisor.next_restart_at = None;
                return Ok(());
            }
            Some(address) => {
                if supervisor.current_address.as_deref() != Some(address) {
                    supervisor.stop_current().await?;
                    supervisor.current_address = Some(address.to_string());
                    supervisor.player_state = PlayerState::Waiting;
                    supervisor.restart_attempts = 0;
                    supervisor.next_restart_at = None;
                }
            }
        }

        if supervisor.current.is_some() {
            supervisor.player_state = PlayerState::Playing;
            return Ok(());
        }

        if supervisor
            .next_restart_at
            .is_some_and(|deadline| Instant::now() < deadline)
        {
            if supervisor.last_error.is_some() {
                supervisor.player_state = PlayerState::Error;
            }
            return Ok(());
        }

        let address = supervisor
            .current_address
            .clone()
            .context("missing active device address for BlueALSA playback")?;

        let child = Command::new(&assets.bluealsa_aplay)
            .arg(&address)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| {
                format!(
                    "failed to start `{} {address}`",
                    assets.bluealsa_aplay.display()
                )
            });

        match child {
            Ok(child) => {
                supervisor.current = Some(ManagedPlayer { child });
                supervisor.player_state = PlayerState::Starting;
                supervisor.last_error = None;
                supervisor.next_restart_at = None;
            }
            Err(error) => {
                supervisor.current = None;
                supervisor.player_state = PlayerState::Error;
                supervisor.last_error = Some(error.to_string());
                supervisor.restart_attempts = supervisor.restart_attempts.saturating_add(1);
                if supervisor.restart_attempts >= PLAYER_MAX_RESTARTS {
                    supervisor.next_restart_at = Some(Instant::now() + Duration::from_secs(30));
                } else {
                    supervisor.next_restart_at = Some(Instant::now() + PLAYER_RESTART_BACKOFF);
                }
            }
        }

        Ok(())
    }

    pub async fn backend_status(
        &self,
        installed: bool,
        system_service_ready: bool,
    ) -> MediaBackendStatus {
        let mut supervisor = self.supervisor.lock().await;
        let _ = supervisor.reap_current().await;
        MediaBackendStatus {
            backend: MediaBackendKind::Bluealsa,
            installed,
            system_service_ready,
            player_state: supervisor.player_state.clone(),
            player_running: supervisor.current.is_some(),
            active_device_address: supervisor.current_address.clone(),
            last_error: supervisor.last_error.clone(),
        }
    }
}

impl Default for BluealsaRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct PlayerSupervisor {
    current: Option<ManagedPlayer>,
    current_address: Option<String>,
    player_state: PlayerState,
    last_error: Option<String>,
    restart_attempts: u8,
    next_restart_at: Option<Instant>,
}

struct ManagedPlayer {
    child: Child,
}

impl PlayerSupervisor {
    async fn reap_current(&mut self) -> Result<()> {
        let Some(player) = self.current.as_mut() else {
            return Ok(());
        };

        match player.child.try_wait() {
            Ok(None) => {
                if self.player_state == PlayerState::Starting {
                    self.player_state = PlayerState::Playing;
                }
            }
            Ok(Some(status)) => {
                self.current = None;
                self.player_state = PlayerState::Error;
                self.last_error = Some(format!("bluealsa-aplay exited with status {status}"));
                self.restart_attempts = self.restart_attempts.saturating_add(1);
                self.next_restart_at = Some(Instant::now() + PLAYER_RESTART_BACKOFF);
            }
            Err(error) => {
                self.current = None;
                self.player_state = PlayerState::Error;
                self.last_error = Some(format!("failed to inspect bluealsa-aplay: {error}"));
                self.restart_attempts = self.restart_attempts.saturating_add(1);
                self.next_restart_at = Some(Instant::now() + PLAYER_RESTART_BACKOFF);
            }
        }

        Ok(())
    }

    async fn stop_current(&mut self) -> Result<()> {
        let Some(mut player) = self.current.take() else {
            return Ok(());
        };

        if player.child.try_wait()?.is_none() {
            player
                .child
                .kill()
                .await
                .context("failed to stop bluealsa-aplay")?;
            let _ = player.child.wait().await;
        }

        Ok(())
    }
}

fn find_binary(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    find_binary_in_path(name, &path)
}

fn find_binary_in_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .is_some_and(|name| name != OsStr::new("") && std::fs::metadata(path).is_ok())
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use tempfile::tempdir;

    use super::{BluealsaAssets, find_binary_in_path};

    #[test]
    fn finds_executable_in_path() {
        let dir = tempdir().unwrap();
        let binary = dir.path().join("bluealsad");
        fs::write(&binary, "#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).unwrap();

        let found = find_binary_in_path("bluealsad", dir.path().as_os_str());

        assert_eq!(found.as_deref(), Some(binary.as_path()));
    }

    #[test]
    fn bluealsa_assets_require_all_binaries() {
        let dir = tempdir().unwrap();
        for name in ["bluealsad", "bluealsa-aplay", "bluealsactl"] {
            let binary = dir.path().join(name);
            fs::write(&binary, "#!/bin/sh\n").unwrap();
            let mut permissions = fs::metadata(&binary).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary, permissions).unwrap();
        }

        let assets = BluealsaAssets::discover_in_path(dir.path().as_os_str()).unwrap();

        assert!(assets.bluealsad.ends_with("bluealsad"));
        assert!(assets.bluealsa_aplay.ends_with("bluealsa-aplay"));
        assert!(assets.bluealsactl.ends_with("bluealsactl"));
    }
}
