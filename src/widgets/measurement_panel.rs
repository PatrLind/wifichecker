use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow, Spinner};
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use crate::models::{Measurement, ThroughputUnit};

#[derive(Clone)]
pub struct MeasurementPanel {
    pub widget: GtkBox,
    current_label: Label,
    selected_label: Label,
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

        let current_group = libadwaita::PreferencesGroup::new();
        current_group.set_title("Current Signal");
        current_group.add(&current_label);
        vbox.append(&current_group);

        // Selected measurement details (inspect mode)
        let selected_label = Label::new(Some("No measurement selected — use the Select tool and click a point on the map."));
        selected_label.set_xalign(0.0);
        selected_label.set_wrap(true);
        selected_label.add_css_class("caption");
        let selected_group = libadwaita::PreferencesGroup::new();
        selected_group.set_title("Selected Measurement");
        selected_group.add(&selected_label);
        vbox.append(&selected_group);

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
        vbox.append(&status_box);

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

        vbox.append(&list_group);
        vbox.append(&scroll);

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

        Self {
            widget: vbox,
            current_label,
            selected_label,
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
        }
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
        iperf_mbps: Option<f64>,
        smb_mbps: Option<f64>,
        unit: ThroughputUnit,
    ) {
        let band = if freq >= 5000 { "5 GHz" } else { "2.4 GHz" };
        let quality = signal_quality_str(dbm);
        let mut text = format!(
            "SSID: {}\nBSSID: {}\nSignal: {} dBm ({})\nBand: {} | Ch: {}",
            ssid, bssid, dbm, quality, band, channel
        );
        if let Some(mbps) = iperf_mbps {
            text.push_str(&format!("\niperf3: {}", unit.format(mbps)));
        }
        if let Some(mbps) = smb_mbps {
            text.push_str(&format!("\nSamba: {}", unit.format(mbps)));
        }
        self.current_label.set_label(&text);
    }

    pub fn set_no_wifi(&self) {
        self.current_label.set_label("No WiFi connection detected");
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
        let text = {
            let measurements = self.measurements.borrow();
            let unit = self.unit.borrow();
            match id.as_ref().and_then(|id| measurements.iter().find(|m| &m.id == id)) {
                Some(m) => format_measurement_details(m, &unit),
                None => "No measurement selected — use the Select tool and click a point on the map.".to_string(),
            }
        };
        self.selected_label.set_label(&text);

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

        let mut info_str = format!(
            "{} | {} dBm | {}",
            m.ssid, m.signal_dbm,
            m.timestamp.format("%H:%M:%S")
        );
        if let Some(mbps) = m.iperf_mbps {
            info_str.push_str(&format!(" | ⚡{}", self.unit.borrow().format_short(mbps)));
        } else if let Some(mbps) = m.smb_mbps {
            info_str.push_str(&format!(" | 🗂{}", self.unit.borrow().format_short(mbps)));
        }

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

fn format_measurement_details(m: &Measurement, unit: &ThroughputUnit) -> String {
    let band = if m.frequency_mhz >= 5000 { "5 GHz" } else { "2.4 GHz" };
    let mut text = format!(
        "SSID: {}\nBSSID: {}\nSignal: {} dBm ({})\nBand: {} | Ch: {} | {} MHz\nTime: {}",
        m.ssid, m.bssid, m.signal_dbm, signal_quality_str(m.signal_dbm), band, m.channel, m.frequency_mhz,
        m.timestamp.format("%m-%d %H:%M:%S")
    );
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
    text
}
