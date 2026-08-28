use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, Button, DropDown, FileDialog,
    MenuButton, Orientation, Separator, StringList, ToggleButton,
};
use gtk4::glib;
use gtk4::gio::prelude::ActionMapExt;
use libadwaita::prelude::*;
use libadwaita::{ApplicationWindow, HeaderBar, MessageDialog, Toast, ToastOverlay};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::models::{AppSettings, Floor, Measurement, Project, ScanEntry, SignalSource};
use crate::persistence::{JsonStore, SettingsStore};
use crate::persistence::json_store::{drawings_dir_for, ensure_config_dirs};
use crate::services::{IperfClient, SmbTester, WifiInfo, WifiScanner};
use crate::services::nl80211;
use crate::widgets::{FloorPlanView, LegendBar, MeasurementPanel, SettingsDialog};
use crate::widgets::floor_plan_view::DrawMode;
use std::collections::HashMap;

/// Max APs stored in a measurement's scan list (keeps project files lean;
/// only the strongest ones matter for coverage analysis).
const MAX_SCAN_RESULTS: usize = 40;

struct MeasureResult {
    rx: f64,
    ry: f64,
    wifi: Option<WifiInfo>,
    /// Full scan list for the measured card (strongest first), taken when
    /// the measurement was recorded.
    scan_list: Vec<WifiInfo>,
    iperf_mbps: Option<f64>,
    iperf_error: Option<String>,
    smb_mbps: Option<f64>,
    smb_error: Option<String>,
}

pub struct Window {
    pub window: ApplicationWindow,
}

impl Window {
    pub fn new(app: &libadwaita::Application) -> Self {
        let window = ApplicationWindow::builder()
            .application(app)
            .title("WiFi Checker")
            .default_width(1200)
            .default_height(780)
            .build();

        let _ = ensure_config_dirs();
        let settings = Rc::new(RefCell::new(SettingsStore::load()));

        // Restore the remembered window size (if it was saved).
        {
            let s = settings.borrow();
            if s.window_width >= 400 && s.window_height >= 300 {
                window.set_default_size(s.window_width, s.window_height);
            }
        }

        // Restore the last opened project (fall back to the default project
        // file when none was remembered or the file can't be read).
        let (project, project_path) = {
            let last = settings.borrow().last_project_path.clone();
            let mut restored = None;
            if let Some(p) = last {
                let pb = std::path::PathBuf::from(p);
                match JsonStore::load(&pb) {
                    Ok(proj) => restored = Some((proj, pb)),
                    Err(e) => log::warn!("Could not restore last project {pb:?} ({e}); using the default project"),
                }
            }
            restored.unwrap_or_else(|| {
                let default = JsonStore::default_path();
                (
                    JsonStore::load(&default).unwrap_or_else(|_| Project::new("My Project")),
                    default,
                )
            })
        };

        window.set_title(Some(&format!("{} — WiFi Checker", project.name)));

        let state = Rc::new(RefCell::new(AppState {
            project,
            current_floor: 0,
            project_path,
        }));

        let content = build_ui(&window, state.clone(), settings.clone());
        window.set_content(Some(&content));

        // Ctrl+Q / Ctrl+W → close window
        let key_ctrl = gtk4::EventControllerKey::new();
        {
            let win = window.clone();
            key_ctrl.connect_key_pressed(move |_, key, _, mods| {
                if mods.contains(gtk4::gdk::ModifierType::CONTROL_MASK)
                    && (key == gtk4::gdk::Key::q || key == gtk4::gdk::Key::w)
                {
                    win.close();
                    return gtk4::glib::Propagation::Stop;
                }
                gtk4::glib::Propagation::Proceed
            });
        }
        window.add_controller(key_ctrl);

        Self { window }
    }
}

struct AppState {
    project: Project,
    current_floor: usize,
    /// File that the current project auto-saves to.
    project_path: PathBuf,
}

/// Save the current floor's canvas PNG and persist the whole project to its file.
fn auto_save(fp: &FloorPlanView, state: &Rc<RefCell<AppState>>) {
    let (idx, project_path) = {
        let s = state.borrow();
        (s.current_floor, s.project_path.clone())
    };
    let canvas_dir = drawings_dir_for(&project_path);
    if let Err(e) = std::fs::create_dir_all(&canvas_dir) {
        log::warn!("Failed to create drawings dir {}: {e}", canvas_dir.display());
    }
    let canvas_path = canvas_dir.join(format!("floor_{idx}.png"));
    {
        let mut s = state.borrow_mut();
        if let Some(floor) = s.project.floors.get_mut(idx) {
            // Only update drawing_path if a canvas was actually saved.
            if fp.save_canvas(&canvas_path).is_ok() {
                floor.drawing_path = Some(canvas_path.to_string_lossy().to_string());
            }
            // Origin is always persisted, independent of whether there is a drawing.
            floor.origin = fp.get_origin();
        }
    }
    let project = state.borrow().project.clone();
    let _ = JsonStore::save(&project, &project_path);
}

/// Save the current window size + sidebar width to the settings store
/// (no-op when the window is unrealistically small, e.g. mid-restore).
fn save_ui_geometry_now<W: gtk4::prelude::IsA<gtk4::Window>>(
    window: &W, body: &gtk4::Paned, settings: &Rc<RefCell<AppSettings>>,
) {
    let window = window.upcast_ref::<gtk4::Window>();
    let (w, h) = (window.width(), window.height());
    if w < 400 || h < 300 {
        return;
    }
    let sidebar = (w - body.position()).clamp(180, 700);
    let mut s = settings.borrow_mut();
    s.window_width = w;
    s.window_height = h;
    s.sidebar_width = sidebar;
    let _ = SettingsStore::save(&s);
}

/// Debounced version of [`save_ui_geometry_now`]: coalesces the burst of
/// resize/position notifications during a drag into one save ~400 ms later.
///
/// A pending timer is never *removed* (only replaced): once a timer fires
/// its `SourceId` is dead, and calling `remove()` on it would panic inside
/// the GTK signal handler. A stale pending timer simply fires a redundant
/// (harmless) save within 400 ms.
fn remember_ui_geometry<W: gtk4::prelude::IsA<gtk4::Window>>(
    window: &W, body: &gtk4::Paned, settings: Rc<RefCell<AppSettings>>,
    sched: Rc<RefCell<Option<glib::SourceId>>>,
) {
    let (window, body, settings) = (window.clone(), body.clone(), settings.clone());
    let id = glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
        save_ui_geometry_now(&window, &body, &settings);
        glib::ControlFlow::Break
    });
    sched.borrow_mut().replace(id);
}

/// "Known APs" dialog: lists every BSSID seen in this project's measurements
/// and lets the user assign a friendly alias to each (stored per project).
/// Aliases are used in the signal-source dropdown and in measurement details.
fn show_known_aps_dialog(
    parent: &libadwaita::ApplicationWindow,
    state: &Rc<RefCell<AppState>>,
    fp: &FloorPlanView,
    panel: &MeasurementPanel,
    overlay: &ToastOverlay,
    rebuild_source_dd: &Rc<dyn Fn()>,
) {
    use gtk4::prelude::*;

    let dialog = gtk4::Window::builder()
        .title("Known Access Points")
        .transient_for(parent)
        .modal(true)
        .default_width(580)
        .default_height(440)
        .build();

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 10);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let (sections, rest, aliases) = {
        let s = state.borrow();
        (
            s.project.measured_ap_sections(),
            s.project.unmeasured_bssids(),
            s.project.bssid_aliases.clone(),
        )
    };
    let total: usize = sections.iter().map(|(_, b)| b.len()).sum::<usize>() + rest.len();

    let desc = gtk4::Label::new(Some(
        "Access points seen in this project's measurements, grouped by the SSIDs you have measurements for — other networks are listed below. Type an alias to give an AP a friendly name — it is used in the signal-source dropdown and in measurement details. Leave the field empty to clear the alias.",
    ));
    desc.set_xalign(0.0);
    desc.set_wrap(true);
    vbox.append(&desc);

    if total == 0 {
        let empty = gtk4::Label::new(Some("No access points recorded yet — take a measurement first."));
        empty.set_xalign(0.0);
        vbox.append(&empty);
    }

    let list = gtk4::ListBox::new();
    list.add_css_class("boxed-list");
    // (bssid, entry) pairs so pending edits can be flushed when the dialog
    // closes (editing-done may not fire if the dialog is closed first).
    let rows: Rc<RefCell<Vec<(String, gtk4::Entry)>>> = Rc::new(RefCell::new(Vec::new()));
    for (ssid, bssids) in &sections {
        if bssids.is_empty() {
            continue;
        }
        add_section_header_row(&list, ssid);
        for b in bssids {
            add_known_ap_row(
                &list, &rows, state, fp, panel, overlay, rebuild_source_dd,
                b, aliases.get(b).map(|s| s.as_str()),
            );
        }
    }
    if !rest.is_empty() {
        add_section_header_row(&list, &format!("Other networks ({} APs)", rest.len()));
        for b in &rest {
            add_known_ap_row(
                &list, &rows, state, fp, panel, overlay, rebuild_source_dd,
                b, aliases.get(b).map(|s| s.as_str()),
            );
        }
    }

    // Flush any not-yet-confirmed edits and persist when the dialog closes.
    {
        let state = state.clone();
        let fp = fp.clone();
        let panel = panel.clone();
        let overlay = overlay.clone();
        let rebuild = rebuild_source_dd.clone();
        let rows = rows.clone();
        dialog.connect_close_request(move |_| {
            let mut changed = false;
            {
                let mut s = state.borrow_mut();
                for (bssid, entry) in rows.borrow().iter() {
                    let text = entry.text().trim().to_string();
                    // Normalize the same way set_alias does (trim, empty = clear)
                    // so change detection matches the stored value.
                    let new = if text.is_empty() { None } else { Some(text) };
                    let old = s.project.bssid_aliases.get(bssid).cloned();
                    if old != new {
                        s.project.set_alias(bssid, new);
                        changed = true;
                    }
                }
            }
            if changed {
                auto_save(&fp, &state);
                // Re-render all views (Current Signal, selected details, dropdown).
                panel.set_aliases(state.borrow().project.bssid_aliases.clone());
                rebuild();
                overlay.add_toast(Toast::new("Aliases saved"));
            }
            glib::Propagation::Proceed
        });
    }

    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_child(Some(&list));
    vbox.append(&scroll);

    dialog.set_child(Some(&vbox));
    dialog.present();
}

/// A non-selectable section header row in the Known APs list.
fn add_section_header_row(list: &gtk4::ListBox, title: &str) {
    let row = gtk4::ListBoxRow::new();
    row.set_selectable(false);
    let label = gtk4::Label::new(Some(title));
    label.set_xalign(0.0);
    label.add_css_class("heading");
    label.set_margin_start(6);
    label.set_margin_end(6);
    label.set_margin_top(10);
    label.set_margin_bottom(2);
    row.set_child(Some(&label));
    list.append(&row);
}

/// One AP row (BSSID + last-seen hint + alias entry) in the Known APs list.
fn add_known_ap_row(
    list: &gtk4::ListBox,
    rows: &Rc<RefCell<Vec<(String, gtk4::Entry)>>> ,
    state: &Rc<RefCell<AppState>>,
    fp: &FloorPlanView,
    panel: &MeasurementPanel,
    overlay: &ToastOverlay,
    rebuild_source_dd: &Rc<dyn Fn()>,
    bssid: &str,
    alias: Option<&str>,
) {
    use gtk4::prelude::*;

    let row = gtk4::ListBoxRow::new();
    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    hbox.set_margin_start(6);
    hbox.set_margin_end(6);
    hbox.set_margin_top(4);
    hbox.set_margin_bottom(4);

    let vleft = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    vleft.set_hexpand(true);
    let b_label = gtk4::Label::new(Some(bssid));
    b_label.set_xalign(0.0);
    b_label.add_css_class("monospace");
    b_label.add_css_class("caption");
    vleft.append(&b_label);
    if let Some(ssid) = state.borrow().project.last_ssid_for(bssid) {
        let s_label = gtk4::Label::new(Some(&format!("last seen: {ssid}")));
        s_label.set_xalign(0.0);
        s_label.add_css_class("dim-label");
        s_label.add_css_class("caption");
        vleft.append(&s_label);
    }

    let entry = gtk4::Entry::new();
    entry.set_placeholder_text(Some("alias…"));
    entry.set_width_chars(18);
    if let Some(a) = alias {
        entry.set_text(a);
    }
    {
        let state = state.clone();
        let fp = fp.clone();
        let panel = panel.clone();
        let overlay = overlay.clone();
        let rebuild = rebuild_source_dd.clone();
        let bssid = bssid.to_string();
        let entry_cb = entry.clone();
        entry.connect_editing_done(move |_| {
            let text = entry_cb.text().trim().to_string();
            {
                let mut s = state.borrow_mut();
                s.project.set_alias(&bssid, Some(text.clone()));
            }
            panel.set_aliases(state.borrow().project.bssid_aliases.clone());
            auto_save(&fp, &state);
            rebuild();
            overlay.add_toast(Toast::new(if text.is_empty() {
                "Alias cleared"
            } else {
                "Alias updated"
            }));
        });
    }

    hbox.append(&vleft);
    hbox.append(&entry);
    row.set_child(Some(&hbox));
    list.append(&row);
    rows.borrow_mut().push((bssid.to_string(), entry.clone()));
}

/// Notify the open Channel Report window about a measurement selection
/// change (the window stores a `"channel-report-select"` callback).
fn notify_channel_report(report_ref: &Rc<RefCell<Option<gtk4::Window>>>, id: Option<String>) {
    if let Some(w) = report_ref.borrow().as_ref() {
        if let Some(cb) = unsafe { w.data::<Rc<dyn Fn(Option<String>)>>("channel-report-select") } {
            let f = unsafe { cb.as_ref() };
            f(id);
        }
    }
}

/// Look up a measurement by id in the current floor (used to feed the
/// legend's selection pointer with the selected sample).
fn find_measurement(state: &Rc<RefCell<AppState>>, id: &str) -> Option<Measurement> {
    let s = state.borrow();
    s.project.floors
        .get(s.current_floor)
        .and_then(|f| f.measurements.iter().find(|m| m.id == id).cloned())
}

/// The distinct SSIDs already measured on the current floor, in first-seen
/// order. This is the set of "known" networks for this floor; a sample is
/// only flagged when its SSID is not in this set (so multiple networks, e.g.
/// a 2 GHz and a 5 GHz SSID, can be mapped together without warnings).
fn known_ssids(state: &Rc<RefCell<AppState>>) -> Vec<String> {
    let s = state.borrow();
    let mut v: Vec<String> = Vec::new();
    if let Some(floor) = s.project.floors.get(s.current_floor) {
        for m in &floor.measurements {
            if m.no_signal {
                continue; // no-signal points have no SSID
            }
            if !m.ssid.is_empty() && !v.contains(&m.ssid) {
                v.push(m.ssid.clone());
            }
        }
    }
    v
}

/// Format the "new network" warning message for the current SSID, listing the
/// known networks on this floor (capped so it stays readable).
fn format_new_network_warning(ssid: &str, known: &[String]) -> String {
    let known_str = if known.len() <= 3 {
        known.join(", ")
    } else {
        format!("{} networks", known.len())
    };
    format!("⚠ New network: \"{ssid}\" (this floor has: {known_str})")
}

/// Display name for a BSSID: the project alias when set, else the BSSID.
fn bssid_display_label(aliases: &HashMap<String, String>, bssid: &str) -> String {
    aliases.get(bssid).cloned().unwrap_or_else(|| bssid.to_string())
}

/// Index of the WiFi card to measure: the preferred card if it is currently
/// active, otherwise the first active card.
fn pick_card_index(preferred: &Option<String>, cards: &[WifiInfo]) -> Option<usize> {
    if cards.is_empty() {
        return None;
    }
    Some(match preferred {
        Some(p) => cards.iter().position(|c| &c.device == p).unwrap_or(0),
        None => 0,
    })
}

/// Show a confirm dialog when the user tries to measure on a network that
/// isn't one of this floor's measured networks. `on_response` is invoked with
/// true to proceed (record the sample) or false to cancel. The continuation
/// runs inside the dialog's choose callback — the robust pattern here, since
/// it works whether or not `choose` blocks the main loop.
fn show_new_network_confirm(
    window: &libadwaita::ApplicationWindow,
    current: &str,
    known: &[String],
    on_response: impl FnOnce(bool) + 'static,
) {
    let known_str = if known.len() <= 3 {
        known.join(", ")
    } else {
        format!("{} networks", known.len())
    };
    let dialog = libadwaita::MessageDialog::builder()
        .heading("New network")
        .body(&format!(
            "You're connected to \"{current}\", which isn't one of this floor's measured networks ({known_str}).\n\nRecord a sample on \"{current}\"?",
        ))
        .default_response("cancel")
        .close_response("cancel")
        .transient_for(window)
        .modal(true)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("ok", "Measure anyway");
    dialog.set_response_appearance("ok", libadwaita::ResponseAppearance::Suggested);
    dialog.choose(gtk4::gio::Cancellable::NONE, move |resp| {
        on_response(resp.as_str() == "ok");
    });
}

/// Convert a runtime scan entry to a stored one.
fn wifi_to_scan_entry(w: &WifiInfo) -> ScanEntry {
    ScanEntry {
        ssid: w.ssid.clone(),
        bssid: w.bssid.clone(),
        frequency_mhz: w.frequency_mhz,
        channel: w.channel,
        signal_dbm: w.signal_dbm,
        is_active: w.is_active,
        channel_width_mhz: w.channel_width_mhz,
        center_freq_mhz: w.center_freq_mhz,
        center_freq2_mhz: w.center_freq2_mhz,
    }
}

/// Apply nl80211 scan-channel data (width/center per BSSID) to a scan list.
///
/// BSSIDs are matched case-insensitively: NM reports them uppercase
/// ("B4:FB:…"), netlink keys are lowercase ("b4:fb:…").
fn merge_nl_scan(list: &mut [WifiInfo], scan: &nl80211::ScanChannels) {
    for e in list.iter_mut() {
        let key = e.bssid.to_ascii_lowercase();
        if let Some(c) = scan.get(&key) {
            e.channel_width_mhz = c.width_mhz;
            e.center_freq_mhz = c.center_freq_mhz;
            e.center_freq2_mhz = c.center_freq2_mhz;
        }
    }
}

/// Show a confirm dialog when no WiFi connection is detected at the
/// measurement point. Invokes `on_response(true)` to record a "no signal"
/// point (marking a dead zone) and `on_response(false)` to cancel.
/// `ap_count` is the number of networks detected in range (recorded with
/// the point when it is non-zero).
fn show_no_signal_confirm(
    window: &libadwaita::ApplicationWindow,
    ap_count: usize,
    on_response: impl FnOnce(bool) + 'static,
) {
    let body = if ap_count > 0 {
        format!(
            "No WiFi connection was detected at this point.\n{ap_count} network(s) in range — they will be recorded with this point.\n\nRecord it as a \"no signal\" point to mark this as a dead zone?"
        )
    } else {
        "No WiFi connection was detected at this point.\n\nRecord it as a \"no signal\" point to mark this as a dead zone?".to_string()
    };
    let dialog = libadwaita::MessageDialog::builder()
        .heading("No WiFi connection")
        .body(&body)
        .default_response("cancel")
        .close_response("cancel")
        .transient_for(window)
        .modal(true)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("ok", "Record 'no signal'");
    dialog.set_response_appearance("ok", libadwaita::ResponseAppearance::Suggested);
    dialog.choose(gtk4::gio::Cancellable::NONE, move |resp| {
        on_response(resp.as_str() == "ok");
    });
}


/// Record a "no signal" measurement at a position and update the UI.
/// `scan_list` is the (possibly empty) list of networks detected in range
/// at the point — stored with the measurement.
fn record_no_signal_point(
    rx: f64,
    ry: f64,
    scan_list: &[WifiInfo],
    state: &Rc<RefCell<AppState>>,
    fp: &FloorPlanView,
    legend: &LegendBar,
    panel: &MeasurementPanel,
    overlay: &ToastOverlay,
) {
    let mut m = Measurement::no_signal(rx, ry);
    m.scan_results = scan_list
        .iter()
        .take(MAX_SCAN_RESULTS)
        .map(wifi_to_scan_entry)
        .collect();
    let new_id = m.id.clone();
    let n_aps = m.scan_results.len();
    let (measurements, panel_measurements) = {
        let mut s = state.borrow_mut();
        let idx = s.current_floor;
        if let Some(floor) = s.project.floors.get_mut(idx) {
            floor.add_measurement(m);
            let measurements = floor.measurements.clone();
            (measurements.clone(), measurements)
        } else {
            return;
        }
    };
    fp.set_measurements(measurements.clone());
    legend.set_measurements(&measurements);
    panel.set_measurements(panel_measurements);
    fp.set_selected_measurement(Some(new_id.clone()));
    legend.set_selected_measurement(measurements.iter().find(|mm| mm.id == new_id).cloned());
    panel.set_selected_by_id(Some(new_id.clone()));
    auto_save(fp, state);
    let toast = if n_aps == 0 {
        "Recorded \"no signal\" point".to_string()
    } else {
        format!("Recorded \"no signal\" point ({n_aps} APs in range)")
    };
    overlay.add_toast(Toast::new(&toast));
}

/// Load a floor's data (image, canvas, calibration, measurements) into the
/// floor-plan view, legend bar, and measurement panel.
fn load_floor_into_view(
    fp: &FloorPlanView,
    legend: &LegendBar,
    panel: &MeasurementPanel,
    floor: &Floor,
) {
    fp.set_image("");
    fp.clear_canvas();
    fp.clear_calibration();
    // Selection is transient; clear it when switching/loading a floor.
    fp.set_selected_measurement(None);
    panel.set_selected_by_id(None);
    legend.set_selected_measurement(None);
    panel.set_network_warning(None);
    fp.set_pending_measurement(None);
    if let Some(ref p) = floor.image_path {
        if p.to_lowercase().ends_with(".pdf") {
            fp.set_pdf(p, floor.pdf_page.unwrap_or(0));
        } else {
            fp.set_image(p);
        }
    }
    if let Some(ref p) = floor.drawing_path {
        fp.load_canvas(std::path::Path::new(p));
    }
    if let (Some(sc), Some(a), Some(b)) = (floor.scale_px_per_m, floor.calib_point_a, floor.calib_point_b) {
        fp.set_scale(sc, a, b);
    }
    fp.set_origin(floor.origin);
    fp.set_measurements(floor.measurements.clone());
    legend.set_measurements(&floor.measurements);
    panel.set_measurements(floor.measurements.clone());
}

/// Return a copy of `project` whose drawing files are guaranteed to live in
/// `drawings_dir`: existing drawings are copied there and the references are
/// rewritten. This lets a project file and its drawings travel together.
fn independent_project_copy(project: &Project, drawings_dir: &std::path::Path) -> Project {
    let mut out = project.clone();
    let _ = std::fs::create_dir_all(drawings_dir);
    for (i, floor) in out.floors.iter_mut().enumerate() {
        let Some(ref p) = floor.drawing_path else { continue; };
        let src = std::path::Path::new(p);
        if !src.exists() { continue; }
        let dst = drawings_dir.join(format!("floor_{i}.png"));
        // Already stored in this project's drawings dir — just fix up the ref
        if let (Ok(a), Ok(b)) = (src.canonicalize(), drawings_dir.canonicalize()) {
            if a == b || a.starts_with(&b) {
                floor.drawing_path = Some(dst.to_string_lossy().to_string());
                continue;
            }
        }
        match std::fs::copy(src, &dst) {
            Ok(_) => floor.drawing_path = Some(dst.to_string_lossy().to_string()),
            Err(e) => log::warn!("Failed to copy drawing {} → {}: {e}", src.display(), dst.display()),
        }
    }
    out
}

/// Replace the in-memory project and refresh every UI element to match it.
/// Returns the project name.
/// Remember a project file as the last opened one and push it to the
/// front of the recent list (deduped, capped at 10). Persists the settings.
fn remember_project_path(path: &std::path::Path, settings: &Rc<RefCell<AppSettings>>) {
    let p = path.to_string_lossy().to_string();
    let mut s = settings.borrow_mut();
    s.last_project_path = Some(p.clone());
    s.recent_projects.retain(|r| *r != p);
    s.recent_projects.insert(0, p);
    s.recent_projects.truncate(10);
    let _ = SettingsStore::save(&s);
}

/// (Re)build the project menu: the base actions plus a "Recent Projects"
/// section from the settings (most recent first, file names only).
fn rebuild_project_menu(menu: &gtk4::gio::Menu, settings: &Rc<RefCell<AppSettings>>) {
    menu.remove_all();
    let base = gtk4::gio::Menu::new();
    base.append(Some("New Project"), Some("win.new-project"));
    base.append(Some("Open Project…"), Some("win.open-project"));
    base.append(Some("Save Project As…"), Some("win.save-project-as"));
    menu.append_section(Some("Project"), &base);

    let recents = settings.borrow().recent_projects.clone();
    if !recents.is_empty() {
        let rec = gtk4::gio::Menu::new();
        for (i, path) in recents.iter().enumerate() {
            let label = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            rec.append(Some(&label), Some(&format!("win.open-recent::{i}")));
        }
        menu.append_section(Some("Recent Projects"), &rec);
    }
}

fn apply_project(
    state: &Rc<RefCell<AppState>>,
    fp: &FloorPlanView,
    legend: &LegendBar,
    panel: &MeasurementPanel,
    floor_model: &StringList,
    floor_dropdown: &DropDown,
    suppress: &Rc<std::cell::Cell<bool>>,
    settings: &Rc<RefCell<AppSettings>>,
    rebuild_source_dd: &Rc<dyn Fn()>,
    project: Project,
    project_path: PathBuf,
) -> String {
    let mut project = project;
    if project.floors.is_empty() {
        project.add_floor(Floor::new("Floor 1"));
    }
    // Make sure the project's drawing files travel with the project file
    let project = independent_project_copy(&project, &drawings_dir_for(&project_path));

    let (names, start_floor, old_count) = {
        let mut s = state.borrow_mut();
        let old_count = s.project.floors.len();
        s.project = project;
        s.project_path = project_path.clone();
        s.current_floor = 0;
        let names = s.project.floors.iter().map(|f| f.name.clone()).collect::<Vec<_>>();
        (names, s.project.floors[0].clone(), old_count)
    };

    settings.borrow_mut().last_floor_index = 0;
    remember_project_path(&project_path, settings);
    let _ = SettingsStore::save(&settings.borrow());

    // Rebuild the floor dropdown without triggering selected_notify side-effects
    suppress.set(true);
    let name_refs: Vec<&str> = names.iter().map(|n| n.as_str()).collect();
    floor_model.splice(0, old_count as u32, &name_refs);
    floor_dropdown.set_selected(0);
    suppress.set(false);

    load_floor_into_view(fp, legend, panel, &start_floor);
    // New project in effect — refresh panel aliases and the signal-source
    // dropdown (its selected BSSID may not exist in this project).
    panel.set_aliases(state.borrow().project.bssid_aliases.clone());
    rebuild_source_dd();
    state.borrow().project.name.clone()
}


fn build_ui(
    window: &ApplicationWindow,
    state: Rc<RefCell<AppState>>,
    settings: Rc<RefCell<AppSettings>>,
) -> ToastOverlay {
    let overlay = ToastOverlay::new();
    let main_box = GtkBox::new(Orientation::Vertical, 0);

    // ── Header bar ────────────────────────────────────────────────────────────
    let header = HeaderBar::new();

    // Project menu (hamburger). Actions are wired up further down, once all
    // the widgets they touch exist. Rebuilt whenever the recent list changes.
    let project_menu = gtk4::gio::Menu::new();
    rebuild_project_menu(&project_menu, &settings);
    let menu_btn = MenuButton::new();
    menu_btn.set_icon_name("open-menu-symbolic");
    menu_btn.set_tooltip_text(Some("Project: new / open / save as"));
    menu_btn.set_menu_model(Some(&project_menu));
    header.pack_end(&menu_btn);

    let floor_model = StringList::new(&[]);
    let floor_dropdown = DropDown::new(Some(floor_model.clone()), gtk4::Expression::NONE);
    floor_dropdown.set_tooltip_text(Some("Select floor"));
    header.pack_start(&floor_dropdown);

    let add_floor_btn = Button::from_icon_name("list-add-symbolic");
    add_floor_btn.set_tooltip_text(Some("Add floor"));
    header.pack_start(&add_floor_btn);

    let edit_floor_btn = Button::from_icon_name("document-edit-symbolic");
    edit_floor_btn.set_tooltip_text(Some("Rename or delete current floor"));
    header.pack_start(&edit_floor_btn);

    // Signal source filter: which AP's signal the map/list show (connected
    // AP, best AP of the SSID, or a specific BSSID from this project).
    let source_model = StringList::new(&[
        SignalSource::CONNECTED_AP_LABEL,
        SignalSource::SSID_LABEL,
    ]);
    let source_dd = DropDown::new(Some(source_model.clone()), gtk4::Expression::NONE);
    source_dd.set_tooltip_text(Some(
        "Signal source: which AP's signal strength the map shows (connected AP, best AP of the SSID, or a specific BSSID)",
    ));
    header.pack_start(&source_dd);

    let known_aps_btn = Button::from_icon_name("network-wireless-symbolic");
    known_aps_btn.set_tooltip_text(Some(
        "Known APs: access points seen in this project — set alias names",
    ));
    header.pack_start(&known_aps_btn);

    let channel_report_btn = Button::with_label("Channel report");
    channel_report_btn.set_tooltip_text(Some(
        "Channel report: channel / AP / signal per measurement point and across the site survey",
    ));
    header.pack_start(&channel_report_btn);

    let heatmap_toggle = ToggleButton::new();
    heatmap_toggle.set_icon_name("view-grid-symbolic");
    heatmap_toggle.set_tooltip_text(Some("Toggle heatmap"));
    heatmap_toggle.set_active(true);
    header.pack_end(&heatmap_toggle);

    let settings_btn = Button::from_icon_name("preferences-system-symbolic");
    settings_btn.set_tooltip_text(Some("Settings (iperf / Samba)"));
    header.pack_end(&settings_btn);

    main_box.append(&header);

    // ── Signal source: rebuild the dropdown options ────────────────────────
    // Options: "Connected AP", then one entry per SSID that has saved
    // measurements ("best AP of this network"), each followed by its BSSIDs
    // (indented). Networks that only appear in scan lists are not options.
    // Rebuilding preserves the current selection; if the selected AP no
    // longer exists (e.g. measurements deleted) it falls back to
    // "Connected AP".
    let suppress_source_change = Rc::new(std::cell::Cell::new(false));
    // The options currently shown, parallel to the dropdown rows; used by
    // selected_notify to resolve the chosen source.
    let source_options: Rc<RefCell<Vec<SignalSource>>> = Rc::new(RefCell::new(Vec::new()));
    let rebuild_source_dd: Rc<dyn Fn()> = Rc::new({
        let state = state.clone();
        let settings = settings.clone();
        let source_model = source_model.clone();
        let source_dd = source_dd.clone();
        let suppress = suppress_source_change.clone();
        let source_options = source_options.clone();
        move || {
            let (opts, labels, current) = {
                let s = state.borrow();
                let ssids = s.project.measured_ssids();
                let mut opts: Vec<SignalSource> = vec![SignalSource::ConnectedAp];
                let mut labels: Vec<String> = vec![SignalSource::CONNECTED_AP_LABEL.to_string()];
                for ssid in &ssids {
                    opts.push(SignalSource::Ssid(ssid.clone()));
                    labels.push(ssid.clone());
                    for b in s.project.bssids_of_ssid(ssid) {
                        opts.push(SignalSource::Bssid(b.clone()));
                        labels.push(format!("  {}", bssid_display_label(&s.project.bssid_aliases, &b)));
                    }
                }
                (opts, labels, settings.borrow().signal_source.clone())
            };
            // Resolve the current source to a row.
            let (target_idx, fallback) = match &current {
                SignalSource::ConnectedAp => (0u32, false),
                SignalSource::Ssid(s) if !s.is_empty() => match opts.iter().position(|x| *x == current) {
                    Some(p) => (p as u32, false),
                    None => (0, true), // SSID no longer measured → fall back
                },
                SignalSource::Ssid(_) => match opts
                    .iter()
                    .position(|x| matches!(x, SignalSource::Ssid(x) if !x.is_empty()))
                {
                    Some(p) => (p as u32, false), // legacy generic → first measured SSID
                    None => (0, false),
                },
                SignalSource::Bssid(_) => match opts.iter().position(|x| *x == current) {
                    Some(p) => (p as u32, false),
                    None => (0, true), // chosen BSSID is gone → fall back
                },
            };
            *source_options.borrow_mut() = opts;
            suppress.set(true);
            let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            source_model.splice(0, source_model.n_items(), &refs);
            source_dd.set_selected(target_idx);
            suppress.set(false);
            if fallback {
                settings.borrow_mut().signal_source = SignalSource::ConnectedAp;
                let _ = SettingsStore::save(&settings.borrow());
            }
        }
    });

    // ── Drawing toolbar ───────────────────────────────────────────────────────
    let draw_bar = GtkBox::new(Orientation::Horizontal, 4);
    draw_bar.set_margin_start(6);
    draw_bar.set_margin_end(6);
    draw_bar.set_margin_top(4);
    draw_bar.set_margin_bottom(4);

    let mode_measure = ToggleButton::builder()
        .icon_name("find-location-symbolic")
        .tooltip_text("Measure mode (click to record WiFi point)")
        .active(true)
        .build();
    let mode_draw = ToggleButton::builder()
        .icon_name("edit-symbolic")
        .tooltip_text("Draw mode (freehand floor plan)")
        .group(&mode_measure)
        .build();
    let mode_calib = ToggleButton::builder()
        .icon_name("zoom-fit-best-symbolic")
        .tooltip_text("Calibrate scale (click two points)")
        .group(&mode_measure)
        .build();
    let mode_origin = ToggleButton::builder()
        .icon_name("mark-location-symbolic")
        .tooltip_text("Set origin (0, 0) — click to place")
        .group(&mode_measure)
        .build();
    let mode_select = ToggleButton::builder()
        .icon_name("cursor-arrow-symbolic")
        .tooltip_text("Select mode (click a point to inspect a measurement)")
        .group(&mode_measure)
        .build();
    let mode_ruler = ToggleButton::builder()
        .icon_name("measure-symbolic")
        .tooltip_text("Ruler (click two points to measure the distance between them)")
        .group(&mode_measure)
        .build();

    let clear_canvas_btn = Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text("Clear drawing")
        .build();

    let grid_toggle = ToggleButton::builder()
        .icon_name("view-grid-symbolic")
        .tooltip_text("Toggle grid")
        .active(settings.borrow().show_grid)
        .build();

    let origin_toggle = ToggleButton::builder()
        .icon_name("crosshairs-symbolic")
        .tooltip_text("Show/hide the origin (0, 0) marker")
        .active(settings.borrow().show_origin)
        .build();
    let scale_toggle = ToggleButton::builder()
        .icon_name("view-reveal-symbolic")
        .tooltip_text("Show/hide the calibration scale line")
        .active(settings.borrow().show_scale)
        .build();

    // Grid spacing selector
    let spacing_model = StringList::new(&["0.5 m", "1 m", "2 m", "5 m"]);
    let spacing_values = [0.5f64, 1.0, 2.0, 5.0];
    let cur_spacing = settings.borrow().grid_spacing_m;
    let spacing_idx = spacing_values.iter().position(|&v| (v - cur_spacing).abs() < 0.01).unwrap_or(1) as u32;
    let grid_spacing_dd = DropDown::new(Some(spacing_model), gtk4::Expression::NONE);
    grid_spacing_dd.set_selected(spacing_idx);
    grid_spacing_dd.set_tooltip_text(Some("Grid spacing"));

    let import_btn = Button::builder()
        .icon_name("insert-image-symbolic")
        .tooltip_text("Import floor plan image")
        .build();

    let zoom_in_btn = Button::builder()
        .icon_name("zoom-in-symbolic")
        .tooltip_text("Zoom in")
        .build();
    let zoom_out_btn = Button::builder()
        .icon_name("zoom-out-symbolic")
        .tooltip_text("Zoom out")
        .build();
    let zoom_reset_btn = Button::builder()
        .icon_name("zoom-original-symbolic")
        .tooltip_text("Reset zoom")
        .build();

    draw_bar.append(&mode_select);
    draw_bar.append(&mode_measure);
    draw_bar.append(&mode_draw);
    draw_bar.append(&mode_calib);
    draw_bar.append(&mode_origin);
    draw_bar.append(&mode_ruler);
    draw_bar.append(&Separator::new(Orientation::Vertical));
    draw_bar.append(&clear_canvas_btn);
    draw_bar.append(&Separator::new(Orientation::Vertical));
    draw_bar.append(&grid_toggle);
    draw_bar.append(&grid_spacing_dd);
    draw_bar.append(&origin_toggle);
    draw_bar.append(&scale_toggle);
    draw_bar.append(&Separator::new(Orientation::Vertical));
    draw_bar.append(&import_btn);
    draw_bar.append(&Separator::new(Orientation::Vertical));
    draw_bar.append(&zoom_in_btn);
    draw_bar.append(&zoom_out_btn);
    draw_bar.append(&zoom_reset_btn);

    main_box.append(&draw_bar);

    // ── Body ──────────────────────────────────────────────────────────────────
    let body = gtk4::Paned::new(Orientation::Horizontal);
    body.set_vexpand(true);

    let floor_plan = FloorPlanView::new();
    floor_plan.set_show_grid(settings.borrow().show_grid);
    floor_plan.set_grid_spacing(settings.borrow().grid_spacing_m);
    floor_plan.set_measurement_grid_spacing(settings.borrow().measurement_grid_spacing_m);
    floor_plan.set_snap_to_grid(settings.borrow().snap_to_grid);
    floor_plan.set_show_origin(settings.borrow().show_origin);
    floor_plan.set_show_scale(settings.borrow().show_scale);
    floor_plan.set_color_metric(settings.borrow().color_metric);

    let legend = LegendBar::new();
    legend.set_color_metric(settings.borrow().color_metric);
    let fp_col = GtkBox::new(Orientation::Vertical, 0);
    fp_col.set_hexpand(true);
    fp_col.set_vexpand(true);
    fp_col.append(&floor_plan.widget);
    fp_col.append(&legend.widget);

    let sidebar = GtkBox::new(Orientation::Vertical, 6);
    sidebar.set_width_request(290);

    let panel = MeasurementPanel::new();

    sidebar.append(&panel.widget);

    body.set_start_child(Some(&fp_col));
    body.set_end_child(Some(&sidebar));
    // One-shot: once the paned has a size, give the sidebar its remembered
    // (or default) width. The divider is draggable afterwards to resize
    // either side.
    {
        let body_init = body.clone();
        let settings_init = settings.clone();
        let _ = glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let total = body_init.width();
            if total > 0 {
                let sidebar = {
                    let s = settings_init.borrow();
                    if s.sidebar_width >= 180 { s.sidebar_width } else { 290 }
                };
                body_init.set_position((total - sidebar).max(100));
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });

        // Remember window size + sidebar width across sessions: debounced
        // saves on resize/pane-drag, plus one immediate save on close.
        let ui_sched = Rc::new(RefCell::new(None::<glib::SourceId>));
        {
            let (w, b, s, c) = (window.clone(), body.clone(), settings.clone(), ui_sched.clone());
            window.connect_notify_local(Some("width"), move |_, _| remember_ui_geometry(&w, &b, s.clone(), c.clone()));
        }
        {
            let (w, b, s, c) = (window.clone(), body.clone(), settings.clone(), ui_sched.clone());
            window.connect_notify_local(Some("height"), move |_, _| remember_ui_geometry(&w, &b, s.clone(), c.clone()));
        }
        {
            let (w, b, s, c) = (window.clone(), body.clone(), settings.clone(), ui_sched.clone());
            body.connect_notify_local(Some("position"), move |_, _| remember_ui_geometry(&w, &b, s.clone(), c.clone()));
        }
        {
            let (w, b, s) = (window.clone(), body.clone(), settings.clone());
            window.connect_destroy(move |_| save_ui_geometry_now(&w, &b, &s));
        }
    }
    main_box.append(&body);
    overlay.set_child(Some(&main_box));

    // ── Wire up callbacks ──────────────────────────────────────────────────────

    // Heatmap toggle
    {
        let fp = floor_plan.clone();
        heatmap_toggle.connect_toggled(move |btn| fp.set_show_heatmap(btn.is_active()));
    }

    // Draw mode buttons
    {
        let fp = floor_plan.clone();
        mode_measure.connect_toggled(move |btn| {
            if btn.is_active() { fp.set_draw_mode(DrawMode::Measure); }
        });
    }
    {
        let fp = floor_plan.clone();
        mode_draw.connect_toggled(move |btn| {
            if btn.is_active() { fp.set_draw_mode(DrawMode::Draw); }
        });
    }
    {
        let fp = floor_plan.clone();
        mode_calib.connect_toggled(move |btn| {
            if btn.is_active() { fp.set_draw_mode(DrawMode::Calibrate); }
        });
    }
    {
        let fp = floor_plan.clone();
        mode_origin.connect_toggled(move |btn| {
            if btn.is_active() { fp.set_draw_mode(DrawMode::SetOrigin); }
        });
    }
    {
        let fp = floor_plan.clone();
        mode_select.connect_toggled(move |btn| {
            if btn.is_active() { fp.set_draw_mode(DrawMode::Select); }
        });
    }
    {
        let fp = floor_plan.clone();
        mode_ruler.connect_toggled(move |btn| {
            if btn.is_active() { fp.set_draw_mode(DrawMode::Ruler); }
        });
    }

    // Clear canvas
    {
        let fp = floor_plan.clone();
        let state = state.clone();
        let overlay_ref = overlay.clone();
        clear_canvas_btn.connect_clicked(move |_| {
            fp.clear_canvas();
            {
                let mut s = state.borrow_mut();
                let idx = s.current_floor;
                if let Some(floor) = s.project.floors.get_mut(idx) {
                    floor.drawing_path = None;
                }
            }
            auto_save(&fp, &state);
            overlay_ref.add_toast(Toast::new("Drawing cleared"));
        });
    }

    // Grid toggle
    {
        let fp = floor_plan.clone();
        let settings = settings.clone();
        grid_toggle.connect_toggled(move |btn| {
            fp.set_show_grid(btn.is_active());
            settings.borrow_mut().show_grid = btn.is_active();
            let _ = SettingsStore::save(&settings.borrow());
        });
    }

    // Origin marker visibility toggle (persisted).
    {
        let fp = floor_plan.clone();
        let settings = settings.clone();
        origin_toggle.connect_toggled(move |btn| {
            fp.set_show_origin(btn.is_active());
            settings.borrow_mut().show_origin = btn.is_active();
            let _ = SettingsStore::save(&settings.borrow());
        });
    }

    // Scale line visibility toggle (persisted).
    {
        let fp = floor_plan.clone();
        let settings = settings.clone();
        scale_toggle.connect_toggled(move |btn| {
            fp.set_show_scale(btn.is_active());
            settings.borrow_mut().show_scale = btn.is_active();
            let _ = SettingsStore::save(&settings.borrow());
        });
    }

    // Grid spacing dropdown
    {
        let fp = floor_plan.clone();
        let settings = settings.clone();
        grid_spacing_dd.connect_selected_notify(move |dd| {
            let m = spacing_values[dd.selected() as usize];
            fp.set_grid_spacing(m);
            settings.borrow_mut().grid_spacing_m = m;
            let _ = SettingsStore::save(&settings.borrow());
        });
    }

    // Measure button — WiFi scan in background thread
    // ── on_measure_click: click on map = immediate full measurement ───────────
    {
        let state = state.clone();
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let overlay_ref = overlay.clone();
        let settings = settings.clone();
        let legend = legend.clone();
        let window = window.clone();
        // Guards against starting a new measurement while one is in flight.
        let measuring: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

        // Start the actual measurement: guard, pending box, background scan +
        // speed tests, and recording. Called directly, or after a new-network
        // confirm.
        let start_measurement: Rc<dyn Fn(f64, f64)> = Rc::new({
            let fp = fp.clone();
            let panel = panel.clone();
            let state = state.clone();
            let overlay_ref = overlay_ref.clone();
            let settings = settings.clone();
            let legend = legend.clone();
            let window = window.clone();
            let measuring = measuring.clone();
            let rebuild_source_dd = rebuild_source_dd.clone();
            move |rx: f64, ry: f64| {
            // Block new measurements while one is in progress.
            if *measuring.borrow() {
                overlay_ref.add_toast(Toast::new("Measurement in progress — please wait"));
                return;
            }
            *measuring.borrow_mut() = true;
            fp.set_pending_measurement(Some((rx, ry)));

            let (iperf_enabled, iperf_server, iperf_port, iperf_dur, iperf_streams,
                 smb_enabled, smb_server, smb_share, smb_user, smb_pass,
                 unit) = {
                let s = settings.borrow();
                (
                    s.iperf_enabled, s.iperf_server.clone(), s.iperf_port, s.iperf_duration_secs,
                    s.iperf_parallel_streams,
                    s.smb_enabled, s.smb_server.clone(), s.smb_share.clone(),
                    s.smb_username.clone(), s.smb_password.clone(),
                    s.throughput_unit,
                )
            };

            let panel2 = panel.clone();
            let status_msg = match (iperf_enabled && !iperf_server.is_empty(),
                                    smb_enabled   && !smb_server.is_empty()) {
                (true,  true)  => "Scanning + iperf3 + Samba…",
                (true,  false) => "Scanning + iperf3…",
                (false, true)  => "Scanning + Samba…",
                (false, false) => "Scanning WiFi…",
            };
            panel2.set_measuring(true, status_msg);

            let preferred_device = settings.borrow().wifi_device.clone();
            let (tx, recv) = async_channel::bounded::<MeasureResult>(1);
            std::thread::spawn(move || {
                let cards = WifiScanner::scan_all().unwrap_or_default();
                let mut wifi = pick_card_index(&preferred_device, &cards).and_then(|i| cards.get(i).cloned());

                // Fresh scan list for the measured card (all APs in range,
                // strongest first). Requests a new scan and waits up to ~6 s
                // for it; falls back to the cached list on failure.
                //
                // Without an active connection we still scan (on the
                // preferred card if available, else the first WiFi card),
                // so a "no signal" point can record which networks were in
                // range.
                let mut scan_list: Vec<WifiInfo> = if let Some(w) = &wifi {
                    WifiScanner::scan_list(Some(&w.device)).unwrap_or_default()
                } else {
                    let devs = crate::services::nm_dbus::query_wifi_device_names();
                    let dev = devs
                        .iter()
                        .find(|d| preferred_device.as_deref() == Some(d.as_str()))
                        .or_else(|| devs.first());
                    dev.map(|d| WifiScanner::scan_list(Some(d)).unwrap_or_default())
                        .unwrap_or_default()
                };

                // Channel width + center frequency via native netlink
                // (best effort — unavailable data simply stays unknown).
                let nl_device = wifi
                    .as_ref()
                    .map(|w| w.device.clone())
                    .or_else(|| scan_list.first().map(|e| e.device.clone()));
                if let Some(dev) = nl_device.as_deref() {
                    if let Some(ic) = nl80211::interface_channel(dev) {
                        if let Some(w) = wifi.as_mut() {
                            w.channel_width_mhz = ic.channel.width_mhz;
                            w.center_freq_mhz = ic.channel.center_freq_mhz;
                            w.center_freq2_mhz = ic.channel.center_freq2_mhz;
                        }
                        merge_nl_scan(&mut scan_list, &nl80211::scan_channels(ic.ifindex));
                    }
                }

                let (iperf_mbps, iperf_error) = if iperf_enabled && !iperf_server.is_empty() {
                    match IperfClient::new(&iperf_server, iperf_port, iperf_dur, iperf_streams).run_test() {
                        Ok(mbps) => (Some(mbps), None),
                        Err(e)   => (None, Some(e.to_string())),
                    }
                } else {
                    (None, None)
                };

                let (smb_mbps, smb_error) = if smb_enabled && !smb_server.is_empty() {
                    let mut tester = SmbTester::new(&smb_server, &smb_share);
                    tester.username = if smb_user.is_empty() { None } else { Some(smb_user) };
                    tester.password = if smb_pass.is_empty() { None } else { Some(smb_pass) };
                    match tester.run_test() {
                        Ok(mbps) => (Some(mbps), None),
                        Err(e)   => (None, Some(e.to_string())),
                    }
                } else {
                    (None, None)
                };

                tx.send_blocking(MeasureResult {
                    rx, ry, wifi, scan_list, iperf_mbps, iperf_error, smb_mbps, smb_error,
                }).ok();
            });

            let state2 = state.clone();
            let fp2 = fp.clone();
            let panel3 = panel.clone();
            let overlay2 = overlay_ref.clone();
            let legend2 = legend.clone();
            let measuring2 = measuring.clone();
            let window2 = window.clone();
            let rebuild_source_dd2 = rebuild_source_dd.clone();

            glib::spawn_future_local(async move {
                let Ok(result) = recv.recv().await else {
                    *measuring2.borrow_mut() = false;
                    fp2.set_pending_measurement(None);
                    panel3.set_measuring(false, "");
                    return;
                };
                panel3.set_measuring(false, "");

                let Some(info) = result.wifi else {
                    let (rx, ry) = (result.rx, result.ry);
                    // Still surface speed-test errors even without WiFi
                    if let Some(ref e) = result.iperf_error {
                        overlay2.add_toast(Toast::new(&format!("iperf3 error: {e}")));
                    }
                    if let Some(ref e) = result.smb_error {
                        overlay2.add_toast(Toast::new(&format!("Samba error: {e}")));
                    }
                    // Offer to record this point as "no signal" (WiFi off / not
                    // connected) so the user can mark dead zones. The scan list
                    // (networks in range) is recorded with the point.
                    let state3 = state2.clone();
                    let fp3 = fp2.clone();
                    let legend3 = legend2.clone();
                    let panel4 = panel3.clone();
                    let overlay3 = overlay2.clone();
                    let scan_list = result.scan_list;
                    let n_aps = scan_list.len();
                    show_no_signal_confirm(&window2, n_aps, move |ok| {
                        if ok {
                            record_no_signal_point(rx, ry, &scan_list, &state3, &fp3, &legend3, &panel4, &overlay3);
                        }
                    });
                    *measuring2.borrow_mut() = false;
                    fp2.set_pending_measurement(None);
                    return;
                };

                let mut m = Measurement::new(
                    result.rx, result.ry,
                    info.ssid.clone(), info.bssid.clone(),
                    info.frequency_mhz, info.channel, info.signal_dbm,
                );
                // Store the scan list taken at this point (strongest first).
                m.scan_results = result
                    .scan_list
                    .iter()
                    .take(MAX_SCAN_RESULTS)
                    .map(wifi_to_scan_entry)
                    .collect();
                m.channel_width_mhz = info.channel_width_mhz;
                m.center_freq_mhz = info.center_freq_mhz;
                m.center_freq2_mhz = info.center_freq2_mhz;
                m.device = Some(info.device.clone());
                m.iperf_mbps = result.iperf_mbps;
                m.smb_mbps = result.smb_mbps;
                let new_id = m.id.clone();

                // Surface speed-test errors as toasts
                if let Some(ref e) = result.iperf_error {
                    overlay2.add_toast(Toast::new(&format!("iperf3 error: {e}")));
                }
                if let Some(ref e) = result.smb_error {
                    overlay2.add_toast(Toast::new(&format!("Samba error: {e}")));
                }

                let (measurements, panel_measurements) = {
                    let mut s = state2.borrow_mut();
                    let idx = s.current_floor;
                    if let Some(floor) = s.project.floors.get_mut(idx) {
                        floor.add_measurement(m);
                        let measurements = floor.measurements.clone();
                        (measurements.clone(), measurements)
                    } else {
                        *measuring2.borrow_mut() = false;
                        fp2.set_pending_measurement(None);
                        return;
                    }
                };

                fp2.set_measurements(measurements.clone());
                legend2.set_measurements(&measurements);
                panel3.set_measurements(panel_measurements);
                panel3.set_throughput_unit(unit);
                // New measurements may have introduced new BSSIDs — refresh
                // the signal-source dropdown options (selection is preserved).
                rebuild_source_dd2();
                panel3.update_current_wifi(
                    &info.ssid, &info.bssid,
                    info.signal_dbm, info.frequency_mhz, info.channel,
                    &info.device,
                    info.channel_width_mhz, info.center_freq_mhz, info.center_freq2_mhz,
                    result.iperf_mbps, result.smb_mbps, unit,
                    Some(&result.scan_list),
                );
                legend2.set_current_signal(Some(info.signal_dbm as f64));
                // Show the just-recorded sample in Selected Measurement + highlight it.
                fp2.set_selected_measurement(Some(new_id.clone()));
                legend2.set_selected_measurement(measurements.iter().find(|m| m.id == new_id).cloned());
                panel3.set_selected_by_id(Some(new_id));
                auto_save(&fp2, &state2);
                *measuring2.borrow_mut() = false;
                fp2.set_pending_measurement(None);
                let mut toast_msg = format!("{} dBm | {}", info.signal_dbm, info.ssid);
                if let Some(mbps) = result.iperf_mbps {
                    toast_msg.push_str(&format!(" | ⚡{}", unit.format_short(mbps)));
                }
                panel3.set_network_warning(None);
                overlay2.add_toast(Toast::new(&toast_msg));
            });
            }
        });

        let wifi_settings = settings.clone();
        floor_plan.set_on_measure_click(move |rx, ry| {
            // Block new measurements while one is in progress.
            if *measuring.borrow() {
                overlay_ref.add_toast(Toast::new("Measurement in progress — please wait"));
                return;
            }
            let known = known_ssids(&state);
            if known.is_empty() {
                // First sample on this floor: no measured networks to be
                // consistent with yet.
                start_measurement(rx, ry);
                return;
            }
            // There are known networks. Do a quick background scan for the
            // current SSID, then (on the main thread, deferred out of the
            // drag/pointer-grab context) confirm if it is a new network.
            let (tx, rchan) = async_channel::bounded(1);
            let preferred = wifi_settings.borrow().wifi_device.clone();
            std::thread::spawn(move || {
                let cards = WifiScanner::scan_all().unwrap_or_default();
                let cur = pick_card_index(&preferred, &cards)
                    .and_then(|i| cards.get(i).cloned())
                    .map(|w| w.ssid);
                let _ = tx.send_blocking((cur, known));
            });
            let start = start_measurement.clone();
            let window = window.clone();
            let overlay = overlay_ref.clone();
            glib::spawn_future_local(async move {
                let (cur, known) = rchan.recv().await.unwrap_or((None, Vec::new()));
                match cur {
                    Some(c) if !known.contains(&c) => {
                        // Confirm; the continuation runs in the dialog callback
                        // (robust whether or not `choose` blocks the main loop).
                        let start2 = start.clone();
                        show_new_network_confirm(&window, &c, &known, move |ok| {
                            if ok {
                                start2(rx, ry);
                            } else {
                                overlay.add_toast(Toast::new("Measurement cancelled"));
                            }
                        });
                    }
                    _ => start(rx, ry),
                }
            });
        });
    }

    // Calibration callback → show distance dialog
    {
        let fp = floor_plan.clone();
        let state = state.clone();
        let window_ref = window.clone();
        floor_plan.set_on_calibration_complete(move |ax, ay, bx, by| {
            let dialog = MessageDialog::builder()
                .heading("Set real distance")
                .body("Enter the real-world distance between points A and B (in meters):")
                .default_response("ok")
                .close_response("cancel")
                .transient_for(&window_ref)
                .modal(true)
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("ok", "Set Scale");
            dialog.set_response_appearance("ok", libadwaita::ResponseAppearance::Suggested);

            let entry = gtk4::Entry::builder()
                .placeholder_text("e.g. 3.5")
                .input_purpose(gtk4::InputPurpose::Number)
                .build();
            dialog.set_extra_child(Some(&entry));

            let fp2 = fp.clone();
            let state2 = state.clone();
            let entry2 = entry.clone();
            dialog.choose(gtk4::gio::Cancellable::NONE, move |response| {
                if response.as_str() != "ok" { return; }
                let text = entry2.text();
                let Ok(real_m) = text.trim().parse::<f64>() else { return; };
                if real_m <= 0.0 { return; }

                let Some(scale) = fp2.set_calibration((ax, ay), (bx, by), real_m) else { return; };

                {
                    let mut s = state2.borrow_mut();
                    let idx = s.current_floor;
                    if let Some(floor) = s.project.floors.get_mut(idx) {
                        floor.scale_px_per_m = Some(scale);
                        floor.calib_point_a = Some((ax, ay));
                        floor.calib_point_b = Some((bx, by));
                    }
                }
                auto_save(&fp2, &state2);
            });
        });
    }

    // Persist drawing strokes when the user finishes a draw gesture
    {
        let fp = floor_plan.clone();
        let state = state.clone();
        floor_plan.set_on_draw_complete(move || {
            auto_save(&fp, &state);
        });
    }

    // Persist the origin immediately when it is placed (so it survives reload)
    {
        let fp = floor_plan.clone();
        let state = state.clone();
        let overlay_ref = overlay.clone();
        floor_plan.set_on_origin_set(move || {
            auto_save(&fp, &state);
            overlay_ref.add_toast(Toast::new("Origin updated"));
        });
    }

    // Measurement selection: two-way correlation between map and list
    // (also kept in sync with the open Channel Report window).
    let report_ref: Rc<RefCell<Option<gtk4::Window>>> = Rc::new(RefCell::new(None));
    {
        let panel = panel.clone();
        let legend = legend.clone();
        let state = state.clone();
        let report_ref = report_ref.clone();
        floor_plan.set_on_select_measurement(move |id| {
            let sel = id.as_ref().and_then(|id| find_measurement(&state, id));
            legend.set_selected_measurement(sel);
            panel.set_selected_by_id(id.clone());
            notify_channel_report(&report_ref, id);
        });
    }
    {
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let panel_cb = panel.clone();
        let legend = legend.clone();
        let state = state.clone();
        let report_ref = report_ref.clone();
        panel.set_on_row_clicked(move |id| {
            fp.set_selected_measurement(Some(id.clone()));
            legend.set_selected_measurement(find_measurement(&state, &id));
            panel_cb.set_selected_by_id(Some(id.clone()));
            notify_channel_report(&report_ref, Some(id));
        });
    }
    // Remember the chosen WiFi card when the user switches it in the selector.
    {
        let settings = settings.clone();
        let ov = overlay.clone();
        panel.set_on_device_changed(move |device: String| {
            settings.borrow_mut().wifi_device = Some(device.clone());
            let _ = SettingsStore::save(&settings.borrow());
            ov.add_toast(Toast::new(&format!("Measuring card: {device}")));
        });
    }

    // Live Current Signal: periodically refresh with the active WiFi AP.
    {
        let panel = panel.clone();
        let legend = legend.clone();
        let state = state.clone();
        let overlay = overlay.clone();
        let settings = settings.clone();
        // Tracks whether the "new network" warning is currently shown, so the
        // toast only fires on the transition into a mismatch (not every tick).
        let off_flag: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
        // Tracks whether the "preferred card unavailable" warning is shown.
        let pref_flag: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
        // Shared cached AP list: (device, LastScan counter, list). The full
        // AP property read only happens when NM's LastScan advances (a scan
        // completed), so the per-tick cost is a single property read.
        let scan_cache: std::sync::Arc<std::sync::Mutex<(String, i64, Vec<WifiInfo>)>> =
            std::sync::Arc::new(std::sync::Mutex::new((String::new(), 0, Vec::new())));
        let _ = glib::timeout_add_local(std::time::Duration::from_millis(1500), move || {
            let p = panel.clone();
            let lg = legend.clone();
            let st = state.clone();
            let ov = overlay.clone();
            let off = off_flag.clone();
            let pref = pref_flag.clone();
            let preferred = settings.borrow().wifi_device.clone();
            let source = settings.borrow().signal_source.clone();
            let (tx, rx) = async_channel::bounded(1);
            {
                let pt = preferred.clone();
                let scan_cache = scan_cache.clone();
                std::thread::spawn(move || {
                    let cards = WifiScanner::scan_all().unwrap_or_default();
                    let chosen = pick_card_index(&pt, &cards);
                    let mut info = chosen.and_then(|i| cards.get(i).cloned());
                    // AP list for the measured card: re-read the cached scan
                    // list only when NM completed a new scan since the last
                    // tick (LastScan counter advanced), or when the card changed.
                    let mut list: Vec<WifiInfo> = Vec::new();
                    let mut list_refreshed = false;
                    if let Some(w) = info.as_ref() {
                        let dev = w.device.clone();
                        let (cdev, prev_ls) = {
                            let c = scan_cache.lock().unwrap();
                            (c.0.clone(), c.1)
                        };
                        if dev != cdev {
                            // Card switched — force a fresh read.
                            if let Ok(Some((ls, new_list))) = WifiScanner::scan_list_if_newer(Some(&dev), 0) {
                                *scan_cache.lock().unwrap() = (dev, ls, new_list.clone());
                                list = new_list;
                                list_refreshed = true;
                            }
                        } else if let Ok(Some((ls, new_list))) = WifiScanner::scan_list_if_newer(Some(&dev), prev_ls) {
                            *scan_cache.lock().unwrap() = (dev, ls, new_list.clone());
                            list = new_list;
                            list_refreshed = true;
                        } else {
                            list = scan_cache.lock().unwrap().2.clone();
                        }
                    }
                    // Channel width + center frequency (best effort, native
                    // netlink): the active AP from the interface query (every
                    // tick), the list entries from the scan cache (only when
                    // the list was refreshed).
                    if let Some(w) = info.as_ref() {
                        if let Some(ic) = nl80211::interface_channel(&w.device) {
                            info = Some(WifiInfo {
                                channel_width_mhz: ic.channel.width_mhz,
                                center_freq_mhz: ic.channel.center_freq_mhz,
                                center_freq2_mhz: ic.channel.center_freq2_mhz,
                                ..w.clone()
                            });
                            if list_refreshed {
                                merge_nl_scan(&mut list, &nl80211::scan_channels(ic.ifindex));
                            }
                        }
                    }
                    let _ = tx.send_blocking((cards, info, list));
                });
            }
            glib::spawn_future_local(async move {
                let Ok((cards, info, list)) = rx.recv().await else { return };
                match info {
                    Some(w) => {
                        p.refresh_live_signal(&w.ssid, &w.bssid, w.signal_dbm, w.frequency_mhz, w.channel, &w.device,
                            w.channel_width_mhz, w.center_freq_mhz, w.center_freq2_mhz, Some(&list));
                        // Legend pointer value follows the active signal source.
                        let legend_dbm = match &source {
                            SignalSource::ConnectedAp => Some(w.signal_dbm as f64),
                            SignalSource::Ssid(ssid) => {
                                let target = if ssid.is_empty() { w.ssid.as_str() } else { ssid.as_str() };
                                list.iter()
                                    .filter(|e| e.ssid == target)
                                    .map(|e| e.signal_dbm)
                                    .max()
                                    .map(|v| v as f64)
                                    .or(if ssid.is_empty() { Some(w.signal_dbm as f64) } else { None })
                            }
                            SignalSource::Bssid(b) => list
                                .iter()
                                .find(|e| &e.bssid == b)
                                .map(|e| e.signal_dbm as f64),
                        };
                        lg.set_current_signal(legend_dbm);
                        // Show the card selector when more than one card is active.
                        let opts: Vec<String> = cards.iter().map(|c| c.device.clone()).collect();
                        p.set_card_selector(&opts, Some(w.device.as_str()));
                        // Warn (on transition) if the preferred card is no longer active.
                        let pref_missing = preferred.as_ref()
                            .map(|pn| !cards.iter().any(|c| &c.device == pn))
                            .unwrap_or(false);
                        if pref_missing && !*pref.borrow() {
                            *pref.borrow_mut() = true;
                            if let Some(pn) = preferred.as_ref() {
                                ov.add_toast(Toast::new(&format!("Card {pn} unavailable — using {}", w.device)));
                            }
                        } else if !pref_missing {
                            *pref.borrow_mut() = false;
                        }
                        // Warn if we're on a network not yet measured on this floor.
                        let known = known_ssids(&st);
                        let msg = if !known.is_empty() && !known.contains(&w.ssid) {
                            Some(format_new_network_warning(&w.ssid, &known))
                        } else {
                            None
                        };
                        p.set_network_warning(msg.clone());
                        match msg {
                            Some(ref m) if !*off.borrow() => {
                                *off.borrow_mut() = true;
                                ov.add_toast(Toast::new(m));
                            }
                            None => *off.borrow_mut() = false,
                            _ => {}
                        }
                    }
                    None => {
                        p.set_no_wifi();
                        lg.set_current_signal(None);
                        p.set_network_warning(None);
                        *off.borrow_mut() = false;
                        *pref.borrow_mut() = false;
                    }
                }
            });
            glib::ControlFlow::Continue
        });
    }

    // Delete measurement
    {
        let state = state.clone();
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let panel2 = panel.clone();
        let legend = legend.clone();
        let rebuild = rebuild_source_dd.clone();
        panel.set_on_delete(move |id| {
            let was_selected = fp.get_selected_measurement().as_deref() == Some(id.as_str());
            let measurements = {
                let mut s = state.borrow_mut();
                let idx = s.current_floor;
                if let Some(floor) = s.project.floors.get_mut(idx) {
                    floor.remove_measurement(&id);
                    floor.measurements.clone()
                } else {
                    return;
                }
            };
            if was_selected {
                fp.set_selected_measurement(None);
            }
            fp.set_measurements(measurements.clone());
            legend.set_measurements(&measurements);
            panel2.set_measurements(measurements);
            if was_selected {
                panel2.set_selected_by_id(None);
            }
            auto_save(&fp, &state);
            rebuild();
        });
    }

    // Delete all measurements (triggered from panel's trash button)
    {
        let state = state.clone();
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let overlay_ref = overlay.clone();
        let window_ref = window.clone();
        let panel_ref = panel.clone();
        let legend = legend.clone();
        let rebuild = rebuild_source_dd.clone();
        panel_ref.set_on_delete_all(move || {
            let n_floors = state.borrow().project.floors.len();
            let dialog = MessageDialog::builder()
                .heading("Delete All Measurements")
                .body(if n_floors > 1 {
                    "Delete measurements on the current floor, or on all floors?"
                } else {
                    "Delete all measurements on this floor?"
                })
                .default_response("cancel")
                .close_response("cancel")
                .transient_for(&window_ref)
                .modal(true)
                .build();
            dialog.add_response("cancel", "Cancel");
            if n_floors > 1 {
                dialog.add_response("all", "All Floors");
                dialog.set_response_appearance("all", libadwaita::ResponseAppearance::Destructive);
            }
            dialog.add_response("current", "This Floor");
            dialog.set_response_appearance("current", libadwaita::ResponseAppearance::Destructive);

            let state2 = state.clone();
            let fp2 = fp.clone();
            let panel2 = panel.clone();
            let overlay2 = overlay_ref.clone();
            let legend2 = legend.clone();
            // `rebuild` is captured by the outer Fn closure — clone it before
            // the one-shot continuation closure is created.
            let rebuild2 = rebuild.clone();
            dialog.choose(gtk4::gio::Cancellable::NONE, move |response| {
                match response.as_str() {
                    "current" => {
                        let idx = state2.borrow().current_floor;
                        {
                            let mut s = state2.borrow_mut();
                            if let Some(floor) = s.project.floors.get_mut(idx) {
                                floor.measurements.clear();
                            }
                        }
                        fp2.set_measurements(vec![]);
                        legend2.set_measurements(&[]);
                        panel2.set_measurements(vec![]);
                        fp2.set_selected_measurement(None);
                        panel2.set_selected_by_id(None);
                        legend2.set_selected_measurement(None);
                        panel2.set_network_warning(None);
                        auto_save(&fp2, &state2);
                        rebuild2();
                        overlay2.add_toast(Toast::new("Measurements deleted"));
                    }
                    "all" => {
                        {
                            let mut s = state2.borrow_mut();
                            for floor in s.project.floors.iter_mut() {
                                floor.measurements.clear();
                            }
                        }
                        fp2.set_measurements(vec![]);
                        legend2.set_measurements(&[]);
                        panel2.set_measurements(vec![]);
                        fp2.set_selected_measurement(None);
                        panel2.set_selected_by_id(None);
                        legend2.set_selected_measurement(None);
                        panel2.set_network_warning(None);
                        auto_save(&fp2, &state2);
                        rebuild2();
                        overlay2.add_toast(Toast::new("All measurements deleted"));
                    }
                    _ => {}
                }
            });
        });
    }

    // Add floor
    {
        let state = state.clone();
        let floor_model = floor_model.clone();
        let floor_dropdown = floor_dropdown.clone();
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let overlay_ref = overlay.clone();
        add_floor_btn.connect_clicked(move |_| {
            // Save current floor before switching
            auto_save(&fp, &state);

            let (name, new_idx) = {
                let mut s = state.borrow_mut();
                let name = format!("Floor {}", s.project.floors.len() + 1);
                s.project.add_floor(Floor::new(&name));
                s.current_floor = s.project.floors.len() - 1;
                (name, s.current_floor)
            };

            floor_model.append(&name);
            if floor_dropdown.selected() as usize == new_idx {
                fp.set_measurements(vec![]);
                fp.set_image("");
                panel.set_measurements(vec![]);
            } else {
                floor_dropdown.set_selected(new_idx as u32);
            }
            auto_save(&fp, &state);
            overlay_ref.add_toast(Toast::new(&format!("Added: {name}")));
        });
    }

    // Shared flag: suppress floor_dropdown's selected_notify during programmatic changes
    let suppress_floor_change = Rc::new(std::cell::Cell::new(false));

    // Edit floor (rename / delete)
    {
        let state = state.clone();
        let floor_model = floor_model.clone();
        let floor_dropdown = floor_dropdown.clone();
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let overlay_ref = overlay.clone();
        let window_ref = window.clone();
        let suppress = suppress_floor_change.clone();
        let legend = legend.clone();
        edit_floor_btn.connect_clicked(move |_| {
            let (current_name, n_floors, current_idx) = {
                let s = state.borrow();
                (
                    s.project.floors[s.current_floor].name.clone(),
                    s.project.floors.len(),
                    s.current_floor,
                )
            };

            let dialog = MessageDialog::builder()
                .heading("Edit Floor")
                .body(if n_floors > 1 { "Rename this floor or delete it." }
                      else            { "Rename this floor." })
                .default_response("rename")
                .close_response("cancel")
                .transient_for(&window_ref)
                .modal(true)
                .build();
            dialog.add_response("cancel", "Cancel");
            if n_floors > 1 {
                dialog.add_response("delete", "Delete Floor");
                dialog.set_response_appearance("delete", libadwaita::ResponseAppearance::Destructive);
            }
            dialog.add_response("rename", "Rename");
            dialog.set_response_appearance("rename", libadwaita::ResponseAppearance::Suggested);

            let entry = gtk4::Entry::builder()
                .text(&current_name)
                .activates_default(true)
                .build();
            dialog.set_extra_child(Some(&entry));

            let state2        = state.clone();
            let floor_model2  = floor_model.clone();
            let floor_dd2     = floor_dropdown.clone();
            let fp2           = fp.clone();
            let panel2        = panel.clone();
            let overlay2      = overlay_ref.clone();
            let entry2        = entry.clone();
            let suppress2     = suppress.clone();
            let legend2       = legend.clone();

            dialog.choose(gtk4::gio::Cancellable::NONE, move |response| {
                match response.as_str() {
                    "rename" => {
                        let new_name = entry2.text().trim().to_string();
                        if new_name.is_empty() { return; }
                        state2.borrow_mut().project.floors[current_idx].name = new_name.clone();
                        floor_model2.splice(current_idx as u32, 1, &[new_name.as_str()]);
                        auto_save(&fp2, &state2);
                        overlay2.add_toast(Toast::new(&format!("Renamed to \"{new_name}\"")));
                    }
                    "delete" => {
                        // Which floor to show after deletion
                        let new_idx = if current_idx + 1 < n_floors { current_idx } else { current_idx - 1 };

                        // Remove drawing file from disk
                        if let Some(path) = state2.borrow().project.floors[current_idx].drawing_path.clone() {
                            let _ = std::fs::remove_file(&path);
                        }

                        // Update model state
                        {
                            let mut s = state2.borrow_mut();
                            s.project.floors.remove(current_idx);
                            s.current_floor = new_idx;
                        }

                        // Update the dropdown without triggering selected_notify side-effects
                        suppress2.set(true);
                        floor_model2.remove(current_idx as u32);
                        floor_dd2.set_selected(new_idx as u32);
                        suppress2.set(false);

                        // Load the replacement floor into the UI
                        let floor = state2.borrow().project.floors[new_idx].clone();
                        load_floor_into_view(&fp2, &legend2, &panel2, &floor);

                        auto_save(&fp2, &state2);
                        overlay2.add_toast(Toast::new("Floor deleted"));
                    }
                    _ => {}
                }
            });
        });
    }

    // Floor dropdown
    {
        let state = state.clone();
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let suppress = suppress_floor_change.clone();
        let legend = legend.clone();
        let settings = settings.clone();
        floor_dropdown.connect_selected_notify(move |dd| {
            if suppress.get() { return; }
            let new_idx = dd.selected() as usize;
            // Auto-save the floor being left
            auto_save(&fp, &state);

            let floor = {
                let mut s = state.borrow_mut();
                if new_idx >= s.project.floors.len() { return; }
                s.current_floor = new_idx;
                settings.borrow_mut().last_floor_index = new_idx;
                let _ = SettingsStore::save(&settings.borrow());
                s.project.floors[new_idx].clone()
            };
            load_floor_into_view(&fp, &legend, &panel, &floor);
        });
    }

    // Import floor plan image
    {
        let state = state.clone();
        let fp = floor_plan.clone();
        let overlay_ref = overlay.clone();
        let window_ref = window.clone();
        import_btn.connect_clicked(move |_| {
            let dialog = FileDialog::builder().title("Import Floor Plan").modal(true).build();
            let filter = gtk4::FileFilter::new();
            filter.add_mime_type("image/png");
            filter.add_mime_type("image/jpeg");
            filter.add_mime_type("application/pdf");
            filter.set_name(Some("Images & PDF (PNG, JPG, PDF)"));
            let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
            filters.append(&filter);
            dialog.set_filters(Some(&filters));

            let state2 = state.clone();
            let fp2 = fp.clone();
            let overlay2 = overlay_ref.clone();
            let window_ref2 = window_ref.clone();
            dialog.open(Some(&window_ref), gtk4::gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        let path_str = path.to_string_lossy().to_string();
                        if path_str.to_lowercase().ends_with(".pdf") {
                            // Count pages; if > 1 show page-picker dialog
                            match poppler::PopplerDocument::new_from_file(&path_str, None) {
                                Err(e) => {
                                    log::warn!("Cannot open PDF {path_str}: {e}");
                                    overlay2.add_toast(Toast::new("Failed to open PDF"));
                                }
                                Ok(doc) => {
                                    let n_pages = doc.get_n_pages();
                                    if n_pages <= 1 {
                                        import_pdf_page(&state2, &fp2, &overlay2, path_str, 0);
                                    } else {
                                        show_pdf_page_picker(&window_ref2, &state2, &fp2, &overlay2, path_str, n_pages);
                                    }
                                }
                            }
                        } else {
                            let mut s = state2.borrow_mut();
                            let idx = s.current_floor;
                            if let Some(floor) = s.project.floors.get_mut(idx) {
                                floor.image_path = Some(path_str.clone());
                                floor.pdf_page = None;
                            }
                            drop(s);
                            fp2.set_image(&path_str);
                            auto_save(&fp2, &state2);
                            overlay2.add_toast(Toast::new("Floor plan imported"));
                        }
                    }
                }
            });
        });
    }

    // Zoom buttons
    {
        let fp = floor_plan.clone();
        zoom_in_btn.connect_clicked(move |_| { fp.zoom_in(); });
    }
    {
        let fp = floor_plan.clone();
        zoom_out_btn.connect_clicked(move |_| { fp.zoom_out(); });
    }
    {
        let fp = floor_plan.clone();
        zoom_reset_btn.connect_clicked(move |_| { fp.reset_zoom(); });
    }

    // Settings button
    {
        let settings = settings.clone();
        let window_ref = window.clone();
        let fp = floor_plan.clone();
        let grid_toggle = grid_toggle.clone();
        let origin_toggle = origin_toggle.clone();
        let scale_toggle = scale_toggle.clone();
        let panel_ref = panel.clone();
        let legend_ref = legend.clone();
        settings_btn.connect_clicked(move |_| {
            let dlg = SettingsDialog::new(&window_ref, settings.clone());
            let fp2 = fp.clone();
            let grid_toggle2 = grid_toggle.clone();
            let origin_toggle2 = origin_toggle.clone();
            let scale_toggle2 = scale_toggle.clone();
            let settings2 = settings.clone();
            let panel2 = panel_ref.clone();
            let legend2 = legend_ref.clone();
            dlg.window.connect_close_request(move |_| {
                let s = settings2.borrow();
                fp2.set_show_grid(s.show_grid);
                fp2.set_grid_spacing(s.grid_spacing_m);
                fp2.set_measurement_grid_spacing(s.measurement_grid_spacing_m);
                fp2.set_snap_to_grid(s.snap_to_grid);
                fp2.set_show_origin(s.show_origin);
                fp2.set_show_scale(s.show_scale);
                fp2.set_color_metric(s.color_metric);
                grid_toggle2.set_active(s.show_grid);
                origin_toggle2.set_active(s.show_origin);
                scale_toggle2.set_active(s.show_scale);
                panel2.set_throughput_unit(s.throughput_unit);
                legend2.set_color_metric(s.color_metric);
                gtk4::glib::Propagation::Proceed
            });
            dlg.window.present();
        });
    }

    // Signal source dropdown (user selection)
    {
        let settings = settings.clone();
        let fp = floor_plan.clone();
        let legend = legend.clone();
        let panel = panel.clone();
        let suppress = suppress_source_change.clone();
        source_dd.connect_selected_notify(move |dd| {
            if suppress.get() { return; }
            let idx = dd.selected() as usize;
            let src: SignalSource = source_options
                .borrow()
                .get(idx)
                .cloned()
                .unwrap_or(SignalSource::ConnectedAp);
            settings.borrow_mut().signal_source = src.clone();
            let _ = SettingsStore::save(&settings.borrow());
            fp.set_signal_source(src.clone());
            legend.set_signal_source(src.clone());
            panel.set_signal_source(src.clone());
        });
    }

    // Known APs dialog (manage BSSID aliases)
    {
        let window_ref = window.clone();
        let state = state.clone();
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let overlay_ref = overlay.clone();
        let rebuild = rebuild_source_dd.clone();
        known_aps_btn.connect_clicked(move |_| {
            show_known_aps_dialog(&window_ref, &state, &fp, &panel, &overlay_ref, &rebuild);
        });
    }

    // Channel report (per-point + site-survey views of recorded scans).
    // Non-modal; shares the floor-plan measurement selection with the
    // main window.
    {
        let window_ref = window.clone();
        let state2 = state.clone();
        let fp = floor_plan.clone();
        let panel = panel.clone();
        let legend = legend.clone();
        let report_ref = report_ref.clone();
        channel_report_btn.connect_clicked(move |_| {
            let (floor_name, measurements, aliases) = {
                let s = state2.borrow();
                let floor = &s.project.floors[s.current_floor];
                (
                    floor.name.clone(),
                    floor.measurements.clone(),
                    s.project.bssid_aliases.clone(),
                )
            };
            let fp2 = fp.clone();
            let legend2 = legend.clone();
            let panel2 = panel.clone();
            let state2c = Rc::clone(&state2);
            let on_select: Box<dyn Fn(Option<String>)> = Box::new(move |id: Option<String>| {
                if let Some(id) = id {
                    fp2.set_selected_measurement(Some(id.clone()));
                    legend2.set_selected_measurement(find_measurement(&state2c, &id));
                    panel2.set_selected_by_id(Some(id));
                }
            });
            let win = crate::widgets::channel_report::show_channel_report(
                &window_ref,
                &floor_name,
                measurements,
                aliases,
                on_select,
            );
            let rr = report_ref.clone();
            *report_ref.borrow_mut() = Some(win.clone());
            win.connect_destroy(move |_| {
                rr.borrow_mut().take();
            });
        });
    }

    // ── Project menu actions ────────────────────────────────────────────────
    {
        let win_actions = gtk4::gio::SimpleActionGroup::new();
        let window_ref = window.clone();

        // New project: keep the current one (it is auto-saved) and start fresh
        let act_new = gtk4::gio::SimpleAction::new("new-project", None);
        {
            let state = state.clone();
            let fp = floor_plan.clone();
            let legend = legend.clone();
            let panel = panel.clone();
            let floor_model = floor_model.clone();
            let floor_dd = floor_dropdown.clone();
            let suppress = suppress_floor_change.clone();
            let settings = settings.clone();
            let overlay_ref = overlay.clone();
            let window_ref2 = window_ref.clone();
            let rebuild_source_dd = rebuild_source_dd.clone();
            let project_menu2 = project_menu.clone();
            act_new.connect_activate(move |_, _| {
                let dialog = MessageDialog::builder()
                    .heading("Start New Project")
                    .body("The current project is saved at its current location and can be reopened from there.\n\nStart a new, empty project?")
                    .default_response("new")
                    .close_response("cancel")
                    .transient_for(&window_ref2)
                    .modal(true)
                    .build();
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("new", "New Project");
                dialog.set_response_appearance("new", libadwaita::ResponseAppearance::Suggested);

                let state2 = state.clone();
                let fp2 = fp.clone();
                let legend2 = legend.clone();
                let panel2 = panel.clone();
                let fm2 = floor_model.clone();
                let dd2 = floor_dd.clone();
                let suppress2 = suppress.clone();
                let settings2 = settings.clone();
                let overlay2 = overlay_ref.clone();
                let window_ref3 = window_ref2.clone();
                let rebuild2 = rebuild_source_dd.clone();
                let project_menu3 = project_menu2.clone();
                dialog.choose(gtk4::gio::Cancellable::NONE, move |response| {
                    if response.as_str() != "new" { return; }
                    // The current project stays saved at its current path
                    auto_save(&fp2, &state2);
                    let mut project = Project::new("New Project");
                    project.add_floor(Floor::new("Floor 1"));
                    let name = apply_project(
                        &state2, &fp2, &legend2, &panel2, &fm2, &dd2, &suppress2, &settings2, &rebuild2,
                        project, JsonStore::default_path(),
                    );
                    window_ref3.set_title(Some(&format!("{name} — WiFi Checker")));
                    overlay2.add_toast(Toast::new("New project started"));
                    rebuild_project_menu(&project_menu3, &settings2);
                });
            });
        }
        ActionMapExt::add_action(&win_actions, &act_new);

        // Open project: load a project file, replacing the current one
        let act_open = gtk4::gio::SimpleAction::new("open-project", None);
        {
            let state = state.clone();
            let fp = floor_plan.clone();
            let legend = legend.clone();
            let panel = panel.clone();
            let floor_model = floor_model.clone();
            let floor_dd = floor_dropdown.clone();
            let suppress = suppress_floor_change.clone();
            let settings = settings.clone();
            let overlay_ref = overlay.clone();
            let window_ref2 = window_ref.clone();
            let rebuild_source_dd = rebuild_source_dd.clone();
            let project_menu2 = project_menu.clone();
            act_open.connect_activate(move |_, _| {
                let dialog = FileDialog::builder().title("Open Project").modal(true).build();
                let filter = gtk4::FileFilter::new();
                filter.add_suffix("json");
                filter.set_name(Some("WiFi Checker projects (JSON)"));
                let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));

                let state2 = state.clone();
                let fp2 = fp.clone();
                let legend2 = legend.clone();
                let panel2 = panel.clone();
                let fm2 = floor_model.clone();
                let dd2 = floor_dd.clone();
                let suppress2 = suppress.clone();
                let settings2 = settings.clone();
                let overlay2 = overlay_ref.clone();
                let window_ref3 = window_ref2.clone();
                let dialog_parent = window_ref2.clone();
                let rebuild2 = rebuild_source_dd.clone();
                let project_menu3 = project_menu2.clone();
                dialog.open(Some(&dialog_parent), gtk4::gio::Cancellable::NONE, move |result| {
                    let Ok(file) = result else { return; };
                    let Some(path) = file.path() else { return; };

                    let project = match JsonStore::load(&path) {
                        Ok(p) => p,
                        Err(e) => {
                            overlay2.add_toast(Toast::new(&format!("Failed to open project: {e}")));
                            return;
                        }
                    };

                    // Persist the project we are about to replace
                    auto_save(&fp2, &state2);
                    let name = apply_project(
                        &state2, &fp2, &legend2, &panel2, &fm2, &dd2, &suppress2, &settings2, &rebuild2,
                        project, path,
                    );
                    window_ref3.set_title(Some(&format!("{name} — WiFi Checker")));
                    overlay2.add_toast(Toast::new(&format!("Opened: {name}")));
                    rebuild_project_menu(&project_menu3, &settings2);
                });
            });
        }
        ActionMapExt::add_action(&win_actions, &act_open);

        // Open a recent project by index into the settings' recent list.
        let act_recent = gtk4::gio::SimpleAction::new("open-recent", Some(&glib::VariantTy::STRING));
        {
            let state = state.clone();
            let fp = floor_plan.clone();
            let legend = legend.clone();
            let panel = panel.clone();
            let floor_model = floor_model.clone();
            let floor_dd = floor_dropdown.clone();
            let suppress = suppress_floor_change.clone();
            let settings = settings.clone();
            let overlay_ref = overlay.clone();
            let window_ref2 = window_ref.clone();
            let rebuild_source_dd = rebuild_source_dd.clone();
            let project_menu2 = project_menu.clone();
            act_recent.connect_activate(move |_action, param| {
                let Some(idx) = param
                    .and_then(|p| p.str())
                    .and_then(|s| s.parse::<usize>().ok())
                else { return };
                let Some(path_str) = settings.borrow().recent_projects.get(idx).cloned() else {
                    overlay_ref.add_toast(Toast::new("This project is no longer in the recent list"));
                    return;
                };
                let path = std::path::PathBuf::from(path_str);
                let project = match JsonStore::load(&path) {
                    Ok(p) => p,
                    Err(e) => {
                        overlay_ref.add_toast(Toast::new(&format!("Failed to open project: {e}")));
                        return;
                    }
                };
                auto_save(&fp, &state);
                let name = apply_project(
                    &state, &fp, &legend, &panel, &floor_model, &floor_dd, &suppress, &settings, &rebuild_source_dd,
                    project, path,
                );
                window_ref2.set_title(Some(&format!("{name} — WiFi Checker")));
                overlay_ref.add_toast(Toast::new(&format!("Opened: {name}")));
                rebuild_project_menu(&project_menu2, &settings);
            });
        }
        ActionMapExt::add_action(&win_actions, &act_recent);

        // Save project as: write the project (with its drawings) to a new file
        // and switch auto-save to that location
        let act_save = gtk4::gio::SimpleAction::new("save-project-as", None);
        {
            let state = state.clone();
            let fp = floor_plan.clone();
            let settings = settings.clone();
            let project_menu2 = project_menu.clone();
            let overlay_ref = overlay.clone();
            let window_ref2 = window_ref.clone();
            act_save.connect_activate(move |_, _| {
                let dialog = FileDialog::builder().title("Save Project As").modal(true).build();
                let filter = gtk4::FileFilter::new();
                filter.add_suffix("json");
                filter.set_name(Some("WiFi Checker projects (JSON)"));
                let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));

                let state2 = state.clone();
                let fp2 = fp.clone();
                let settings2 = settings.clone();
                let project_menu3 = project_menu2.clone();
                let overlay2 = overlay_ref.clone();
                let dialog_parent = window_ref2.clone();
                dialog.save(Some(&dialog_parent), gtk4::gio::Cancellable::NONE, move |result| {
                    let Ok(file) = result else { return; };
                    let Some(mut path) = file.path() else { return; };
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        let stem = path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "project".to_string());
                        path.set_file_name(format!("{stem}.json"));
                    }

                    let project = state2.borrow().project.clone();
                    // Copy the project's drawing files next to the new project file
                    let project = independent_project_copy(&project, &drawings_dir_for(&path));

                    match JsonStore::save(&project, &path) {
                        Ok(()) => {
                            // From now on auto-save goes to the new location
                            state2.borrow_mut().project_path = path.clone();
                            auto_save(&fp2, &state2);
                            remember_project_path(&path, &settings2);
                            rebuild_project_menu(&project_menu3, &settings2);
                            overlay2.add_toast(Toast::new(&format!("Project saved to {}", path.display())));
                        }
                        Err(e) => {
                            overlay2.add_toast(Toast::new(&format!("Failed to save project: {e}")));
                        }
                    }
                });
            });
        }
        ActionMapExt::add_action(&win_actions, &act_save);

        window.insert_action_group("win", Some(&win_actions));
    }

    // ── Initialize from loaded project ────────────────────────────────────────
    {
        // Collect all data needed, then release borrow before touching the model
        let (floor_names, start_floor, start_idx) = {
            let mut s = state.borrow_mut();
            if s.project.floors.is_empty() {
                s.project.add_floor(Floor::new("Floor 1"));
            }
            let last_idx = settings.borrow().last_floor_index;
            let start_idx = if last_idx < s.project.floors.len() { last_idx } else { 0 };
            s.current_floor = start_idx;
            let names: Vec<String> = s.project.floors.iter().map(|f| f.name.clone()).collect();
            let start = s.project.floors.get(start_idx).cloned();
            (names, start, start_idx)
        }; // state borrow fully released here

        // Suppress all notifications during bulk initialization to prevent premature
        // selected_notify firing (GTK auto-selects index 0 on first append).
        suppress_floor_change.set(true);
        for name in &floor_names {
            floor_model.append(name);
        }
        floor_dropdown.set_selected(start_idx as u32);
        suppress_floor_change.set(false);

        // Load restored floor into view
        if let Some(floor) = start_floor {
            load_floor_into_view(&floor_plan, &legend, &panel, &floor);
        }
        // Apply the saved signal source and project aliases to the UI.
        {
            let (aliases, src) = {
                let s = state.borrow();
                (s.project.bssid_aliases.clone(), settings.borrow().signal_source.clone())
            };
            panel.set_aliases(aliases.clone());
            panel.set_signal_source(src.clone());
            floor_plan.set_signal_source(src.clone());
            legend.set_signal_source(src);
        }
        rebuild_source_dd();
    }

    // Auto-save if we just created the default floor
    auto_save(&floor_plan, &state);
    // Apply saved settings to panel
    panel.set_throughput_unit(settings.borrow().throughput_unit);

    overlay
}

fn import_pdf_page(
    state: &Rc<RefCell<AppState>>,
    fp: &FloorPlanView,
    overlay: &libadwaita::ToastOverlay,
    path_str: String,
    page_idx: u32,
) {
    {
        let mut s = state.borrow_mut();
        let idx = s.current_floor;
        if let Some(floor) = s.project.floors.get_mut(idx) {
            floor.image_path = Some(path_str.clone());
            floor.pdf_page = Some(page_idx);
        }
    }
    fp.set_pdf(&path_str, page_idx);
    auto_save(fp, state);
    overlay.add_toast(libadwaita::Toast::new("Floor plan imported"));
}

fn show_pdf_page_picker(
    parent: &libadwaita::ApplicationWindow,
    state: &Rc<RefCell<AppState>>,
    fp: &FloorPlanView,
    overlay: &libadwaita::ToastOverlay,
    path_str: String,
    n_pages: usize,
) {
    use gtk4::prelude::*;

    let dialog = gtk4::Window::builder()
        .title("Select PDF Page")
        .transient_for(parent)
        .modal(true)
        .default_width(320)
        .resizable(false)
        .build();

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(20);
    vbox.set_margin_start(20);
    vbox.set_margin_end(20);

    let label = gtk4::Label::new(Some(&format!(
        "This PDF has {n_pages} pages.\nSelect which page to use as floor plan:"
    )));
    label.set_halign(gtk4::Align::Start);
    vbox.append(&label);

    let spin = gtk4::SpinButton::with_range(1.0, n_pages as f64, 1.0);
    spin.set_value(1.0);
    vbox.append(&spin);

    let btn_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk4::Align::End);

    let cancel_btn = gtk4::Button::with_label("Cancel");
    let ok_btn = gtk4::Button::with_label("Import");
    ok_btn.add_css_class("suggested-action");

    btn_row.append(&cancel_btn);
    btn_row.append(&ok_btn);
    vbox.append(&btn_row);

    dialog.set_child(Some(&vbox));

    // Cancel
    {
        let dialog_weak = dialog.downgrade();
        cancel_btn.connect_clicked(move |_| {
            if let Some(d) = dialog_weak.upgrade() { d.close(); }
        });
    }

    // OK
    {
        let dialog_weak = dialog.downgrade();
        let state = state.clone();
        let fp = fp.clone();
        let overlay = overlay.clone();
        ok_btn.connect_clicked(move |_| {
            let page_idx = (spin.value() as u32).saturating_sub(1);
            import_pdf_page(&state, &fp, &overlay, path_str.clone(), page_idx);
            if let Some(d) = dialog_weak.upgrade() { d.close(); }
        });
    }

    dialog.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("wifichecker_window_test_{}_{}", std::process::id(), tag))
    }

    #[test]
    fn test_independent_copy_migrates_drawings() {
        let dir = temp_dir("migrate");
        let src_dir = dir.join("src_drawings");
        let dst_dir = dir.join("dst_drawings");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(src_dir.join("floor_0.png"), b"fake-png").unwrap();

        let mut floor0 = Floor::new("L0");
        floor0.drawing_path = Some(src_dir.join("floor_0.png").to_string_lossy().to_string());
        let mut project = Project::new("Test");
        project.add_floor(floor0);
        project.add_floor(Floor::new("L1"));

        let out = independent_project_copy(&project, &dst_dir);
        let expected = dst_dir.join("floor_0.png").to_string_lossy().to_string();
        assert_eq!(out.floors[0].drawing_path.as_deref(), Some(expected.as_str()));
        assert!(dst_dir.join("floor_0.png").exists());
        // Floor without a drawing is untouched
        assert!(out.floors[1].drawing_path.is_none());

        // Idempotent: a second run keeps the reference stable
        let out2 = independent_project_copy(&out, &dst_dir);
        assert_eq!(out2.floors[0].drawing_path, out.floors[0].drawing_path);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_independent_copy_missing_source_keeps_reference() {
        let dir = temp_dir("missing");
        let dst_dir = dir.join("dst_drawings");
        std::fs::create_dir_all(&dst_dir).unwrap();

        let mut floor0 = Floor::new("L0");
        floor0.drawing_path = Some(dir.join("gone").join("floor_0.png").to_string_lossy().to_string());
        let mut project = Project::new("Test");
        project.add_floor(floor0);

        let out = independent_project_copy(&project, &dst_dir);
        assert_eq!(out.floors[0].drawing_path, project.floors[0].drawing_path);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
