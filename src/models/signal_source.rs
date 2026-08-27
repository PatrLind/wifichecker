use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

use super::Measurement;

/// Which access point's signal strength the display (heatmap cells, list,
/// legend pointer) is based on:
///
/// - `ConnectedAp`: the AP the computer is associated with (classic behavior).
/// - `Ssid(s)`: the strongest AP broadcasting the SSID `s`. An empty `s`
///   means "the SSID the measurement is connected to" (legacy behavior).
/// - `Bssid(b)`: one specific AP. `None` is returned for points where that
///   AP was not in range (rendered as "no data", not as the worst value).
///
/// Serialization is string-based ("ConnectedAp", "Ssid", "Ssid:<name>",
/// "Bssid:<mac>") so old project settings keep loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalSource {
    ConnectedAp,
    Ssid(String),
    Bssid(String),
}

impl Serialize for SignalSource {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match self {
            SignalSource::ConnectedAp => ser.serialize_str("ConnectedAp"),
            SignalSource::Ssid(s) if s.is_empty() => ser.serialize_str("Ssid"),
            SignalSource::Ssid(s) => ser.serialize_str(&format!("Ssid:{s}")),
            SignalSource::Bssid(b) => ser.serialize_str(&format!("Bssid:{b}")),
        }
    }
}

impl<'de> Deserialize<'de> for SignalSource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        match s.as_str() {
            "ConnectedAp" => Ok(SignalSource::ConnectedAp),
            "Ssid" => Ok(SignalSource::Ssid(String::new())),
            other if other.starts_with("Ssid:") => {
                Ok(SignalSource::Ssid(other["Ssid:".len()..].to_string()))
            }
            other if other.starts_with("Bssid:") => {
                Ok(SignalSource::Bssid(other["Bssid:".len()..].to_string()))
            }
            _ => Err(de::Error::unknown_variant(
                &s,
                &["ConnectedAp", "Ssid", "Ssid:<name>", "Bssid:<mac>"],
            )),
        }
    }
}

impl Default for SignalSource {
    fn default() -> Self {
        SignalSource::ConnectedAp
    }
}

impl SignalSource {
    /// Fixed dropdown labels for the non-BSSID options.
    pub const CONNECTED_AP_LABEL: &'static str = "Connected AP";
    pub const SSID_LABEL: &'static str = "SSID (best AP)";
    /// Prefix for the per-BSSID dropdown entries.
    pub const BSSID_PREFIX: &'static str = "AP: ";

    pub fn is_connected_ap(&self) -> bool {
        matches!(self, SignalSource::ConnectedAp)
    }
}

/// The display signal value (dBm) of a measurement for a given source, if
/// that source has data at this point.
///
/// `None` means "no data" (e.g. the selected BSSID was not in range when
/// this point was measured) — the cell is left uncolored.
pub fn signal_for(m: &Measurement, source: &SignalSource) -> Option<f64> {
    if m.no_signal {
        return None;
    }
    match source {
        SignalSource::ConnectedAp => Some(m.signal_dbm as f64),
        SignalSource::Ssid(ssid) => {
            // Empty ssid = "the connected SSID" (legacy behavior).
            let target = if ssid.is_empty() { m.ssid.as_str() } else { ssid.as_str() };
            if m.scan_results.is_empty() {
                // Old measurement without a scan list: the connected AP is
                // the only signal we know — and only for the connected SSID.
                if ssid.is_empty() { Some(m.signal_dbm as f64) } else { None }
            } else {
                // Best (strongest) AP broadcasting the target SSID.
                let best = m
                    .scan_results
                    .iter()
                    .filter(|e| e.ssid == target)
                    .map(|e| e.signal_dbm)
                    .max();
                best.map(|v| v as f64)
                    // The connected AP always matches m.ssid when a scan
                    // list exists; the fallback is just defensive.
                    .or(if ssid.is_empty() { Some(m.signal_dbm as f64) } else { None })
            }
        }
        SignalSource::Bssid(b) => {
            if let Some(e) = m.scan_results.iter().find(|e| &e.bssid == b) {
                Some(e.signal_dbm as f64)
            } else if m.bssid == *b {
                // The requested AP is the connected one (e.g. an old
                // measurement without a scan list).
                Some(m.signal_dbm as f64)
            } else {
                None // AP not in range at this point → no data
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ScanEntry;

    fn m_with_scan(dbm: i32, scan: Vec<ScanEntry>) -> Measurement {
        let mut m =
            Measurement::new(0.5, 0.5, "MyHome".to_string(), "AA:BB:CC:DD:EE:01".to_string(), 5180, 36, dbm);
        m.scan_results = scan;
        m
    }

    fn entry(ssid: &str, bssid: &str, dbm: i32, active: bool) -> ScanEntry {
        ScanEntry {
            ssid: ssid.to_string(),
            bssid: bssid.to_string(),
            frequency_mhz: 5180,
            channel: 36,
            signal_dbm: dbm,
            is_active: active,
        }
    }

    #[test]
    fn default_source_is_connected_ap() {
        assert!(SignalSource::default().is_connected_ap());
    }

    #[test]
    fn connected_ap_uses_measurement_dbm() {
        let m = m_with_scan(-55, vec![entry("MyHome", "AA:BB:CC:DD:EE:01", -55, true)]);
        assert_eq!(signal_for(&m, &SignalSource::ConnectedAp), Some(-55.0));
    }

    #[test]
    fn ssid_uses_best_ap_of_same_ssid() {
        let m = m_with_scan(
            -55,
            vec![
                entry("MyHome", "AA:BB:CC:DD:EE:01", -55, true),
                entry("MyHome", "AA:BB:CC:DD:EE:02", -70, false),
                entry("MyHome", "AA:BB:CC:DD:EE:03", -62, false),
                entry("OtherNet", "AA:BB:CC:DD:EE:09", -40, false),
            ],
        );
        // Best MyHome AP is the active one at -55.
        assert_eq!(signal_for(&m, &SignalSource::Ssid(String::new())), Some(-55.0));
        // Named SSID resolves to the same best AP…
        assert_eq!(
            signal_for(&m, &SignalSource::Ssid("MyHome".to_string())),
            Some(-55.0)
        );
        // …and a different SSID uses its own (stronger) AP.
        assert_eq!(
            signal_for(&m, &SignalSource::Ssid("OtherNet".to_string())),
            Some(-40.0)
        );
    }

    #[test]
    fn named_ssid_absent_is_no_data() {
        let m = m_with_scan(
            -55,
            vec![entry("MyHome", "AA:BB:CC:DD:EE:01", -55, true)],
        );
        assert_eq!(
            signal_for(&m, &SignalSource::Ssid("Gone".to_string())),
            None
        );
        // Legacy generic (empty) still falls back to the connected AP.
        assert_eq!(signal_for(&m, &SignalSource::Ssid(String::new())), Some(-55.0));
    }

    #[test]
    fn ssid_falls_back_to_connected_ap_when_no_scan_list() {
        let m = m_with_scan(-61, vec![]);
        assert_eq!(signal_for(&m, &SignalSource::Ssid(String::new())), Some(-61.0));
        // A named SSID can't be resolved without a scan list.
        assert_eq!(signal_for(&m, &SignalSource::Ssid("MyHome".to_string())), None);
    }

    #[test]
    fn bssid_found_in_scan_list() {
        let m = m_with_scan(
            -55,
            vec![
                entry("MyHome", "AA:BB:CC:DD:EE:01", -55, true),
                entry("MyHome", "AA:BB:CC:DD:EE:02", -70, false),
            ],
        );
        assert_eq!(
            signal_for(&m, &SignalSource::Bssid("AA:BB:CC:DD:EE:02".to_string())),
            Some(-70.0)
        );
    }

    #[test]
    fn bssid_absent_is_no_data() {
        let m = m_with_scan(
            -55,
            vec![entry("MyHome", "AA:BB:CC:DD:EE:01", -55, true)],
        );
        assert_eq!(
            signal_for(&m, &SignalSource::Bssid("AA:BB:CC:DD:EE:99".to_string())),
            None
        );
    }

    #[test]
    fn bssid_falls_back_to_connected_ap_for_old_measurements() {
        // Old measurement: no scan list, requested BSSID is the connected one.
        let m = m_with_scan(-60, vec![]);
        assert_eq!(
            signal_for(&m, &SignalSource::Bssid("AA:BB:CC:DD:EE:01".to_string())),
            Some(-60.0)
        );
        // ... but a different BSSID has no data.
        assert_eq!(
            signal_for(&m, &SignalSource::Bssid("AA:BB:CC:DD:EE:02".to_string())),
            None
        );
    }

    #[test]
    fn no_signal_point_has_no_value_for_any_source() {
        let m = Measurement::no_signal(0.2, 0.3);
        assert_eq!(signal_for(&m, &SignalSource::ConnectedAp), None);
        assert_eq!(signal_for(&m, &SignalSource::Ssid(String::new())), None);
        assert_eq!(signal_for(&m, &SignalSource::Bssid("AA:BB:CC:DD:EE:01".to_string())), None);
    }

    #[test]
    fn signal_source_serde_roundtrip() {
        for src in [
            SignalSource::ConnectedAp,
            SignalSource::Ssid(String::new()),
            SignalSource::Ssid("My Home".to_string()),
            SignalSource::Bssid("AA:BB:CC:DD:EE:02".to_string()),
        ] {
            let json = serde_json::to_string(&src).unwrap();
            let src2: SignalSource = serde_json::from_str(&json).unwrap();
            assert_eq!(src, src2);
        }
    }

    #[test]
    fn signal_source_legacy_strings_load() {
        // The unit variant "Ssid" written by older builds → generic SSID source.
        let legacy: SignalSource = serde_json::from_str("\"Ssid\"").unwrap();
        assert_eq!(legacy, SignalSource::Ssid(String::new()));
        let bssid: SignalSource = serde_json::from_str("\"Bssid:AA:BB:CC:DD:EE:01\"").unwrap();
        assert_eq!(bssid, SignalSource::Bssid("AA:BB:CC:DD:EE:01".to_string()));
        let named: SignalSource = serde_json::from_str("\"Ssid:My Home\"").unwrap();
        assert_eq!(named, SignalSource::Ssid("My Home".to_string()));
    }
}
