use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow, Spinner};
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use crate::models::{Measurement, SignalSource, ThroughputUnit, signal_for};
use crate::services::WifiInfo;

/// Snapshot of the inputs behind the Current Signal text, so it can be
/// re-rendered (e.g. after an alias edit) without a new WiFi scan.
#[derive(Clone)]
struct CurrentSignalState {
    ssid: String,
    bssid: String,
    dbm: i32,
    freq: u32,
    channel: u8,
    device: String,
    channel_width_mhz: Option<u32>,
    center_freq_mhz: Option<u32>,
    center_freq2_mhz: Option<u32>,
    iperf_mbps: Option<f64>,
    smb_mbps: Option<f64>,
    scan_list: Option<Vec<WifiInfo>>,
}

#[derive(Clone)]
pub struct MeasurementPanel {
    pub widget: GtkBox,
    current_label: Label,
    network_warn: Label,
    device_row: GtkBox,
    device_combo: gtk4::ComboBoxText,
    expected_device_active: Rc<RefCell<Option<u32>>>,
    last_device_options: Rc<RefCell<Vec<String>>>,
    on_device_changed: Rc<RefCell<Option<Box<dyn Fn(String)>>>>,
    selected_label: Label,
    details_more_btn: gtk4::Button,
    status_box: GtkBox,
    spinner: Spinner,
    status_label: Label,
    list: ListBox,
    scroll: ScrolledWindow,
    measurements: Rc<RefCell<Vec<Measurement>>>,
    rows: Rc<RefCell<Vec<(String, ListBoxRow)>>>,
    on_delete: Rc<RefCell<Option<Box<dyn Fn(String)>>>>,
    on_delete_all: Rc<RefCell<Option<Box<dyn Fn()>>>>,
    on_row_clicked: Rc<RefCell<Option<Box<dyn Fn(String)>>>>,
    selecting: Rc<RefCell<bool>>,
    unit: Rc<RefCell<ThroughputUnit>>,
    signal_source: Rc<RefCell<SignalSource>>,
    aliases: Rc<RefCell<HashMap<String, String>>>,
    // Last rendered Current Signal input — kept so alias changes can
    // re-render it immediately (without waiting for the next live tick).
    last_current: Rc<RefCell<Option<CurrentSignalState>>>,
}

impl MeasurementPanel {
    pub fn new() -> Self {
        let vbox = GtkBox::new(Orientation::Vertical, 6);
        vbox.set_width_request(280);
        vbox.set_margin_start(6);
        vbox.set_margin_end(6);
        vbox.set_margin_top(6);
        vbox.set_margin_bottom(6);

        // Current WiFi info section
        let current_label = Label::new(Some("No WiFi data"));
        current_label.set_xalign(0.0);
        current_label.set_wrap(true);
        current_label.add_css_class("caption");

        // Shown when the current SSID differs from the survey network (the
        // network of the most recent sample).
        let network_warn = Label::new(None);
        network_warn.set_xalign(0.0);
        network_warn.set_wrap(true);
        network_warn.add_css_class("warning");
        network_warn.set_visible(false);

        // WiFi card selector (shown only when 2 or more cards are active).
        let device_row = GtkBox::new(Orientation::Horizontal, 6);
        device_row.set_margin_start(4);
        device_row.set_visible(false);
        let device_caption = Label::new(Some("Measure card:"));
        device_caption.add_css_class("caption");
        let device_combo = gtk4::ComboBoxText::new();
        device_row.append(&device_caption);
        device_row.append(&device_combo);

        let expected_device_active = Rc::new(RefCell::new(None::<u32>));
        let last_device_options = Rc::new(RefCell::new(Vec::<String>::new()));
        let on_device_changed: Rc<RefCell<Option<Box<dyn Fn(String)>>>> = Rc::new(RefCell::new(None));
        {
            let expected = expected_device_active.clone();
            let combo_ref = device_combo.clone();
            let cb_ref = on_device_changed.clone();
            device_combo.connect_changed(move |_| {
                let active = combo_ref.active();
                if active == *expected.borrow() {
                    return; // programmatic update, not a user choice
                }
                *expected.borrow_mut() = active;
                if let Some(text) = combo_ref.active_text() {
                    if let Some(ref cb) = *cb_ref.borrow() {
                        cb(text.to_string());
                    }
                }
            });
        }

        let current_group = libadwaita::PreferencesGroup::new();
        current_group.set_title("Current Signal");
        current_group.add(&current_label);
        current_group.add(&device_row);
        current_group.add(&network_warn);
        vbox.append(&current_group);

        // Selected measurement details (inspect mode). Lives in the top pane
        // of a resizable split: long details (many APs in range) scroll
        // instead of pushing the measurements list around.
        let selected_label = Label::new(Some("No measurement selected — use the Select tool and click a point on the map."));
        selected_label.set_xalign(0.0);
        selected_label.set_wrap(true);
        selected_label.add_css_class("caption");
        // Opens the full list of the other networks seen at the selected point.
        let details_more_btn = gtk4::Button::with_label("All networks in range…");
        details_more_btn.add_css_class("flat");
        details_more_btn.set_halign(gtk4::Align::Start);
        details_more_btn.set_visible(false);
        let details_box = GtkBox::new(Orientation::Vertical, 4);
        details_box.set_margin_start(6);
        details_box.set_margin_end(6);
        details_box.set_margin_top(4);
        details_box.set_margin_bottom(6);
        let sel_header = gtk4::Label::new(Some("Selected Measurement"));
        sel_header.set_xalign(0.0);
        sel_header.add_css_class("heading");
        details_box.append(&sel_header);
        details_box.append(&selected_label);
        details_box.append(&details_more_btn);
        let details_scroll = ScrolledWindow::new();
        details_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        details_scroll.set_min_content_height(90);
        details_scroll.set_child(Some(&details_box));

        // Spinner / status row (shown while measuring)
        let status_box = GtkBox::new(Orientation::Horizontal, 8);
        status_box.set_margin_start(4);
        status_box.set_margin_bottom(4);
        let spinner = Spinner::new();
        let status_label = Label::new(None);
        status_label.add_css_class("caption");
        status_label.set_xalign(0.0);
        status_box.append(&spinner);
        status_box.append(&status_label);
        status_box.set_visible(false);

        // Measurements list
        let list_group = libadwaita::PreferencesGroup::new();
        list_group.set_title("Measurements");

        let on_delete_all: Rc<RefCell<Option<Box<dyn Fn()>>>> = Rc::new(RefCell::new(None));
        let delete_all_btn = Button::from_icon_name("user-trash-symbolic");
        delete_all_btn.add_css_class("flat");
        delete_all_btn.set_tooltip_text(Some("Delete all measurements"));
        {
            let on_delete_all = on_delete_all.clone();
            delete_all_btn.connect_clicked(move |_| {
                if let Some(ref cb) = *on_delete_all.borrow() {
                    cb();
                }
            });
        }
        list_group.set_header_suffix(Some(&delete_all_btn));

        let list = ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::Single);
        list.add_css_class("boxed-list");

        let scroll = ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scroll.set_child(Some(&list));

        // Resizable split: details (top, scrollable) / list (bottom).
        // The divider lets the user pick how tall the details section is.
        let bottom_box = GtkBox::new(Orientation::Vertical, 6);
        bottom_box.append(&status_box);
        bottom_box.append(&list_group);
        bottom_box.append(&scroll);
        let paned = gtk4::Paned::builder()
            .orientation(gtk4::Orientation::Vertical)
            .start_child(&details_scroll)
            .end_child(&bottom_box)
            .position(240)
            .build();
        paned.set_vexpand(true);
        vbox.append(&paned);

        let measurements = Rc::new(RefCell::new(Vec::<Measurement>::new()));
        let rows: Rc<RefCell<Vec<(String, ListBoxRow)>>> = Rc::new(RefCell::new(Vec::new()));
        let selecting: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
        let on_delete: Rc<RefCell<Option<Box<dyn Fn(String)>>>> = Rc::new(RefCell::new(None));
        let on_row_clicked: Rc<RefCell<Option<Box<dyn Fn(String)>>>> = Rc::new(RefCell::new(None));

        // Selecting a row (single click) highlights the corresponding point on the map.
        {
            let rows_cb = rows.clone();
            let selecting_cb = selecting.clone();
            let on_row_clicked_cb = on_row_clicked.clone();
            list.connect_row_selected(move |_list, row| {
                if *selecting_cb.borrow() {
                    return; // programmatic selection from set_selected_by_id
                }
                if let Some(row) = row {
                    if let Some((id, _)) = rows_cb.borrow().iter().find(|(_, r)| r == row) {
                        if let Some(ref cb) = *on_row_clicked_cb.borrow() {
                            cb(id.clone());
                        }
                    }
                }
            });
        }

        let panel = Self {
            widget: vbox,
            current_label,
            network_warn,
            device_row,
            device_combo,
            expected_device_active,
            last_device_options,
            on_device_changed,
            selected_label,
            details_more_btn,
            status_box,
            spinner,
            status_label,
            list,
            scroll: scroll.clone(),
            measurements,
            rows,
            on_delete,
            on_delete_all,
            on_row_clicked,
            selecting,
            unit: Rc::new(RefCell::new(ThroughputUnit::Mbit)),
            signal_source: Rc::new(RefCell::new(SignalSource::default())),
            aliases: Rc::new(RefCell::new(HashMap::new())),
            last_current: Rc::new(RefCell::new(None)),
        };
        {
            let me = panel.clone();
            panel.details_more_btn.connect_clicked(move |_| me.show_other_networks());
        }
        panel
    }

    /// Popup with the full list of all networks (APs) seen at the selected
    /// point, including the measured/target SSID (marked with ●).
    fn show_other_networks(&self) {
        let Some(id) = self.selected_id() else { return };
        let (m, aliases, floor_ssids) = {
            let measurements = self.measurements.borrow();
            let aliases = self.aliases.borrow();
            match measurements.iter().find(|m| m.id == id) {
                Some(m) => (
                    m.clone(),
                    aliases.clone(),
                    Self::floor_ssids(&measurements),
                ),
                None => return,
            }
        };
        let all: Vec<&crate::models::ScanEntry> = m.scan_results.iter().collect();
        if all.is_empty() {
            return;
        }
        let count = all.len();
        // Mark the target AP(s): the connected AP for regular measurements,
        // or the APs of this floor's measured SSIDs for no-signal points.
        let mut marks: std::collections::HashSet<String> = std::collections::HashSet::new();
        if m.no_signal {
            for e in &all {
                if floor_ssids.contains(&e.ssid) {
                    marks.insert(e.bssid.clone());
                }
            }
        } else if !m.bssid.is_empty() {
            marks.insert(m.bssid.clone());
        }
        let text = format!("{count} APs in range at this point:\n\n{}", format_ap_table(&all, &aliases, &marks));
        // Find the enclosing top-level window (for transient_for stacking).
        let mut parent: Option<gtk4::Widget> = self.widget.parent();
        let mut win: Option<gtk4::Window> = None;
        while let Some(w) = parent {
            if let Some(w) = w.downcast_ref::<gtk4::Window>() {
                win = Some(w.clone());
                break;
            }
            parent = w.parent();
        }
        let mut builder = gtk4::Window::builder()
            .title(format!("All networks in range — {count} APs"))
            .modal(true)
            .default_width(560)
            .default_height(420);
        if let Some(win) = win {
            builder = builder.transient_for(&win);
        }
        let dialog = builder.build();
        let label = gtk4::Label::new(Some(&text));
        label.set_xalign(0.0);
        label.set_yalign(0.0);
        label.add_css_class("monospace");
        label.set_margin_top(12);
        label.set_margin_bottom(12);
        label.set_margin_start(12);
        label.set_margin_end(12);
        let scroll = ScrolledWindow::new();
        scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Automatic);
        scroll.set_child(Some(&label));
        dialog.set_child(Some(&scroll));
        dialog.present();
    }

    /// The distinct SSIDs of this floor's regular (connected) measurements,
    /// in first-seen order — "the networks this floor is measuring".
    fn floor_ssids(measurements: &[Measurement]) -> Vec<String> {
        let mut seen = Vec::new();
        for m in measurements.iter() {
            if !m.no_signal && !m.ssid.is_empty() && !seen.contains(&m.ssid) {
                seen.push(m.ssid.clone());
            }
        }
        seen
    }

    /// Show or hide the measuring spinner with a status message.
    pub fn set_measuring(&self, active: bool, msg: &str) {
        if active {
            self.spinner.start();
            self.status_label.set_label(msg);
            self.status_box.set_visible(true);
        } else {
            self.spinner.stop();
            self.status_box.set_visible(false);
        }
    }

    pub fn update_current_wifi(
        &self,
        ssid: &str,
        bssid: &str,
        dbm: i32,
        freq: u32,
        channel: u8,
        device: &str,
        channel_width_mhz: Option<u32>,
        center_freq_mhz: Option<u32>,
        center_freq2_mhz: Option<u32>,
        iperf_mbps: Option<f64>,
        smb_mbps: Option<f64>,
        _unit: ThroughputUnit,
        scan_list: Option<&[WifiInfo]>,
    ) {
        *self.last_current.borrow_mut() = Some(CurrentSignalState {
            ssid: ssid.to_string(),
            bssid: bssid.to_string(),
            dbm,
            freq,
            channel,
            device: device.to_string(),
            channel_width_mhz,
            center_freq_mhz,
            center_freq2_mhz,
            iperf_mbps,
            smb_mbps,
            scan_list: scan_list.map(|l| l.to_vec()),
        });
        self.render_current();
    }

    /// (Re)build the Current Signal text from the last stored input, using
    /// the current aliases (so alias edits are reflected immediately).
    fn render_current(&self) {
        let Some(input) = self.last_current.borrow().clone() else {
            return;
        };
        let unit = *self.unit.borrow();
        let band = crate::models::band_label(input.freq);
        let quality = signal_quality_str(input.dbm);
        let bssid_label = ap_full_label(&self.aliases.borrow(), &input.bssid);
        let ch_suffix = crate::models::width_center_suffix(
            input.channel_width_mhz,
            input.center_freq_mhz,
            input.center_freq2_mhz,
        );
        let mut text = format!(
            "SSID: {}\nBSSID: {}\nSignal: {} dBm ({})\nBand: {} | Ch: {}{}\nCard: {}",
            input.ssid, bssid_label, input.dbm, quality, band, input.channel, ch_suffix, input.device
        );
        if let Some(mbps) = input.iperf_mbps {
            text.push_str(&format!("\niperf3: {}", unit.format(mbps)));
        }
        if let Some(mbps) = input.smb_mbps {
            text.push_str(&format!("\nSamba: {}", unit.format(mbps)));
        }
        // All APs broadcasting the current SSID (the multi-AP view).
        if let Some(list) = &input.scan_list {
            if let Some(group) = same_ssid_group_text(&input.ssid, list, &self.aliases.borrow()) {
                text.push_str(&group);
            }
        }
        self.current_label.set_label(&text);
    }

    pub fn set_no_wifi(&self) {
        *self.last_current.borrow_mut() = None;
        self.current_label.set_label("No WiFi connection detected");
        self.device_row.set_visible(false);
    }

    /// Show/hide a warning that the current SSID is not one of this floor's
    /// measured networks. Pass a pre-formatted message to show, `None` to hide.
    pub fn set_network_warning(&self, msg: Option<String>) {
        match msg {
            Some(text) => {
                self.network_warn.set_label(&text);
                self.network_warn.set_visible(true);
            }
            None => self.network_warn.set_visible(false),
        }
    }

    /// Refresh the live Current Signal with the active AP. No throughput is
    /// shown — that only appears right after a measurement (update_current_wifi).
    pub fn refresh_live_signal(
        &self,
        ssid: &str,
        bssid: &str,
        dbm: i32,
        freq: u32,
        channel: u8,
        device: &str,
        channel_width_mhz: Option<u32>,
        center_freq_mhz: Option<u32>,
        center_freq2_mhz: Option<u32>,
        scan_list: Option<&[WifiInfo]>,
    ) {
        self.update_current_wifi(
            ssid, bssid, dbm, freq, channel, device,
            channel_width_mhz, center_freq_mhz, center_freq2_mhz,
            None, None, *self.unit.borrow(), scan_list,
        );
    }

    pub fn set_on_device_changed<F: Fn(String) + 'static>(&self, cb: F) {
        *self.on_device_changed.borrow_mut() = Some(Box::new(cb));
    }

    /// Show/hide the WiFi card selector. `options` is the list of active card
    /// interface names; the selector is shown only when there are two or more.
    /// `selected` is the card currently in use (highlighted in the combo).
    pub fn set_card_selector(&self, options: &[String], selected: Option<&str>) {
        if options.len() < 2 {
            self.device_row.set_visible(false);
            return;
        }
        // Repopulate the combo only if the set of options changed.
        if *self.last_device_options.borrow() != options.to_vec() {
            *self.expected_device_active.borrow_mut() = None; // ignore spurious `changed`
            self.device_combo.remove_all();
            for opt in options {
                self.device_combo.append_text(opt);
            }
            *self.last_device_options.borrow_mut() = options.to_vec();
        }
        // Highlight the card currently in use.
        let target: Option<u32> = selected
            .and_then(|sel| options.iter().position(|o| o == sel))
            .map(|p| p as u32)
            .or(Some(0));
        *self.expected_device_active.borrow_mut() = target;
        if self.device_combo.active() != target {
            self.device_combo.set_active(target);
        }
        self.device_row.set_visible(true);
    }

    pub fn set_measurements(&self, measurements: Vec<Measurement>) {
        *self.measurements.borrow_mut() = measurements.clone();
        self.rebuild_list(&measurements);
    }

    pub fn set_on_delete<F: Fn(String) + 'static>(&self, cb: F) {
        *self.on_delete.borrow_mut() = Some(Box::new(cb));
    }

    pub fn set_on_delete_all<F: Fn() + 'static>(&self, cb: F) {
        *self.on_delete_all.borrow_mut() = Some(Box::new(cb));
    }

    pub fn set_on_row_clicked<F: Fn(String) + 'static>(&self, cb: F) {
        *self.on_row_clicked.borrow_mut() = Some(Box::new(cb));
    }

    /// Update the "Selected Measurement" section and highlight/scroll to the
    /// matching list row. Pass `None` to clear the selection.
    pub fn set_selected_by_id(&self, id: Option<String>) {
        let (text, scan_count) = {
            let measurements = self.measurements.borrow();
            let unit = self.unit.borrow();
            let source = self.signal_source.borrow();
            let aliases = self.aliases.borrow();
            match id.as_ref().and_then(|id| measurements.iter().find(|m| &m.id == id)) {
                Some(m) => {
                    let floor_ssids = Self::floor_ssids(&measurements);
                    (
                        format_measurement_details(m, &unit, &source, &aliases, &floor_ssids),
                        m.scan_results.len(),
                    )
                }
                None => (
                    "No measurement selected — use the Select tool and click a point on the map.".to_string(),
                    0,
                ),
            }
        };
        self.selected_label.set_label(&text);
        // The "all networks" button shows when the point has a recorded scan
        // list (including no-signal points with a scan).
        self.details_more_btn.set_visible(scan_count > 0);
        if scan_count > 0 {
            self.details_more_btn
                .set_label(&format!("All networks in range ({scan_count} APs) ▸"));
        }

        let rows = self.rows.borrow();
        *self.selecting.borrow_mut() = true;
        if let Some(id) = &id {
            if let Some((idx, (_, row))) = rows.iter().enumerate().find(|(_, (rid, _))| rid == id) {
                self.list.select_row(Some(row));
                // Scroll the list so the selected row is visible (approximate row height).
                let adj = self.scroll.vadjustment();
                let row_h = 34.0;
                let page = adj.page_size();
                let target = idx as f64 * row_h + row_h / 2.0 - page / 2.0;
                let max = (adj.upper() - page).max(0.0);
                adj.set_value(target.clamp(0.0, max));
            }
        } else {
            self.list.unselect_all();
        }
        *self.selecting.borrow_mut() = false;
    }

    pub fn set_throughput_unit(&self, unit: ThroughputUnit) {
        *self.unit.borrow_mut() = unit;
        // Rebuild list with new unit
        let measurements = self.measurements.borrow().clone();
        self.rebuild_list(&measurements);
        // Keep the selected details in sync with the unit.
        if let Some(id) = self.selected_id() {
            self.set_selected_by_id(Some(id));
        }
    }

    /// Set which AP's signal the list values and details show (connected AP /
    /// best AP of the SSID / a specific BSSID).
    pub fn set_signal_source(&self, source: SignalSource) {
        *self.signal_source.borrow_mut() = source;
        let measurements = self.measurements.borrow().clone();
        self.rebuild_list(&measurements);
        if let Some(id) = self.selected_id() {
            self.set_selected_by_id(Some(id));
        }
    }

    /// Set the project's BSSID aliases (used in the scan list of the selected
    /// measurement details).
    pub fn set_aliases(&self, aliases: HashMap<String, String>) {
        *self.aliases.borrow_mut() = aliases;
        // Re-render everything that shows AP labels, so new aliases are
        // reflected immediately in all views.
        self.render_current();
        if let Some(id) = self.selected_id() {
            self.set_selected_by_id(Some(id));
        }
    }

    /// The id currently selected in the list, if any.
    fn selected_id(&self) -> Option<String> {
        self.list
            .selected_row()
            .as_ref()
            .and_then(|row| self.rows.borrow().iter().find(|(_, r)| r == row).map(|(id, _)| id.clone()))
    }

    fn rebuild_list(&self, measurements: &[Measurement]) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let mut rows = Vec::new();
        for m in measurements.iter().rev() {
            let row = self.make_row(m);
            rows.push((m.id.clone(), row.clone()));
            self.list.append(&row);
        }
        *self.rows.borrow_mut() = rows;
    }

    fn make_row(&self, m: &Measurement) -> ListBoxRow {
        let hbox = GtkBox::new(Orientation::Horizontal, 6);
        hbox.set_margin_start(6);
        hbox.set_margin_end(6);
        hbox.set_margin_top(4);
        hbox.set_margin_bottom(4);

        let info_str = if m.no_signal {
            // "No connection" when APs were in range at the point, "No
            // signal" when the scan found nothing (true dead zone).
            let label = if m.scan_results.is_empty() { "No signal" } else { "No connection" };
            format!("⚫ {label} | {}", m.timestamp.format("%H:%M:%S"))
        } else {
            let source = self.signal_source.borrow();
            // The dBm shown follows the active signal source; "—" means the
            // chosen AP was not in range at this point (no data).
            let dbm_str = signal_for(m, &source).map(|v| format!("{} dBm", v as i32)).unwrap_or_else(|| "—".to_string());
            let mut s = format!(
                "{} | {} | {}",
                m.ssid, dbm_str,
                m.timestamp.format("%H:%M:%S")
            );
            if let Some(mbps) = m.iperf_mbps {
                s.push_str(&format!(" | ⚡{}", self.unit.borrow().format_short(mbps)));
            } else if let Some(mbps) = m.smb_mbps {
                s.push_str(&format!(" | 🗂{}", self.unit.borrow().format_short(mbps)));
            }
            s
        };

        let info = Label::new(Some(&info_str));
        info.set_hexpand(true);
        info.set_xalign(0.0);
        info.set_ellipsize(gtk4::pango::EllipsizeMode::End);

        let del_btn = Button::from_icon_name("edit-delete-symbolic");
        del_btn.add_css_class("flat");
        del_btn.set_tooltip_text(Some("Delete measurement"));

        let id = m.id.clone();
        let on_delete = self.on_delete.clone();
        del_btn.connect_clicked(move |_| {
            if let Some(ref cb) = *on_delete.borrow() {
                cb(id.clone());
            }
        });

        hbox.append(&info);
        hbox.append(&del_btn);

        let row = ListBoxRow::new();
        row.set_child(Some(&hbox));
        row
    }
}

fn signal_quality_str(dbm: i32) -> &'static str {
    match dbm {
        -50..=0   => "Excellent",
        -60..=-51 => "Good",
        -70..=-61 => "Fair",
        -80..=-71 => "Poor",
        _         => "No signal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(bssid: &str, ssid: &str, dbm: i32, active: bool) -> WifiInfo {
        WifiInfo {
            ssid: ssid.to_string(),
            bssid: bssid.to_string(),
            frequency_mhz: 5180,
            channel: 36,
            signal_dbm: dbm,
            link_speed_mbps: None,
            device: "wlan0".to_string(),
            is_active: active,
            channel_width_mhz: None,
            center_freq_mhz: None,
            center_freq2_mhz: None,
        }
    }

    fn entry(bssid: &str, ssid: &str, dbm: i32, active: bool) -> crate::models::ScanEntry {
        crate::models::ScanEntry {
            ssid: ssid.to_string(),
            bssid: bssid.to_string(),
            frequency_mhz: 5180,
            channel: 36,
            signal_dbm: dbm,
            is_active: active,
            channel_width_mhz: None,
            center_freq_mhz: None,
            center_freq2_mhz: None,
        }
    }

    #[test]
    fn same_ssid_group_lists_all_aps_of_ssid() {
        let list = vec![
            ap("AA:BB:CC:DD:EE:01", "Home", -55, true),
            ap("AA:BB:CC:DD:EE:02", "Home", -70, false),
            ap("AA:BB:CC:DD:EE:03", "Home", -84, false),
            ap("FF:FF:FF:FF:FF:FF", "Other", -40, false),
        ];
        let text = same_ssid_group_text("Home", &list, &HashMap::new()).unwrap();
        assert!(text.contains("This SSID in range (3 APs)"));
        assert!(text.contains("● AA:BB:CC:DD:EE:01  -55 dBm ch36 5 GHz"));
        assert!(text.contains("AA:BB:CC:DD:EE:02  -70 dBm ch36 5 GHz"));
        assert!(text.contains("AA:BB:CC:DD:EE:03  -84 dBm ch36 5 GHz"));
        assert!(!text.contains("FF:FF:FF:FF:FF:FF"));
    }

    #[test]
    fn same_ssid_group_hidden_for_single_ap() {
        let list = vec![ap("AA:BB:CC:DD:EE:01", "Home", -55, true)];
        assert!(same_ssid_group_text("Home", &list, &HashMap::new()).is_none());
        assert!(same_ssid_group_text("Home", &[], &HashMap::new()).is_none());
    }

    #[test]
    fn same_ssid_group_shows_aliases() {
        let list = vec![
            ap("AA:BB:CC:DD:EE:01", "Home", -55, true),
            ap("AA:BB:CC:DD:EE:02", "Home", -70, false),
        ];
        let mut aliases = HashMap::new();
        aliases.insert("AA:BB:CC:DD:EE:02".to_string(), "Office-AP-1".to_string());
        let text = same_ssid_group_text("Home", &list, &aliases).unwrap();
        // Alias is the primary label, MAC in parentheses, band included.
        assert!(text.contains("Office-AP-1 (AA:BB:CC:DD:EE:02)  -70 dBm ch36 5 GHz"));
    }

    #[test]
    fn ap_table_columns_are_aligned() {
        let mut e24 = entry("AA:BB:CC:DD:EE:01", "Home", -55, true);
        e24.frequency_mhz = 2437;
        e24.channel = 6;
        let e1 = entry("FF:FF:FF:FF:FF:01", "VeryLongNetworkNameHere", -60, false);
        let e2 = entry("FF:FF:FF:FF:FF:02", "Noise", -100, false);
        let scan = vec![&e24, &e1, &e2];
        let text = format_ap_table(&scan, &HashMap::new(), &std::collections::HashSet::new());
        let lines: Vec<&str> = text.trim_end().split('\n').collect();
        // Header + separator + 3 rows.
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("SSID"));
        // Fixed-width columns → every row has exactly the same length
        // (monospace font renders this as aligned columns).
        let w = lines[1].len();
        for l in &lines {
            assert_eq!(l.len(), w, "row widths differ: {l:?}");
        }
        // The dBm value is right-aligned: it ends right before the CH column.
        assert!(lines[2].contains("      -55"));
    }

    #[test]
    fn ap_table_empty_ssid_shows_hidden() {
        let mut e = entry("AA:BB:CC:DD:EE:01", "Home", -55, true);
        e.ssid = String::new();
        let scan = vec![&e];
        let text = format_ap_table(&scan, &HashMap::new(), &std::collections::HashSet::new());
        assert!(text.contains("(hidden)"));
    }

    #[test]
    fn scan_list_section_shows_own_ssid_aps_only() {
        let scan = vec![
            entry("AA:BB:CC:DD:EE:01", "Home", -55, true),
            entry("AA:BB:CC:DD:EE:02", "Home", -70, false),
            entry("FF:FF:FF:FF:FF:01", "Other", -60, false),
            entry("FF:FF:FF:FF:FF:02", "Net2", -65, false),
        ];
        let text = format_scan_list_section(&scan, "Home", &HashMap::new());
        // Own SSID group header + its APs; the other networks are only in
        // the popup (not in the details text).
        assert!(text.contains("── Home (2 APs) ──"));
        assert!(text.contains("● AA:BB:CC:DD:EE:01  -55 dBm"));
        assert!(text.contains("AA:BB:CC:DD:EE:02  -70 dBm"));
        assert!(!text.contains("FF:FF:FF:FF:FF:01"));
        assert!(!text.contains("Other networks"));
    }

    #[test]
    fn scan_list_section_empty_scan_gives_empty_text() {
        assert_eq!(format_scan_list_section(&[], "Home", &HashMap::new()), "");
    }

    #[test]
    fn scan_list_section_no_own_aps_gives_empty_text() {
        // The measured SSID's APs are not in the scan list (shouldn't happen
        // in practice) → no inline section; the popup button still works.
        let scan = vec![entry("FF:FF:FF:FF:FF:01", "Other", -60, false)];
        assert_eq!(format_scan_list_section(&scan, "Home", &HashMap::new()), "");
    }
}

fn format_measurement_details(m: &Measurement, unit: &ThroughputUnit, source: &SignalSource, aliases: &HashMap<String, String>, floor_ssids: &[String]) -> String {
    if m.no_signal {
        let head = if m.scan_results.is_empty() {
            "No signal (dead zone)"
        } else {
            "No connection"
        };
        let mut text = format!(
            "{}\nTime: {}",
            head,
            m.timestamp.format("%m-%d %H:%M:%S")
        );
        if !m.scan_results.is_empty() {
            text.push_str(&format!("\nNetworks in range: {}", m.scan_results.len()));
            // The networks this floor is measuring: strongest detected
            // signal, or "not detected".
            for ssid in floor_ssids {
                let best = m
                    .scan_results
                    .iter()
                    .filter(|e| &e.ssid == ssid)
                    .max_by_key(|e| e.signal_dbm);
                match best {
                    Some(e) => {
                        let tag = crate::models::width_tag(e.channel_width_mhz, e.center_freq2_mhz)
                            .map(|t| format!(" {t}"))
                            .unwrap_or_default();
                        text.push_str(&format!(
                            "\n  {ssid}: {} dBm (ch{} {}{})",
                            e.signal_dbm, e.channel, e.band(), tag
                        ));
                    }
                    None => text.push_str(&format!("\n  {ssid}: not detected")),
                }
            }
        }
        return text;
    }
    let band = crate::models::band_label(m.frequency_mhz);
    let bssid_label = ap_full_label(aliases, &m.bssid);
    let ch_suffix = crate::models::width_center_suffix(
        m.channel_width_mhz,
        m.center_freq_mhz,
        m.center_freq2_mhz,
    );
    let mut text = format!(
        "SSID: {}\nBSSID: {}\nSignal: {} dBm ({})\nBand: {} | Ch: {} | {} MHz{}\nTime: {}",
        m.ssid, bssid_label, m.signal_dbm, signal_quality_str(m.signal_dbm), band, m.channel, m.frequency_mhz, ch_suffix,
        m.timestamp.format("%m-%d %H:%M:%S")
    );
    // When a non-default signal source is active, show the filtered value too.
    if !source.is_connected_ap() {
        let src_label = match source {
            SignalSource::Ssid(s) if s.is_empty() => "SSID (best AP)".to_string(),
            SignalSource::Ssid(s) => format!("{s} (best AP)"),
            SignalSource::Bssid(b) => format!("{} ({})", aliases.get(b).map(|s| s.as_str()).unwrap_or(b.as_str()), b),
            SignalSource::ConnectedAp => unreachable!(),
        };
        match signal_for(m, source) {
            Some(v) => text.push_str(&format!("\nSignal ({src_label}): {} dBm ({})", v as i32, signal_quality_str(v as i32))),
            None => text.push_str(&format!("\nSignal ({src_label}): — (not in range)")),
        }
    }
    if let Some(mbps) = m.iperf_mbps {
        text.push_str(&format!("\niperf3: {}", unit.format(mbps)));
    }
    if let Some(mbps) = m.smb_mbps {
        text.push_str(&format!("\nSamba: {}", unit.format(mbps)));
    }
    if let Some(dbm) = m.noise_dbm {
        text.push_str(&format!("\nNoise: {} dBm", dbm));
    }
    if let Some(mbps) = m.link_speed_mbps {
        text.push_str(&format!("\nLink speed: {} Mbps", mbps));
    }
    // The scan list: all APs in range when this point was measured, grouped
    // by SSID (the measured SSID's APs first, other networks after).
    text.push_str(&format_scan_list_section(&m.scan_results, &m.ssid, aliases));
    text
}

/// Display label for an AP line: "alias (MAC)" when an alias is set,
/// otherwise the MAC address.
fn ap_full_label(aliases: &HashMap<String, String>, bssid: &str) -> String {
    match aliases.get(bssid) {
        Some(a) => format!("{a} ({bssid})"),
        None => bssid.to_string(),
    }
}

/// Text for the "APs broadcasting the current SSID" block shown in the
/// live Current Signal panel. `None` when there is nothing extra to show
/// (empty list, or only the connected AP itself is in range).
fn same_ssid_group_text(ssid: &str, list: &[WifiInfo], aliases: &HashMap<String, String>) -> Option<String> {
    let group: Vec<&WifiInfo> = list.iter().filter(|e| e.ssid == ssid).collect();
    if group.len() < 2 {
        return None;
    }
    let mut text = format!("\n── This SSID in range ({} APs) ──", group.len());
    for e in &group {
        let marker = if e.is_active { "● " } else { "  " };
        let label = ap_full_label(aliases, &e.bssid);
        let ch = if e.channel > 0 {
            format!(" ch{} {}", e.channel, crate::models::band_label(e.frequency_mhz))
        } else {
            crate::models::band_label(e.frequency_mhz).to_string()
        };
        let tag = crate::models::width_tag(e.channel_width_mhz, e.center_freq2_mhz)
            .map(|t| format!(" {t}"))
            .unwrap_or_default();
        text.push_str(&format!("\n{marker}{label}  {} dBm{}{}", e.signal_dbm, ch, tag));
    }
    Some(text)
}

/// ASCII column-aligned table of APs (for the "all networks" popup).
/// All columns are fixed width so rows line up in a monospace font.
/// `marks`: BSSIDs to highlight with ● (the target network's APs).
fn format_ap_table(
    entries: &[&crate::models::ScanEntry],
    aliases: &HashMap<String, String>,
    marks: &std::collections::HashSet<String>,
) -> String {
    let ssid_text = |e: &crate::models::ScanEntry| {
        let base = if e.ssid.is_empty() {
            "(hidden)".to_string()
        } else {
            e.ssid.clone()
        };
        if marks.contains(&e.bssid) {
            format!("● {base}")
        } else {
            base
        }
    };
    let width_text = |e: &crate::models::ScanEntry| {
        crate::models::width_tag(e.channel_width_mhz, e.center_freq2_mhz).unwrap_or_default()
    };
    let ssid_w = entries
        .iter()
        .map(|e| ssid_text(e).len())
        .max()
        .unwrap_or(4)
        .min(26)
        .max(4);
    // The AP column holds "alias (MAC)" when an alias is set — size it to fit.
    let ap_w = entries
        .iter()
        .map(|e| ap_full_label(aliases, &e.bssid).len())
        .max()
        .unwrap_or(17)
        .max(17);
    // The WIDTH column is hidden entirely when no entry has width data
    // (old projects keep the original table shape).
    let show_width = entries.iter().any(|e| !width_text(e).is_empty());
    let w_w = entries
        .iter()
        .map(|e| width_text(e).len())
        .max()
        .unwrap_or(1)
        .max(1);
    if show_width {
        let mut text = format!(
            "{:<ssid_w$}  {:<ap_w$}  {:>7}  {:>3}  {:<w_w$}  {:>7}\n",
            "SSID", "AP", "SIGNAL", "CH", "WIDTH", "BAND"
        );
        text.push_str(&format!(
            "{:-<ssid_w$}  {:-<ap_w$}  {:->7}  {:->3}  {:-<w_w$}  {:->7}\n",
            "", "", "-------", "---", "", "-------"
        ));
        for e in entries {
            let ssid = ssid_text(e);
            let label = ap_full_label(aliases, &e.bssid);
            text.push_str(&format!(
                "{:<ssid_w$}  {:<ap_w$}  {:>7}  {:>3}  {:<w_w$}  {:>7}\n",
                ssid, label, e.signal_dbm, e.channel, width_text(e), e.band()
            ));
        }
        text
    } else {
        let mut text = format!(
            "{:<ssid_w$}  {:<ap_w$}  {:>7}  {:>3}  {:>7}\n",
            "SSID", "AP", "SIGNAL", "CH", "BAND"
        );
        text.push_str(&format!(
            "{:-<ssid_w$}  {:-<ap_w$}  {:->7}  {:->3}  {:->7}\n",
            "", "", "-------", "---", "-------"
        ));
        for e in entries {
            let ssid = ssid_text(e);
            let label = ap_full_label(aliases, &e.bssid);
            text.push_str(&format!(
                "{:<ssid_w$}  {:<ap_w$}  {:>7}  {:>3}  {:>7}\n",
                ssid, label, e.signal_dbm, e.channel, e.band()
            ));
        }
        text
    }
}

/// Text for the recorded scan list of a measurement: the measured SSID's
/// APs (its APs are the interesting ones for a multi-AP network). The other
/// networks are shown in a popup (the panel's "other networks" button).
fn format_scan_list_section(scan_results: &[crate::models::ScanEntry], connected_ssid: &str, aliases: &HashMap<String, String>) -> String {
    if scan_results.is_empty() {
        return String::new();
    }
    let own: Vec<&crate::models::ScanEntry> =
        scan_results.iter().filter(|e| e.ssid == connected_ssid).collect();
    if own.is_empty() {
        return String::new();
    }
    let mut text = format!("\n── {connected_ssid} ({} APs) ──", own.len());
    for e in &own {
        let marker = if e.is_active { "● " } else { "  " };
        let label = ap_full_label(aliases, &e.bssid);
        let suffix = crate::models::width_center_suffix(
            e.channel_width_mhz,
            e.center_freq_mhz,
            e.center_freq2_mhz,
        );
        text.push_str(&format!(
            "\n{marker}{label}  {} dBm  ch{} {}{}",
            e.signal_dbm, e.channel, e.band(), suffix
        ));
    }
    text
}
