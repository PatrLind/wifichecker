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
    /// True when this AP is the one the computer is associated with.
    pub is_active: bool,
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

    /// All APs in range for one WiFi card (its full scan list), strongest
    /// first, with the associated AP flagged `is_active`.
    ///
    /// `device`: the interface to scan (e.g. "wlan0"); `None` uses the first
    /// WiFi device. Requests a fresh scan first (waits up to ~6 s for it to
    /// complete) and falls back to the cached list if that fails.
    pub fn scan_list(device: Option<&str>) -> Result<Vec<WifiInfo>> {
        super::nm_dbus::query_scan_list(device, true)
    }

    /// The cached scan list for one WiFi card, re-read only when NM has
    /// completed a new scan since `prev_last_scan` (its `LastScan` counter
    /// advanced). Returns `(new_last_scan, list)` when it changed, else
    /// `None` — cheap enough for the periodic live-signal refresh.
    pub fn scan_list_if_newer(device: Option<&str>, prev_last_scan: i64) -> Result<Option<(i64, Vec<WifiInfo>)>> {
        super::nm_dbus::query_scan_list_if_newer(device, prev_last_scan)
    }
}
