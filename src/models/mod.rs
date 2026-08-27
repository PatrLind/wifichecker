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
