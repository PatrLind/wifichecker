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
            super::super::ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:01".into(), frequency_mhz: 5180, channel: 36, signal_dbm: -55, is_active: true },
            super::super::ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:02".into(), frequency_mhz: 2437, channel: 6, signal_dbm: -72, is_active: false },
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
