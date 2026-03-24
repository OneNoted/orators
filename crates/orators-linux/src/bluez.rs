use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result};
use orators_core::DeviceInfo;
use zbus::{
    Connection, DBusError, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue},
};

const BLUEZ_SERVICE: &str = "org.bluez";
const BLUEZ_ROOT_PATH: &str = "/";
const AGENT_MANAGER_PATH: &str = "/org/bluez";
const AGENT_PATH: &str = "/dev/orators/bluez/agent";
const NO_INPUT_NO_OUTPUT: &str = "NoInputNoOutput";

type InterfaceProperties = HashMap<String, HashMap<String, OwnedValue>>;
type ManagedObjects = HashMap<OwnedObjectPath, InterfaceProperties>;

pub struct BluetoothCtlBluez {
    connection: Connection,
    agent_state: Arc<PairingAgentState>,
}

impl BluetoothCtlBluez {
    pub async fn new() -> Result<Self> {
        let connection = Connection::system()
            .await
            .context("failed to connect to system bus for BlueZ")?;
        let agent_state = Arc::new(PairingAgentState::default());

        connection
            .object_server()
            .at(AGENT_PATH, PairingAgent::new(Arc::clone(&agent_state)))
            .await
            .context("failed to export BlueZ agent object")?;

        let runtime = Self {
            connection,
            agent_state,
        };
        runtime.register_agent().await?;
        Ok(runtime)
    }

    pub async fn adapter_available(&self) -> Result<bool> {
        Ok(self.adapter_path().await.is_ok())
    }

    pub async fn list_devices(&self, auto_reconnect: bool) -> Result<Vec<DeviceInfo>> {
        let managed = self.managed_objects().await?;
        Ok(parse_devices(&managed, auto_reconnect))
    }

    pub async fn start_pairing(&self, timeout_secs: u64) -> Result<()> {
        self.request_default_agent().await?;

        let adapter_path = self.adapter_path().await?;
        let adapter = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            adapter_path.clone(),
            "org.bluez.Adapter1",
        )
        .await
        .context("failed to create BlueZ adapter proxy")?;
        self.agent_state.set_pairing_active(true);

        let timeout = timeout_secs as u32;
        let result = async {
            adapter
                .set_property("Powered", true)
                .await
                .context("failed to power on Bluetooth adapter")?;
            adapter
                .set_property("PairableTimeout", timeout)
                .await
                .context("failed to set pairable timeout")?;
            adapter
                .set_property("DiscoverableTimeout", timeout)
                .await
                .context("failed to set discoverable timeout")?;
            adapter
                .set_property("Pairable", true)
                .await
                .context("failed to enable pairable mode")?;
            adapter
                .set_property("Discoverable", true)
                .await
                .context("failed to enable discoverable mode")?;
            Result::<()>::Ok(())
        }
        .await;

        if result.is_err() {
            self.agent_state.set_pairing_active(false);
        }

        result
    }

    pub async fn stop_pairing(&self) -> Result<()> {
        let adapter_path = self.adapter_path().await?;
        let adapter = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            adapter_path.clone(),
            "org.bluez.Adapter1",
        )
        .await
        .context("failed to create BlueZ adapter proxy")?;
        self.agent_state.set_pairing_active(false);
        adapter
            .set_property("Discoverable", false)
            .await
            .context("failed to disable discoverable mode")?;
        adapter
            .set_property("Pairable", false)
            .await
            .context("failed to disable pairable mode")?;
        Ok(())
    }

    pub async fn trust_device(&self, address: &str) -> Result<()> {
        let device_path = self.device_path(address).await?;
        let device = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            device_path.clone(),
            "org.bluez.Device1",
        )
        .await
        .with_context(|| format!("failed to create BlueZ device proxy for {address}"))?;
        device
            .set_property("Trusted", true)
            .await
            .with_context(|| format!("failed to trust device {address}"))?;
        Ok(())
    }

    pub async fn forget_device(&self, address: &str) -> Result<()> {
        let adapter_path = self.adapter_path().await?;
        let device_path = self.device_path(address).await?;
        let adapter = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            adapter_path.clone(),
            "org.bluez.Adapter1",
        )
        .await
        .context("failed to create BlueZ adapter proxy")?;
        adapter
            .call_method("RemoveDevice", &(device_path.clone()))
            .await
            .with_context(|| format!("failed to remove device {address}"))?;
        Ok(())
    }

    pub async fn connect_device(&self, address: &str) -> Result<()> {
        let device_path = self.device_path(address).await?;
        let device = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            device_path.clone(),
            "org.bluez.Device1",
        )
        .await
        .with_context(|| format!("failed to create BlueZ device proxy for {address}"))?;
        device
            .call_method("Connect", &())
            .await
            .with_context(|| format!("failed to connect device {address}"))?;
        Ok(())
    }

    pub async fn disconnect_device(&self, address: &str) -> Result<()> {
        let device_path = self.device_path(address).await?;
        let device = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            device_path.clone(),
            "org.bluez.Device1",
        )
        .await
        .with_context(|| format!("failed to create BlueZ device proxy for {address}"))?;
        device
            .call_method("Disconnect", &())
            .await
            .with_context(|| format!("failed to disconnect device {address}"))?;
        Ok(())
    }

    async fn register_agent(&self) -> Result<()> {
        let manager = self.agent_manager_proxy().await?;
        match manager
            .call_method(
                "RegisterAgent",
                &(OwnedObjectPath::try_from(AGENT_PATH)?, NO_INPUT_NO_OUTPUT),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().contains("AlreadyExists") => Ok(()),
            Err(error) => Err(error).context("failed to register BlueZ agent"),
        }
    }

    async fn request_default_agent(&self) -> Result<()> {
        let manager = self.agent_manager_proxy().await?;
        manager
            .call_method(
                "RequestDefaultAgent",
                &(OwnedObjectPath::try_from(AGENT_PATH)?),
            )
            .await
            .context("failed to make Orators the default BlueZ agent")?;
        Ok(())
    }

    async fn agent_manager_proxy(&self) -> Result<Proxy<'_>> {
        Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            AGENT_MANAGER_PATH,
            "org.bluez.AgentManager1",
        )
        .await
        .context("failed to create BlueZ agent manager proxy")
    }

    async fn managed_objects(&self) -> Result<ManagedObjects> {
        let proxy = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            BLUEZ_ROOT_PATH,
            "org.freedesktop.DBus.ObjectManager",
        )
        .await
        .context("failed to create BlueZ object manager proxy")?;

        proxy
            .call("GetManagedObjects", &())
            .await
            .context("failed to fetch BlueZ managed objects")
    }

    async fn adapter_path(&self) -> Result<OwnedObjectPath> {
        let managed = self.managed_objects().await?;
        managed
            .into_iter()
            .find_map(|(path, interfaces)| {
                interfaces
                    .contains_key("org.bluez.Adapter1")
                    .then_some(path)
            })
            .context("no BlueZ adapter found")
    }

    async fn device_path(&self, address: &str) -> Result<OwnedObjectPath> {
        let managed = self.managed_objects().await?;
        managed
            .into_iter()
            .find_map(|(path, interfaces)| {
                let props = interfaces.get("org.bluez.Device1")?;
                let candidate = string_prop(props, "Address")?;
                (candidate == address).then_some(path)
            })
            .with_context(|| format!("no BlueZ device found for {address}"))
    }
}

#[derive(Debug, Default)]
struct PairingAgentState {
    pairing_active: AtomicBool,
}

impl PairingAgentState {
    fn set_pairing_active(&self, active: bool) {
        self.pairing_active.store(active, Ordering::SeqCst);
    }

    fn ensure_pairing_active(&self) -> std::result::Result<(), AgentError> {
        if self.pairing_active.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(AgentError::Rejected(
                "pairing mode is not currently enabled".to_string(),
            ))
        }
    }
}

struct PairingAgent {
    state: Arc<PairingAgentState>,
}

impl PairingAgent {
    fn new(state: Arc<PairingAgentState>) -> Self {
        Self { state }
    }
}

#[derive(Debug, DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl PairingAgent {
    fn release(&self) {}

    fn request_pin_code(
        &self,
        _device: OwnedObjectPath,
    ) -> std::result::Result<String, AgentError> {
        Err(AgentError::Rejected(
            "pin code entry is not supported".to_string(),
        ))
    }

    fn display_pin_code(&self, _device: OwnedObjectPath, _pincode: &str) {}

    fn request_passkey(&self, _device: OwnedObjectPath) -> std::result::Result<u32, AgentError> {
        Err(AgentError::Rejected(
            "passkey entry is not supported".to_string(),
        ))
    }

    fn display_passkey(&self, _device: OwnedObjectPath, _passkey: u32, _entered: u16) {}

    fn request_confirmation(
        &self,
        _device: OwnedObjectPath,
        _passkey: u32,
    ) -> std::result::Result<(), AgentError> {
        self.state.ensure_pairing_active()
    }

    fn request_authorization(
        &self,
        _device: OwnedObjectPath,
    ) -> std::result::Result<(), AgentError> {
        self.state.ensure_pairing_active()
    }

    fn authorize_service(
        &self,
        _device: OwnedObjectPath,
        _uuid: &str,
    ) -> std::result::Result<(), AgentError> {
        self.state.ensure_pairing_active()
    }

    fn cancel(&self) {}
}

fn parse_devices(managed: &ManagedObjects, auto_reconnect: bool) -> Vec<DeviceInfo> {
    managed
        .values()
        .filter_map(|interfaces| interfaces.get("org.bluez.Device1"))
        .filter_map(|properties| parse_device(properties, auto_reconnect))
        .collect()
}

fn parse_device(
    properties: &HashMap<String, OwnedValue>,
    auto_reconnect: bool,
) -> Option<DeviceInfo> {
    let address = string_prop(properties, "Address")?;
    let alias = string_prop(properties, "Alias").or_else(|| string_prop(properties, "Name"));
    let trusted = bool_prop(properties, "Trusted").unwrap_or(false);
    let paired = bool_prop(properties, "Paired").unwrap_or(false);
    let connected = bool_prop(properties, "Connected").unwrap_or(false);

    Some(DeviceInfo {
        address,
        alias,
        trusted,
        paired,
        connected,
        active_profile: None,
        auto_reconnect: auto_reconnect && trusted,
    })
}

fn string_prop(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    properties
        .get(key)
        .and_then(|value| <&str>::try_from(value).ok())
        .map(ToString::to_string)
}

fn bool_prop(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    properties
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use zbus::zvariant::{OwnedValue, Str};

    use super::{AgentError, PairingAgentState, parse_device};

    #[test]
    fn parses_device_from_managed_properties() {
        let properties = HashMap::from([
            (
                "Address".to_string(),
                OwnedValue::from(Str::from("AA:BB:CC:DD:EE:FF")),
            ),
            (
                "Alias".to_string(),
                OwnedValue::from(Str::from("Pixel 8 Pro")),
            ),
            ("Trusted".to_string(), OwnedValue::from(true)),
            ("Paired".to_string(), OwnedValue::from(true)),
            ("Connected".to_string(), OwnedValue::from(false)),
        ]);

        let device = parse_device(&properties, true).unwrap();
        assert_eq!(device.address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(device.alias.as_deref(), Some("Pixel 8 Pro"));
        assert!(device.trusted);
        assert!(device.paired);
        assert!(device.auto_reconnect);
    }

    #[test]
    fn pairing_agent_rejects_requests_when_pairing_is_inactive() {
        let state = PairingAgentState::default();
        let error = state.ensure_pairing_active().unwrap_err();

        assert!(matches!(error, AgentError::Rejected(_)));
    }
}
