use gtk4::prelude::*;
use gtk4::DrawingArea;
use cairo::Context;
use std::cell::RefCell;
use std::rc::Rc;

use crate::models::Measurement;
use crate::models::color_metric::{ColorMetric, metric_value, value_color};

#[derive(Clone)]
pub struct LegendBar {
    pub widget: DrawingArea,
    state: Rc<RefCell<LegendState>>,
}

struct LegendState {
    metric: ColorMetric,
    min: f64,
    max: f64,
    active: bool,
    /// The selected measurement, if any. When set, a pointer marker is drawn
    /// on the gradient at the value this measurement has for the current metric.
    selected: Option<Measurement>,
    /// Live current WiFi signal (dBm). Shown by the pointer when no measurement
    /// is selected and the metric is signal strength.
    current_signal_dbm: Option<f64>,
}

impl LegendBar {
    pub fn new() -> Self {
        let area = DrawingArea::new();
        area.set_height_request(52);
        area.set_hexpand(true);
        // The legend is always shown, even when there are no measurements.
        area.set_visible(true);

        let state = Rc::new(RefCell::new(LegendState {
            metric: ColorMetric::SignalDbm,
            min: ColorMetric::SignalDbm.reference_range().0,
            max: ColorMetric::SignalDbm.reference_range().1,
            active: false,
            selected: None,
            current_signal_dbm: None,
        }));

        {
            let state = state.clone();
            area.set_draw_func(move |_area, ctx, w, h| {
                let s = state.borrow();
                if !s.active { return; }
                // Value the pointer marks: the selected measurement's value, or
                // (when nothing is selected, or it lacks the metric's value) the
                // live current signal for the signal-strength metric.
                let marker_val = match s.selected.as_ref() {
                    // A no-signal point has no value on the scale -> no marker.
                    Some(m) if m.no_signal => None,
                    Some(m) => metric_value(m, s.metric),
                    None => {
                        if s.metric == ColorMetric::SignalDbm {
                            s.current_signal_dbm
                        } else {
                            None
                        }
                    }
                };
                draw_legend(ctx, w as f64, h as f64, s.metric, s.min, s.max, marker_val);
            });
        }

        Self { widget: area, state }
    }

    pub fn set_measurements(&self, _measurements: &[Measurement]) {
        // The legend is always visible (it shows the fixed reference scale for
        // the current metric, independent of whether measurements exist).
        let mut s = self.state.borrow_mut();
        s.active = true;
        drop(s);
        self.widget.set_visible(true);
        self.widget.queue_draw();
    }

    /// Set the selected measurement (or `None` to clear). When set, a pointer
    /// marker is drawn on the gradient at its value for the current metric.
    pub fn set_selected_measurement(&self, m: Option<Measurement>) {
        let mut s = self.state.borrow_mut();
        s.selected = m;
        drop(s);
        self.widget.queue_draw();
    }

    /// Set the live current WiFi signal (dBm). When no measurement is selected
    /// and the metric is signal strength, the pointer marks this value.
    pub fn set_current_signal(&self, dbm: Option<f64>) {
        let mut s = self.state.borrow_mut();
        s.current_signal_dbm = dbm;
        drop(s);
        self.widget.queue_draw();
    }

    /// Set the colour metric (and its fixed reference range) shown by the legend.
    pub fn set_color_metric(&self, metric: ColorMetric) {
        let (min, max) = metric.reference_range();
        let mut s = self.state.borrow_mut();
        s.metric = metric;
        s.min = min;
        s.max = max;
        drop(s);
        self.widget.queue_draw();
    }
}

/// Tick positions and labels for the legend. Signal shows the rating names
/// (per the common WiFi signal scale); throughput shows Mbit/s values.
fn legend_ticks(metric: ColorMetric) -> Vec<(f64, String)> {
    match metric {
        ColorMetric::SignalDbm => vec![
            (-90.0, "Very poor".to_string()),
            (-80.0, "Poor".to_string()),
            (-70.0, "Fair".to_string()),
            (-60.0, "Good".to_string()),
            (-50.0, "Excellent".to_string()),
        ],
        ColorMetric::IperfMbps | ColorMetric::SmbMbps => vec![
            (0.0, "0".to_string()),
            (250.0, "250".to_string()),
            (500.0, "500".to_string()),
            (750.0, "750".to_string()),
            (1000.0, "1 Gbit".to_string()),
        ],
    }
}

fn draw_legend(ctx: &Context, w: f64, h: f64, metric: ColorMetric, min: f64, max: f64, marker_val: Option<f64>) {
    const MARGIN: f64 = 8.0;
    const NAME_Y: f64 = 7.0;   // baseline of the metric-name / unit header
    const BAR_Y: f64 = 16.0;
    const BAR_H: f64 = 12.0;
    const TICK_LEN: f64 = 3.0;
    const LABEL_Y: f64 = BAR_Y + BAR_H + TICK_LEN + 12.0; // baseline of tick labels

    let bar_w = w - MARGIN * 2.0;
    if bar_w <= 0.0 { return; }

    // Dark panel background (matches the floor-plan canvas) so the tick labels
    // are legible regardless of the system colour scheme.
    ctx.set_source_rgb(0.12, 0.12, 0.12);
    ctx.rectangle(0.0, 0.0, w, h);
    ctx.fill().unwrap();

    // Header: metric name (left), unit (right)
    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.75);
    ctx.set_font_size(10.0);
    ctx.move_to(MARGIN, NAME_Y);
    let _ = ctx.show_text(metric.label());
    let unit = metric.unit();
    if let Ok(ext) = ctx.text_extents(unit) {
        ctx.move_to(MARGIN + bar_w - ext.width(), NAME_Y);
        let _ = ctx.show_text(unit);
    }

    // Gradient bar — one 1-px column per pixel
    for xi in 0..(bar_w as i32) {
        let t = xi as f64 / bar_w;
        let val = min + (max - min) * t;
        let (r, g, b) = value_color(val, min, max);
        ctx.set_source_rgb(r, g, b);
        ctx.rectangle(MARGIN + xi as f64, BAR_Y, 1.0, BAR_H);
        ctx.fill().unwrap();
    }

    // Bar border
    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.3);
    ctx.set_line_width(1.0);
    ctx.rectangle(MARGIN, BAR_Y, bar_w, BAR_H);
    ctx.stroke().unwrap();

    // Tick marks + labels
    ctx.set_source_rgba(1.0, 1.0, 1.0, 0.9);
    ctx.set_line_width(1.0);
    ctx.set_font_size(10.0);
    for (val, label) in legend_ticks(metric) {
        let t = ((val - min) / (max - min)).clamp(0.0, 1.0);
        let x = MARGIN + bar_w * t;
        ctx.move_to(x, BAR_Y + BAR_H);
        ctx.line_to(x, BAR_Y + BAR_H + TICK_LEN);
        ctx.stroke().unwrap();
        if let Ok(ext) = ctx.text_extents(&label) {
            let mut lx = x - ext.width() / 2.0;
            if lx < MARGIN { lx = MARGIN; }
            if lx + ext.width() > MARGIN + bar_w { lx = MARGIN + bar_w - ext.width(); }
            ctx.move_to(lx, LABEL_Y);
            let _ = ctx.show_text(&label);
        }
    }

    // Pointer marker for the value in question (selected measurement, or the
    // live current signal): an outlined vertical line through the bar plus a
    // downward triangle above it.
    if let Some(val) = marker_val {
        let t = ((val - min) / (max - min)).clamp(0.0, 1.0);
        let x = MARGIN + bar_w * t;
        // Outlined vertical line through the bar
        ctx.set_line_width(3.0);
        ctx.set_source_rgba(0.0, 0.0, 0.0, 0.6);
        ctx.move_to(x, BAR_Y);
        ctx.line_to(x, BAR_Y + BAR_H);
        ctx.stroke().unwrap();
        ctx.set_line_width(1.5);
        ctx.set_source_rgba(1.0, 1.0, 1.0, 1.0);
        ctx.move_to(x, BAR_Y);
        ctx.line_to(x, BAR_Y + BAR_H);
        ctx.stroke().unwrap();
        // Downward triangle pointer above the bar
        ctx.move_to(x, BAR_Y - 0.5);
        ctx.line_to(x - 4.5, BAR_Y - 7.0);
        ctx.line_to(x + 4.5, BAR_Y - 7.0);
        ctx.close_path();
        ctx.set_source_rgba(1.0, 1.0, 1.0, 1.0);
        ctx.fill_preserve().unwrap();
        ctx.set_line_width(1.0);
        ctx.set_source_rgba(0.0, 0.0, 0.0, 0.6);
        ctx.stroke().unwrap();
    }
}
