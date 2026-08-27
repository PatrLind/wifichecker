use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One access point seen in the scan list at a measurement point.
///
/// A measurement stores the full scan list taken at that moment, so the
/// signal of *any* in-range AP (not just the one the computer is connected
/// to) can be visualized later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanEntry {
    pub ssid: String,
    pub bssid: String,
    pub frequency_mhz: u32,
    pub channel: u8,
    pub signal_dbm: i32,
    /// True for the AP the computer is currently associated with.
    #[serde(default)]
    pub is_active: bool,
}

impl ScanEntry {
    pub fn band(&self) -> &'static str {
        super::band_label(self.frequency_mhz)
    }
}

/// A single WiFi measurement at a position on the floor plan.
/// x and y are relative coordinates in [0.0, 1.0].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurement {
    pub id: String,
    /// Relative x coordinate on floor plan image (0.0 = left, 1.0 = right)
    pub x: f64,
    /// Relative y coordinate on floor plan image (0.0 = top, 1.0 = bottom)
    pub y: f64,
    pub timestamp: DateTime<Utc>,
    pub ssid: String,
    pub bssid: String,
    pub frequency_mhz: u32,
    pub channel: u8,
    pub signal_dbm: i32,
    pub noise_dbm: Option<i32>,
    pub link_speed_mbps: Option<u32>,
    pub iperf_mbps: Option<f64>,
    pub smb_mbps: Option<f64>,
    /// True when this point records "no WiFi reception" (WiFi off or not
    /// connected) — used to mark dead zones on the coverage map.
    #[serde(default)]
    pub no_signal: bool,
    /// All access points in range when this measurement was taken
    /// (strongest first). Old projects have an empty list here.
    #[serde(default)]
    pub scan_results: Vec<ScanEntry>,
}

impl Measurement {
    pub fn new(x: f64, y: f64, ssid: String, bssid: String, frequency_mhz: u32, channel: u8, signal_dbm: i32) -> Self {
        Self {
            id: uuid_v4(),
            x,
            y,
            timestamp: Utc::now(),
            ssid,
            bssid,
            frequency_mhz,
            channel,
            signal_dbm,
            noise_dbm: None,
            link_speed_mbps: None,
            iperf_mbps: None,
            smb_mbps: None,
            no_signal: false,
            scan_results: Vec::new(),
        }
    }

    /// A measurement recording "no WiFi reception" at a position (WiFi off or
    /// not connected). Marks a dead zone on the coverage map.
    pub fn no_signal(x: f64, y: f64) -> Self {
        Self {
            id: uuid_v4(),
            x,
            y,
            timestamp: Utc::now(),
            ssid: String::new(),
            bssid: String::new(),
            frequency_mhz: 0,
            channel: 0,
            signal_dbm: -90,
            noise_dbm: None,
            link_speed_mbps: None,
            iperf_mbps: None,
            smb_mbps: None,
            no_signal: true,
            scan_results: Vec::new(),
        }
    }

    pub fn signal_quality_percent(&self) -> u8 {
        // Convert dBm to quality percentage (0-100)
        // Typical range: -30 dBm (excellent) to -90 dBm (no signal)
        let clamped = self.signal_dbm.clamp(-90, -30);
        ((clamped + 90) * 100 / 60) as u8
    }
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        t,
        (t >> 16) & 0xffff,
        (t >> 4) & 0x0fff,
        0x8000 | ((t >> 2) & 0x3fff),
        t as u64 * 0x1000000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_measurement(signal_dbm: i32) -> Measurement {
        Measurement::new(0.5, 0.5, "TestSSID".to_string(), "AA:BB:CC:DD:EE:FF".to_string(), 2412, 1, signal_dbm)
    }

    #[test]
    fn test_signal_quality_excellent() {
        assert_eq!(make_measurement(-30).signal_quality_percent(), 100);
    }

    #[test]
    fn test_signal_quality_no_signal() {
        assert_eq!(make_measurement(-90).signal_quality_percent(), 0);
    }

    #[test]
    fn test_signal_quality_midpoint() {
        // (-60 + 90) * 100 / 60 = 50
        assert_eq!(make_measurement(-60).signal_quality_percent(), 50);
    }

    #[test]
    fn test_signal_quality_clamped_above() {
        assert_eq!(make_measurement(-20).signal_quality_percent(), 100);
    }

    #[test]
    fn test_signal_quality_clamped_below() {
        assert_eq!(make_measurement(-100).signal_quality_percent(), 0);
    }

    #[test]
    fn test_signal_quality_near_boundaries() {
        // -31 dBm → (59 * 100) / 60 = 98
        assert_eq!(make_measurement(-31).signal_quality_percent(), 98);
        // -89 dBm → (1 * 100) / 60 = 1
        assert_eq!(make_measurement(-89).signal_quality_percent(), 1);
    }

    #[test]
    fn test_measurement_new_optional_fields_are_none() {
        let m = make_measurement(-60);
        assert!(m.noise_dbm.is_none());
        assert!(m.link_speed_mbps.is_none());
        assert!(m.iperf_mbps.is_none());
        assert!(m.smb_mbps.is_none());
    }

    #[test]
    fn test_measurement_new_fields() {
        let m = Measurement::new(0.3, 0.7, "SSID".to_string(), "AA:BB:CC:DD:EE:FF".to_string(), 5180, 36, -65);
        assert_eq!(m.x, 0.3);
        assert_eq!(m.y, 0.7);
        assert_eq!(m.frequency_mhz, 5180);
        assert_eq!(m.channel, 36);
        assert_eq!(m.signal_dbm, -65);
        assert_eq!(m.ssid, "SSID");
        assert_eq!(m.bssid, "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn test_no_signal_measurement() {
        let m = Measurement::no_signal(0.2, 0.8);
        assert!(m.no_signal);
        assert_eq!(m.x, 0.2);
        assert_eq!(m.y, 0.8);
        assert!(m.ssid.is_empty());
        assert!(m.bssid.is_empty());
        assert_eq!(m.signal_dbm, -90);
        assert!(m.iperf_mbps.is_none());
        assert!(m.smb_mbps.is_none());
    }

    #[test]
    fn test_regular_measurement_is_not_no_signal() {
        let m = make_measurement(-60);
        assert!(!m.no_signal);
    }

    #[test]
    fn test_scan_entry_band() {
        let e24 = ScanEntry { ssid: "A".into(), bssid: "B".into(), frequency_mhz: 2437, channel: 6, signal_dbm: -60, is_active: false };
        let e5 = ScanEntry { ssid: "A".into(), bssid: "B".into(), frequency_mhz: 5180, channel: 36, signal_dbm: -60, is_active: true };
        let e6 = ScanEntry { ssid: "A".into(), bssid: "B".into(), frequency_mhz: 5955, channel: 5, signal_dbm: -60, is_active: false };
        assert_eq!(e24.band(), "2.4 GHz");
        assert_eq!(e5.band(), "5 GHz");
        assert_eq!(e6.band(), "6 GHz");
    }

    #[test]
    fn test_measurement_json_with_scan_results_roundtrip() {
        let mut m = make_measurement(-60);
        m.scan_results = vec![
            ScanEntry { ssid: "MyHome".into(), bssid: "AA:BB:CC:DD:EE:01".into(), frequency_mhz: 5180, channel: 36, signal_dbm: -55, is_active: true },
            ScanEntry { ssid: "MyHome".into(), bssid: "AA:BB:CC:DD:EE:02".into(), frequency_mhz: 2437, channel: 6, signal_dbm: -70, is_active: false },
        ];
        let json = serde_json::to_string(&m).unwrap();
        let m2: Measurement = serde_json::from_str(&json).unwrap();
        assert_eq!(m2.scan_results.len(), 2);
        assert!(m2.scan_results[0].is_active);
        assert_eq!(m2.scan_results[1].bssid, "AA:BB:CC:DD:EE:02");
    }

    #[test]
    fn test_measurement_json_without_scan_results_uses_default() {
        // Old saved projects have no `scan_results` field; it should default to empty.
        let json = r#"{"id":"x","x":0.5,"y":0.5,"timestamp":"2024-01-01T00:00:00Z","ssid":"A","bssid":"B","frequency_mhz":2412,"channel":1,"signal_dbm":-70,"noise_dbm":null,"link_speed_mbps":null,"iperf_mbps":null,"smb_mbps":null}"#;
        let m: Measurement = serde_json::from_str(json).unwrap();
        assert!(m.scan_results.is_empty());
    }

    #[test]
    fn test_no_signal_deserializes_from_old_data_without_field() {
        // Old saved projects have no `no_signal` field; it should default to false.
        let json = r#"{"id":"x","x":0.5,"y":0.5,"timestamp":"2024-01-01T00:00:00Z","ssid":"A","bssid":"B","frequency_mhz":2412,"channel":1,"signal_dbm":-70,"noise_dbm":null,"link_speed_mbps":null,"iperf_mbps":null,"smb_mbps":null}"#;
        let m: Measurement = serde_json::from_str(json).unwrap();
        assert!(!m.no_signal);
    }

    #[test]
    fn test_uuid_format() {
        let m = make_measurement(-60);
        assert!(!m.id.is_empty());
        // UUID-like format: 5 groups separated by dashes
        let parts: Vec<&str> = m.id.split('-').collect();
        assert_eq!(parts.len(), 5);
        // All parts are non-empty hex strings
        for part in &parts {
            assert!(!part.is_empty());
            assert!(part.chars().all(|c| c.is_ascii_hexdigit()), "non-hex char in part: {part}");
        }
        // First group is exactly 8 hex chars (formatted with {:08x})
        assert_eq!(parts[0].len(), 8);
        // Second group is exactly 4 hex chars
        assert_eq!(parts[1].len(), 4);
        // Third group starts with '4' (version marker)
        assert!(parts[2].starts_with('4'));
        // Fourth group is exactly 4 hex chars
        assert_eq!(parts[3].len(), 4);
    }

    #[test]
    fn test_uuid_version_and_variant_bits() {
        let m = make_measurement(-60);
        let parts: Vec<&str> = m.id.split('-').collect();
        // Version 4: third group starts with '4'
        assert!(parts[2].starts_with('4'));
        // Variant: fourth group starts with '8', '9', 'a', or 'b'
        let variant = parts[3].chars().next().unwrap();
        assert!(matches!(variant, '8' | '9' | 'a' | 'b'));
    }
}
