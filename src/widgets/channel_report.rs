//! Channel report: per-point and site-survey views of the recorded
//! channel / AP / signal data.
//!
//! Pure post-processing of the scan lists stored with each measurement —
//! no new radio work. Charts are drawn with cairo (same approach as the
//! floor-plan canvas):
//!
//! - x = **frequency** (MHz, increasing left → right), with channel
//!   number tick labels;
//! - y = dBm on a **fixed scale** (-90 bottom … -40 top);
//! - one bar per AP, **as wide as its channel bandwidth** (20 MHz when
//!   unknown), overlapping on the same frequency — tallest drawn first
//!   (in back), semi-transparent so all tops stay visible;
//! - bar **color encodes signal strength** (green → yellow → orange →
//!   red, same scale as the coverage map); the matching table row shows
//!   the same color dot;
//! - hover a bar (or a table row) to spotlight it: the other bars dim
//!   and a colored channel marker points to the highlighted bar's
//!   channel on the axis.
//!
//! The survey view marks **free channels** (no AP in range) with a green
//! tint. The window is non-modal and its point selection is shared with
//! the main floor-plan selection.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use cairo::Context;
use gtk4::prelude::*;

use crate::models::{Measurement, width_tag};

// ── Pure logic (unit-testable) ─────────────────────────────────────────────

/// How per-channel (and per-AP) signals are combined across measurement
/// points in the site-survey view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregation {
    /// Max signal seen (default).
    Strongest,
    /// Mean of all sightings.
    Average,
}

impl Aggregation {
    pub const LABELS: [&str; 2] = ["Strongest", "Average"];

    pub fn from_index(i: u32) -> Self {
        if i == 1 {
            Aggregation::Average
        } else {
            Aggregation::Strongest
        }
    }
}

/// One AP sighting on a channel (from one measurement's scan list).
#[derive(Clone, Debug)]
pub struct Sighting {
    pub channel: u8,
    pub frequency_mhz: u32,
    pub ssid: String,
    pub bssid: String,
    pub signal_dbm: i32,
    pub width_mhz: Option<u32>,
    pub center_freq_mhz: Option<u32>,
    pub center_freq2_mhz: Option<u32>,
}

/// One AP's aggregated presence on a channel (across the selected points).
#[derive(Clone, Debug)]
pub struct ApAgg {
    pub bssid: String,
    /// The AP's base (primary) frequency (MHz) for this channel.
    pub freq_mhz: u32,
    /// Aggregated signal (strongest or average, per the selected mode).
    pub signal_dbm: f64,
    /// How many times this AP was seen on this channel.
    pub sightings: usize,
    /// Latest known channel width (for the bar span), if any.
    pub width_mhz: Option<u32>,
    /// Latest known center frequency 1 (MHz), if any.
    pub center_freq_mhz: Option<u32>,
    /// Latest known center frequency 2 (MHz; 80+80), if any.
    pub center2_freq_mhz: Option<u32>,
}

/// One channel's aggregated state.
#[derive(Clone, Debug)]
pub struct ChannelAgg {
    pub channel: u8,
    pub aps: Vec<ApAgg>,
    /// Channel-level value for the table: strongest (max) or average over
    /// all sightings on the channel.
    pub channel_dbm: f64,
}

/// The channel numbers that exist in each band, ascending by frequency.
/// Verified against the iwlwifi channel list (EU regulatory domain):
/// 2.4 GHz 1–14 · 5 GHz 36–144 (step 4) + 149–177 (step 4) ·
/// 6 GHz 2, 1, 5, 9, …, 233 (step 4).
pub fn band_channels(band: &str) -> Vec<u8> {
    match band {
        "2.4 GHz" => (1..=14).collect(),
        "5 GHz" => {
            let mut v: Vec<u8> = (36..=144).step_by(4).collect();
            v.extend((149..=177).step_by(4));
            v
        }
        "6 GHz" => {
            let mut v: Vec<u8> = vec![2, 1];
            v.extend((5..=233).step_by(4));
            v
        }
        _ => Vec::new(),
    }
}

/// The (min, max) center frequency (MHz) used for a band's x-axis.
pub fn band_freq_range(band: &str) -> (f64, f64) {
    match band {
        "2.4 GHz" => (2400.0, 2500.0),
        "5 GHz" => (5150.0, 5925.0),
        "6 GHz" => (5925.0, 7125.0),
        _ => (0.0, 1.0),
    }
}

/// Center frequency (MHz) of a channel number (2.4 GHz and 5 GHz bands).
pub fn channel_freq(ch: u8) -> f64 {
    if ch <= 14 {
        2407.0 + ch as f64 * 5.0
    } else {
        5000.0 + ch as f64 * 5.0
    }
}

/// Center frequency (MHz) of channel `ch` within `band` — the 6 GHz band
/// uses its own numbering (odd ch n → 5950 + 5n, ch 2 → 5935).
pub fn band_channel_freq(band: &str, ch: u8) -> f64 {
    if band == "6 GHz" {
        return if ch == 2 {
            5935.0
        } else {
            5950.0 + ch as f64 * 5.0
        };
    }
    channel_freq(ch)
}

/// All AP sightings of one measurement on the given band.
///
/// Old points predate the width / center fields in the scan list, but the
/// measurement itself still records them for the AP it was measuring —
/// fall back to those for that BSSID.
pub fn sightings_of(m: &Measurement, band: &str) -> Vec<Sighting> {
    m.scan_results
        .iter()
        .filter(|e| e.band() == band)
        .map(|e| {
            let is_measured = e.bssid == m.bssid;
            let width = e.channel_width_mhz.or(is_measured.then(|| m.channel_width_mhz).flatten());
            let c1 = e.center_freq_mhz.or(is_measured.then(|| m.center_freq_mhz).flatten());
            let c2 = e.center_freq2_mhz.or(is_measured.then(|| m.center_freq2_mhz).flatten());
            Sighting {
                channel: e.channel,
                frequency_mhz: e.frequency_mhz,
                ssid: e.ssid.clone(),
                bssid: e.bssid.clone(),
                signal_dbm: e.signal_dbm,
                width_mhz: width,
                center_freq_mhz: c1,
                center_freq2_mhz: c2,
            }
        })
        .collect()
}

/// Aggregate sightings (from one or more measurements) per channel.
///
/// Per channel: one `ApAgg` per distinct AP (BSSID), aggregated with
/// `agg`; `channel_dbm` = strongest (max) or average over all sightings on
/// the channel. Channels with no sightings are absent from the result.
pub fn aggregate_by_channel(sightings: &[Sighting], agg: Aggregation) -> BTreeMap<u8, ChannelAgg> {
    let mut by_ch: BTreeMap<u8, Vec<&Sighting>> = BTreeMap::new();
    for s in sightings {
        by_ch.entry(s.channel).or_default().push(s);
    }
    let mut out: BTreeMap<u8, ChannelAgg> = BTreeMap::new();
    for (ch, ss) in by_ch {
        let mut by_ap: BTreeMap<&str, Vec<&Sighting>> = BTreeMap::new();
        for s in ss.iter() {
            by_ap.entry(s.bssid.as_str()).or_default().push(s);
        }
        let aps = by_ap
            .into_values()
            .map(|v| {
                let first = v[0];
                let sig = match agg {
                    Aggregation::Strongest => v.iter().map(|s| s.signal_dbm).max().unwrap() as f64,
                    Aggregation::Average => {
                        v.iter().map(|s| s.signal_dbm as f64).sum::<f64>() / v.len() as f64
                    }
                };
                let width = v.iter().rev().find(|s| s.width_mhz.is_some()).and_then(|s| s.width_mhz);
                let c1 = v.iter().rev().find(|s| s.center_freq_mhz.is_some()).and_then(|s| s.center_freq_mhz);
                let c2 = v.iter().rev().find(|s| s.center_freq2_mhz.is_some()).and_then(|s| s.center_freq2_mhz);
                ApAgg {
                    bssid: first.bssid.clone(),
                    freq_mhz: first.frequency_mhz,
                    signal_dbm: sig,
                    sightings: v.len(),
                    width_mhz: width,
                    center_freq_mhz: c1,
                    center2_freq_mhz: c2,
                }
            })
            .collect();
        let channel_dbm = match agg {
            Aggregation::Strongest => ss.iter().map(|s| s.signal_dbm).max().unwrap() as f64,
            Aggregation::Average => ss.iter().map(|s| s.signal_dbm as f64).sum::<f64>() / ss.len() as f64,
        };
        out.insert(ch, ChannelAgg { channel: ch, aps, channel_dbm });
    }
    out
}

/// Dropdown / checkbox label for a measurement: date, network, position
/// (times in the local time zone).
pub fn measurement_label(m: &Measurement) -> String {
    let ts = m.timestamp.with_timezone(&chrono::Local);
    let what = if m.no_signal {
        format!("No connection ({} APs)", m.scan_results.len())
    } else {
        format!("{} (ch {})", m.ssid, m.channel)
    };
    format!(
        "{} {} — {} — ({:.2}, {:.2})",
        ts.format("%Y-%m-%d"),
        ts.format("%H:%M:%S"),
        what,
        m.x,
        m.y
    )
}

/// Color for a signal level — the same green → yellow → orange → red
/// scale as the coverage map (strong → weak).
pub fn signal_color(dbm: f64) -> (f64, f64, f64) {
    const STOPS: [(f64, f64, f64, f64); 4] = [
        (-40.0, 0.18, 0.60, 0.24), // green
        (-60.0, 0.75, 0.58, 0.15), // yellow
        (-75.0, 0.90, 0.45, 0.18), // orange
        (-90.0, 0.90, 0.20, 0.20), // red
    ];
    if dbm >= STOPS[0].0 {
        return (STOPS[0].1, STOPS[0].2, STOPS[0].3);
    }
    if dbm <= STOPS[3].0 {
        return (STOPS[3].1, STOPS[3].2, STOPS[3].3);
    }
    for w in STOPS.windows(2) {
        if dbm <= w[0].0 && dbm > w[1].0 {
            let t = (w[0].0 - dbm) / (w[0].0 - w[1].0);
            return (
                w[0].1 + (w[1].1 - w[0].1) * t,
                w[0].2 + (w[1].2 - w[0].2) * t,
                w[0].3 + (w[1].3 - w[0].3) * t,
            );
        }
    }
    (STOPS[3].1, STOPS[3].2, STOPS[3].3)
}

/// "#RRGGBB" for pango markup dots.
fn color_hex(c: (f64, f64, f64)) -> String {
    let r = (c.0 * 255.0).round() as u8;
    let g = (c.1 * 255.0).round() as u8;
    let b = (c.2 * 255.0).round() as u8;
    format!("#{r:02X}{g:02X}{b:02X}")
}

// ── Chart drawing ──────────────────────────────────────────────────────────

/// Fixed dBm scale for all charts (matches the common WiFi rating range).
const DBM_BOTTOM: f64 = -90.0;
const DBM_TOP: f64 = -40.0;

/// The (left, span) in MHz of the bar lobe(s) for an AP heard on
/// `primary_mhz` with the given width and center frequencies.
///
/// The bar covers the spectrum the AP actually occupies: centered on the
/// recorded center frequency (a 40 MHz AP on 5180 with center 5190
/// covers 5170–5210), or — when that is not recorded — derived from the
/// base frequency (channels are numbered from their primary, so the
/// center is base + w/2 − 10). 80+80 (recorded as 160 MHz with a second
/// center) is drawn as two 80 MHz lobes.
pub fn bar_lobes(
    primary_mhz: f64,
    width_mhz: Option<u32>,
    center_mhz: Option<f64>,
    center2_mhz: Option<f64>,
) -> Vec<(f64, f64)> {
    let w = width_mhz.unwrap_or(20) as f64;
    if width_mhz == Some(160) {
        if let (Some(c1), Some(c2)) = (center_mhz, center2_mhz) {
            return vec![(c1 - 40.0, 80.0), (c2 - 40.0, 80.0)];
        }
    }
    let c = center_mhz.unwrap_or(primary_mhz + w / 2.0 - 10.0);
    vec![(c - w / 2.0, w)]
}

struct ChartBar {
    /// Left edge of the occupied spectrum (MHz).
    left_mhz: f64,
    /// Channel bandwidth (MHz) — the bar's width.
    span_mhz: f64,
    dbm: f64,
    color: (f64, f64, f64),
    /// Which AP this bar is (for list ↔ chart hover sync).
    channel: u8,
    bssid: String,
    /// Hover banner text.
    info: String,
}

/// Everything a channel chart needs to draw.
struct ChartData {
    f_lo: f64,
    f_hi: f64,
    /// Channel tick labels: (center freq MHz, channel number).
    ticks: Vec<(f64, u8)>,
    bars: Vec<ChartBar>,
    /// Mark free (no AP in range) channels with a green tint (survey view).
    mark_free: bool,
    /// Width of the green strip around each free channel (MHz).
    free_strip_mhz: f64,
}

/// Screen-space bar rects, refreshed on every draw pass (hit-testing).
struct BarRect {
    x0: f64,
    x1: f64,
    ytop: f64,
    key: (u8, String),
    info: String,
}

/// The hovered AP: (channel, BSSID) — shared between the chart and the
/// table rows below it.
pub type HoverKey = (u8, String);

/// Draw a channel chart. `hovered` is the highlighted AP (if any). Returns
/// the screen-space bar rects (in draw order) for hover hit-testing.
fn draw_channel_chart(
    ctx: &Context,
    width: f64,
    height: f64,
    data: &ChartData,
    hovered: Option<HoverKey>,
) -> Vec<BarRect> {
    let pad_l = 40.0;
    let pad_r = 8.0;
    let pad_t = 10.0;
    let pad_b = 18.0;
    let plot_w = (width - pad_l - pad_r).max(10.0);
    let plot_h = (height - pad_t - pad_b).max(10.0);
    let span = (data.f_hi - data.f_lo).max(1.0);
    let x_of = |f: f64| pad_l + ((f - data.f_lo) / span).clamp(0.0, 1.0) * plot_w;
    let y_of = |dbm: f64| {
        let d = dbm.clamp(DBM_BOTTOM, DBM_TOP);
        pad_t + ((DBM_TOP - d) / (DBM_TOP - DBM_BOTTOM)) * plot_h
    };

    // Background.
    ctx.set_source_rgb(1.0, 1.0, 1.0);
    ctx.rectangle(0.0, 0.0, width, height);
    ctx.fill().unwrap();

    // Free-channel tints (survey view): a channel with no bar covering it.
    if data.mark_free {
        for (f, _ch) in data.ticks.iter() {
            let covered = data
                .bars
                .iter()
                .any(|b| *f >= b.left_mhz && *f <= b.left_mhz + b.span_mhz);
            if !covered {
                let x0 = x_of(f - data.free_strip_mhz / 2.0);
                let x1 = x_of(f + data.free_strip_mhz / 2.0);
                ctx.set_source_rgba(0.30, 0.75, 0.30, 0.16);
                ctx.rectangle(x0, pad_t, x1 - x0, plot_h);
                ctx.fill().unwrap();
            }
        }
    }

    // Grid lines + y labels (every 10 dBm, fixed scale).
    let mut g = DBM_BOTTOM;
    while g <= DBM_TOP + 1e-9 {
        let y = y_of(g);
        ctx.set_source_rgba(0.0, 0.0, 0.0, 0.08);
        ctx.set_line_width(1.0);
        ctx.move_to(pad_l, y);
        ctx.line_to(pad_l + plot_w, y);
        ctx.stroke().unwrap();
        ctx.set_source_rgb(0.3, 0.3, 0.3);
        ctx.set_font_size(9.0);
        let label = format!("{}", g as i32);
        let tw = ctx.text_extents(&label).map(|t| t.width()).unwrap_or(0.0);
        ctx.move_to(pad_l - 5.0 - tw, y + 3.0);
        let _ = ctx.show_text(&label);
        g += 10.0;
    }

    // X tick labels: channel numbers at their center frequencies.
    let n = data.ticks.len().max(1);
    let step = (((n as f64) * 26.0 / plot_w).ceil() as usize).max(1);
    ctx.set_source_rgb(0.3, 0.3, 0.3);
    for (_i, (f, ch)) in data.ticks.iter().enumerate().step_by(step) {
        let x = x_of(*f);
        ctx.set_line_width(1.0);
        ctx.move_to(x, pad_t + plot_h);
        ctx.line_to(x, pad_t + plot_h + 4.0);
        ctx.stroke().unwrap();
        ctx.set_font_size(9.0);
        let text = ch.to_string();
        let tw = ctx.text_extents(&text).map(|t| t.width()).unwrap_or(0.0);
        ctx.move_to(x - tw / 2.0, height - 4.0);
        let _ = ctx.show_text(&text);
    }

    // Bars: tallest first (in back), semi-transparent, as wide as the AP's
    // channel bandwidth, starting at the AP's base frequency.
    let mut bars: Vec<&ChartBar> = data.bars.iter().collect();
    bars.sort_by(|a, b| b.dbm.partial_cmp(&a.dbm).unwrap_or(std::cmp::Ordering::Equal));
    // When a bar is hovered, draw it last (in front) so the dimmed bars
    // don't overlap on top of the highlighted one.
    if let Some(key) = &hovered {
        if let Some(i) = bars.iter().rposition(|b| b.channel == key.0 && b.bssid == key.1) {
            let b = bars.remove(i);
            bars.push(b);
        }
    }
    let bottom = pad_t + plot_h;
    let mut rects = Vec::new();
    for b in bars.iter() {
        let x0 = x_of(b.left_mhz);
        let bw = (b.span_mhz / span * plot_w).max(2.0);
        let x0 = x0.clamp(pad_l, (pad_l + plot_w - bw).max(pad_l));
        let ytop = y_of(b.dbm);
        let hot = hovered.as_ref() == Some(&(b.channel, b.bssid.clone()));
        let alpha = if hot { 0.95 } else if hovered.is_some() { 0.15 } else { 0.55 };
        ctx.set_source_rgba(b.color.0, b.color.1, b.color.2, alpha);
        ctx.rectangle(x0, ytop, bw, bottom - ytop);
        ctx.fill().unwrap();
        if hot {
            // Black outline so the highlighted bar stands out from the
            // dimmed bars behind/in front of it.
            ctx.set_source_rgb(0.03, 0.03, 0.03);
            ctx.set_line_width(2.5);
        } else {
            ctx.set_source_rgba(0.0, 0.0, 0.0, 0.22);
            ctx.set_line_width(1.0);
        }
        ctx.rectangle(x0, ytop, bw, bottom - ytop);
        ctx.stroke().unwrap();
        rects.push(BarRect {
            x0,
            x1: x0 + bw,
            ytop,
            key: (b.channel, b.bssid.clone()),
            info: b.info.clone(),
        });
    }

    // Hover banner.
    if let Some(key) = &hovered {
        if let Some(r) = rects.iter().find(|r| &r.key == key) {
            ctx.set_font_size(10.0);
            let (tw, th) = ctx
                .text_extents(&r.info)
                .map(|t| (t.width(), t.height()))
                .unwrap_or((0.0, 10.0));
            let bw = tw + 12.0;
            let bx = (pad_l + plot_w / 2.0 - bw / 2.0).clamp(pad_l, (pad_l + plot_w - bw).max(pad_l));
            let by = pad_t + 2.0;
            ctx.set_source_rgba(1.0, 1.0, 1.0, 0.92);
            ctx.rectangle(bx, by, bw, th + 6.0);
            ctx.fill().unwrap();
            ctx.set_source_rgba(0.0, 0.0, 0.0, 0.4);
            ctx.set_line_width(1.0);
            ctx.rectangle(bx, by, bw, th + 6.0);
            ctx.stroke().unwrap();
            ctx.set_source_rgb(0.1, 0.1, 0.1);
            ctx.move_to(bx + 6.0, by + th + 2.0);
            let _ = ctx.show_text(&r.info);
        }
    }

    // Plot frame.
    ctx.set_source_rgba(0.0, 0.0, 0.0, 0.25);
    ctx.set_line_width(1.0);
    ctx.rectangle(pad_l, pad_t, plot_w, plot_h);
    ctx.stroke().unwrap();

    // Channel marker: a small black triangle at the bottom of the plot,
    // pointing up at the highlighted bar. Drawn last so it is always on
    // top of the (dimmed) bars and the frame.
    if let Some(key) = &hovered {
        if let Some(r) = rects.iter().find(|r| &r.key == key) {
            let cx = (r.x0 + r.x1) / 2.0;
            let yb = pad_t + plot_h;
            ctx.set_source_rgb(0.03, 0.03, 0.03);
            ctx.move_to(cx - 6.0, yb + 1.0);
            ctx.line_to(cx + 6.0, yb + 1.0);
            ctx.line_to(cx, yb - 11.0);
            ctx.close_path();
            ctx.fill().unwrap();
        }
    }

    rects
}

/// Hit-test the pointer against the last-drawn bar rects (frontmost —
/// last drawn — wins).
fn chart_hit_test(rects: &Rc<RefCell<Vec<BarRect>>>, x: f64, y: f64) -> Option<HoverKey> {
    rects
        .borrow()
        .iter()
        .rposition(|r| x >= r.x0 && x <= r.x1 && y >= r.ytop)
        .map(|i| rects.borrow()[i].key.clone())
}

// ── UI ─────────────────────────────────────────────────────────────────────

const BANDS: [&str; 3] = ["2.4 GHz", "5 GHz", "6 GHz"];

/// Open the Channel Report (non-modal) window for one floor's
/// measurements. Returns the window so the caller can track it.
///
/// `on_select`: called when the user picks a measurement in the Point view
/// (the main window selects it on the map too). The main window in turn
/// calls the window's stored `"channel-report-select"` callback whenever
/// the map/list selection changes, keeping both in sync.
///
/// `measurements`: all of the floor's measurements (points without scan
/// data are skipped).
pub fn show_channel_report(
    window: &libadwaita::ApplicationWindow,
    floor_name: &str,
    measurements: Vec<Measurement>,
    aliases: HashMap<String, String>,
    on_select: Box<dyn Fn(Option<String>) + 'static>,
) -> gtk4::Window {
    let points: Vec<Measurement> =
        measurements.into_iter().filter(|m| !m.scan_results.is_empty()).collect();

    let dialog = gtk4::Window::builder()
        .title(format!("Channel Report — {floor_name}"))
        .transient_for(window)
        .default_width(1020)
        .default_height(640)
        .build();

    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    root.set_margin_top(10);
    root.set_margin_bottom(10);
    root.set_margin_start(12);
    root.set_margin_end(12);

    if points.is_empty() {
        let label = gtk4::Label::new(Some(
            "No scan data to report yet.\nRecord measurements (with the radio on) first — every point stores the networks in range.",
        ));
        label.set_wrap(true);
        label.set_max_width_chars(60);
        root.append(&label);
        dialog.set_child(Some(&root));
        dialog.present();
        return dialog;
    }

    let points = Rc::new(points);
    let aliases = Rc::new(aliases);
    let on_select: Rc<dyn Fn(Option<String>)> = Rc::from(on_select);
    let current_id: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let stack = gtk4::Stack::new();
    let switcher = gtk4::StackSwitcher::builder().stack(&stack).build();
    root.append(&switcher);
    root.append(&stack);

    // ── Point view ────────────────────────────────────────────────────────
    let point_page = gtk4::Box::new(gtk4::Orientation::Vertical, 8);

    let point_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let point_lbl = gtk4::Label::new(Some("Measurement:"));
    point_lbl.set_xalign(0.0);
    let point_model = gtk4::StringList::new(&[]);
    for m in points.iter() {
        point_model.append(&measurement_label(m));
    }
    let point_dd = gtk4::DropDown::new(Some(point_model), gtk4::Expression::NONE);
    point_dd.set_valign(gtk4::Align::Center);
    point_row.append(&point_lbl);
    point_row.append(&point_dd);
    point_page.append(&point_row);

    let point_content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    let point_sw = gtk4::ScrolledWindow::new();
    point_sw.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Always);
    point_sw.set_child(Some(&point_content));
    point_sw.set_vexpand(true);
    point_page.append(&point_sw);

    let (psw, pp, pa) = (point_sw.clone(), Rc::clone(&points), Rc::clone(&aliases));
    rebuild_point_content(&psw, &pp, &pa, 0);
    point_dd.connect_selected_notify(move |dd| {
        let idx = dd.selected() as usize;
        if let Some(m) = pp.get(idx) {
            on_select(Some(m.id.clone()));
        }
    });

    stack.add_titled(&point_page, Some("point"), "Point");

    // ── Site survey view ──────────────────────────────────────────────────
    let survey_page = gtk4::Box::new(gtk4::Orientation::Vertical, 8);

    let survey_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let select_all = gtk4::Button::with_label("Select all");
    let clear_all = gtk4::Button::with_label("Clear all");
    let agg_lbl = gtk4::Label::new(Some("Aggregation:"));
    agg_lbl.set_xalign(0.0);
    let agg_model = gtk4::StringList::new(&Aggregation::LABELS);
    let agg_dd = gtk4::DropDown::new(Some(agg_model), gtk4::Expression::NONE);
    agg_dd.set_valign(gtk4::Align::Center);
    survey_row.append(&select_all);
    survey_row.append(&clear_all);
    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    survey_row.append(&spacer);
    survey_row.append(&agg_lbl);
    survey_row.append(&agg_dd);
    survey_page.append(&survey_row);

    let sel = Rc::new(RefCell::new(vec![true; points.len()]));
    let agg = Rc::new(RefCell::new(Aggregation::Strongest));
    let updating = Rc::new(RefCell::new(false));

    let survey_content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    let survey_sw = gtk4::ScrolledWindow::new();
    survey_sw.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Always);
    survey_sw.set_child(Some(&survey_content));
    survey_sw.set_hexpand(true);
    survey_sw.set_vexpand(true);

    // Rebuild the survey content from the shared state. Each rebuild swaps
    // a fresh content box into the scrolled window.
    let rebuild = {
        let (sp, ss, sa, sg, sw) = (
            Rc::clone(&points),
            Rc::clone(&sel),
            Rc::clone(&aliases),
            Rc::clone(&agg),
            survey_sw.clone(),
        );
        Rc::new(move || {
            let sel_now = ss.borrow().clone();
            let al = Rc::clone(&sa);
            rebuild_survey_content(&sw, &sp, &al, &sel_now, *sg.borrow());
        })
    };

    let checks_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let checks_sw = gtk4::ScrolledWindow::new();
    checks_sw.set_min_content_width(340);
    checks_sw.set_propagate_natural_width(true);
    checks_sw.set_child(Some(&checks_box));

    let checks: Rc<RefCell<Vec<gtk4::CheckButton>>> = Rc::new(RefCell::new(Vec::new()));

    for (i, m) in points.iter().enumerate() {
        let lbl = measurement_label(m);
        let cb = gtk4::CheckButton::builder().label(lbl).active(true).build();
        cb.set_margin_start(4);
        cb.set_margin_end(4);
        checks_box.append(&cb);
        let cb2 = cb.clone();
        let (updating, sel, rebuild) =
            (Rc::clone(&updating), Rc::clone(&sel), Rc::clone(&rebuild));
        let i = i as i32;
        cb.connect_toggled(move |_| {
            if *updating.borrow() {
                return;
            }
            sel.borrow_mut()[i as usize] = cb2.is_active();
            rebuild();
        });
        checks.borrow_mut().push(cb);
    }

    let wire_all = |btn: &gtk4::Button, on: bool| {
        let (checks, updating, sel, rebuild) =
            (checks.clone(), Rc::clone(&updating), Rc::clone(&sel), Rc::clone(&rebuild));
        let n = points.len();
        btn.connect_clicked(move |_| {
            *updating.borrow_mut() = true;
            for cb in checks.borrow().iter() {
                cb.set_active(on);
            }
            *updating.borrow_mut() = false;
            *sel.borrow_mut() = vec![on; n];
            rebuild();
        });
    };
    wire_all(&select_all, true);
    wire_all(&clear_all, false);

    {
        let (agg, rebuild) = (Rc::clone(&agg), Rc::clone(&rebuild));
        agg_dd.connect_selected_notify(move |dd| {
            *agg.borrow_mut() = Aggregation::from_index(dd.selected());
            rebuild();
        });
    }

    let survey_hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    survey_hbox.append(&checks_sw);
    survey_hbox.append(&survey_sw);
    survey_page.append(&survey_hbox);

    rebuild();

    stack.add_titled(&survey_page, Some("survey"), "Site survey");
    stack.set_visible_child_name("point");

    // Shared selection: the main window calls this when the map/list
    // selection changes.
    let (dd_for, pts_for, cur_for) = (point_dd.clone(), Rc::clone(&points), Rc::clone(&current_id));
    let select_cb = Rc::new(move |id: Option<String>| {
        *cur_for.borrow_mut() = id.clone();
        if let Some(id) = id {
            if let Some(idx) = pts_for.iter().position(|m| m.id == id) {
                dd_for.set_selected(idx as u32);
            }
        }
    });
    unsafe {
        dialog.set_data("channel-report-select", select_cb as Rc<dyn Fn(Option<String>)>);
    }

    dialog.set_child(Some(&root));
    dialog.present();
    dialog
}

/// (Re)build the point view for the selected measurement. Each call swaps a
/// fresh content box into the scrolled window.
fn rebuild_point_content(
    sw: &gtk4::ScrolledWindow,
    points: &Rc<Vec<Measurement>>,
    aliases: &Rc<HashMap<String, String>>,
    idx: u32,
) {
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    let Some(m) = points.get(idx as usize) else {
        let l = gtk4::Label::new(Some("Select a measurement."));
        content.append(&l);
        sw.set_child(Some(&content));
        return;
    };
    let mut any = false;
    for &band in BANDS.iter() {
        let sightings = sightings_of(m, band);
        if sightings.is_empty() {
            continue;
        }
        any = true;
        append_point_band_section(&content, band, &sightings, aliases);
    }
    if !any {
        let l = gtk4::Label::new(Some("This measurement has no scan data."));
        content.append(&l);
    }
    sw.set_child(Some(&content));
}

fn append_point_band_section(
    content: &gtk4::Box,
    band: &str,
    sightings: &[Sighting],
    aliases: &Rc<HashMap<String, String>>,
) {
    let heading = gtk4::Label::new(Some(band));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    content.append(&heading);

    let (f_lo, f_hi) = band_freq_range(band);
    let ticks: Vec<(f64, u8)> =
        band_channels(band).iter().map(|&c| (band_channel_freq(band, c), c)).collect();

    // One bar per (channel, BSSID); duplicates keep the strongest sighting.
    let mut best: BTreeMap<u8, BTreeMap<String, (i32, Option<u32>, u32, Option<u32>, Option<u32>)>> =
        BTreeMap::new();
    for s in sightings {
        let e = best
            .entry(s.channel)
            .or_default()
            .entry(s.bssid.clone())
            .or_insert((i32::MIN, None, s.frequency_mhz, None, None));
        e.0 = e.0.max(s.signal_dbm);
        if e.1.is_none() {
            e.1 = s.width_mhz;
        }
        if e.3.is_none() {
            e.3 = s.center_freq_mhz;
        }
        if e.4.is_none() {
            e.4 = s.center_freq2_mhz;
        }
    }
    let bars: Vec<ChartBar> = best
        .iter()
        .flat_map(|(ch, aps)| {
            aps.iter().flat_map(move |(bssid, (dbm, width, freq, c1, c2))| {
                let f = *freq as f64;
                let ap = ap_name(aliases, bssid);
                let width_txt = width_tag(*width, *c2).unwrap_or_else(|| "20M".to_string());
                let color = signal_color(*dbm as f64);
                bar_lobes(f, *width, c1.map(|c| c as f64), c2.map(|c| c as f64))
                    .into_iter()
                    .map(move |(left, w)| ChartBar {
                        left_mhz: left,
                        span_mhz: w,
                        dbm: *dbm as f64,
                        color,
                        channel: *ch,
                        bssid: bssid.clone(),
                        info: format!("{ap} · ch {ch} · {freq} MHz · {width_txt} · {dbm} dBm"),
                    })
            })
        })
        .collect();

    let sync = BandSync::new();
    append_chart(
        content,
        ChartData { f_lo, f_hi, ticks, bars, mark_free: false, free_strip_mhz: 0.0 },
        &sync,
    );

    // Table: one row per AP sighting (strongest first per channel).
    // Hovering a row highlights its chart bar (and vice versa).
    let header = monospace_row(
        &format!("{:<5} {:<9} {:<7} {:<16} {:<19} {:<14} {:>5}", "ch", "freq MHz", "width", "SSID", "BSSID", "AP", "dBm"),
        false,
    );
    content.append(&header);
    let mut sorted: Vec<&Sighting> = sightings.iter().collect();
    // Strongest signal first.
    sorted.sort_by(|a, b| {
        b.signal_dbm.cmp(&a.signal_dbm).then(b.channel.cmp(&a.channel))
    });
    for s in sorted {
        let w = width_tag(s.width_mhz, s.center_freq2_mhz).unwrap_or_else(|| "20M".to_string());
        let hex = color_hex(signal_color(s.signal_dbm as f64));
        let bssid_cell = format!("<span foreground=\"{hex}\">●</span> {}", s.bssid);
        let ap = aliases.get(&s.bssid).map(|a| truncate(a, 14)).unwrap_or_default();
        let row = monospace_row(
            &format!(
                "{:<5} {:<9} {:<7} {:<16} {:<19} {:<14} {:>5}",
                s.channel,
                s.frequency_mhz,
                w,
                truncate(&s.ssid, 16),
                bssid_cell,
                ap,
                s.signal_dbm
            ),
            true,
        );
        let key = (s.channel, s.bssid.clone());
        sync.add_row(key.clone(), &row);
        wire_row_hover(&row, key, &sync);
        content.append(&row);
    }
}

/// (Re)build the site-survey content for the current selection /
/// aggregation. Each call swaps a fresh content box into the scrolled
/// window.
fn rebuild_survey_content(
    sw: &gtk4::ScrolledWindow,
    points: &Rc<Vec<Measurement>>,
    aliases: &Rc<HashMap<String, String>>,
    sel: &[bool],
    agg: Aggregation,
) {
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    let selected: Vec<&Measurement> = points
        .iter()
        .enumerate()
        .filter(|(i, _)| sel[*i])
        .map(|(_, m)| m)
        .collect();

    let count_lbl = gtk4::Label::new(Some(&format!(
        "{} of {} measurement(s) selected",
        selected.len(),
        points.len()
    )));
    count_lbl.set_xalign(0.0);
    content.append(&count_lbl);

    if selected.is_empty() {
        let l = gtk4::Label::new(Some("Select at least one measurement."));
        content.append(&l);
        sw.set_child(Some(&content));
        return;
    }

    let mut any = false;
    for &band in BANDS.iter() {
        let sightings: Vec<Sighting> =
            selected.iter().flat_map(|m| sightings_of(m, band)).collect();
        if sightings.is_empty() {
            continue;
        }
        any = true;
        append_survey_band_section(&content, band, &sightings, agg, aliases);
    }
    if !any {
        let l = gtk4::Label::new(Some("No scan data in the selected measurements."));
        content.append(&l);
    }
    sw.set_child(Some(&content));
}

fn append_survey_band_section(
    content: &gtk4::Box,
    band: &str,
    sightings: &[Sighting],
    agg: Aggregation,
    aliases: &Rc<HashMap<String, String>>,
) {
    let heading = gtk4::Label::new(Some(band));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    content.append(&heading);

    let agg_map = aggregate_by_channel(sightings, agg);
    let (f_lo, f_hi) = band_freq_range(band);
    let ticks: Vec<(f64, u8)> =
        band_channels(band).iter().map(|&c| (band_channel_freq(band, c), c)).collect();
    let free_strip = if band == "2.4 GHz" { 5.0 } else { 20.0 };

    let bars: Vec<ChartBar> = agg_map
        .iter()
        .flat_map(|(ch, c)| {
            c.aps.iter().flat_map(move |a| {
                let ap = ap_name(aliases, &a.bssid);
                let sig = match agg {
                    Aggregation::Strongest => format!("{:.0}", a.signal_dbm),
                    Aggregation::Average => format!("{:.1}", a.signal_dbm),
                };
                let color = signal_color(a.signal_dbm);
                bar_lobes(
                    a.freq_mhz as f64,
                    a.width_mhz,
                    a.center_freq_mhz.map(|c| c as f64),
                    a.center2_freq_mhz.map(|c| c as f64),
                )
                .into_iter()
                .map(move |(left, w)| ChartBar {
                    left_mhz: left,
                    span_mhz: w,
                    dbm: a.signal_dbm,
                    color,
                    channel: *ch,
                    bssid: a.bssid.clone(),
                    info: format!("{ap} · ch {ch} · {} MHz · {sig} dBm (×{})", a.freq_mhz, a.sightings),
                })
            })
        })
        .collect();

    let sync = BandSync::new();
    append_chart(
        content,
        ChartData { f_lo, f_hi, ticks, bars, mark_free: true, free_strip_mhz: free_strip },
        &sync,
    );

    // Table: per channel (only channels with data): AP count + aggregated
    // signal, then one row per AP (with its color dot).
    let head = match agg {
        Aggregation::Strongest => format!("{:<5} {:<8} {:>8}", "ch", "APs", "strongest"),
        Aggregation::Average => format!("{:<5} {:<8} {:>8}", "ch", "APs", "average"),
    };
    let header = monospace_row(&head, false);
    content.append(&header);
    for c in agg_map.values() {
        let sig = match agg {
            Aggregation::Strongest => format!("{:5.0}", c.channel_dbm),
            Aggregation::Average => format!("{:5.1}", c.channel_dbm),
        };
        let row = monospace_row(&format!("{:<5} {:<8} {:>8}", c.channel, c.aps.len(), sig), false);
        content.append(&row);
        let mut aps: Vec<&ApAgg> = c.aps.iter().collect();
        aps.sort_by(|a, b| b.signal_dbm.partial_cmp(&a.signal_dbm).unwrap_or(std::cmp::Ordering::Equal));
        for a in aps {
            let hex = color_hex(signal_color(a.signal_dbm));
            let ap = aliases.get(&a.bssid).map(|x| truncate(x, 14)).unwrap_or_default();
            let sig = match agg {
                Aggregation::Strongest => format!("{:.0}", a.signal_dbm),
                Aggregation::Average => format!("{:.1}", a.signal_dbm),
            };
            let row = monospace_row(
                &format!(
                    "  <span foreground=\"{hex}\">●</span> {:<17}  {:>6} (×{})  {}",
                    a.bssid,
                    sig,
                    a.sightings,
                    ap
                ),
                true,
            );
            let key = (c.channel, a.bssid.clone());
            sync.add_row(key.clone(), &row);
            wire_row_hover(&row, key, &sync);
            content.append(&row);
        }
    }
}

/// Hover state shared between one band's chart and its table rows: the
/// hovered AP key, plus the registered rows (for highlighting the row
/// when the chart bar is hovered, and vice versa). Cheap to clone (Rc
/// fields), so handlers each keep their own copy.
#[derive(Clone)]
struct BandSync {
    hover: Rc<RefCell<Option<HoverKey>>>,
    rows: Rc<RefCell<Vec<(HoverKey, gtk4::Label)>>>,
    /// The band's chart areas (redrawn when the hover changes, from
    /// either side).
    areas: Rc<RefCell<Vec<gtk4::DrawingArea>>>,
}

impl BandSync {
    fn new() -> Self {
        Self {
            hover: Rc::new(RefCell::new(None)),
            rows: Rc::new(RefCell::new(Vec::new())),
            areas: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Register a table row for an AP.
    fn add_row(&self, key: HoverKey, row: &gtk4::Label) {
        row.add_css_class("channel-row");
        let on = self.hover.borrow().as_ref() == Some(&key);
        if on {
            row.add_css_class("channel-row-hover");
        }
        self.rows.borrow_mut().push((key, row.clone()));
    }

    /// Set the hovered AP (from the chart or from a row). Refreshes which
    /// rows are highlighted and redraws the chart.
    fn set_hover(&self, key: Option<HoverKey>) {
        if self.hover.borrow().as_ref() != key.as_ref() {
            *self.hover.borrow_mut() = key.clone();
            let hov = self.hover.borrow();
            for (k, l) in self.rows.borrow().iter() {
                if hov.as_ref() == Some(k) {
                    l.add_css_class("channel-row-hover");
                } else {
                    l.remove_css_class("channel-row-hover");
                }
            }
            for a in self.areas.borrow().iter() {
                a.queue_draw();
            }
        }
    }
}

/// Append a cairo chart (with hover support, synced with `sync`'s rows)
/// for the given data.
fn append_chart(content: &gtk4::Box, chart: ChartData, sync: &BandSync) {
    let area = gtk4::DrawingArea::new();
    area.set_content_height(190);
    area.set_valign(gtk4::Align::Start);
    let rects: Rc<RefCell<Vec<BarRect>>> = Rc::new(RefCell::new(Vec::new()));
    sync.areas.borrow_mut().push(area.clone());
    let (rects_draw, hover_draw) = (Rc::clone(&rects), Rc::clone(&sync.hover));
    area.set_draw_func(move |_area, ctx, w, h| {
        let r = draw_channel_chart(ctx, w as f64, h as f64, &chart, hover_draw.borrow().clone());
        *rects_draw.borrow_mut() = r;
    });
    let motion = gtk4::EventControllerMotion::new();
    {
        let (a, r, s) = (area.clone(), Rc::clone(&rects), sync.clone());
        motion.connect_enter(move |_c, x, y| {
            s.set_hover(chart_hit_test(&r, x, y));
            a.queue_draw();
        });
    }
    {
        let (a, r, s) = (area.clone(), Rc::clone(&rects), sync.clone());
        motion.connect_motion(move |_c, x, y| {
            s.set_hover(chart_hit_test(&r, x, y));
            a.queue_draw();
        });
    }
    {
        let (a, s) = (area.clone(), sync.clone());
        motion.connect_leave(move |_c| {
            s.set_hover(None);
            a.queue_draw();
        });
    }
    area.add_controller(motion);
    content.append(&area);
}

/// A selectable monospace text line (the "table" rows). `markup` enables
/// pango markup (used for the colored AP dots).
fn monospace_row(text: &str, markup: bool) -> gtk4::Label {
    let l = gtk4::Label::new(Some(text));
    l.add_css_class("monospace");
    l.set_selectable(true);
    l.set_use_markup(markup);
    l.set_xalign(0.0);
    l
}

/// The AP's display name: its alias when known, otherwise its BSSID.
fn ap_name(aliases: &Rc<HashMap<String, String>>, bssid: &str) -> String {
    match aliases.get(bssid) {
        Some(a) => a.clone(),
        None => bssid.to_string(),
    }
}

/// Wire a table row's hover to the shared band hover state (highlights
/// the matching chart bar).
fn wire_row_hover(row: &gtk4::Label, key: HoverKey, sync: &BandSync) {
    let motion = gtk4::EventControllerMotion::new();
    let (s, k) = (sync.clone(), key.clone());
    motion.connect_enter(move |_c, _x, _y| {
        s.set_hover(Some(k.clone()));
    });
    let (s, k) = (sync.clone(), key);
    motion.connect_leave(move |_c| {
        if s.hover.borrow().as_ref() == Some(&k) {
            s.set_hover(None);
        }
    });
    row.add_controller(motion);
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ScanEntry;

    fn entry(ssid: &str, bssid: &str, ch: u8, freq: u32, dbm: i32, width: Option<u32>) -> ScanEntry {
        ScanEntry {
            ssid: ssid.to_string(),
            bssid: bssid.to_string(),
            frequency_mhz: freq,
            channel: ch,
            signal_dbm: dbm,
            is_active: false,
            channel_width_mhz: width,
            center_freq_mhz: None,
            center_freq2_mhz: None,
        }
    }

    fn m_with_scan(scan: Vec<ScanEntry>) -> Measurement {
        let mut m =
            Measurement::new(0.5, 0.5, "Home".into(), "AA:BB:CC:DD:EE:01".into(), 5180, 36, -55);
        m.scan_results = scan;
        m
    }

    fn sig(ch: u8, bssid: &str, dbm: i32) -> Sighting {
        Sighting {
            channel: ch,
            frequency_mhz: 5000 + ch as u32 * 5,
            ssid: "Home".into(),
            bssid: bssid.to_string(),
            signal_dbm: dbm,
            width_mhz: None,
            center_freq_mhz: None,
            center_freq2_mhz: None,
        }
    }

    #[test]
    fn band_channels_lists() {
        assert_eq!(band_channels("2.4 GHz"), (1..=14).collect::<Vec<u8>>());
        let g5 = band_channels("5 GHz");
        assert!(g5.contains(&36) && g5.contains(&144) && g5.contains(&149) && g5.contains(&177));
        assert_eq!(g5[0], 36);
        assert_eq!(g5.last(), Some(&177));
        let g6 = band_channels("6 GHz");
        assert_eq!(g6[0], 2); // 5935 MHz is the lowest 6 GHz channel
        assert_eq!(g6[1], 1);
        assert_eq!(g6.last(), Some(&233));
        assert_eq!(g6.len(), 60);
        assert!(band_channels("weird").is_empty());
    }

    #[test]
    fn band_freq_ranges_and_channel_freq() {
        assert_eq!(band_freq_range("2.4 GHz"), (2400.0, 2500.0));
        assert_eq!(band_freq_range("5 GHz"), (5150.0, 5925.0));
        assert_eq!(band_freq_range("6 GHz"), (5925.0, 7125.0));
        assert_eq!(channel_freq(1), 2412.0);
        assert_eq!(channel_freq(6), 2437.0);
        assert_eq!(channel_freq(36), 5180.0);
        assert_eq!(channel_freq(149), 5745.0);
        // 6 GHz: its own numbering
        assert_eq!(band_channel_freq("6 GHz", 2), 5935.0);
        assert_eq!(band_channel_freq("6 GHz", 1), 5955.0);
        assert_eq!(band_channel_freq("6 GHz", 233), 7115.0);
        assert_eq!(band_channel_freq("5 GHz", 36), 5180.0);
        assert_eq!(band_channel_freq("2.4 GHz", 6), 2437.0);
    }

    #[test]
    fn bar_lobes_cover_the_actual_spectrum() {
        // 20 MHz (width unknown): centered on the channel frequency.
        assert_eq!(bar_lobes(5180.0, None, None, None), vec![(5170.0, 20.0)]);
        assert_eq!(bar_lobes(2437.0, Some(20), None, None), vec![(2427.0, 20.0)]);
        // 40 MHz with recorded center: 5180 + center 5190 → 5170–5210.
        assert_eq!(bar_lobes(5180.0, Some(40), Some(5190.0), None), vec![(5170.0, 40.0)]);
        // 40 MHz without center: derived from the base (5180 + 10 = 5190).
        assert_eq!(bar_lobes(5180.0, Some(40), None, None), vec![(5170.0, 40.0)]);
        // 80 MHz: base + 30 = center, so 5180 → 5170–5250.
        assert_eq!(bar_lobes(5180.0, Some(80), Some(5210.0), None), vec![(5170.0, 80.0)]);
        // 160 MHz: base + 70 = center 5250 → 5180 → 5170–5330.
        assert_eq!(bar_lobes(5180.0, Some(160), None, None), vec![(5170.0, 160.0)]);
        // 80+80 (stored as 160 with a second center): two 80 MHz lobes.
        assert_eq!(
            bar_lobes(5180.0, Some(160), Some(5190.0), Some(5270.0)),
            vec![(5150.0, 80.0), (5230.0, 80.0)]
        );
    }

    #[test]
    fn sightings_filter_by_band() {
        let m = m_with_scan(vec![
            entry("Home", "AA:BB:CC:DD:EE:01", 6, 2437, -60, None),
            entry("Home", "AA:BB:CC:DD:EE:02", 36, 5180, -55, Some(40)),
            entry("Home", "AA:BB:CC:DD:EE:03", 1, 5955, -70, None),
        ]);
        assert_eq!(sightings_of(&m, "2.4 GHz").len(), 1);
        assert_eq!(sightings_of(&m, "5 GHz").len(), 1);
        assert_eq!(sightings_of(&m, "6 GHz").len(), 1);
        assert_eq!(sightings_of(&m, "5 GHz")[0].channel, 36);
        assert_eq!(sightings_of(&m, "5 GHz")[0].width_mhz, Some(40));
    }

    #[test]
    fn sightings_fall_back_to_measurement_width() {
        // A point whose scan entries lack width/center (old data), but
        // whose measurement recorded them for the measured AP.
        let mut m = m_with_scan(vec![
            entry("Home", "AA:BB:CC:DD:EE:01", 36, 5180, -55, None),
            entry("Other", "AA:BB:CC:DD:EE:02", 36, 5180, -70, None),
        ]);
        m.channel_width_mhz = Some(40);
        m.center_freq_mhz = Some(5190);
        let ss = sightings_of(&m, "5 GHz");
        assert_eq!(ss.len(), 2);
        let own = ss.iter().find(|s| s.bssid == "AA:BB:CC:DD:EE:01").unwrap();
        assert_eq!(own.width_mhz, Some(40));
        assert_eq!(own.center_freq_mhz, Some(5190));
        let other = ss.iter().find(|s| s.bssid == "AA:BB:CC:DD:EE:02").unwrap();
        assert_eq!(other.width_mhz, None);
    }

    #[test]
    fn aggregate_strongest_and_average() {
        let mut ss = vec![
            sig(36, "AA:BB:CC:DD:EE:01", -60),
            sig(36, "AA:BB:CC:DD:EE:01", -70),
            sig(36, "AA:BB:CC:DD:EE:02", -50),
            sig(40, "AA:BB:CC:DD:EE:02", -55),
        ];
        ss[2].width_mhz = Some(40);
        let strong = aggregate_by_channel(&ss, Aggregation::Strongest);
        assert_eq!(strong.len(), 2);
        let ch36 = &strong[&36];
        assert_eq!(ch36.aps.len(), 2);
        let a1 = ch36.aps.iter().find(|a| a.bssid == "AA:BB:CC:DD:EE:01").unwrap();
        assert_eq!(a1.signal_dbm, -60.0);
        assert_eq!(a1.sightings, 2);
        assert_eq!(a1.freq_mhz, 5180);
        assert_eq!(ch36.channel_dbm, -50.0); // strongest across the channel
        let ch40 = &strong[&40];
        assert_eq!(ch40.channel_dbm, -55.0);
        let a2 = ch36.aps.iter().find(|a| a.bssid == "AA:BB:CC:DD:EE:02").unwrap();
        assert_eq!(a2.width_mhz, Some(40));

        let avg = aggregate_by_channel(&ss, Aggregation::Average);
        let ch36 = &avg[&36];
        let a1 = ch36.aps.iter().find(|a| a.bssid == "AA:BB:CC:DD:EE:01").unwrap();
        assert!((a1.signal_dbm - (-65.0)).abs() < 1e-9);
        assert!((ch36.channel_dbm - (-60.0)).abs() < 1e-9); // (-60-70-50)/3
    }

    #[test]
    fn signal_color_scale() {
        let strong = signal_color(-45.0);
        let mid = signal_color(-65.0);
        let weak = signal_color(-85.0);
        // Green when strong, red when weak (R/G relationship flips).
        assert!(strong.1 > strong.0); // green-dominant
        assert!(weak.0 > weak.1); // red-dominant
        // Monotonic: red component grows, green component falls as the
        // signal weakens.
        assert!(weak.0 > mid.0 && mid.0 > strong.0);
        assert!(strong.1 > mid.1 && mid.1 > weak.1);
    }

    #[test]
    fn color_hex_format() {
        assert_eq!(color_hex((1.0, 0.0, 0.5)), "#FF0080");
        assert_eq!(color_hex((0.2, 0.51, 0.98)), "#3382FA");
    }

    #[test]
    fn measurement_label_format() {
        let m = m_with_scan(vec![entry("Home", "AA:BB:CC:DD:EE:01", 36, 5180, -55, None)]);
        let s = measurement_label(&m);
        assert!(s.contains("Home (ch 36)"), "{s}");
        assert!(s.contains("(0.50, 0.50)"), "{s}");
        assert!(s.contains("—"), "{s}");
    }

    #[test]
    fn measurement_label_no_signal() {
        let mut m = m_with_scan(vec![
            entry("Other", "AA:BB:CC:DD:EE:02", 6, 2437, -80, None),
            entry("Third", "AA:BB:CC:DD:EE:03", 36, 5180, -70, None),
        ]);
        m.no_signal = true;
        let s = measurement_label(&m);
        assert!(s.contains("No connection (2 APs)"), "{s}");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("short", 10), "short");
        let t = truncate("aabbccddee", 4);
        assert_eq!(t.chars().count(), 4);
        assert!(t.ends_with('…'));
    }
}
