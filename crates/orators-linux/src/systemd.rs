use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use dirs::home_dir;
use tempfile::NamedTempFile;
use tokio::{fs, process::Command};

use crate::bluealsa::{BluealsaAssets, SYSTEM_BACKEND_UNIT};

const USER_UNIT_NAME: &str = "oratorsd.service";
const BACKEND_FRAGMENT_NAME: &str = "90-orators-disable-bluez.conf";
const BACKEND_DBUS_POLICY_NAME: &str = "orators-bluealsa.conf";
const LEGACY_WIREPLUMBER_DROPIN: &str = "90-orators-audio-owner.conf";
const LEGACY_WIREPLUMBER_FRAGMENT: &str = "90-orators-bluetooth.conf";
const LEGACY_SYSTEM_BACKEND_UNIT: &str = "orators-bluealsa.service";
const SYSTEM_UNIT_DIR: &str = "/etc/systemd/system";
const SYSTEM_DBUS_POLICY_DIR: &str = "/etc/dbus-1/system.d";

pub struct SystemdUserRuntime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub active_state: String,
    pub sub_state: String,
    pub result: Option<String>,
    pub restart_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBackendInstallResult {
    pub user_service_path: PathBuf,
    pub wireplumber_fragment_path: PathBuf,
    pub dbus_policy_path: PathBuf,
    pub system_unit_path: PathBuf,
    pub adapter: String,
}

impl SystemdUserRuntime {
    pub async fn install_user_service(&self, daemon_path: &Path) -> Result<PathBuf> {
        let unit_path = user_unit_path()?;
        if let Some(parent) = unit_path.parent() {
            fs::create_dir_all(parent).await?;
        }

        fs::write(&unit_path, render_user_unit(daemon_path)).await?;
        self.run_user_systemctl(["daemon-reload"]).await?;

        Ok(unit_path)
    }

    pub async fn start_orators_service(&self) -> Result<()> {
        self.run_user_systemctl(["start", USER_UNIT_NAME]).await
    }

    pub async fn install_system_backend(
        &self,
        assets: &BluealsaAssets,
        adapter: &str,
    ) -> Result<SystemBackendInstallResult> {
        self.cleanup_legacy_wireplumber_dropin().await?;
        self.remove_legacy_fragment().await?;

        let fragment_path = wireplumber_fragment_path()?;
        if let Some(parent) = fragment_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&fragment_path, render_wireplumber_fragment()).await?;

        let system_unit_path = system_backend_unit_path();
        let dbus_policy_path = system_backend_dbus_policy_path();
        let mut temp_unit =
            NamedTempFile::new().context("failed to create temporary systemd unit file")?;
        let mut temp_policy =
            NamedTempFile::new().context("failed to create temporary D-Bus policy file")?;
        temp_unit
            .write_all(render_system_backend_unit(&assets.bluealsad, adapter).as_bytes())
            .context("failed to write temporary systemd unit file")?;
        temp_policy
            .write_all(render_system_backend_dbus_policy().as_bytes())
            .context("failed to write temporary D-Bus policy file")?;
        let install_result = async {
            self.run_sudo([
                "install",
                "-Dm644",
                temp_unit.path().to_string_lossy().as_ref(),
                system_unit_path.to_string_lossy().as_ref(),
            ])
            .await?;
            self.run_sudo([
                "install",
                "-Dm644",
                temp_policy.path().to_string_lossy().as_ref(),
                dbus_policy_path.to_string_lossy().as_ref(),
            ])
            .await?;
            self.run_sudo([
                "rm",
                "-f",
                legacy_system_backend_unit_path().to_string_lossy().as_ref(),
            ])
            .await?;
            self.run_sudo([
                "busctl",
                "call",
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ReloadConfig",
            ])
            .await?;
            self.run_sudo(["systemctl", "daemon-reload"]).await?;
            self.run_sudo(["systemctl", "reset-failed", SYSTEM_BACKEND_UNIT])
                .await?;
            self.run_sudo(["systemctl", "enable", "--now", SYSTEM_BACKEND_UNIT])
                .await?;
            self.run_user_systemctl(["try-restart", "wireplumber.service"])
                .await?;

            Ok(SystemBackendInstallResult {
                user_service_path: user_unit_path()?,
                wireplumber_fragment_path: fragment_path.clone(),
                dbus_policy_path: dbus_policy_path.clone(),
                system_unit_path: system_unit_path.clone(),
                adapter: adapter.to_string(),
            })
        }
        .await;

        if install_result.is_err() {
            let _ = self.rollback_failed_install(&fragment_path).await;
        }

        install_result
    }

    pub async fn uninstall_system_backend(&self) -> Result<()> {
        self.run_sudo_allow_missing(["systemctl", "disable", "--now", SYSTEM_BACKEND_UNIT])
            .await?;
        self.run_sudo_allow_missing(["systemctl", "disable", "--now", LEGACY_SYSTEM_BACKEND_UNIT])
            .await?;
        self.run_sudo([
            "rm",
            "-f",
            system_backend_unit_path().to_string_lossy().as_ref(),
        ])
        .await?;
        self.run_sudo([
            "rm",
            "-f",
            system_backend_dbus_policy_path().to_string_lossy().as_ref(),
        ])
        .await?;
        self.run_sudo([
            "rm",
            "-f",
            legacy_system_backend_unit_path().to_string_lossy().as_ref(),
        ])
        .await?;
        self.run_sudo([
            "busctl",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ReloadConfig",
        ])
        .await?;
        self.run_sudo(["systemctl", "daemon-reload"]).await?;

        let fragment_path = wireplumber_fragment_path()?;
        if fragment_path.exists() {
            fs::remove_file(&fragment_path).await.with_context(|| {
                format!(
                    "failed to remove WirePlumber Bluetooth-disable fragment {}",
                    fragment_path.display()
                )
            })?;
        }

        self.cleanup_legacy_wireplumber_dropin().await?;
        self.remove_legacy_fragment().await?;
        self.run_user_systemctl(["try-restart", "wireplumber.service"])
            .await?;
        Ok(())
    }

    pub async fn cleanup_legacy_wireplumber_dropin(&self) -> Result<bool> {
        let dropin_path = legacy_wireplumber_dropin_path()?;
        if !dropin_path.exists() {
            return Ok(false);
        }

        fs::remove_file(&dropin_path).await.with_context(|| {
            format!(
                "failed to remove legacy WirePlumber drop-in {}",
                dropin_path.display()
            )
        })?;
        self.run_user_systemctl(["daemon-reload"]).await?;
        Ok(true)
    }

    pub async fn remove_legacy_fragment(&self) -> Result<bool> {
        let fragment_path = legacy_wireplumber_fragment_path()?;
        if !fragment_path.exists() {
            return Ok(false);
        }

        fs::remove_file(&fragment_path).await.with_context(|| {
            format!(
                "failed to remove legacy WirePlumber fragment {}",
                fragment_path.display()
            )
        })?;
        Ok(true)
    }

    pub async fn user_service_status(&self, unit_name: &str) -> Result<ServiceStatus> {
        self.service_status(["--user", "show", unit_name]).await
    }

    pub async fn system_service_status(&self, unit_name: &str) -> Result<ServiceStatus> {
        self.service_status(["show", unit_name]).await
    }

    pub fn wireplumber_fragment_installed(&self) -> Result<bool> {
        Ok(wireplumber_fragment_path()?.exists())
    }

    async fn service_status<const N: usize>(&self, args: [&str; N]) -> Result<ServiceStatus> {
        let output = Command::new("systemctl")
            .args(args)
            .args(["--property=ActiveState,SubState,Result,NRestarts"])
            .output()
            .await
            .with_context(|| format!("failed to invoke systemctl {:?}", args))?;

        if !output.status.success() {
            anyhow::bail!(
                "systemctl {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        parse_service_status(&String::from_utf8_lossy(&output.stdout))
    }

    async fn run_user_systemctl<const N: usize>(&self, args: [&str; N]) -> Result<()> {
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

    async fn run_sudo<const N: usize>(&self, args: [&str; N]) -> Result<()> {
        let output = Command::new("sudo")
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to invoke sudo {:?}", args))?;

        if !output.status.success() {
            anyhow::bail!(
                "sudo {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        Ok(())
    }

    async fn run_sudo_allow_missing<const N: usize>(&self, args: [&str; N]) -> Result<()> {
        match self.run_sudo(args).await {
            Ok(()) => Ok(()),
            Err(error)
                if error.to_string().contains("No such file")
                    || error.to_string().contains("not loaded")
                    || error.to_string().contains("not-found")
                    || error.to_string().contains("could not be found") =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    async fn rollback_failed_install(&self, fragment_path: &Path) -> Result<()> {
        let _ = self
            .run_sudo(["systemctl", "disable", "--now", SYSTEM_BACKEND_UNIT])
            .await;
        let _ = self
            .run_sudo([
                "rm",
                "-f",
                system_backend_unit_path().to_string_lossy().as_ref(),
            ])
            .await;
        let _ = self
            .run_sudo([
                "rm",
                "-f",
                system_backend_dbus_policy_path().to_string_lossy().as_ref(),
            ])
            .await;
        let _ = self
            .run_sudo([
                "busctl",
                "call",
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "ReloadConfig",
            ])
            .await;
        let _ = self.run_sudo(["systemctl", "daemon-reload"]).await;

        if fragment_path.exists() {
            let _ = fs::remove_file(fragment_path).await;
        }
        let _ = self
            .run_user_systemctl(["try-restart", "wireplumber.service"])
            .await;
        Ok(())
    }
}

fn user_unit_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home.join(".config/systemd/user").join(USER_UNIT_NAME))
}

fn wireplumber_fragment_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home
        .join(".config/wireplumber/wireplumber.conf.d")
        .join(BACKEND_FRAGMENT_NAME))
}

fn legacy_wireplumber_dropin_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home
        .join(".config/systemd/user/wireplumber.service.d")
        .join(LEGACY_WIREPLUMBER_DROPIN))
}

fn legacy_wireplumber_fragment_path() -> Result<PathBuf> {
    let home = home_dir().context("unable to determine home directory")?;
    Ok(home
        .join(".config/wireplumber/wireplumber.conf.d")
        .join(LEGACY_WIREPLUMBER_FRAGMENT))
}

fn system_backend_unit_path() -> PathBuf {
    Path::new(SYSTEM_UNIT_DIR).join(SYSTEM_BACKEND_UNIT)
}

fn system_backend_dbus_policy_path() -> PathBuf {
    Path::new(SYSTEM_DBUS_POLICY_DIR).join(BACKEND_DBUS_POLICY_NAME)
}

fn legacy_system_backend_unit_path() -> PathBuf {
    Path::new(SYSTEM_UNIT_DIR).join(LEGACY_SYSTEM_BACKEND_UNIT)
}

fn parse_service_status(output: &str) -> Result<ServiceStatus> {
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

    Ok(ServiceStatus {
        active_state: active_state.context("missing ActiveState")?,
        sub_state: sub_state.context("missing SubState")?,
        result,
        restart_count,
    })
}

pub fn render_user_unit(daemon_path: &Path) -> String {
    format!(
        "[Unit]\nDescription=Orators Bluetooth speaker daemon\nAfter=default.target bluetooth.target\n\n[Service]\nType=simple\nExecStart={} \nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        daemon_path.display()
    )
}

pub fn render_system_backend_unit(bluealsad_path: &Path, adapter: &str) -> String {
    format!(
        "[Unit]\nDescription=Orators BlueALSA backend\nAfter=bluetooth.service\nRequires=bluetooth.service\n\n[Service]\nType=simple\nExecStart={} -p a2dp-sink -i {}\nRestart=on-failure\nRestartSec=2\n\n[Install]\nWantedBy=multi-user.target\n",
        bluealsad_path.display(),
        adapter,
    )
}

pub fn render_system_backend_dbus_policy() -> &'static str {
    r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <policy user="root">
    <allow own="org.bluealsa"/>
    <allow send_destination="org.bluez"/>
  </policy>
  <policy context="default">
    <allow send_destination="org.bluealsa"/>
  </policy>
</busconfig>
"#
}

pub fn render_wireplumber_fragment() -> &'static str {
    "wireplumber.profiles = {\n  main = {\n    monitor.bluez = disabled\n    monitor.bluez-midi = disabled\n  }\n}\n"
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        parse_service_status, render_system_backend_dbus_policy, render_system_backend_unit,
        render_user_unit, render_wireplumber_fragment,
    };

    #[test]
    fn renders_user_unit_file_with_binary_path() {
        let unit = render_user_unit(Path::new("/tmp/oratorsd"));
        assert!(unit.contains("ExecStart=/tmp/oratorsd"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn renders_system_backend_unit() {
        let unit = render_system_backend_unit(Path::new("/usr/bin/bluealsad"), "hci1");
        assert!(unit.contains("ExecStart=/usr/bin/bluealsad -p a2dp-sink -i hci1"));
        assert!(unit.contains("WantedBy=multi-user.target"));
    }

    #[test]
    fn renders_wireplumber_fragment() {
        let fragment = render_wireplumber_fragment();
        assert!(fragment.contains("monitor.bluez = disabled"));
        assert!(fragment.contains("monitor.bluez-midi = disabled"));
    }

    #[test]
    fn renders_dbus_policy() {
        let policy = render_system_backend_dbus_policy();
        assert!(policy.contains("allow own=\"org.bluealsa\""));
        assert!(policy.contains("allow send_destination=\"org.bluealsa\""));
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
