use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dirs::home_dir;
use tokio::{fs, process::Command};

const UNIT_NAME: &str = "oratorsd.service";
const WIREPLUMBER_UNIT_NAME: &str = "wireplumber.service";
const WIREPLUMBER_AUDIO_OWNER_DROPIN: &str = "90-orators-audio-owner.conf";

pub struct SystemdUserRuntime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserServiceStatus {
    pub active_state: String,
    pub sub_state: String,
    pub result: Option<String>,
    pub restart_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedBackendStatus {
    pub installed: bool,
    pub wireplumber_audio_profile: bool,
    pub unit_path: PathBuf,
    pub wireplumber_dropin_path: PathBuf,
}

impl SystemdUserRuntime {
    pub async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf> {
        let unit_path = unit_path()?;
        if let Some(parent) = unit_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&unit_path, render_unit(daemon_path)).await?;
        self.run_systemctl(["daemon-reload"]).await?;

        Ok(unit_path)
    }

    pub async fn install_host_backend(&self, daemon_path: &Path) -> Result<ManagedBackendStatus> {
        let unit_path = self.install_user_service(daemon_path).await?;
        let dropin_path = wireplumber_dropin_path()?;
        if let Some(parent) = dropin_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&dropin_path, render_wireplumber_audio_owner_override()).await?;
        self.run_systemctl(["daemon-reload"]).await?;

        let installed = unit_path.exists() && dropin_path.exists();
        let wireplumber_audio_profile = self.wireplumber_uses_audio_profile().await?;
        Ok(ManagedBackendStatus {
            installed,
            wireplumber_audio_profile,
            unit_path,
            wireplumber_dropin_path: dropin_path,
        })
    }

    pub async fn uninstall_host_backend(&self) -> Result<()> {
        let unit_path = unit_path()?;
        let dropin_path = wireplumber_dropin_path()?;
        remove_file_if_exists(&unit_path).await?;
        remove_file_if_exists(&dropin_path).await?;
        self.run_systemctl(["daemon-reload"]).await?;
        Ok(())
    }

    pub async fn start_orators_service(&self) -> Result<()> {
        self.run_systemctl(["start", UNIT_NAME]).await
    }

    pub async fn try_restart(&self, unit_name: &str) -> Result<()> {
        self.run_systemctl(["try-restart", unit_name]).await
    }

    pub async fn service_status(&self, unit_name: &str) -> Result<UserServiceStatus> {
        let output = Command::new("systemctl")
            .args([
                "--user",
                "show",
                unit_name,
                "--property=ActiveState,SubState,Result,NRestarts",
            ])
            .output()
            .await
            .with_context(|| format!("failed to invoke systemctl --user show {unit_name}"))?;

        if !output.status.success() {
            anyhow::bail!(
                "systemctl --user show {unit_name} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        parse_service_status(&String::from_utf8_lossy(&output.stdout)).with_context(|| {
            format!("failed to parse systemctl --user show output for {unit_name}")
        })
    }

    pub async fn managed_backend_status(&self) -> Result<ManagedBackendStatus> {
        let unit_path = unit_path()?;
        let dropin_path = wireplumber_dropin_path()?;
        Ok(ManagedBackendStatus {
            installed: unit_path.exists() && dropin_path.exists(),
            wireplumber_audio_profile: self.wireplumber_uses_audio_profile().await?,
            unit_path,
            wireplumber_dropin_path: dropin_path,
        })
    }

    async fn run_systemctl<const N: usize>(&self, args: [&str; N]) -> Result<()> {
        let output = Command::new("systemctl")
            .args(["--user"])
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to invoke systemctl --user {:?}", args))?;

        if !output.status.success() {
            anyhow::bail!(
                "systemctl --user {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        Ok(())
    }

    async fn wireplumber_uses_audio_profile(&self) -> Result<bool> {
        let output = Command::new("systemctl")
            .args([
                "--user",
                "show",
                WIREPLUMBER_UNIT_NAME,
                "--property=ExecStart",
            ])
            .output()
            .await
            .context("failed to inspect wireplumber ExecStart")?;

        if !output.status.success() {
            anyhow::bail!(
                "systemctl --user show {} failed: {}",
                WIREPLUMBER_UNIT_NAME,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains("wireplumber -p audio"))
    }
}

fn unit_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home.join(".config/systemd/user").join(UNIT_NAME))
}

fn wireplumber_dropin_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home
        .join(".config/systemd/user")
        .join(format!("{WIREPLUMBER_UNIT_NAME}.d"))
        .join(WIREPLUMBER_AUDIO_OWNER_DROPIN))
}

fn parse_service_status(output: &str) -> Result<UserServiceStatus> {
    let mut active_state = None;
    let mut sub_state = None;
    let mut result = None;
    let mut restart_count = 0;

    for line in output.lines() {
        if let Some(value) = line.strip_prefix("ActiveState=") {
            active_state = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("SubState=") {
            sub_state = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("Result=") {
            if !value.is_empty() {
                result = Some(value.to_string());
            }
        } else if let Some(value) = line.strip_prefix("NRestarts=") {
            restart_count = value.parse().unwrap_or_default();
        }
    }

    Ok(UserServiceStatus {
        active_state: active_state.context("missing ActiveState")?,
        sub_state: sub_state.context("missing SubState")?,
        result,
        restart_count,
    })
}

pub fn render_unit(daemon_path: &Path) -> String {
    format!(
        "[Unit]\nDescription=Orators Bluetooth speaker daemon\nAfter=default.target bluetooth.target\n\n[Service]\nType=simple\nExecStart={} \nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        daemon_path.display()
    )
}

pub fn render_wireplumber_audio_owner_override() -> &'static str {
    "[Service]\nExecStart=\nExecStart=/usr/bin/wireplumber -p audio\n"
}

async fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{parse_service_status, render_unit, render_wireplumber_audio_owner_override};

    #[test]
    fn renders_unit_file_with_binary_path() {
        let unit = render_unit(Path::new("/tmp/oratorsd"));
        assert!(unit.contains("ExecStart=/tmp/oratorsd"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn parses_systemctl_show_output() {
        let status = parse_service_status(
            "ActiveState=active\nSubState=running\nResult=success\nNRestarts=2\n",
        )
        .unwrap();

        assert_eq!(status.active_state, "active");
        assert_eq!(status.sub_state, "running");
        assert_eq!(status.result.as_deref(), Some("success"));
        assert_eq!(status.restart_count, 2);
    }

    #[test]
    fn renders_wireplumber_audio_override() {
        let override_contents = render_wireplumber_audio_owner_override();
        assert!(override_contents.contains("ExecStart=/usr/bin/wireplumber -p audio"));
    }
}
