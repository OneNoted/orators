use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dirs::home_dir;
use tokio::{fs, process::Command};

const UNIT_NAME: &str = "oratorsd.service";

pub struct SystemdUserRuntime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserServiceStatus {
    pub active_state: String,
    pub sub_state: String,
    pub result: Option<String>,
    pub restart_count: u32,
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

fn unit_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home.join(".config/systemd/user").join(UNIT_NAME))
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{parse_service_status, render_unit};

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
}
