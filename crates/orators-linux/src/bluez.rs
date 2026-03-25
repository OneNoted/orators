use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, Result};
use orators_core::{BluetoothProfile, DeviceInfo};
use zbus::{
    Connection, DBusError, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue},
};

const BLUEZ_SERVICE: &str = "org.bluez";
const BLUEZ_ROOT_PATH: &str = "/";
const AGENT_MANAGER_PATH: &str = "/org/bluez";
const AGENT_PATH: &str = "/dev/orators/bluez/agent";
const A2DP_SOURCE_UUID: &str = "0000110a-0000-1000-8000-00805f9b34fb";
const A2DP_SINK_UUID: &str = "0000110b-0000-1000-8000-00805f9b34fb";
const NO_INPUT_NO_OUTPUT: &str = "NoInputNoOutput";

type InterfaceProperties = HashMap<String, HashMap<String, OwnedValue>>;
type ManagedObjects = HashMap<OwnedObjectPath, InterfaceProperties>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterInfo {
    pub address: Option<String>,
    pub alias: Option<String>,
    pub name: Option<String>,
    pub uuids: Vec<String>,
    pub powered: bool,
    pub discoverable: bool,
    pub pairable: bool,
    pub discovering: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDeviceInfo {
    pub address: String,
    pub alias: Option<String>,
    pub paired: bool,
    pub connected: bool,
    pub uuids: Vec<String>,
}

pub struct BluetoothCtlBluez {
    connection: Connection,
    agent_state: Arc<PairingAgentState>,
}

impl BluetoothCtlBluez {
    pub async fn new() -> Result<Self> {
        let connection = Connection::system()
            .await
            .context("failed to connect to system bus for BlueZ")?;
        let agent_state = Arc::new(PairingAgentState::new(connection.clone()));

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

    pub async fn adapter_info(&self) -> Result<AdapterInfo> {
        let managed = self.managed_objects().await?;
        let adapter = managed
            .values()
            .find_map(|interfaces| interfaces.get("org.bluez.Adapter1"))
            .context("no BlueZ adapter found")?;

        Ok(AdapterInfo {
            address: string_prop(adapter, "Address"),
            alias: string_prop(adapter, "Alias"),
            name: string_prop(adapter, "Name"),
            uuids: string_array_prop(adapter, "UUIDs").unwrap_or_default(),
            powered: bool_prop(adapter, "Powered").unwrap_or(false),
            discoverable: bool_prop(adapter, "Discoverable").unwrap_or(false),
            pairable: bool_prop(adapter, "Pairable").unwrap_or(false),
            discovering: bool_prop(adapter, "Discovering").unwrap_or(false),
        })
    }

    pub async fn list_devices(&self, auto_reconnect: bool) -> Result<Vec<DeviceInfo>> {
        let managed = self.managed_objects().await?;
        Ok(parse_devices(&managed, auto_reconnect))
    }

    pub async fn remote_devices(&self) -> Result<Vec<RemoteDeviceInfo>> {
        let managed = self.managed_objects().await?;
        Ok(parse_remote_devices(&managed))
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

        self.agent_state.set_pairing_window_open(true);
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
            self.agent_state.set_pairing_window_open(false);
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

        self.agent_state.set_pairing_window_open(false);
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

    pub async fn untrust_device(&self, address: &str) -> Result<()> {
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
            .set_property("Trusted", false)
            .await
            .with_context(|| format!("failed to untrust device {address}"))?;
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
        let managed = self.managed_objects().await?;
        let device_path = self.device_path(address).await?;
        let device = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            device_path.clone(),
            "org.bluez.Device1",
        )
        .await
        .with_context(|| format!("failed to create BlueZ device proxy for {address}"))?;
        let media_uuid = managed
            .get(&device_path)
            .and_then(|interfaces| interfaces.get("org.bluez.Device1"))
            .and_then(|properties| string_array_prop(properties, "UUIDs"))
            .as_deref()
            .and_then(select_media_profile_uuid);

        if let Some(media_uuid) = media_uuid {
            device
                .call_method("ConnectProfile", &(media_uuid))
                .await
                .with_context(|| format!("failed to connect media profile for device {address}"))?;
        } else {
            device
                .call_method("Connect", &())
                .await
                .with_context(|| format!("failed to connect device {address}"))?;
        }
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

struct PairingAgentState {
    pairing_window_open: AtomicBool,
    connection: Connection,
}

impl PairingAgentState {
    fn new(connection: Connection) -> Self {
        Self {
            pairing_window_open: AtomicBool::new(false),
            connection,
        }
    }

    fn set_pairing_window_open(&self, open: bool) {
        self.pairing_window_open.store(open, Ordering::SeqCst);
    }

    async fn authorize_device(
        &self,
        device: &OwnedObjectPath,
    ) -> std::result::Result<(), AgentError> {
        let device_state = self.lookup_device_state(device).await?;
        authorize_device_state(
            self.pairing_window_open.load(Ordering::SeqCst),
            device_state,
        )
    }

    async fn lookup_device_state(
        &self,
        device: &OwnedObjectPath,
    ) -> std::result::Result<DeviceAuthorizationState, AgentError> {
        let proxy = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            device.clone(),
            "org.bluez.Device1",
        )
        .await
        .map_err(AgentError::ZBus)?;

        let paired = proxy
            .get_property("Paired")
            .await
            .map_err(AgentError::ZBus)?;
        let trusted = proxy
            .get_property("Trusted")
            .await
            .map_err(AgentError::ZBus)?;
        let connected = proxy
            .get_property("Connected")
            .await
            .map_err(AgentError::ZBus)?;

        Ok(DeviceAuthorizationState {
            paired,
            trusted,
            connected,
        })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceAuthorizationState {
    paired: bool,
    trusted: bool,
    connected: bool,
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

    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        _passkey: u32,
    ) -> std::result::Result<(), AgentError> {
        self.state.authorize_device(&device).await
    }

    async fn request_authorization(
        &self,
        device: OwnedObjectPath,
    ) -> std::result::Result<(), AgentError> {
        self.state.authorize_device(&device).await
    }

    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        _uuid: &str,
    ) -> std::result::Result<(), AgentError> {
        self.state.authorize_device(&device).await
    }

    fn cancel(&self) {}
}

fn parse_devices(managed: &ManagedObjects, auto_reconnect: bool) -> Vec<DeviceInfo> {
    let active_profiles = parse_active_profiles(managed);
    managed
        .iter()
        .filter_map(|(path, interfaces)| {
            interfaces.get("org.bluez.Device1").and_then(|properties| {
                parse_device(path, properties, &active_profiles, auto_reconnect)
            })
        })
        .collect()
}

fn parse_device(
    path: &OwnedObjectPath,
    properties: &HashMap<String, OwnedValue>,
    active_profiles: &HashMap<String, BluetoothProfile>,
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
        active_profile: active_profiles.get(path.as_str()).cloned(),
        auto_reconnect: auto_reconnect && trusted,
    })
}

fn parse_active_profiles(managed: &ManagedObjects) -> HashMap<String, BluetoothProfile> {
    managed
        .values()
        .filter_map(|interfaces| interfaces.get("org.bluez.MediaTransport1"))
        .filter_map(parse_transport_profile)
        .collect()
}

fn parse_transport_profile(
    properties: &HashMap<String, OwnedValue>,
) -> Option<(String, BluetoothProfile)> {
    let device_path = object_path_prop(properties, "Device")?;
    let uuid = string_prop(properties, "UUID")?;
    let profile = profile_from_uuid(&uuid)?;
    Some((device_path.as_str().to_string(), profile))
}

fn profile_from_uuid(uuid: &str) -> Option<BluetoothProfile> {
    let uuid = uuid.to_ascii_lowercase();
    if matches!(uuid.as_str(), A2DP_SOURCE_UUID | A2DP_SINK_UUID) {
        Some(BluetoothProfile::Media)
    } else if matches!(
        uuid.as_str(),
        "00001108-0000-1000-8000-00805f9b34fb"
            | "00001112-0000-1000-8000-00805f9b34fb"
            | "0000111e-0000-1000-8000-00805f9b34fb"
            | "0000111f-0000-1000-8000-00805f9b34fb"
    ) {
        Some(BluetoothProfile::Call)
    } else {
        None
    }
}

fn select_media_profile_uuid(uuids: &[String]) -> Option<&'static str> {
    if uuids
        .iter()
        .any(|uuid| uuid.eq_ignore_ascii_case(A2DP_SOURCE_UUID))
    {
        Some(A2DP_SOURCE_UUID)
    } else if uuids
        .iter()
        .any(|uuid| uuid.eq_ignore_ascii_case(A2DP_SINK_UUID))
    {
        Some(A2DP_SINK_UUID)
    } else {
        None
    }
}

fn authorize_device_state(
    pairing_window_open: bool,
    device: DeviceAuthorizationState,
) -> std::result::Result<(), AgentError> {
    if pairing_window_open || device.paired || device.trusted || device.connected {
        Ok(())
    } else {
        Err(AgentError::Rejected(
            "pairing window is closed for new devices".to_string(),
        ))
    }
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

fn string_array_prop(properties: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<String>> {
    properties
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| <Vec<String>>::try_from(value).ok())
}

fn object_path_prop(
    properties: &HashMap<String, OwnedValue>,
    key: &str,
) -> Option<OwnedObjectPath> {
    properties
        .get(key)
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| OwnedObjectPath::try_from(value).ok())
}

fn parse_remote_devices(managed: &ManagedObjects) -> Vec<RemoteDeviceInfo> {
    managed
        .values()
        .filter_map(|interfaces| interfaces.get("org.bluez.Device1"))
        .filter_map(|properties| {
            Some(RemoteDeviceInfo {
                address: string_prop(properties, "Address")?,
                alias: string_prop(properties, "Alias").or_else(|| string_prop(properties, "Name")),
                paired: bool_prop(properties, "Paired").unwrap_or(false),
                connected: bool_prop(properties, "Connected").unwrap_or(false),
                uuids: string_array_prop(properties, "UUIDs").unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use orators_core::BluetoothProfile;
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Str};

    use super::{
        A2DP_SINK_UUID, A2DP_SOURCE_UUID, AdapterInfo, AgentError, DeviceAuthorizationState,
        authorize_device_state, parse_device, parse_transport_profile, select_media_profile_uuid,
    };

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

        let device = parse_device(
            &OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").unwrap(),
            &properties,
            &HashMap::new(),
            true,
        )
        .unwrap();
        assert_eq!(device.address, "AA:BB:CC:DD:EE:FF");
        assert_eq!(device.alias.as_deref(), Some("Pixel 8 Pro"));
        assert!(device.trusted);
        assert!(device.paired);
        assert!(device.auto_reconnect);
    }

    #[test]
    fn pairing_agent_rejects_new_devices_when_pairing_is_inactive() {
        let error = authorize_device_state(
            false,
            DeviceAuthorizationState {
                paired: false,
                trusted: false,
                connected: false,
            },
        )
        .unwrap_err();

        assert!(matches!(error, AgentError::Rejected(_)));
    }

    #[test]
    fn pairing_agent_allows_known_devices_when_pairing_is_inactive() {
        authorize_device_state(
            false,
            DeviceAuthorizationState {
                paired: true,
                trusted: false,
                connected: false,
            },
        )
        .unwrap();
    }

    #[test]
    fn parses_transport_profile_from_a2dp_uuid() {
        let properties = HashMap::from([
            (
                "Device".to_string(),
                OwnedValue::from(
                    ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").unwrap(),
                ),
            ),
            (
                "UUID".to_string(),
                OwnedValue::from(Str::from("0000110b-0000-1000-8000-00805f9b34fb")),
            ),
        ]);

        let (device_path, profile) = parse_transport_profile(&properties).unwrap();
        assert_eq!(device_path, "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF");
        assert_eq!(profile, BluetoothProfile::Media);
    }

    #[test]
    fn parses_transport_profile_from_hfp_uuid() {
        let properties = HashMap::from([
            (
                "Device".to_string(),
                OwnedValue::from(
                    ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").unwrap(),
                ),
            ),
            (
                "UUID".to_string(),
                OwnedValue::from(Str::from("0000111f-0000-1000-8000-00805f9b34fb")),
            ),
        ]);

        let (device_path, profile) = parse_transport_profile(&properties).unwrap();
        assert_eq!(device_path, "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF");
        assert_eq!(profile, BluetoothProfile::Call);
    }

    #[test]
    fn prefers_a2dp_source_uuid_when_available() {
        let uuids = vec![
            "0000110b-0000-1000-8000-00805f9b34fb".to_string(),
            "0000110a-0000-1000-8000-00805f9b34fb".to_string(),
        ];

        assert_eq!(select_media_profile_uuid(&uuids), Some(A2DP_SOURCE_UUID));
    }

    #[test]
    fn falls_back_to_a2dp_sink_uuid_when_source_is_missing() {
        let uuids = vec!["0000110b-0000-1000-8000-00805f9b34fb".to_string()];

        assert_eq!(select_media_profile_uuid(&uuids), Some(A2DP_SINK_UUID));
    }

    #[test]
    fn returns_none_when_no_media_uuid_is_available() {
        let uuids = vec!["00001108-0000-1000-8000-00805f9b34fb".to_string()];

        assert_eq!(select_media_profile_uuid(&uuids), None);
    }

    #[test]
    fn adapter_info_captures_pairing_state() {
        let info = AdapterInfo {
            address: Some("04:7F:0E:02:13:3C".to_string()),
            alias: Some("aeolus".to_string()),
            name: Some("aeolus".to_string()),
            uuids: Vec::new(),
            powered: true,
            discoverable: true,
            pairable: true,
            discovering: false,
        };

        assert!(info.powered);
        assert!(info.discoverable);
        assert_eq!(info.alias.as_deref(), Some("aeolus"));
    }
}
