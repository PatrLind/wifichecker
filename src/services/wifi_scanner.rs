use anyhow::Result;

#[derive(Debug, Clone)]
pub struct WifiInfo {
    pub ssid: String,
    pub bssid: String,
    pub frequency_mhz: u32,
    pub channel: u8,
    pub signal_dbm: i32,
    pub link_speed_mbps: Option<u32>,
    /// The WiFi device/interface this reading came from (e.g. "wlan0").
    pub device: String,
}

pub struct WifiScanner;

impl WifiScanner {
    /// The currently connected AP on the first active WiFi card.
    ///
    /// Works both on native installs and inside the Flatpak sandbox (where
    /// `nmcli` is not available as a subprocess), via NetworkManager D-Bus.
    pub fn scan() -> Result<Option<WifiInfo>> {
        super::nm_dbus::query_active_ap()
    }

    /// All WiFi cards that currently have an active connection, each tagged
    /// with its interface name in `device`.
    pub fn scan_all() -> Result<Vec<WifiInfo>> {
        super::nm_dbus::query_active_wifi_devices()
    }
}
