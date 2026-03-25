use std::collections::BTreeMap;

use crate::{
    OratorsConfig,
    error::{OratorsError, Result},
    types::{
        AudioDefaults, BluetoothProfile, DeviceInfo, MediaBackendStatus, PairingWindow,
        RuntimeStatus,
    },
};

#[derive(Debug, Clone)]
pub struct OratorsState {
    config: OratorsConfig,
    devices: BTreeMap<String, DeviceInfo>,
    pairing_until_epoch_secs: Option<u64>,
    active_device: Option<String>,
    audio: AudioDefaults,
    backend: MediaBackendStatus,
}

impl OratorsState {
    pub fn new(config: OratorsConfig) -> Self {
        Self {
            config,
            devices: BTreeMap::new(),
            pairing_until_epoch_secs: None,
            active_device: None,
            audio: AudioDefaults::default(),
            backend: MediaBackendStatus::default(),
        }
    }

    pub fn config(&self) -> &OratorsConfig {
        &self.config
    }

    pub fn sync_devices(&mut self, observed: Vec<DeviceInfo>) {
        let existing = self.devices.clone();
        self.devices = observed
            .into_iter()
            .map(|mut device| {
                if let Some(previous) = existing.get(&device.address) {
                    if device.active_profile.is_none() {
                        device.active_profile = previous.active_profile.clone();
                    }
                }

                if self.config.auto_reconnect && device.trusted {
                    device.auto_reconnect = true;
                }

                (device.address.clone(), device)
            })
            .collect();

        if let Some(active) = self.active_device.as_ref() {
            let keep_active = self
                .devices
                .get(active)
                .is_some_and(|device| device.connected);
            if !keep_active {
                self.active_device = None;
            }
        }
    }

    pub fn start_pairing(
        &mut self,
        now_epoch_secs: u64,
        timeout_secs: Option<u64>,
    ) -> PairingWindow {
        let timeout_secs = timeout_secs.unwrap_or(self.config.pairing_timeout_secs);
        self.pairing_until_epoch_secs = Some(now_epoch_secs + timeout_secs);
        self.pairing_window(now_epoch_secs)
    }

    pub fn stop_pairing(&mut self) -> PairingWindow {
        self.pairing_until_epoch_secs = None;
        self.pairing_window(0)
    }

    pub fn tick(&mut self, now_epoch_secs: u64) -> bool {
        if self
            .pairing_until_epoch_secs
            .is_some_and(|until| now_epoch_secs >= until)
        {
            self.pairing_until_epoch_secs = None;
            return true;
        }

        false
    }

    pub fn trust_device(&mut self, address: &str) -> Result<()> {
        let device = self
            .devices
            .get_mut(address)
            .ok_or_else(|| OratorsError::UnknownDevice(address.to_string()))?;
        device.trusted = true;
        device.auto_reconnect = self.config.auto_reconnect;
        Ok(())
    }

    pub fn untrust_device(&mut self, address: &str) -> Result<()> {
        let device = self
            .devices
            .get_mut(address)
            .ok_or_else(|| OratorsError::UnknownDevice(address.to_string()))?;
        device.trusted = false;
        device.auto_reconnect = false;
        Ok(())
    }

    pub fn forget_device(&mut self, address: &str) -> Result<()> {
        if self.devices.remove(address).is_none() {
            return Err(OratorsError::UnknownDevice(address.to_string()));
        }

        if self.active_device.as_deref() == Some(address) {
            self.active_device = None;
        }

        Ok(())
    }

    pub fn connect_device(&mut self, address: &str, profile: BluetoothProfile) -> Result<()> {
        let device = self.ensure_connectable(address)?;
        device.connected = true;
        device.active_profile = Some(profile);
        self.active_device = Some(address.to_string());
        Ok(())
    }

    pub fn can_connect_device(&self, address: &str) -> Result<()> {
        if self.config.single_active_device
            && self
                .active_device
                .as_ref()
                .is_some_and(|active| active != address)
        {
            return Err(OratorsError::AlreadyActiveDevice(
                self.active_device.clone().unwrap_or_default(),
            ));
        }

        self.devices
            .get(address)
            .ok_or_else(|| OratorsError::UnknownDevice(address.to_string()))?;
        Ok(())
    }

    pub fn disconnect_active(&mut self) -> Option<String> {
        let active = self.active_device.take()?;
        if let Some(device) = self.devices.get_mut(&active) {
            device.connected = false;
            device.active_profile = None;
        }
        self.backend.active_device_address = None;
        self.backend.player_running = false;
        self.backend.player_state = crate::types::PlayerState::Waiting;
        Some(active)
    }

    pub fn update_audio(&mut self, audio: AudioDefaults) {
        self.audio = audio;
    }

    pub fn update_backend(&mut self, backend: MediaBackendStatus) {
        self.active_device = backend.active_device_address.clone();
        self.backend = backend;

        for device in self.devices.values_mut() {
            if self.active_device.as_deref() == Some(device.address.as_str()) {
                device.active_profile = Some(BluetoothProfile::Media);
            } else if matches!(device.active_profile, Some(BluetoothProfile::Media)) {
                device.active_profile = None;
            }
        }
    }

    pub fn pairing_window(&self, now_epoch_secs: u64) -> PairingWindow {
        PairingWindow {
            enabled: self
                .pairing_until_epoch_secs
                .is_some_and(|until| now_epoch_secs < until),
            timeout_secs: self.config.pairing_timeout_secs,
            expires_at_epoch_secs: self.pairing_until_epoch_secs,
        }
    }

    pub fn status(&mut self, now_epoch_secs: u64) -> RuntimeStatus {
        self.tick(now_epoch_secs);
        RuntimeStatus {
            pairing: self.pairing_window(now_epoch_secs),
            active_device: self.active_device.clone(),
            devices: self.devices.values().cloned().collect(),
            audio: self.audio.clone(),
            backend: self.backend.clone(),
        }
    }

    fn ensure_connectable(&mut self, address: &str) -> Result<&mut DeviceInfo> {
        self.can_connect_device(address)?;
        self.devices
            .get_mut(address)
            .ok_or_else(|| OratorsError::UnknownDevice(address.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use crate::{BluetoothProfile, DeviceInfo, OratorsConfig, OratorsState};

    fn sample_device(address: &str) -> DeviceInfo {
        DeviceInfo {
            address: address.to_string(),
            alias: Some("Phone".to_string()),
            trusted: false,
            paired: true,
            connected: false,
            active_profile: None,
            auto_reconnect: false,
        }
    }

    #[test]
    fn pairing_window_expires() {
        let mut state = OratorsState::new(OratorsConfig::default());
        let pairing = state.start_pairing(100, Some(10));
        assert!(pairing.enabled);
        assert_eq!(pairing.expires_at_epoch_secs, Some(110));

        let status = state.status(111);
        assert!(!status.pairing.enabled);
    }

    #[test]
    fn single_active_device_is_enforced() {
        let mut state = OratorsState::new(OratorsConfig::default());
        state.sync_devices(vec![sample_device("AA"), sample_device("BB")]);

        state.connect_device("AA", BluetoothProfile::Media).unwrap();
        let error = state
            .connect_device("BB", BluetoothProfile::Call)
            .unwrap_err();
        assert!(error.to_string().contains("already active"));
    }

    #[test]
    fn forgetting_device_clears_active_state() {
        let mut state = OratorsState::new(OratorsConfig::default());
        state.sync_devices(vec![sample_device("AA")]);
        state.connect_device("AA", BluetoothProfile::Media).unwrap();

        state.forget_device("AA").unwrap();

        let status = state.status(10);
        assert!(status.active_device.is_none());
        assert!(status.devices.is_empty());
    }
}
