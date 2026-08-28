use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::models::Floor;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub floors: Vec<Floor>,
    /// Human-readable alias names for BSSIDs seen in this project's
    /// measurements (e.g. "AA:BB:…" → "Office-AP-1"). Stored per project so
    /// aliases travel with the project file.
    #[serde(default)]
    pub bssid_aliases: HashMap<String, String>,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            floors: Vec::new(),
            bssid_aliases: HashMap::new(),
        }
    }

    pub fn add_floor(&mut self, floor: Floor) {
        self.floors.push(floor);
    }

    pub fn remove_floor(&mut self, index: usize) {
        if index < self.floors.len() {
            self.floors.remove(index);
        }
    }

    /// The alias for a BSSID, if one has been set.
    pub fn alias_for(&self, bssid: &str) -> Option<&str> {
        self.bssid_aliases.get(bssid).map(|s| s.as_str())
    }

    /// Display name for an AP: the alias when set, otherwise the BSSID.
    pub fn ap_label(&self, bssid: &str) -> String {
        self.alias_for(bssid).unwrap_or(bssid).to_string()
    }

    /// Set (or clear, when `alias` is empty/None) the alias for a BSSID.
    pub fn set_alias(&mut self, bssid: &str, alias: Option<String>) {
        match alias {
            Some(a) if !a.trim().is_empty() => {
                self.bssid_aliases.insert(bssid.to_string(), a.trim().to_string());
            }
            _ => {
                self.bssid_aliases.remove(bssid);
            }
        }
    }

    /// All distinct BSSIDs seen anywhere in this project (active APs and scan
    /// lists), in first-seen order. Used to build the "Known APs" list.
    pub fn known_bssids(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        let mut push = |v: &mut Vec<String>, b: &str| {
            if !b.is_empty() && !v.iter().any(|x| x == b) {
                v.push(b.to_string());
            }
        };
        for floor in &self.floors {
            for m in &floor.measurements {
                if m.no_signal {
                    continue;
                }
                push(&mut v, &m.bssid);
                for e in &m.scan_results {
                    push(&mut v, &e.bssid);
                }
            }
        }
        v
    }

    /// The SSIDs that have saved measurements (each measurement's connected
    /// SSID), in first-seen order. This is the set of networks the
    /// signal-source dropdown offers.
    pub fn measured_ssids(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for floor in &self.floors {
            for m in &floor.measurements {
                if !m.no_signal && !m.ssid.is_empty() && !v.contains(&m.ssid) {
                    v.push(m.ssid.clone());
                }
            }
        }
        v
    }

    /// The BSSIDs seen in scan lists broadcasting `ssid`, in first-seen
    /// order (scan lists are strongest-first, so this is ≈ strongest-first).
    pub fn bssids_of_ssid(&self, ssid: &str) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for floor in &self.floors {
            for m in &floor.measurements {
                for e in &m.scan_results {
                    if e.ssid == ssid && !v.contains(&e.bssid) {
                        v.push(e.bssid.clone());
                    }
                }
            }
        }
        v
    }

    /// (SSID, BSSIDs) for each SSID that has saved measurements, in
    /// first-seen order. Besides the SSID's scan-list BSSIDs this includes
    /// the connected AP of its measurements (for old measurements without a
    /// scan list). Used by the Known APs dialog's first section.
    pub fn measured_ap_sections(&self) -> Vec<(String, Vec<String>)> {
        let mut sections: Vec<(String, Vec<String>)> = Vec::new();
        let mut claimed: Vec<String> = Vec::new();
        for ssid in self.measured_ssids() {
            let mut bssids = self.bssids_of_ssid(&ssid);
            for floor in &self.floors {
                for m in &floor.measurements {
                    if !m.no_signal
                        && m.ssid == ssid
                        && !m.bssid.is_empty()
                        && !bssids.contains(&m.bssid)
                        && !claimed.contains(&m.bssid)
                    {
                        bssids.push(m.bssid.clone());
                    }
                }
            }
            // A BSSID can (rarely) broadcast two measured SSIDs — show it
            // only under the first one.
            bssids.retain(|b| !claimed.contains(b));
            for b in &bssids {
                claimed.push(b.clone());
            }
            sections.push((ssid, bssids));
        }
        sections
    }

    /// BSSIDs that appear only in scan lists of networks that have no
    /// measurements (the "other networks" section of the Known APs dialog).
    pub fn unmeasured_bssids(&self) -> Vec<String> {
        let mut claimed: Vec<String> = Vec::new();
        for (_, bssids) in self.measured_ap_sections() {
            claimed.extend(bssids);
        }
        self.known_bssids()
            .into_iter()
            .filter(|b| !claimed.contains(b))
            .collect()
    }

    /// The last SSID seen for a BSSID in this project (used as a hint in the
    /// Known APs dialog). `None` if the BSSID was never seen (e.g. hidden SSID).
    pub fn last_ssid_for(&self, bssid: &str) -> Option<String> {
        for floor in self.floors.iter().rev() {
            for m in floor.measurements.iter().rev() {
                if m.no_signal {
                    continue;
                }
                if m.bssid == bssid && !m.ssid.is_empty() {
                    return Some(m.ssid.clone());
                }
                if let Some(e) = m.scan_results.iter().find(|e| e.bssid == bssid) {
                    if !e.ssid.is_empty() {
                        return Some(e.ssid.clone());
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Measurement, ScanEntry};

    #[test]
    fn test_project_new() {
        let p = Project::new("My Project");
        assert_eq!(p.name, "My Project");
        assert!(p.floors.is_empty());
    }

    #[test]
    fn test_add_floor() {
        let mut p = Project::new("Test");
        p.add_floor(Floor::new("Floor 1"));
        assert_eq!(p.floors.len(), 1);
        assert_eq!(p.floors[0].name, "Floor 1");
        p.add_floor(Floor::new("Floor 2"));
        assert_eq!(p.floors.len(), 2);
    }

    #[test]
    fn test_remove_floor_at_index() {
        let mut p = Project::new("Test");
        p.add_floor(Floor::new("Floor 1"));
        p.add_floor(Floor::new("Floor 2"));
        p.add_floor(Floor::new("Floor 3"));
        p.remove_floor(1);
        assert_eq!(p.floors.len(), 2);
        assert_eq!(p.floors[0].name, "Floor 1");
        assert_eq!(p.floors[1].name, "Floor 3");
    }

    #[test]
    fn test_remove_floor_first() {
        let mut p = Project::new("Test");
        p.add_floor(Floor::new("Floor 1"));
        p.add_floor(Floor::new("Floor 2"));
        p.remove_floor(0);
        assert_eq!(p.floors.len(), 1);
        assert_eq!(p.floors[0].name, "Floor 2");
    }

    #[test]
    fn test_remove_floor_out_of_bounds_does_not_panic() {
        let mut p = Project::new("Test");
        p.remove_floor(0);
        p.remove_floor(5);
        assert!(p.floors.is_empty());
    }

    #[test]
    fn test_remove_last_floor() {
        let mut p = Project::new("Test");
        p.add_floor(Floor::new("Only Floor"));
        p.remove_floor(0);
        assert!(p.floors.is_empty());
    }

    #[test]
    fn test_project_new_with_owned_string() {
        let p = Project::new(String::from("My Building"));
        assert_eq!(p.name, "My Building");
    }

    #[test]
    fn test_set_alias_insert_clear() {
        let mut p = Project::new("T");
        p.set_alias("AA:BB:CC:DD:EE:01", Some("Office AP".to_string()));
        assert_eq!(p.alias_for("AA:BB:CC:DD:EE:01"), Some("Office AP"));
        assert_eq!(p.ap_label("AA:BB:CC:DD:EE:01"), "Office AP");
        // Unknown BSSID falls back to the BSSID itself
        assert_eq!(p.ap_label("AA:BB:CC:DD:EE:02"), "AA:BB:CC:DD:EE:02");
        // Empty alias clears it
        p.set_alias("AA:BB:CC:DD:EE:01", Some(String::new()));
        assert_eq!(p.alias_for("AA:BB:CC:DD:EE:01"), None);
        p.set_alias("AA:BB:CC:DD:EE:01", None);
        assert!(p.bssid_aliases.is_empty());
    }

    #[test]
    fn test_known_bssids_collects_active_and_scan() {
        let mut p = Project::new("T");
        let mut f = Floor::new("F1");
        let mut m1 =
            Measurement::new(0.1, 0.1, "Home".to_string(), "AA:BB:CC:DD:EE:01".to_string(), 5180, 36, -55);
        m1.scan_results = vec![
            ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:01".into(), frequency_mhz: 5180, channel: 36, signal_dbm: -55, is_active: true, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
            ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:02".into(), frequency_mhz: 2437, channel: 6, signal_dbm: -70, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
        ];
        let m2 = Measurement::no_signal(0.2, 0.2);
        f.add_measurement(m1);
        f.add_measurement(m2);
        p.add_floor(f);
        // No-signal points contribute nothing; duplicates are collapsed.
        assert_eq!(p.known_bssids(), vec!["AA:BB:CC:DD:EE:01", "AA:BB:CC:DD:EE:02"]);
    }

    #[test]
    fn test_measured_ssids_and_bssids_of_ssid() {
        let mut p = Project::new("T");
        let mut f = Floor::new("F1");
        let mut m1 =
            Measurement::new(0.1, 0.1, "Home".to_string(), "AA:BB:CC:DD:EE:01".to_string(), 5180, 36, -55);
        m1.scan_results = vec![
            ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:01".into(), frequency_mhz: 5180, channel: 36, signal_dbm: -55, is_active: true, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
            ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:02".into(), frequency_mhz: 2437, channel: 6, signal_dbm: -70, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
            ScanEntry { ssid: "GuestNet".into(), bssid: "FF:FF:FF:FF:FF:01".into(), frequency_mhz: 2412, channel: 1, signal_dbm: -60, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
        ];
        // A second floor measuring a different SSID.
        let m2 = Measurement::new(0.3, 0.3, "Office".to_string(), "BB:BB:BB:BB:BB:01".to_string(), 5180, 36, -58);
        f.add_measurement(m1);
        p.add_floor(f);
        let mut f2 = Floor::new("F2");
        f2.add_measurement(m2);
        p.add_floor(f2);

        // Only SSIDs with saved measurements are offered (GuestNet, which is
        // only seen in a scan list, is excluded).
        assert_eq!(p.measured_ssids(), vec!["Home", "Office"]);
        // BSSIDs of a measured SSID come from its scan entries.
        assert_eq!(p.bssids_of_ssid("Home"), vec!["AA:BB:CC:DD:EE:01", "AA:BB:CC:DD:EE:02"]);
        // A measurement without a scan list: its connected AP is not in the
        // BSSID list (only scan entries count).
        assert_eq!(p.bssids_of_ssid("Office"), Vec::<String>::new());
        assert_eq!(p.bssids_of_ssid("GuestNet"), vec!["FF:FF:FF:FF:FF:01"]);
    }

    #[test]
    fn test_measured_ap_sections_and_unmeasured_bssids() {
        let mut p = Project::new("T");
        let mut f = Floor::new("F1");
        // Measurement WITH a scan list (Home + GuestNet visible).
        let mut m1 =
            Measurement::new(0.1, 0.1, "Home".to_string(), "AA:BB:CC:DD:EE:01".to_string(), 5180, 36, -55);
        m1.scan_results = vec![
            ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:01".into(), frequency_mhz: 5180, channel: 36, signal_dbm: -55, is_active: true, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
            ScanEntry { ssid: "Home".into(), bssid: "AA:BB:CC:DD:EE:02".into(), frequency_mhz: 2437, channel: 6, signal_dbm: -70, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
            ScanEntry { ssid: "GuestNet".into(), bssid: "FF:FF:FF:FF:FF:01".into(), frequency_mhz: 2412, channel: 1, signal_dbm: -60, is_active: false, channel_width_mhz: None, center_freq_mhz: None, center_freq2_mhz: None },
        ];
        // Old measurement WITHOUT a scan list (connected AP must still show
        // up under its SSID section).
        let m2 = Measurement::new(0.2, 0.2, "Home".to_string(), "AA:BB:CC:DD:EE:03".to_string(), 5180, 40, -61);
        f.add_measurement(m1);
        f.add_measurement(m2);
        p.add_floor(f);

        let sections = p.measured_ap_sections();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "Home");
        assert_eq!(
            sections[0].1,
            vec!["AA:BB:CC:DD:EE:01", "AA:BB:CC:DD:EE:02", "AA:BB:CC:DD:EE:03"]
        );
        // GuestNet has no measurements → its APs are "unmeasured".
        assert_eq!(p.unmeasured_bssids(), vec!["FF:FF:FF:FF:FF:01"]);
    }

    #[test]
    fn test_last_ssid_for_prefers_recent() {
        let mut p = Project::new("T");
        let mut f = Floor::new("F1");
        let mut m1 =
            Measurement::new(0.1, 0.1, "OldName".to_string(), "AA:BB:CC:DD:EE:01".to_string(), 5180, 36, -55);
        m1.scan_results = vec![ScanEntry {
            ssid: "ScanOld".into(),
            bssid: "AA:BB:CC:DD:EE:02".into(),
            frequency_mhz: 5180,
            channel: 36,
            signal_dbm: -60,
            is_active: false,
            channel_width_mhz: None,
            center_freq_mhz: None,
            center_freq2_mhz: None,
        }];
        let m2 =
            Measurement::new(0.2, 0.2, "NewName".to_string(), "AA:BB:CC:DD:EE:01".to_string(), 5180, 36, -60);
        f.add_measurement(m1);
        f.add_measurement(m2);
        p.add_floor(f);
        assert_eq!(p.last_ssid_for("AA:BB:CC:DD:EE:01"), Some("NewName".to_string()));
        assert_eq!(p.last_ssid_for("AA:BB:CC:DD:EE:02"), Some("ScanOld".to_string()));
        assert_eq!(p.last_ssid_for("AA:BB:CC:DD:EE:99"), None);
    }

    #[test]
    fn test_project_json_without_aliases_uses_default() {
        // Old project files have no bssid_aliases field.
        let json = r#"{"name":"Old","floors":[]}"#;
        let p: Project = serde_json::from_str(json).unwrap();
        assert!(p.bssid_aliases.is_empty());
    }
}
