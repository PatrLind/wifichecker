use serde::{Deserialize, Serialize};

use super::Measurement;
use super::signal_source::{SignalSource, signal_for};

/// Which measurement value the floor-plan cell colours are based on.
///
/// This is a user setting (see `AppSettings::color_metric`); the colour scale
/// itself is absolute (fixed reference range), so a sample's colour is stable
/// regardless of what other samples exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorMetric {
    SignalDbm,
    IperfMbps,
    SmbMbps,
}

impl Default for ColorMetric {
    fn default() -> Self { ColorMetric::SignalDbm }
}

impl ColorMetric {
    /// Fixed (absolute) reference range for the colour scale: red at `min`,
    /// green at `max`. Values outside the range are clamped.
    ///
    /// - Signal: −90 dBm (very poor/red) … −50 dBm (excellent/green).
    ///   This follows the common WiFi signal rating, so −70 dBm ("Fair")
    ///   lands at the midpoint (yellow):
    ///     −30…−50 excellent (green) · −51…−67 very good/good (green‑yellow)
    ///     −68…−70 fair (yellow) · −71…−80 poor (orange) · −81…−90 very poor
    ///     (red) · < −90 unusable (red).
    /// - Bandwidth: 0 … 1 Gbit/s (1000 Mbit/s)
    pub fn reference_range(self) -> (f64, f64) {
        match self {
            ColorMetric::SignalDbm => (-90.0, -50.0),
            ColorMetric::IperfMbps | ColorMetric::SmbMbps => (0.0, 1000.0),
        }
    }

    /// Short display name (used in the legend).
    pub fn label(self) -> &'static str {
        match self {
            ColorMetric::SignalDbm => "Signal",
            ColorMetric::IperfMbps => "iperf",
            ColorMetric::SmbMbps   => "Samba",
        }
    }

    /// Unit label for the scale endpoints (used in the legend).
    pub fn unit(self) -> &'static str {
        match self {
            ColorMetric::SignalDbm => "dBm",
            _ => "Mbit/s",
        }
    }
}

/// The value of a measurement for a given metric, if that metric is present.
///
/// For the signal metric the value is resolved through `signal_source`
/// (connected AP / best AP of the SSID / a specific BSSID); `None` means the
/// source has no data at this point (cell left uncolored).
pub fn metric_value(m: &Measurement, metric: ColorMetric, signal_source: &SignalSource) -> Option<f64> {
    // A no-signal point has no reading to place on the scale — it is "no
    // data", not the worst signal — so it contributes no value.
    if m.no_signal {
        return None;
    }
    match metric {
        ColorMetric::SmbMbps   => m.smb_mbps,
        ColorMetric::IperfMbps => m.iperf_mbps,
        ColorMetric::SignalDbm => signal_for(m, signal_source),
    }
}

/// The scale value for a **no-signal** point, if one of the wanted APs was
/// actually in range when the point was recorded (its scan list).
///
/// A no-signal point is "no connection", not necessarily "no signal": when
/// the wanted network(s) are detected (just not associated), the point can
/// be coloured like a regular sample based on the active signal source:
///
/// - `ConnectedAp` → always `None` (the point is by definition unconnected);
/// - `Ssid(s)` (named) → strongest AP of that SSID in the point's scan;
/// - `Ssid("")` ("SSID (best AP)") → strongest AP whose SSID is one of this
///   floor's measured SSIDs (`wanted_ssids`);
/// - `Bssid(b)` → that AP's signal in the point's scan.
///
/// `None` → the cell is drawn gray with a blue cross ("no data for this
/// selection at this point").
pub fn no_signal_metric_value(
    m: &Measurement,
    signal_source: &SignalSource,
    wanted_ssids: &[String],
) -> Option<f64> {
    if !m.no_signal {
        return None;
    }
    match signal_source {
        SignalSource::ConnectedAp => None,
        SignalSource::Ssid(s) if s.is_empty() => m
            .scan_results
            .iter()
            .filter(|e| wanted_ssids.iter().any(|w| w == &e.ssid))
            .map(|e| e.signal_dbm)
            .max()
            .map(|v| v as f64),
        SignalSource::Ssid(s) => m
            .scan_results
            .iter()
            .filter(|e| &e.ssid == s)
            .map(|e| e.signal_dbm)
            .max()
            .map(|v| v as f64),
        SignalSource::Bssid(b) => m
            .scan_results
            .iter()
            .find(|e| &e.bssid == b)
            .map(|e| e.signal_dbm as f64),
    }
}

/// Map a value to a (r, g, b) colour on the red → yellow → green scale.
pub fn value_color(val: f64, min: f64, max: f64) -> (f64, f64, f64) {
    let t = if max > min { ((val - min) / (max - min)).clamp(0.0, 1.0) } else { 0.5 };
    if t >= 0.5 { (1.0 - (t - 0.5) * 2.0, 1.0, 0.0) } else { (1.0, t * 2.0, 0.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_metric_is_signal() {
        assert_eq!(ColorMetric::default(), ColorMetric::SignalDbm);
    }

    #[test]
    fn reference_range_signal() {
        assert_eq!(ColorMetric::SignalDbm.reference_range(), (-90.0, -50.0));
    }

    #[test]
    fn reference_range_bandwidth_is_1_gbit() {
        assert_eq!(ColorMetric::IperfMbps.reference_range(), (0.0, 1000.0));
        assert_eq!(ColorMetric::SmbMbps.reference_range(), (0.0, 1000.0));
    }

    #[test]
    fn metric_value_no_signal_is_none() {
        let m = Measurement::no_signal(0.5, 0.5);
        // No-signal points have no value on the scale (they are "no data").
        assert_eq!(metric_value(&m, ColorMetric::SignalDbm, &SignalSource::ConnectedAp), None);
        assert_eq!(metric_value(&m, ColorMetric::IperfMbps, &SignalSource::ConnectedAp), None);
        assert_eq!(metric_value(&m, ColorMetric::SmbMbps, &SignalSource::ConnectedAp), None);
    }

    #[test]
    fn metric_value_signal_resolves_through_source() {
        let mut m = Measurement::new(0.5, 0.5, "Home".to_string(), "AA:BB:CC:DD:EE:01".to_string(), 5180, 36, -55);
        m.scan_results = vec![
            super::super::ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:01".into(), frequency_mhz: 5180, channel: 36, signal_dbm: -55, is_active: true, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
            super::super::ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:02".into(), frequency_mhz: 2437, channel: 6, signal_dbm: -72, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
        ];
        // Connected AP source → the associated AP's dBm.
        assert_eq!(metric_value(&m, ColorMetric::SignalDbm, &SignalSource::ConnectedAp), Some(-55.0));
        // A specific BSSID source → that AP's dBm.
        assert_eq!(metric_value(&m, ColorMetric::SignalDbm, &SignalSource::Bssid("AA:BB:CC:DD:EE:02".to_string())), Some(-72.0));
        // A BSSID not in range → no data.
        assert_eq!(metric_value(&m, ColorMetric::SignalDbm, &SignalSource::Bssid("AA:BB:CC:DD:EE:99".to_string())), None);
        // Throughput metrics ignore the source.
        m.iperf_mbps = Some(120.0);
        assert_eq!(metric_value(&m, ColorMetric::IperfMbps, &SignalSource::Bssid("AA:BB:CC:DD:EE:99".to_string())), Some(120.0));
    }

    #[test]
    fn no_signal_metric_value_resolves_from_scan() {
        use crate::models::ScanEntry;
        let mut m = Measurement::no_signal(0.5, 0.5);
        m.scan_results = vec![
            ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:01".into(), frequency_mhz: 5180, channel: 36, signal_dbm: -60, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
            ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:02".into(), frequency_mhz: 5180, channel: 40, signal_dbm: -75, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
            ScanEntry { ssid: "OtherNet".into(), bssid: "AA:BB:CC:DD:EE:09".into(), frequency_mhz: 2437, channel: 6, signal_dbm: -45, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
        ];
        let wanted = vec!["Home".to_string()];
        // Connected AP: by definition unconnected → no data (gray + cross).
        assert_eq!(no_signal_metric_value(&m, &SignalSource::ConnectedAp, &wanted), None);
        // Named SSID → strongest AP of that SSID in the scan.
        assert_eq!(no_signal_metric_value(&m, &SignalSource::Ssid("Home".to_string()), &wanted), Some(-60.0));
        // Generic SSID source → best AP among the floor's wanted SSIDs
        // ("OtherNet" is in range but not wanted → ignored).
        assert_eq!(no_signal_metric_value(&m, &SignalSource::Ssid(String::new()), &wanted), Some(-60.0));
        // Specific BSSID → that AP's signal.
        assert_eq!(no_signal_metric_value(&m, &SignalSource::Bssid("AA:BB:CC:DD:EE:02".to_string()), &wanted), Some(-75.0));
        // Wanted SSID / BSSID absent from the scan → no data.
        assert_eq!(no_signal_metric_value(&m, &SignalSource::Ssid("Gone".to_string()), &wanted), None);
        assert_eq!(no_signal_metric_value(&m, &SignalSource::Bssid("AA:BB:CC:DD:EE:99".to_string()), &wanted), None);

        // No scan at all (radio off / old point) → no data for any source.
        let bare = Measurement::no_signal(0.5, 0.5);
        assert_eq!(no_signal_metric_value(&bare, &SignalSource::Ssid("Home".to_string()), &wanted), None);
        assert_eq!(no_signal_metric_value(&bare, &SignalSource::Ssid(String::new()), &wanted), None);
        // Regular points are never resolved through this path.
        let reg = Measurement::new(0.5, 0.5, "Home".into(), "AA:BB:CC:DD:EE:01".into(), 5180, 36, -55);
        assert_eq!(no_signal_metric_value(&reg, &SignalSource::Bssid("AA:BB:CC:DD:EE:01".into()), &wanted), None);
    }

    #[test]
    fn value_color_endpoints() {
        // red at min, green at max, yellow at mid (using the signal range)
        let (min, max) = (-90.0, -50.0);
        let (r, g, b) = value_color(-90.0, min, max);
        assert!((r - 1.0).abs() < 1e-6 && g < 1e-6 && b < 1e-6); // red
        let (r, g, b) = value_color(-50.0, min, max);
        assert!(r < 1e-6 && (g - 1.0).abs() < 1e-6 && b < 1e-6); // green
        let (r, g, b) = value_color(-70.0, min, max);
        assert!((r - 1.0).abs() < 1e-6 && (g - 1.0).abs() < 1e-6 && b < 1e-6); // yellow
    }

    #[test]
    fn value_color_clamps_out_of_range() {
        let (min, max) = (0.0, 1000.0);
        // Above max → green, below min → red
        let (r, g, _b) = value_color(5000.0, min, max);
        assert!(r < 1e-6 && (g - 1.0).abs() < 1e-6);
        let (r, g, _b) = value_color(-50.0, min, max);
        assert!((r - 1.0).abs() < 1e-6 && g < 1e-6);
    }
}
