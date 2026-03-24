use anyhow::{Context, Result};
use orators_core::DeviceInfo;
use tokio::process::Command;

pub struct BluetoothCtlBluez;

impl BluetoothCtlBluez {
    pub async fn adapter_available(&self) -> Result<bool> {
        let output = self.run(["show"]).await?;
        Ok(output
            .lines()
            .any(|line| line.trim_start().starts_with("Controller ")))
    }

    pub async fn list_devices(&self, auto_reconnect: bool) -> Result<Vec<DeviceInfo>> {
        let output = self.run(["devices"]).await?;
        let mut devices = Vec::new();

        for line in output.lines() {
            if let Some((address, alias)) = parse_device_line(line) {
                let info = self.device_info(&address, alias, auto_reconnect).await?;
                devices.push(info);
            }
        }

        Ok(devices)
    }

    pub async fn start_pairing(&self, timeout_secs: u64) -> Result<()> {
        self.run(["power", "on"]).await?;
        self.run(["agent", "on"]).await?;
        self.run(["default-agent"]).await?;
        self.run(["pairable-timeout", &timeout_secs.to_string()])
            .await?;
        self.run(["discoverable-timeout", &timeout_secs.to_string()])
            .await?;
        self.run(["pairable", "on"]).await?;
        self.run(["discoverable", "on"]).await?;
        Ok(())
    }

    pub async fn stop_pairing(&self) -> Result<()> {
        self.run(["discoverable", "off"]).await?;
        self.run(["pairable", "off"]).await?;
        Ok(())
    }

    pub async fn trust_device(&self, address: &str) -> Result<()> {
        self.run(["trust", address]).await?;
        Ok(())
    }

    pub async fn forget_device(&self, address: &str) -> Result<()> {
        self.run(["remove", address]).await?;
        Ok(())
    }

    pub async fn connect_device(&self, address: &str) -> Result<()> {
        self.run(["connect", address]).await?;
        Ok(())
    }

    pub async fn disconnect_device(&self, address: &str) -> Result<()> {
        self.run(["disconnect", address]).await?;
        Ok(())
    }

    async fn device_info(
        &self,
        address: &str,
        alias: Option<String>,
        auto_reconnect: bool,
    ) -> Result<DeviceInfo> {
        let output = self.run(["info", address]).await?;
        Ok(parse_device_info(address, alias, auto_reconnect, &output))
    }

    async fn run<const N: usize>(&self, args: [&str; N]) -> Result<String> {
        let output = Command::new("bluetoothctl")
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to invoke bluetoothctl {:?}", args))?;

        if !output.status.success() {
            anyhow::bail!(
                "bluetoothctl {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

pub fn parse_device_line(line: &str) -> Option<(String, Option<String>)> {
    let trimmed = line.trim();
    let (prefix, remainder) = trimmed.split_once(' ')?;
    if prefix != "Device" {
        return None;
    }

    let (address, alias) = remainder.split_once(' ')?;
    Some((address.to_string(), Some(alias.trim().to_string())))
}

pub fn parse_device_info(
    address: &str,
    alias: Option<String>,
    auto_reconnect: bool,
    output: &str,
) -> DeviceInfo {
    let mut parsed_alias = alias;
    let mut trusted = false;
    let mut paired = false;
    let mut connected = false;

    for line in output.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("Alias: ") {
            parsed_alias = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Trusted: ") {
            trusted = value.trim() == "yes";
        } else if let Some(value) = line.strip_prefix("Paired: ") {
            paired = value.trim() == "yes";
        } else if let Some(value) = line.strip_prefix("Connected: ") {
            connected = value.trim() == "yes";
        }
    }

    DeviceInfo {
        address: address.to_string(),
        alias: parsed_alias,
        trusted,
        paired,
        connected,
        active_profile: None,
        auto_reconnect: auto_reconnect && trusted,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_device_info, parse_device_line};

    #[test]
    fn parses_device_line_with_spaces() {
        let parsed = parse_device_line("Device AA:BB:CC:DD:EE:FF Pixel 8 Pro").unwrap();
        assert_eq!(parsed.0, "AA:BB:CC:DD:EE:FF");
        assert_eq!(parsed.1.as_deref(), Some("Pixel 8 Pro"));
    }

    #[test]
    fn parses_device_info_output() {
        let output = r#"
Device AA:BB:CC:DD:EE:FF
    Alias: Pixel 8 Pro
    Paired: yes
    Trusted: yes
    Connected: no
"#;

        let device = parse_device_info("AA:BB:CC:DD:EE:FF", None, true, output);
        assert_eq!(device.alias.as_deref(), Some("Pixel 8 Pro"));
        assert!(device.paired);
        assert!(device.trusted);
        assert!(device.auto_reconnect);
    }
}
