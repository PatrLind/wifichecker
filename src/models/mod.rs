pub mod access_point;
pub mod color_metric;
pub mod floor;
pub mod measurement;
pub mod project;
pub mod settings;
pub mod signal_source;

pub use color_metric::ColorMetric;
pub use floor::Floor;
pub use measurement::{Measurement, ScanEntry};
pub use project::Project;
pub use settings::{AppSettings, ThroughputUnit};
pub use signal_source::{SignalSource, signal_for};

/// Band label for a WiFi center frequency in MHz (2.4 / 5 / 6 GHz).
pub fn band_label(freq_mhz: u32) -> &'static str {
    if freq_mhz < 5000 {
        "2.4 GHz"
    } else if freq_mhz < 5925 {
        "5 GHz"
    } else {
        "6 GHz"
    }
}

/// Human-readable channel width: "20 MHz", "40 MHz", "80 MHz", "160 MHz",
/// "320 MHz", or "80+80 MHz" (width 160 with a second center). `None` =
/// unknown.
pub fn width_label(width_mhz: Option<u32>, center_freq2_mhz: Option<u32>) -> Option<String> {
    let w = width_mhz?;
    Some(match w {
        160 if center_freq2_mhz.is_some() => "80+80 MHz".to_string(),
        n => format!("{n} MHz"),
    })
}

/// Human-readable center frequency: "ctr 5190 MHz", or for 80+80
/// "ctr 5190 + 5690 MHz". `None` when the width is 20 MHz or unknown
/// (the center equals the primary frequency then — nothing to show).
pub fn center_label(
    width_mhz: Option<u32>,
    center_freq_mhz: Option<u32>,
    center_freq2_mhz: Option<u32>,
) -> Option<String> {
    let w = width_mhz?;
    if w <= 20 {
        return None;
    }
    let c1 = center_freq_mhz?;
    match center_freq2_mhz {
        Some(c2) if w == 160 => Some(format!("ctr {c1} + {c2} MHz")),
        _ => Some(format!("ctr {c1} MHz")),
    }
}

/// Suffix for channel display lines, e.g. `" | 40 MHz | ctr 5190 MHz"`.
/// Empty for 20 MHz or unknown values (keeps old data displaying as before).
pub fn width_center_suffix(
    width_mhz: Option<u32>,
    center_freq_mhz: Option<u32>,
    center_freq2_mhz: Option<u32>,
) -> String {
    let mut out = String::new();
    // 20 MHz is the default width — omit it from display.
    if width_mhz.unwrap_or(0) > 20 {
        if let Some(w) = width_label(width_mhz, center_freq2_mhz) {
            out.push_str(" | ");
            out.push_str(&w);
        }
    }
    if let Some(c) = center_label(width_mhz, center_freq_mhz, center_freq2_mhz) {
        out.push_str(" | ");
        out.push_str(&c);
    }
    out
}

/// Short width tag for compact list rows: "40M", "80+80M", … `None` for
/// 20 MHz or unknown.
pub fn width_tag(width_mhz: Option<u32>, center_freq2_mhz: Option<u32>) -> Option<String> {
    let w = width_mhz?;
    if w <= 20 {
        return None;
    }
    Some(match w {
        160 if center_freq2_mhz.is_some() => "80+80M".to_string(),
        n => format!("{n}M"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_labels() {
        assert_eq!(band_label(2437), "2.4 GHz");
        assert_eq!(band_label(5180), "5 GHz");
        assert_eq!(band_label(5955), "6 GHz");
    }

    #[test]
    fn width_labels() {
        assert_eq!(width_label(None, None), None);
        assert_eq!(width_label(Some(20), None), Some("20 MHz".to_string()));
        assert_eq!(width_label(Some(40), None), Some("40 MHz".to_string()));
        assert_eq!(width_label(Some(80), None), Some("80 MHz".to_string()));
        assert_eq!(width_label(Some(160), None), Some("160 MHz".to_string()));
        assert_eq!(width_label(Some(160), Some(5690)), Some("80+80 MHz".to_string()));
        assert_eq!(width_label(Some(320), None), Some("320 MHz".to_string()));
    }

    #[test]
    fn center_labels() {
        assert_eq!(center_label(None, Some(5190), None), None);
        assert_eq!(center_label(Some(20), Some(5180), None), None);
        assert_eq!(
            center_label(Some(40), Some(5190), None),
            Some("ctr 5190 MHz".to_string())
        );
        assert_eq!(
            center_label(Some(160), Some(5190), Some(5690)),
            Some("ctr 5190 + 5690 MHz".to_string())
        );
        assert_eq!(center_label(Some(80), None, None), None);
    }

    #[test]
    fn width_center_suffixes() {
        assert_eq!(width_center_suffix(None, None, None), "");
        assert_eq!(width_center_suffix(Some(20), Some(5180), None), "");
        assert_eq!(
            width_center_suffix(Some(40), Some(5190), None),
            " | 40 MHz | ctr 5190 MHz"
        );
        assert_eq!(
            width_center_suffix(Some(160), Some(5190), Some(5690)),
            " | 80+80 MHz | ctr 5190 + 5690 MHz"
        );
    }

    #[test]
    fn width_tags() {
        assert_eq!(width_tag(None, None), None);
        assert_eq!(width_tag(Some(20), None), None);
        assert_eq!(width_tag(Some(40), None), Some("40M".to_string()));
        assert_eq!(width_tag(Some(160), Some(5690)), Some("80+80M".to_string()));
        assert_eq!(width_tag(Some(320), None), Some("320M".to_string()));
    }
}
