//! NetworkManager D-Bus proxies used for WiFi scanning.
//!
//! These replace the `nmcli` subprocess and work both on native installs and
//! inside the Flatpak sandbox (where `nmcli` is not available), provided the
//! manifest grants `--system-talk-name=org.freedesktop.NetworkManager`.

use anyhow::{Context, Result};
use zbus::blocking::Connection;
use zbus::zvariant::{DeserializeDict, OwnedObjectPath, SerializeDict, Type};

use super::wifi_scanner::WifiInfo;

// NM device type constant for 802.11 wireless
const DEVICE_TYPE_WIFI: u32 = 2;

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    fn get_all_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NMDevice {
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<u32>;

    /// The device's primary network interface name (e.g. "wlan0").
    #[zbus(property)]
    fn interface(&self) -> zbus::Result<String>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NMWireless {
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;

    /// All APs the driver has reported (NM's scan cache), as object paths.
    #[zbus(property)]
    fn access_points(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Seconds since the last completed scan (monotonic, not epoch).
    #[zbus(property)]
    fn last_scan(&self) -> zbus::Result<i64>;
}

/// The (empty) `options` argument of NM's modern per-device `RequestScan`,
/// which is typed `a{sv}`.
#[derive(Debug, Default, DeserializeDict, SerializeDict, Type)]
#[zvariant(signature = "a{sv}")]
struct EmptyDict;

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NMAccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    #[zbus(property)]
    fn hw_address(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn frequency(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;

    #[zbus(property)]
    fn max_bitrate(&self) -> zbus::Result<u32>;
}

/// Query all WiFi devices that currently have an active access point, via the
/// NetworkManager D-Bus API. Each returned `WifiInfo` carries the device's
/// interface name (e.g. `wlan0`) in `device`, so the UI can show which card is
/// in use and let the user choose between multiple cards.
pub fn query_active_wifi_devices() -> Result<Vec<WifiInfo>> {
    let conn = Connection::system().context("Failed to connect to system D-Bus")?;

    let nm = NetworkManagerProxyBlocking::new(&conn)
        .context("Failed to create NetworkManager proxy")?;

    let devices = nm
        .get_all_devices()
        .context("NetworkManager.GetAllDevices failed")?;

    let mut result = Vec::new();
    for device_path in devices {
        let device = NMDeviceProxyBlocking::builder(&conn)
            .path(device_path.as_str())?
            .build()
            .context("Failed to build Device proxy")?;

        if device.device_type().context("DeviceType property failed")? != DEVICE_TYPE_WIFI {
            continue;
        }

        let wireless = NMWirelessProxyBlocking::builder(&conn)
            .path(device_path.as_str())?
            .build()
            .context("Failed to build Wireless proxy")?;

        let ap_path = wireless
            .active_access_point()
            .context("ActiveAccessPoint property failed")?;

        // "/" means no active AP on this device
        if ap_path.as_str() == "/" {
            continue;
        }

        let ap = NMAccessPointProxyBlocking::builder(&conn)
            .path(ap_path.as_str())?
            .build()
            .context("Failed to build AccessPoint proxy")?;

        let ssid_bytes = ap.ssid().context("AP Ssid property failed")?;
        let ssid = String::from_utf8_lossy(&ssid_bytes).into_owned();

        let bssid = ap.hw_address().context("AP HwAddress property failed")?;
        let frequency_mhz = ap.frequency().context("AP Frequency property failed")?;
        let strength = ap.strength().context("AP Strength property failed")?;
        let max_bitrate_kbps = ap.max_bitrate().context("AP MaxBitrate property failed")?;

        // NM Strength is 0-100 quality, same as nmcli SIGNAL
        let signal_dbm = (strength as i32 / 2) - 100;
        let channel = freq_to_channel(frequency_mhz);
        let link_speed_mbps =
            if max_bitrate_kbps > 0 { Some(max_bitrate_kbps / 1000) } else { None };

        // Identify the card by its interface name (e.g. "wlan0"); fall back to
        // the AP's BSSID if the interface name is unavailable.
        let iface = device.interface().unwrap_or_default();
        let device_name = if iface.is_empty() { bssid.clone() } else { iface };

        result.push(WifiInfo {
            ssid,
            bssid,
            frequency_mhz,
            channel,
            signal_dbm,
            link_speed_mbps,
            device: device_name,
            is_active: true,
        });
    }

    Ok(result)
}

/// Query the active WiFi access point (the first active WiFi card). Kept for
/// callers that only need a single connection.
pub fn query_active_ap() -> Result<Option<WifiInfo>> {
    Ok(query_active_wifi_devices()?.into_iter().next())
}

/// Read one access point's properties and convert it to a `WifiInfo`.
fn read_ap(conn: &Connection, path: &OwnedObjectPath) -> Option<WifiInfo> {
    let ap = NMAccessPointProxyBlocking::builder(conn)
        .path(path.as_str())
        .ok()?
        .build()
        .ok()?;
    let ssid_bytes = ap.ssid().ok()?;
    let ssid = String::from_utf8_lossy(&ssid_bytes).into_owned();
    let bssid = ap.hw_address().ok()?;
    let frequency_mhz = ap.frequency().ok()?;
    let strength = ap.strength().ok()?;
    let max_bitrate_kbps = ap.max_bitrate().ok()?;

    let signal_dbm = (strength as i32 / 2) - 100;
    let channel = freq_to_channel(frequency_mhz);
    let link_speed_mbps = if max_bitrate_kbps > 0 { Some(max_bitrate_kbps / 1000) } else { None };

    Some(WifiInfo {
        ssid,
        bssid,
        frequency_mhz,
        channel,
        signal_dbm,
        link_speed_mbps,
        device: String::new(),
        is_active: false,
    })
}

/// Request a fresh scan on a WiFi device.
///
/// Tries the modern per-device `RequestScan(a{sv})` first (NM ≥ 1.44), then
/// the legacy root-level `RequestScan(ifaces)` (older NM). Returns `Err` if
/// neither is available (the caller then uses the cached AP list).
pub fn request_fresh_scan(conn: &Connection, device_path: &str, iface: &str) -> Result<()> {
    let res = conn.call_method(
        Some("org.freedesktop.NetworkManager"),
        device_path,
        Some("org.freedesktop.NetworkManager.Device.Wireless"),
        "RequestScan",
        &EmptyDict::default(),
    );
    match res {
        Ok(_) => return Ok(()),
        Err(e) => {
            log::debug!("Per-device RequestScan failed ({e}), trying legacy root RequestScan");
        }
    }
    let res = conn.call_method(
        Some("org.freedesktop.NetworkManager"),
        "/org/freedesktop/NetworkManager",
        Some("org.freedesktop.NetworkManager"),
        "RequestScan",
        &vec![iface.to_string()],
    );
    res.context("Legacy NM RequestScan failed")?;
    Ok(())
}

/// Poll `LastScan` until it advances (a scan completed) or the timeout hits.
fn wait_for_scan(conn: &Connection, device_path: &str, prev_last_scan: i64, timeout_ms: u64) -> Result<()> {
    let device = NMWirelessProxyBlocking::builder(conn)
        .path(device_path)?
        .build()?;
    let start = std::time::Instant::now();
    loop {
        if start.elapsed().as_millis() as u64 > timeout_ms {
            return Ok(()); // timed out — use the cache as-is
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        let now = device.last_scan()?;
        if now > prev_last_scan {
            return Ok(());
        }
    }
}

/// Read the full scan list (all APs in range) for one WiFi device.
///
/// `iface`: the interface to scan (e.g. "wlan0"); `None` uses the first WiFi
/// device found. When `fresh` is true a new scan is requested first and we
/// wait up to ~6 s for it to complete; on any failure the cached AP list is
/// returned instead. Results are sorted by signal strength (strongest first)
/// and the associated AP (if any) is flagged `is_active`.
pub fn query_scan_list(iface: Option<&str>, fresh: bool) -> Result<Vec<WifiInfo>> {
    let conn = Connection::system().context("Failed to connect to system D-Bus")?;

    let (device_path, device_name) = match find_wifi_device(&conn, iface) {
        Some(t) => t,
        None => return Ok(Vec::new()),
    };

    let wireless = NMWirelessProxyBlocking::builder(&conn)
        .path(device_path.as_str())?
        .build()
        .context("Failed to build Wireless proxy")?;

    if fresh {
        let prev_last_scan = wireless.last_scan().unwrap_or(0);
        if let Err(e) = request_fresh_scan(&conn, device_path.as_str(), &device_name) {
            log::debug!("Scan request failed (using cached AP list): {e}");
        } else {
            let _ = wait_for_scan(&conn, device_path.as_str(), prev_last_scan, 6000);
        }
    }

    read_device_ap_list(&conn, device_path.as_str(), device_name)
}

/// Find the target WiFi device: the requested interface name, else the first
/// WiFi device managed by NM.
fn find_wifi_device(conn: &Connection, iface: Option<&str>) -> Option<(OwnedObjectPath, String)> {
    let nm = NetworkManagerProxyBlocking::new(conn).ok()?;
    let devices = nm.get_all_devices().ok()?;

    let mut wifi_devices: Vec<(OwnedObjectPath, String)> = Vec::new();
    for device_path in devices {
        let device = NMDeviceProxyBlocking::builder(conn)
            .path(device_path.as_str())
            .ok()?
            .build()
            .ok()?;
        if device.device_type().ok() != Some(DEVICE_TYPE_WIFI) {
            continue;
        }
        let name = device.interface().unwrap_or_default();
        wifi_devices.push((device_path.clone(), name));
    }

    match iface {
        Some(w) => wifi_devices.iter().find(|(_, n)| n == w).cloned(),
        None => wifi_devices.first().cloned(),
    }
}

/// Read a device's cached AP list (all AP properties), flagging the
/// associated AP and sorting strongest first. No scan is requested.
fn read_device_ap_list(conn: &Connection, device_path: &str, device_name: String) -> Result<Vec<WifiInfo>> {
    let wireless = NMWirelessProxyBlocking::builder(conn)
        .path(device_path)?
        .build()
        .context("Failed to build Wireless proxy")?;

    let active_ap_path = wireless.active_access_point().unwrap_or_default();
    let ap_paths = wireless
        .access_points()
        .context("AccessPoints property failed")?;

    let mut result: Vec<WifiInfo> = ap_paths
        .iter()
        .filter_map(|p| read_ap(conn, p))
        .collect();

    for info in result.iter_mut() {
        info.device = device_name.clone();
    }
    // Flag the associated AP (if this device has one).
    if active_ap_path.as_str() != "/" {
        if let Some(ab) = read_ap(conn, &active_ap_path).map(|w| w.bssid) {
            if let Some(info) = result.iter_mut().find(|i| i.bssid == ab) {
                info.is_active = true;
            }
        }
    }

    // Strongest first — keeps the UI list and stored scan results tidy.
    result.sort_by(|a, b| b.signal_dbm.cmp(&a.signal_dbm));
    Ok(result)
}

/// Read the AP list only when NM's `LastScan` counter for the device has
/// advanced past `prev_last_scan` (i.e. a scan completed since the last
/// read). Returns `(new_last_scan, list)` when it changed, else `None`.
///
/// This is cheap: it only costs one property read per call, and the full AP
/// property read happens only at NM's own scan cadence.
pub fn query_scan_list_if_newer(iface: Option<&str>, prev_last_scan: i64) -> Result<Option<(i64, Vec<WifiInfo>)>> {
    let conn = Connection::system().context("Failed to connect to system D-Bus")?;

    let Some((device_path, device_name)) = find_wifi_device(&conn, iface) else {
        return Ok(None);
    };
    let wireless = NMWirelessProxyBlocking::builder(&conn)
        .path(device_path.as_str())?
        .build()
        .context("Failed to build Wireless proxy")?;

    let last_scan = wireless.last_scan().unwrap_or(0);
    if last_scan <= prev_last_scan {
        return Ok(None);
    }
    let list = read_device_ap_list(&conn, device_path.as_str(), device_name)?;
    Ok(Some((last_scan, list)))
}

/// Convert a WiFi frequency in MHz to an 802.11 channel number.
fn freq_to_channel(freq_mhz: u32) -> u8 {
    match freq_mhz {
        2412..=2472 => ((freq_mhz - 2412) / 5 + 1) as u8,
        2484 => 14,
        5160..=5885 => ((freq_mhz - 5000) / 5) as u8,
        // 6 GHz band (Wi-Fi 6E)
        5955..=7115 => ((freq_mhz - 5955) / 5 + 1) as u8,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_freq_to_channel_24ghz_ch1() {
        assert_eq!(freq_to_channel(2412), 1);
    }

    #[test]
    fn test_freq_to_channel_24ghz_ch6() {
        // (2437 - 2412) / 5 + 1 = 5 + 1 = 6
        assert_eq!(freq_to_channel(2437), 6);
    }

    #[test]
    fn test_freq_to_channel_24ghz_ch11() {
        assert_eq!(freq_to_channel(2462), 11);
    }

    #[test]
    fn test_freq_to_channel_24ghz_ch13() {
        assert_eq!(freq_to_channel(2472), 13);
    }

    #[test]
    fn test_freq_to_channel_24ghz_ch14() {
        assert_eq!(freq_to_channel(2484), 14);
    }

    #[test]
    fn test_freq_to_channel_5ghz_ch32() {
        // (5160 - 5000) / 5 = 32
        assert_eq!(freq_to_channel(5160), 32);
    }

    #[test]
    fn test_freq_to_channel_5ghz_ch36() {
        // (5180 - 5000) / 5 = 36
        assert_eq!(freq_to_channel(5180), 36);
    }

    #[test]
    fn test_freq_to_channel_5ghz_ch100() {
        // (5500 - 5000) / 5 = 100
        assert_eq!(freq_to_channel(5500), 100);
    }

    #[test]
    fn test_freq_to_channel_6ghz_ch1() {
        // Wi-Fi 6E: (5955 - 5955) / 5 + 1 = 1
        assert_eq!(freq_to_channel(5955), 1);
    }

    #[test]
    fn test_freq_to_channel_unknown_returns_zero() {
        assert_eq!(freq_to_channel(0), 0);
        assert_eq!(freq_to_channel(3000), 0);
        assert_eq!(freq_to_channel(2473), 0); // gap between ch13 and ch14
    }

    #[test]
    fn test_signal_dbm_conversion_from_strength() {
        // NM Strength 0-100 → dBm: (strength / 2) - 100
        // Strength 100 → (100 / 2) - 100 = -50
        // Strength 0   → (0 / 2)   - 100 = -100
        let strength_100: i32 = 100;
        let dbm_100 = (strength_100 / 2) - 100;
        assert_eq!(dbm_100, -50);

        let strength_0: i32 = 0;
        let dbm_0 = (strength_0 / 2) - 100;
        assert_eq!(dbm_0, -100);

        let strength_50: i32 = 50;
        let dbm_50 = (strength_50 / 2) - 100;
        assert_eq!(dbm_50, -75);
    }

    /// Live test against a running NetworkManager (skipped unless
    /// WIFICHECKER_LIVE_DBUS=1 is set). Verifies the full scan-list path:
    /// fresh scan, AP properties, active-AP flag, strongest-first ordering.
    #[test]
    fn test_query_scan_list_live() {
        if std::env::var("WIFICHECKER_LIVE_DBUS").is_err() {
            eprintln!("test_query_scan_list_live: skipped (set WIFICHECKER_LIVE_DBUS=1 to run)");
            return;
        }
        let list = query_scan_list(None, true).expect("scan list query failed");
        assert!(!list.is_empty(), "expected at least one AP in range");
        // Sorted strongest first.
        for w in list.windows(2) {
            assert!(w[0].signal_dbm >= w[1].signal_dbm);
        }
        // At most one active AP, and it must be present in the list.
        assert!(list.iter().filter(|a| a.is_active).count() <= 1);
    }
}
