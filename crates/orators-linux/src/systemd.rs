use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dirs::home_dir;
use tokio::{fs, process::Command};

const UNIT_NAME: &str = "oratorsd.service";
const WIREPLUMBER_BACKEND_DROPIN: &str = "90-orators-audio-owner.conf";

pub struct SystemdUserRuntime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserServiceStatus {
    pub active_state: String,
    pub sub_state: String,
    pub result: Option<String>,
    pub restart_count: u32,
    pub exec_start: Option<String>,
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

    pub async fn install_host_backend(&self, daemon_path: &Path) -> Result<(PathBuf, PathBuf)> {
        let unit_path = self.install_user_service(daemon_path).await?;
        let dropin_path = wireplumber_dropin_path()?;
        if let Some(parent) = dropin_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&dropin_path, render_wireplumber_dropin()).await?;
        self.run_systemctl(["daemon-reload"]).await?;
        Ok((unit_path, dropin_path))
    }

    pub async fn uninstall_host_backend(&self) -> Result<bool> {
        let dropin_path = wireplumber_dropin_path()?;
        if !dropin_path.exists() {
            return Ok(false);
        }

        fs::remove_file(&dropin_path).await.with_context(|| {
            format!(
                "failed to remove Orators WirePlumber backend drop-in {}",
                dropin_path.display()
            )
        })?;
        self.run_systemctl(["daemon-reload"]).await?;
        Ok(true)
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
                "--property=ActiveState,SubState,Result,NRestarts,ExecStart",
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

    pub async fn host_backend_installed(&self) -> Result<bool> {
        Ok(wireplumber_dropin_path()?.exists())
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
}

pub fn service_uses_audio_profile(status: &UserServiceStatus) -> bool {
    status.exec_start.as_deref().is_some_and(|value| {
        value.contains("wireplumber") && value.contains("-p") && value.contains("audio")
    })
}

fn unit_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home.join(".config/systemd/user").join(UNIT_NAME))
}

fn wireplumber_dropin_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home
        .join(".config/systemd/user/wireplumber.service.d")
        .join(WIREPLUMBER_BACKEND_DROPIN))
}

fn parse_service_status(output: &str) -> Result<UserServiceStatus> {
    let mut active_state = None;
    let mut sub_state = None;
    let mut result = None;
    let mut restart_count = 0;
    let mut exec_start = None;

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
        } else if let Some(value) = line.strip_prefix("ExecStart=") {
            if !value.is_empty() {
                exec_start = Some(value.to_string());
            }
        }
    }

    Ok(UserServiceStatus {
        active_state: active_state.context("missing ActiveState")?,
        sub_state: sub_state.context("missing SubState")?,
        result,
        restart_count,
        exec_start,
    })
}

pub fn render_unit(daemon_path: &Path) -> String {
    format!(
        "[Unit]\nDescription=Orators Bluetooth speaker daemon\nAfter=default.target bluetooth.target\n\n[Service]\nType=simple\nExecStart={}\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        daemon_path.display()
    )
}

pub fn render_wireplumber_dropin() -> String {
    "[Service]\nExecStart=\nExecStart=/usr/bin/wireplumber -p audio\n".to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        parse_service_status, render_unit, render_wireplumber_dropin, service_uses_audio_profile,
    };

    #[test]
    fn renders_unit_file_with_binary_path() {
        let unit = render_unit(Path::new("/tmp/oratorsd"));
        assert!(unit.contains("ExecStart=/tmp/oratorsd"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn parses_systemctl_show_output() {
        let status = parse_service_status(
            "ActiveState=active\nSubState=running\nResult=success\nNRestarts=2\nExecStart=/usr/bin/wireplumber -p audio\n",
        )
        .unwrap();

        assert_eq!(status.active_state, "active");
        assert_eq!(status.sub_state, "running");
        assert_eq!(status.result.as_deref(), Some("success"));
        assert_eq!(status.restart_count, 2);
        assert!(service_uses_audio_profile(&status));
    }

    #[test]
    fn renders_wireplumber_dropin_for_audio_profile() {
        let dropin = render_wireplumber_dropin();
        assert!(dropin.contains("wireplumber -p audio"));
        assert!(dropin.contains("ExecStart="));
    }
}
