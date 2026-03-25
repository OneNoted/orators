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
const PACKAGED_BLUEALSA_DIRS: &[&str] =
    &["/usr/libexec/orators/bluealsa", "/usr/lib/orators/bluealsa"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluealsaAssets {
    pub bluealsad: PathBuf,
    pub bluealsa_aplay: PathBuf,
    pub bluealsactl: PathBuf,
}

impl BluealsaAssets {
    pub fn discover() -> Result<Self> {
        if let Some(dir) = env::var_os("ORATORS_BLUEALSA_DIR") {
            return Self::discover_in_dir(Path::new(&dir)).with_context(|| {
                format!(
                    "failed to discover BlueALSA assets in {}",
                    Path::new(&dir).display()
                )
            });
        }

        match Self::discover_in_packaged_dirs(PACKAGED_BLUEALSA_DIRS.iter().map(Path::new)) {
            Ok(assets) => Ok(assets),
            Err(packaged_error) => {
                let path = env::var_os("PATH").context("failed to find BlueALSA assets in PATH")?;
                Self::discover_in_path(&path).map_err(|path_error| {
                    anyhow::anyhow!("{packaged_error}; {path_error}")
                })
            }
        }
    }

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

    fn discover_in_dir(dir: &Path) -> Result<Self> {
        let assets = Self {
            bluealsad: dir.join("bluealsad"),
            bluealsa_aplay: dir.join("bluealsa-aplay"),
            bluealsactl: dir.join("bluealsactl"),
        };
        if is_executable(&assets.bluealsad)
            && is_executable(&assets.bluealsa_aplay)
            && is_executable(&assets.bluealsactl)
        {
            Ok(assets)
        } else {
            anyhow::bail!("one or more BlueALSA binaries are missing or not executable");
        }
    }

    fn discover_in_packaged_dirs<'a>(
        dirs: impl IntoIterator<Item = &'a Path>,
    ) -> Result<Self> {
        let mut packaged_error = None;
        for dir in dirs {
            if dir.exists() {
                match Self::discover_in_dir(dir) {
                    Ok(assets) => return Ok(assets),
                    Err(error) => {
                        packaged_error = Some(anyhow::anyhow!(
                            "failed to discover packaged BlueALSA assets in {}: {error}",
                            dir.display()
                        ));
                    }
                }
            }
        }

        match packaged_error {
            Some(error) => Err(error),
            None => Err(anyhow::anyhow!(
                "no packaged BlueALSA asset directories were found"
            )),
        }
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

fn find_binary_in_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        path.is_file()
            && path.file_name().is_some_and(|name| name != OsStr::new(""))
            && std::fs::metadata(path)
                .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        path.is_file()
            && path.file_name().is_some_and(|name| name != OsStr::new(""))
            && std::fs::metadata(path).is_ok()
    }
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

    #[test]
    fn ignores_non_executable_files() {
        let dir = tempdir().unwrap();
        let binary = dir.path().join("bluealsad");
        fs::write(&binary, "#!/bin/sh\n").unwrap();
        let found = find_binary_in_path("bluealsad", dir.path().as_os_str());
        assert!(found.is_none());
    }

    #[test]
    fn packaged_dir_discovery_falls_through_invalid_dir() {
        let invalid_dir = tempdir().unwrap();
        fs::write(invalid_dir.path().join("bluealsad"), "#!/bin/sh\n").unwrap();

        let valid_dir = tempdir().unwrap();
        for name in ["bluealsad", "bluealsa-aplay", "bluealsactl"] {
            let binary = valid_dir.path().join(name);
            fs::write(&binary, "#!/bin/sh\n").unwrap();
            let mut permissions = fs::metadata(&binary).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&binary, permissions).unwrap();
        }

        let assets = BluealsaAssets::discover_in_packaged_dirs([
            invalid_dir.path(),
            valid_dir.path(),
        ])
        .unwrap();

        assert!(assets.bluealsad.starts_with(valid_dir.path()));
    }
}
