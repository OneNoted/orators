use std::{
    collections::HashMap,
    io::{Read, Write},
    os::fd::OwnedFd as StdOwnedFd,
    process::{Child, Command as StdCommand, Stdio},
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use orators_core::{MediaBackendStatus, MediaCodec};
use tokio::sync::Mutex;
use zbus::{
    Connection, DBusError, Proxy,
    zvariant::{OwnedFd, OwnedObjectPath, OwnedValue, Value},
};

const BLUEZ_SERVICE: &str = "org.bluez";
const BLUEZ_ROOT_PATH: &str = "/";
const A2DP_SINK_UUID: &str = "0000110b-0000-1000-8000-00805f9b34fb";
const SBC_ENDPOINT_PATH: &str = "/dev/orators/bluez/media/sbc";
const AAC_ENDPOINT_PATH: &str = "/dev/orators/bluez/media/aac";
const SBC_CODEC_ID: u8 = 0x00;
const AAC_CODEC_ID: u8 = 0x02;
const RTP_HEADER_LEN: usize = 12;

type InterfaceProperties = HashMap<String, HashMap<String, OwnedValue>>;
type ManagedObjects = HashMap<OwnedObjectPath, InterfaceProperties>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecKind {
    Sbc,
    Aac,
}

impl From<CodecKind> for MediaCodec {
    fn from(value: CodecKind) -> Self {
        match value {
            CodecKind::Sbc => Self::Sbc,
            CodecKind::Aac => Self::Aac,
        }
    }
}

impl CodecKind {
    fn codec_id(self) -> u8 {
        match self {
            Self::Sbc => SBC_CODEC_ID,
            Self::Aac => AAC_CODEC_ID,
        }
    }

    fn endpoint_path(self) -> &'static str {
        match self {
            Self::Sbc => SBC_ENDPOINT_PATH,
            Self::Aac => AAC_ENDPOINT_PATH,
        }
    }

    fn capability_blob(self) -> Vec<u8> {
        match self {
            Self::Sbc => vec![0x21, 0x15, 2, 53],
            Self::Aac => vec![0x80, 0x01, 0x8c, 0x82, 0x00, 0x00],
        }
    }

    fn ffmpeg_format(self) -> &'static str {
        match self {
            Self::Sbc => "sbc",
            Self::Aac => "latm",
        }
    }

    fn select_configuration(self, capabilities: &[u8]) -> Result<SelectedConfiguration> {
        match self {
            Self::Sbc => select_sbc_configuration(capabilities),
            Self::Aac => select_aac_configuration(capabilities),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedBackendSnapshot {
    pub endpoints_registered: bool,
    pub active_codec: Option<CodecKind>,
    pub transport_acquired: bool,
    pub playback_connected: bool,
    pub last_error: Option<String>,
}

struct OwnedBackendState {
    endpoints_registered: bool,
    active_codec: Option<CodecKind>,
    transport_acquired: bool,
    playback_connected: bool,
    last_error: Option<String>,
    transports: HashMap<String, TransportRecord>,
}

impl OwnedBackendState {
    fn snapshot(&self) -> OwnedBackendSnapshot {
        OwnedBackendSnapshot {
            endpoints_registered: self.endpoints_registered,
            active_codec: self.active_codec,
            transport_acquired: self.transport_acquired,
            playback_connected: self.playback_connected,
            last_error: self.last_error.clone(),
        }
    }
}

impl From<OwnedBackendSnapshot> for MediaBackendStatus {
    fn from(value: OwnedBackendSnapshot) -> Self {
        Self {
            endpoints_registered: value.endpoints_registered,
            active_codec: value.active_codec.map(MediaCodec::from),
            transport_acquired: value.transport_acquired,
            playback_connected: value.playback_connected,
            last_error: value.last_error,
        }
    }
}

struct TransportRecord {
    codec: CodecKind,
    worker: Option<TransportWorker>,
}

struct TransportWorker {
    ffmpeg: Child,
    pw_play: Child,
    reader: Option<thread::JoinHandle<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedConfiguration {
    bytes: Vec<u8>,
    sample_rate: u32,
    channels: u32,
}

pub struct OwnedBluetoothMediaBackend {
    connection: Connection,
    state: Arc<Mutex<OwnedBackendState>>,
}

impl OwnedBluetoothMediaBackend {
    pub async fn new() -> Result<Self> {
        let connection = Connection::system()
            .await
            .context("failed to connect to system bus for BlueZ media backend")?;
        let state = Arc::new(Mutex::new(OwnedBackendState {
            endpoints_registered: false,
            active_codec: None,
            transport_acquired: false,
            playback_connected: false,
            last_error: None,
            transports: HashMap::new(),
        }));

        connection
            .object_server()
            .at(
                SBC_ENDPOINT_PATH,
                MediaEndpoint::new(Arc::clone(&state), connection.clone(), CodecKind::Sbc),
            )
            .await
            .context("failed to export SBC MediaEndpoint1 object")?;
        connection
            .object_server()
            .at(
                AAC_ENDPOINT_PATH,
                MediaEndpoint::new(Arc::clone(&state), connection.clone(), CodecKind::Aac),
            )
            .await
            .context("failed to export AAC MediaEndpoint1 object")?;

        let backend = Self { connection, state };
        backend.register_endpoints().await?;
        Ok(backend)
    }

    pub async fn snapshot(&self) -> OwnedBackendSnapshot {
        self.state.lock().await.snapshot()
    }

    pub async fn snapshot_status(&self) -> MediaBackendStatus {
        self.snapshot().await.into()
    }

    async fn register_endpoints(&self) -> Result<()> {
        let media_path = self.media_path().await?;
        let proxy = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            media_path,
            "org.bluez.Media1",
        )
        .await
        .context("failed to create BlueZ Media1 proxy")?;

        for codec in [CodecKind::Sbc, CodecKind::Aac] {
            let mut properties: HashMap<&str, Value<'_>> = HashMap::new();
            properties.insert("UUID", Value::from(A2DP_SINK_UUID));
            properties.insert("Codec", Value::from(codec.codec_id()));
            properties.insert("Capabilities", Value::from(codec.capability_blob()));
            proxy
                .call_method(
                    "RegisterEndpoint",
                    &(
                        OwnedObjectPath::try_from(codec.endpoint_path())?,
                        properties,
                    ),
                )
                .await
                .with_context(|| format!("failed to register {:?} endpoint", codec))?;
        }

        self.state.lock().await.endpoints_registered = true;
        Ok(())
    }

    async fn media_path(&self) -> Result<OwnedObjectPath> {
        let proxy = Proxy::new(
            &self.connection,
            BLUEZ_SERVICE,
            BLUEZ_ROOT_PATH,
            "org.freedesktop.DBus.ObjectManager",
        )
        .await
        .context("failed to create BlueZ object manager proxy")?;
        let managed: ManagedObjects = proxy
            .call("GetManagedObjects", &())
            .await
            .context("failed to fetch BlueZ managed objects for media backend")?;
        managed
            .into_iter()
            .find_map(|(path, interfaces)| {
                interfaces.contains_key("org.bluez.Media1").then_some(path)
            })
            .ok_or_else(|| anyhow!("no BlueZ Media1 object was found"))
    }
}

struct MediaEndpoint {
    state: Arc<Mutex<OwnedBackendState>>,
    connection: Connection,
    codec: CodecKind,
}

impl MediaEndpoint {
    fn new(state: Arc<Mutex<OwnedBackendState>>, connection: Connection, codec: CodecKind) -> Self {
        Self {
            state,
            connection,
            codec,
        }
    }
}

#[derive(Debug, DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum MediaEndpointError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
}

#[zbus::interface(name = "org.bluez.MediaEndpoint1")]
impl MediaEndpoint {
    fn release(&self) {}

    fn select_configuration(
        &self,
        capabilities: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, MediaEndpointError> {
        self.codec
            .select_configuration(&capabilities)
            .map(|selected| selected.bytes)
            .map_err(|error| MediaEndpointError::Rejected(error.to_string()))
    }

    async fn set_configuration(
        &self,
        transport: OwnedObjectPath,
        properties: HashMap<String, OwnedValue>,
    ) -> std::result::Result<(), MediaEndpointError> {
        let configuration = properties
            .get("Configuration")
            .and_then(|value| value.try_clone().ok())
            .and_then(|value| <Vec<u8>>::try_from(value).ok())
            .ok_or_else(|| {
                MediaEndpointError::Rejected("missing transport configuration".to_string())
            })?;

        let selected = self
            .codec
            .select_configuration(&configuration)
            .map_err(|error| MediaEndpointError::Rejected(error.to_string()))?;
        let transport_key = transport.as_str().to_string();

        {
            let mut state = self.state.lock().await;
            state.transports.insert(
                transport_key.clone(),
                TransportRecord {
                    codec: self.codec,
                    worker: None,
                },
            );
        }

        let state = Arc::clone(&self.state);
        let connection = self.connection.clone();
        tokio::spawn(async move {
            let state_for_loop = Arc::clone(&state);
            if let Err(error) =
                acquire_transport_loop(connection, state_for_loop, transport_key.clone(), selected)
                    .await
            {
                let mut state = state.lock().await;
                state.last_error = Some(error.to_string());
            }
        });

        Ok(())
    }

    async fn clear_configuration(
        &self,
        transport: OwnedObjectPath,
    ) -> std::result::Result<(), MediaEndpointError> {
        let transport_key = transport.as_str().to_string();
        let worker = {
            let mut state = self.state.lock().await;
            let record = state.transports.remove(&transport_key);
            state.transport_acquired = false;
            state.playback_connected = false;
            state.active_codec = None;
            record.and_then(|record| record.worker)
        };

        if let Some(mut worker) = worker {
            let _ = release_transport(&self.connection, &transport_key).await;
            let _ = worker.ffmpeg.kill();
            let _ = worker.pw_play.kill();
            if let Some(reader) = worker.reader.take() {
                let _ = reader.join();
            }
        }

        Ok(())
    }
}

async fn acquire_transport_loop(
    connection: Connection,
    state: Arc<Mutex<OwnedBackendState>>,
    transport_key: String,
    selected: SelectedConfiguration,
) -> Result<()> {
    for _ in 0..300 {
        {
            if !state.lock().await.transports.contains_key(&transport_key) {
                return Ok(());
            }
        }

        match try_acquire_transport(&connection, &transport_key).await {
            Ok((fd, _read_mtu, _write_mtu)) => {
                let mut worker = spawn_worker(fd, state.clone(), &transport_key, selected.clone())
                    .context("failed to spawn owned playback worker")?;
                let mut state_guard = state.lock().await;
                if let Some(codec) = state_guard
                    .transports
                    .get(&transport_key)
                    .map(|record| record.codec)
                {
                    state_guard.transport_acquired = true;
                    state_guard.playback_connected = true;
                    state_guard.active_codec = Some(codec);
                    if let Some(record) = state_guard.transports.get_mut(&transport_key) {
                        record.worker = Some(worker);
                    }
                } else {
                    let _ = worker.ffmpeg.kill();
                    let _ = worker.pw_play.kill();
                }
                return Ok(());
            }
            Err(error) if error.to_string().contains("NotAvailable") => {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(error) => return Err(error),
        }
    }

    Err(anyhow!(
        "timed out waiting for BlueZ media transport to enter pending state"
    ))
}

async fn try_acquire_transport(
    connection: &Connection,
    transport_path: &str,
) -> Result<(OwnedFd, u16, u16)> {
    let proxy = Proxy::new(
        connection,
        BLUEZ_SERVICE,
        transport_path,
        "org.bluez.MediaTransport1",
    )
    .await
    .with_context(|| format!("failed to create transport proxy for {transport_path}"))?;
    proxy
        .call("TryAcquire", &())
        .await
        .with_context(|| format!("failed to acquire transport {transport_path}"))
}

async fn release_transport(connection: &Connection, transport_path: &str) -> Result<()> {
    let proxy = Proxy::new(
        connection,
        BLUEZ_SERVICE,
        transport_path,
        "org.bluez.MediaTransport1",
    )
    .await
    .with_context(|| format!("failed to create transport proxy for {transport_path}"))?;
    proxy
        .call_method("Release", &())
        .await
        .with_context(|| format!("failed to release transport {transport_path}"))?;
    Ok(())
}

fn spawn_worker(
    fd: OwnedFd,
    state: Arc<Mutex<OwnedBackendState>>,
    transport_key: &str,
    selected: SelectedConfiguration,
) -> Result<TransportWorker> {
    let mut ffmpeg = StdCommand::new("ffmpeg");
    ffmpeg
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-fflags")
        .arg("+nobuffer")
        .arg("-f")
        .arg(
            state
                .blocking_lock()
                .transports
                .get(transport_key)
                .map(|record| record.codec.ffmpeg_format())
                .unwrap_or("sbc"),
        )
        .arg("-i")
        .arg("pipe:0")
        .arg("-f")
        .arg("s16le")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-ac")
        .arg(selected.channels.to_string())
        .arg("-ar")
        .arg(selected.sample_rate.to_string())
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut ffmpeg = ffmpeg.spawn().context("failed to spawn ffmpeg decoder")?;
    let decoder_stdin = ffmpeg
        .stdin
        .take()
        .context("failed to capture ffmpeg stdin")?;
    let decoder_stdout = ffmpeg
        .stdout
        .take()
        .context("failed to capture ffmpeg stdout")?;

    let mut pw_play = StdCommand::new("pw-play");
    pw_play
        .arg("--raw")
        .arg("--format")
        .arg("s16")
        .arg("--channels")
        .arg(selected.channels.to_string())
        .arg("--rate")
        .arg(selected.sample_rate.to_string())
        .arg("-")
        .stdin(Stdio::from(decoder_stdout))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let pw_play = pw_play.spawn().context("failed to spawn pw-play")?;

    let codec = state
        .blocking_lock()
        .transports
        .get(transport_key)
        .map(|record| record.codec)
        .unwrap_or(CodecKind::Sbc);
    let reader = thread::spawn(move || {
        let fd: StdOwnedFd = fd.into();
        let mut transport = std::fs::File::from(fd);
        let mut decoder_stdin = decoder_stdin;
        let mut buffer = vec![0u8; 4096];
        loop {
            match transport.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if let Ok(payload) = extract_media_payload(codec, &buffer[..read]) {
                        if decoder_stdin.write_all(&payload).is_err() {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(TransportWorker {
        ffmpeg,
        pw_play,
        reader: Some(reader),
    })
}

fn extract_media_payload(codec: CodecKind, packet: &[u8]) -> Result<Vec<u8>> {
    if packet.len() <= RTP_HEADER_LEN {
        anyhow::bail!("RTP packet was too short");
    }
    let payload = &packet[RTP_HEADER_LEN..];
    match codec {
        CodecKind::Sbc => {
            if payload.is_empty() {
                anyhow::bail!("SBC payload header was missing");
            }
            Ok(payload[1..].to_vec())
        }
        CodecKind::Aac => Ok(payload.to_vec()),
    }
}

fn select_sbc_configuration(capabilities: &[u8]) -> Result<SelectedConfiguration> {
    if capabilities.len() < 4 {
        anyhow::bail!("remote SBC capability blob was too short");
    }

    let frequencies = capabilities[0] & 0b1111_0000;
    let channel_modes = capabilities[0] & 0b0000_1111;
    let blocks = capabilities[1] & 0b1111_0000;
    let subbands = capabilities[1] & 0b0000_1100;
    let allocation = capabilities[1] & 0b0000_0011;
    let min_bitpool = capabilities[2].max(2);
    let max_bitpool = capabilities[3].min(53);

    let sample_rate = if frequencies & 0x10 != 0 {
        48_000
    } else if frequencies & 0x20 != 0 {
        44_100
    } else {
        anyhow::bail!("remote SBC capabilities do not include 44.1 kHz or 48 kHz");
    };
    let (channel_flag, channels) = if channel_modes & 0x01 != 0 {
        (0x01, 2)
    } else if channel_modes & 0x02 != 0 {
        (0x02, 2)
    } else {
        anyhow::bail!("remote SBC capabilities do not include a stereo mode");
    };

    let block_flag = if blocks & 0x10 != 0 {
        0x10
    } else {
        anyhow::bail!("remote SBC capabilities do not include 16-block mode");
    };
    let subband_flag = if subbands & 0x04 != 0 {
        0x04
    } else {
        anyhow::bail!("remote SBC capabilities do not include 8 subbands");
    };
    let allocation_flag = if allocation & 0x01 != 0 { 0x01 } else { 0x02 };

    Ok(SelectedConfiguration {
        bytes: vec![
            match sample_rate {
                48_000 => 0x10,
                _ => 0x20,
            } | channel_flag,
            block_flag | subband_flag | allocation_flag,
            min_bitpool,
            max_bitpool,
        ],
        sample_rate,
        channels,
    })
}

fn select_aac_configuration(capabilities: &[u8]) -> Result<SelectedConfiguration> {
    if capabilities.len() < 6 {
        anyhow::bail!("remote AAC capability blob was too short");
    }

    let frequency = if capabilities[1] & 0x01 != 0 {
        48_000
    } else if capabilities[2] & 0x80 != 0 {
        44_100
    } else {
        anyhow::bail!("remote AAC capabilities do not include 44.1 kHz or 48 kHz");
    };

    let channels = if capabilities[2] & 0x08 != 0 {
        2
    } else if capabilities[2] & 0x04 != 0 {
        1
    } else {
        anyhow::bail!("remote AAC capabilities do not include a supported channel mode");
    };

    let mut selected = capabilities[..6].to_vec();
    if frequency == 48_000 {
        selected[1] = 0x01;
        selected[2] = if channels == 2 { 0x08 } else { 0x04 };
    } else {
        selected[1] = 0x00;
        selected[2] = 0x80 | if channels == 2 { 0x08 } else { 0x04 };
    }

    Ok(SelectedConfiguration {
        bytes: selected,
        sample_rate: frequency,
        channels,
    })
}

#[cfg(test)]
mod tests {
    use orators_core::{MediaBackendStatus, MediaCodec};

    fn extract_device_address(path: &str) -> Option<String> {
        let suffix = path.split("/dev_").nth(1)?;
        Some(suffix.replace('_', ":"))
    }

    use super::{CodecKind, OwnedBackendSnapshot, extract_media_payload};

    #[test]
    fn extracts_device_address_from_bluez_path() {
        assert_eq!(
            extract_device_address("/org/bluez/hci0/dev_5C_DC_49_92_D0_D8").as_deref(),
            Some("5C:DC:49:92:D0:D8")
        );
    }

    #[test]
    fn strips_sbc_rtp_header() {
        let packet = [0u8; 12]
            .into_iter()
            .chain([1u8, 0x9c, 0x00, 0x10])
            .collect::<Vec<_>>();
        let payload = extract_media_payload(CodecKind::Sbc, &packet).unwrap();
        assert_eq!(payload, vec![0x9c, 0x00, 0x10]);
    }

    #[test]
    fn keeps_aac_payload_intact() {
        let packet = [0u8; 12]
            .into_iter()
            .chain([0x56u8, 0xe0, 0x12, 0x34])
            .collect::<Vec<_>>();
        let payload = extract_media_payload(CodecKind::Aac, &packet).unwrap();
        assert_eq!(payload, vec![0x56, 0xe0, 0x12, 0x34]);
    }

    #[test]
    fn converts_owned_backend_snapshot_to_public_status() {
        let snapshot = OwnedBackendSnapshot {
            endpoints_registered: true,
            active_codec: Some(CodecKind::Aac),
            transport_acquired: true,
            playback_connected: false,
            last_error: Some("boom".to_string()),
        };

        let status: MediaBackendStatus = snapshot.into();
        assert!(status.endpoints_registered);
        assert_eq!(status.active_codec, Some(MediaCodec::Aac));
        assert!(status.transport_acquired);
        assert!(!status.playback_connected);
        assert_eq!(status.last_error.as_deref(), Some("boom"));
    }
}
