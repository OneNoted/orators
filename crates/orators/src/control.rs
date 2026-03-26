use std::{env, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use orators_core::{OratorsConfig, dbus};
use orators_linux::{
    LinuxPlatform,
    systemd::{SystemBackendAdapterMode, SystemBackendInstallResult, SystemdUserRuntime},
};
use zbus::{Connection, Proxy, fdo::DBusProxy, names::BusName};

use crate::daemon::{default_config_path, ensure_config_exists};

pub struct ControllerClient {
    connection: Connection,
}

pub struct ConfigFetch {
    pub json: String,
    pub daemon_backed: bool,
}

impl ControllerClient {
    pub async fn connect() -> Result<Self> {
        let connection = Connection::session()
            .await
            .context("failed to connect to session bus")?;
        ensure_daemon_running(&connection).await?;
        Ok(Self { connection })
    }

    pub async fn status(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("GetStatus", &())
            .await
            .map_err(|error| explain_dbus_error("get status", error))
    }

    pub async fn start_pairing(&self, timeout: u64) -> Result<String> {
        self.proxy()
            .await?
            .call("StartPairing", &(timeout,))
            .await
            .map_err(|error| explain_dbus_error("start pairing", error))
    }

    pub async fn stop_pairing(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("StopPairing", &())
            .await
            .map_err(|error| explain_dbus_error("stop pairing", error))
    }

    pub async fn list_devices(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("ListDevices", &())
            .await
            .map_err(|error| explain_dbus_error("list devices", error))
    }

    pub async fn get_config(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("GetConfig", &())
            .await
            .map_err(|error| explain_dbus_error("get config", error))
    }

    pub async fn get_config_or_local(&self) -> Result<ConfigFetch> {
        match self.get_config().await {
            Ok(json) => Ok(ConfigFetch {
                json,
                daemon_backed: true,
            }),
            Err(error) if is_unknown_method_error(&error, "GetConfig") => {
                let (_, config) = load_local_config()?;
                Ok(ConfigFetch {
                    json: serde_json::to_string(&config)
                        .context("failed to serialize local config fallback")?,
                    daemon_backed: false,
                })
            }
            Err(error) => Err(error),
        }
    }

    pub async fn trust_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("TrustDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("trust device", error))
    }

    pub async fn untrust_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("UntrustDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("untrust device", error))
    }

    pub async fn allow_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("AllowDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("allow device", error))
    }

    pub async fn disallow_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("DisallowDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("disallow device", error))
    }

    pub async fn forget_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("ForgetDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("forget device", error))
    }

    pub async fn connect_device(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("ConnectDevice", &(address,))
            .await
            .map_err(|error| explain_dbus_error("connect device", error))
    }

    pub async fn disconnect_active(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("DisconnectActive", &())
            .await
            .map_err(|error| explain_dbus_error("disconnect active device", error))
    }

    pub async fn get_diagnostics(&self) -> Result<String> {
        self.proxy()
            .await?
            .call("GetDiagnostics", &())
            .await
            .map_err(|error| explain_dbus_error("get diagnostics", error))
    }

    pub async fn set_pairing_timeout(&self, timeout: u64) -> Result<String> {
        self.proxy()
            .await?
            .call("SetPairingTimeout", &(timeout,))
            .await
            .map_err(|error| explain_dbus_error("set pairing timeout", error))
    }

    pub async fn set_auto_reconnect(&self, enabled: bool) -> Result<String> {
        self.proxy()
            .await?
            .call("SetAutoReconnect", &(enabled,))
            .await
            .map_err(|error| explain_dbus_error("set auto reconnect", error))
    }

    pub async fn set_single_active_device(&self, enabled: bool) -> Result<String> {
        self.proxy()
            .await?
            .call("SetSingleActiveDevice", &(enabled,))
            .await
            .map_err(|error| explain_dbus_error("set single active device", error))
    }

    pub async fn set_device_alias(&self, address: &str, alias: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("SetDeviceAlias", &(address, alias))
            .await
            .map_err(|error| explain_dbus_error("set device alias", error))
    }

    pub async fn clear_device_alias(&self, address: &str) -> Result<String> {
        self.proxy()
            .await?
            .call("ClearDeviceAlias", &(address,))
            .await
            .map_err(|error| explain_dbus_error("clear device alias", error))
    }

    async fn proxy(&self) -> Result<Proxy<'_>> {
        Proxy::new(
            &self.connection,
            dbus::BUS_NAME,
            dbus::OBJECT_PATH,
            dbus::CONTROL_INTERFACE,
        )
        .await
        .context("failed to connect to Orators D-Bus interface")
    }
}

pub async fn install_user_service() -> Result<PathBuf> {
    let config_path = default_config_path()?;
    ensure_config_exists(&config_path)?;
    let daemon_path = resolve_daemon_path()?;
    let systemd = SystemdUserRuntime;
    systemd.install_user_service(&daemon_path).await
}

pub async fn install_system_backend(
    adapter: Option<String>,
) -> Result<(PathBuf, SystemBackendInstallResult)> {
    let config_path = default_config_path()?;
    let mut config = ensure_config_exists(&config_path)?;
    let mut effective_config = config.clone();
    if let Some(adapter) = adapter.clone() {
        effective_config.adapter = Some(adapter.to_ascii_lowercase());
    }

    let daemon_path = resolve_daemon_path()?;
    let runtime = LinuxPlatform::new(effective_config).await?;
    let user_unit_path = runtime.install_user_service(&daemon_path).await?;
    let install = runtime.install_system_backend(adapter.as_deref()).await?;

    config.adapter = match install.adapter_mode {
        SystemBackendAdapterMode::Auto => None,
        SystemBackendAdapterMode::Explicit => Some(install.resolved_adapter.clone()),
    };
    config.save(&config_path)?;

    Ok((user_unit_path, install))
}

pub async fn uninstall_system_backend() -> Result<()> {
    let config_path = default_config_path()?;
    let config = ensure_config_exists(&config_path)?;
    let runtime = LinuxPlatform::new(config).await?;
    runtime.uninstall_system_backend().await
}

pub fn load_local_config() -> Result<(PathBuf, OratorsConfig)> {
    let path = default_config_path()?;
    let config = ensure_config_exists(&path)?;
    Ok((path, config))
}

pub fn save_local_config(config: &OratorsConfig) -> Result<PathBuf> {
    let path = default_config_path()?;
    config.save(&path)?;
    Ok(path)
}

pub fn resolve_daemon_path() -> Result<PathBuf> {
    let current_exe = env::current_exe().context("failed to resolve current executable path")?;
    let daemon_path = current_exe
        .parent()
        .context("current executable has no parent directory")?
        .join("oratorsd");
    Ok(daemon_path)
}

async fn ensure_daemon_running(connection: &Connection) -> Result<()> {
    let bus_name = BusName::try_from(dbus::BUS_NAME).context("invalid Orators D-Bus bus name")?;
    let bus = DBusProxy::new(connection)
        .await
        .context("failed to connect to session D-Bus daemon")?;
    if bus
        .name_has_owner(bus_name.clone())
        .await
        .context("failed to query Orators D-Bus ownership")?
    {
        return Ok(());
    }

    SystemdUserRuntime
        .start_orators_service()
        .await
        .context("failed to start oratorsd.service; run `oratorsctl install-user-service` first")?;

    for _ in 0..12 {
        if bus
            .name_has_owner(bus_name.clone())
            .await
            .context("failed to re-check Orators D-Bus ownership")?
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }

    anyhow::bail!("oratorsd.service started, but the D-Bus control interface never appeared")
}

fn explain_dbus_error(action: &str, error: zbus::Error) -> anyhow::Error {
    let detail = error.to_string();
    let next_step = if detail.contains("pairing window is closed") {
        "Run `oratorsctl pair start --timeout 120` before pairing a new device."
    } else if detail.contains("BlueALSA")
        || detail.contains("bluealsad")
        || detail.contains("WirePlumber Bluetooth-disable fragment")
    {
        "Run `oratorsctl doctor`, then install or repair the managed backend with `oratorsctl install-system-backend`."
    } else if detail.contains("the host does not currently expose a usable local playback output") {
        "Run `oratorsctl doctor` and repair the host ALSA/PipeWire playback output before retrying."
    } else if detail.contains("classic A2DP audio-source profile") {
        "This device does not advertise classic Bluetooth media-source support, so it is not a supported speaker source."
    } else if detail.contains("RequestDefaultAgent") {
        "Close other Bluetooth pairing dialogs, then rerun `oratorsctl pair start`."
    } else if detail.contains("no BlueZ adapter found") {
        "Check that Bluetooth is present, powered on, and visible in `bluetoothctl show`."
    } else if detail.contains("already active") {
        "Disconnect the active device first, or disable the single-active-device setting."
    } else if detail.contains("org.bluez.Error") {
        "Run `oratorsctl doctor` and review the BlueZ service logs for the exact failure."
    } else {
        "Run `oratorsctl doctor` for more detail."
    };

    anyhow!("{action} failed: {detail}\nNext step: {next_step}")
}

fn is_unknown_method_error(error: &anyhow::Error, method: &str) -> bool {
    let detail = error.to_string();
    detail.contains("org.freedesktop.DBus.Error.UnknownMethod")
        && detail.contains(&format!("'{method}'"))
}
