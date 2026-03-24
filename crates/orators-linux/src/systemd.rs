use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use dirs::home_dir;
use tokio::{fs, process::Command};

const UNIT_NAME: &str = "oratorsd.service";

pub struct SystemdUserRuntime;

impl SystemdUserRuntime {
    pub async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf> {
        let unit_path = unit_path()?;
        if let Some(parent) = unit_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&unit_path, render_unit(daemon_path)).await?;
        self.run_systemctl(["daemon-reload"]).await?;
        self.run_systemctl(["enable", "--now", UNIT_NAME]).await?;

        Ok(unit_path)
    }

    pub async fn try_restart(&self, unit_name: &str) -> Result<()> {
        self.run_systemctl(["try-restart", unit_name]).await
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

pub fn render_unit(daemon_path: &Path) -> String {
    format!(
        "[Unit]\nDescription=Orators Bluetooth speaker daemon\nAfter=default.target bluetooth.target\n\n[Service]\nType=simple\nExecStart={} \nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        daemon_path.display()
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::render_unit;

    #[test]
    fn renders_unit_file_with_binary_path() {
        let unit = render_unit(Path::new("/tmp/oratorsd"));
        assert!(unit.contains("ExecStart=/tmp/oratorsd"));
        assert!(unit.contains("WantedBy=default.target"));
    }
}
