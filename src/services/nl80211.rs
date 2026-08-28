//! Channel width + center frequency via native nl80211 netlink.
//!
//! Built on the `wl-nl80211` crate (no `iw` subprocess, no new binary, no
//! Flatpak manifest change). Two queries are used:
//!
//! - `NL80211_CMD_GET_INTERFACE` → the currently connected channel of an
//!   interface (primary frequency, channel width, center frequency 1/2).
//! - `NL80211_CMD_GET_SCAN` → the driver's cached scan results (the same
//!   data `iw dev <if> scan dump` reads; no new scan is triggered). The
//!   channel width/center of each BSS is derived from the HT (id 61),
//!   VHT (id 191), HE (extension id 36) and EHT (extension id 106)
//!   *operation* information elements.
//!
//! Everything here is **best-effort**: any failure (no netlink socket,
//! unsupported kernel, driver without scan cache) just yields `None` or an
//! empty result — the rest of the app is unaffected.
//!
//! Element layouts were verified against the iw 6.17 source and the Linux
//! `include/linux/ieee80211.h` kernel headers (see `docs/plan-patrlind-2.md`).

use std::collections::HashMap;

use futures::future::BoxFuture;
use futures::stream::TryStreamExt;
use wl_nl80211::packet_core::Parseable;
use wl_nl80211::{
    new_connection, Ieee80211Element, Ieee80211Elements, Nl80211Attr, Nl80211BssInfo,
    Nl80211ChannelWidth, Nl80211Handle,
};

/// Channel width + center frequencies (MHz).
///
/// 80+80 MHz is stored as width 160 with `center_freq2_mhz` set — the
/// display layer renders it as "80+80".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelInfo {
    /// Primary (lower) channel frequency in MHz.
    pub primary_freq_mhz: Option<u32>,
    /// Channel width in MHz (20/40/80/160/320; 80+80 stored as 160).
    pub width_mhz: Option<u32>,
    /// Center frequency 1 in MHz.
    pub center_freq_mhz: Option<u32>,
    /// Center frequency 2 in MHz (80+80 only).
    pub center_freq2_mhz: Option<u32>,
}

/// Channel info of a wireless interface, plus its interface index.
#[derive(Debug, Clone)]
pub struct InterfaceChannel {
    pub ifindex: u32,
    pub channel: ChannelInfo,
}

/// Channel info for cached-scan BSSes, keyed by BSSID (lowercase, colon
/// separated — the same format as NM's HwAddress).
pub type ScanChannels = HashMap<String, ChannelInfo>;

/// Map the kernel's channel-width enum to MHz.
fn width_to_mhz(w: &Nl80211ChannelWidth) -> Option<u32> {
    match w {
        Nl80211ChannelWidth::Mhz(m) => Some(*m),
        Nl80211ChannelWidth::Mhz80Plus80 => Some(160),
        Nl80211ChannelWidth::NoHt20 => Some(20),
        Nl80211ChannelWidth::Other(_) => None,
    }
}

/// Format a MAC address as lowercase colon-separated string.
pub fn bssid_to_str(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Run an async nl80211 query on a short-lived current-thread tokio runtime
/// (same pattern as the `wl-nl80211` examples). Any error → `None`.
fn with_netlink<T>(
    query: impl FnOnce(Nl80211Handle) -> BoxFuture<'static, Result<T, String>>,
) -> Option<T> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .ok()?;
    rt.block_on(async move {
        let (connection, handle, _) = new_connection().map_err(|e| e.to_string())?;
        tokio::spawn(connection);
        query(handle).await
    })
    .ok()
}

/// Channel info for a WiFi interface by name (e.g. "wlan0").
///
/// Returns the channel the interface is currently on (when associated) plus
/// the interface index (needed for [`scan_channels`]). The interface index
/// is available even when the card has no current channel.
pub fn interface_channel(ifname: &str) -> Option<InterfaceChannel> {
    let name = ifname.to_string();
    with_netlink(move |handle| {
        Box::pin(async move {
            let mut h = handle.interface();
            let mut stream = h.get(Vec::new()).execute().await;
            while let Some(msg) = stream
                .try_next()
                .await
                .map_err(|e| format!("interface dump: {e}"))?
            {
                let attrs = &msg.payload.attributes;
                if !attrs
                    .iter()
                    .any(|a| matches!(a, Nl80211Attr::IfName(n) if n == &name))
                {
                    continue;
                }
                let ifindex = attrs
                    .iter()
                    .find_map(|a| match a {
                        Nl80211Attr::IfIndex(i) => Some(*i),
                        _ => None,
                    })
                    .ok_or_else(|| format!("interface {name}: missing ifindex"))?;
                let mut ch = ChannelInfo::default();
                for a in attrs {
                    match a {
                        Nl80211Attr::WiphyFreq(f) => ch.primary_freq_mhz = Some(*f),
                        Nl80211Attr::ChannelWidth(w) => ch.width_mhz = width_to_mhz(w),
                        Nl80211Attr::CenterFreq1(c) => ch.center_freq_mhz = Some(*c),
                        Nl80211Attr::CenterFreq2(c) => ch.center_freq2_mhz = Some(*c),
                        _ => {}
                    }
                }
                // The kernel omits the width/center attributes for 20 MHz
                // (and legacy) channels — the center is just the primary.
                if let Some(freq) = ch.primary_freq_mhz {
                    ch.width_mhz.get_or_insert(20);
                    ch.center_freq_mhz.get_or_insert(freq);
                }
                return Ok(InterfaceChannel { ifindex, channel: ch });
            }
            Err(format!("no wireless interface named {name}"))
        })
    })
}

/// Channel info for all BSSes in an interface's cached scan results.
///
/// Reads the driver's scan cache (no new scan is triggered). Width/center
/// are derived from the BSS information elements; a BSS with IEs but no
/// HT/VHT/HE/EHT operation element is a 20 MHz BSS (center = primary).
///
/// The cache holds the results of the **last completed scan** on the
/// wiphy and shrinks to just the associated BSS while the connection sits
/// idle. The app triggers an NM scan (`request_fresh_scan`, the same
/// unprivileged path `nmcli device wifi rescan` uses) right before the
/// scan list is recorded, so reading here right afterwards yields the
/// full AP list with width/center — no privilege needed.
pub fn scan_channels(ifindex: u32) -> ScanChannels {
    with_netlink(move |handle| {
        Box::pin(async move {
            let mut h = handle.scan();
            let mut stream = h.dump(ifindex).execute().await;
            let mut out: ScanChannels = HashMap::new();
            while let Some(msg) = stream
                .try_next()
                .await
                .map_err(|e| format!("scan dump: {e}"))?
            {
                for a in &msg.payload.attributes {
                    if let Nl80211Attr::Bss(attrs) = a {
                        if let Some((bssid, ch)) = bss_channel_info(attrs) {
                            out.insert(bssid, ch);
                        }
                    }
                }
            }
            Ok(out)
        })
    })
    .unwrap_or_default()
}

/// Decode one BSS attribute into (bssid, channel info). Returns `None`
/// when the BSSID is missing.
fn bss_channel_info(attrs: &[Nl80211BssInfo]) -> Option<(String, ChannelInfo)> {
    let mut bssid: Option<[u8; 6]> = None;
    let mut freq: Option<u32> = None;
    // Beacon IEs first (most stable), then last-frame IEs.
    let mut ie_lists: Vec<&Vec<u8>> = Vec::new();
    for b in attrs {
        match b {
            Nl80211BssInfo::Bssid(m) => bssid = Some(*m),
            Nl80211BssInfo::Frequency(f) => freq = Some(*f),
            Nl80211BssInfo::RawBeaconInformationElements(v) => { ie_lists.insert(0, v) }
            Nl80211BssInfo::RawInformationElements(v) => { ie_lists.push(v) }
            _ => {}
        }
    }
    let mac = bssid?;
    let mut ch = ChannelInfo {
        primary_freq_mhz: freq,
        ..Default::default()
    };
    if !ie_lists.is_empty() {
        // Use the first IE list that yields channel info.
        if let Some((width, c1, c2)) = ie_lists.iter().find_map(|ies| ie_width_center(ies, freq)) {
            ch.width_mhz = Some(width);
            ch.center_freq_mhz = c1;
            ch.center_freq2_mhz = c2;
        } else {
            // IEs present but no operation element → 20 MHz.
            ch.width_mhz = Some(20);
        }
    }
    if ch.width_mhz == Some(20) {
        ch.center_freq_mhz = ch.center_freq_mhz.or(freq);
    }
    Some((bssid_to_str(&mac), ch))
}

// ── IE operation-element decoding ──────────────────────────────────────────

/// HT op: secondary channel offset (bits 1:0 of byte 1).
const HT_SEC_NONE: u8 = 0;
const HT_SEC_ABOVE: u8 = 1;
const HT_SEC_BELOW: u8 = 3;

/// Decode the channel width + center frequencies from a BSS's information
/// elements.
///
/// Returns `(width_mhz, center1_mhz, center2_mhz)` or `None` when no
/// operation element is present (the caller decides whether that means
/// "20 MHz" or "unknown").
///
/// `primary_freq_mhz` selects the 5 GHz / 6 GHz center-frequency mapping.
///
/// Layouts (verified against iw 6.17 and Linux kernel v6.6 headers):
/// - **HT op (id 61)**, len ≥ 2: `p[0]` = primary channel, `p[1] & 3` =
///   secondary offset (0 none / 1 above / 3 below).
/// - **VHT op (id 191)**, len ≥ 3: `p[0] & 3` = width (0 = 20/40, 1 = 80,
///   2 = 160, 3 = 80+80), `p[1]` = center seg 0, `p[2]` = center seg 1
///   (80+80, len ≥ 6).
/// - **HE op (extension id 36)**: fields `f` after the inner id byte;
///   `params = LE32(f[0..4])`. Optional fields start at `f[6]`:
///   `params & 0x4000` → 3-byte VHT op info `[width, c0, c1]` (VHT
///   encoding); `params & 0x8000` → 1 byte co-hosted; `params & 0x20000`
///   → 5-byte 6 GHz op info `[primary, control, c0, c1, minrate]`,
///   `control & 3` = 0/1/2/3 → 20/40/80/160 MHz.
/// - **EHT op (extension id 106)**: fields `f` after the inner id byte;
///   `params = f[0]` (bit 0 = op info present). Op info at `f[5]`:
///   `[control, c0, c1]`, `control & 7` = 0–4 → 20/40/80/160/320 MHz.
///
/// Priority: EHT op → HE op → VHT op → HT op (the newest PHY's operation
/// element is the accurate one; older ones stay for compatibility).
pub fn ie_width_center(
    ies: &[u8],
    primary_freq_mhz: Option<u32>,
) -> Option<(u32, Option<u32>, Option<u32>)> {
    let mut ht: Option<(u8, u8)> = None; // (primary channel, secondary offset)
    let mut vht: Option<(u8, u8, u8, bool)> = None; // (width, c0, c1, has_c1)
    let mut he6: Option<(u8, u8)> = None; // 6 GHz op info: (control, c0)
    let mut he_vht: Option<(u8, u8, u8)> = None; // VHT op info: (width, c0, c1)
    let mut eht: Option<(u8, u8)> = None; // op info: (control, c0)

    let Ok(elements) = Ieee80211Elements::parse(ies) else {
        return None;
    };
    for e in &elements.0 {
        if let Ieee80211Element::Other(id, p) = e {
            match id {
                61 if p.len() >= 2 => ht = Some((p[0], p[1] & 3)),
                // 192 = VHT *operation* (191 is the VHT capability element,
                // which the crate parses into a typed variant).
                192 if p.len() >= 3 => {
                    vht = Some((
                        p[0] & 3,
                        p[1],
                        p.get(2).copied().unwrap_or(0),
                        p.len() >= 6,
                    ))
                }
                // Extension elements: p[0] = inner id, fields after it.
                255 if p.len() >= 2 => {
                    let f = &p[1..];
                    match p[0] {
                        36 if f.len() >= 6 => {
                            let params = u32::from_le_bytes([f[0], f[1], f[2], f[3]]);
                            // Optional fields, in spec order.
                            let mut off: usize = 6;
                            if params & 0x0000_4000 != 0 && off + 3 <= f.len() {
                                he_vht = Some((f[off] & 3, f[off + 1], f[off + 2]));
                                off += 3;
                            }
                            if params & 0x0000_8000 != 0 {
                                off += 1;
                            }
                            if params & 0x0002_0000 != 0 && off + 5 <= f.len() {
                                he6 = Some((f[off + 1] & 3, f[off + 2]));
                            }
                        }
                        106 if f.len() >= 8 && f[0] & 1 != 0 => {
                            eht = Some((f[5] & 7, f[6]));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    let seg = |seg: u8| seg_to_mhz(seg, primary_freq_mhz);

    if let Some((control, c0)) = eht {
        let width = [20, 40, 80, 160, 320].get((control & 7) as usize).copied()?;
        return Some((width, seg(c0), None));
    }
    if let Some((control, c0)) = he6 {
        let width = [20, 40, 80, 160].get((control & 3) as usize).copied()?;
        return Some((width, seg(c0), None));
    }
    if let Some((width, c0, c1)) = he_vht {
        return Some(vht_width_center(width, c0, c1, true, ht, primary_freq_mhz, seg));
    }
    if let Some((width, c0, c1, has_c1)) = vht {
        return Some(vht_width_center(width, c0, c1, has_c1, ht, primary_freq_mhz, seg));
    }
    if let Some((_, sec)) = ht {
        if let Some(freq) = primary_freq_mhz {
            match sec {
                HT_SEC_ABOVE => return Some((40, Some(freq + 10), None)),
                HT_SEC_BELOW => return Some((40, Some(freq.saturating_sub(10)), None)),
                _ => {}
            }
        }
    }
    None
}

/// Resolve a VHT-encoded width (0 = 20/40) + center segments, falling back
/// to the HT secondary offset for the 40 MHz case.
///
/// For width 0 (20/40 MHz) the VHT center segment is ambiguous in practice
/// (spec says "primary channel", but APs are known to put the center
/// channel there), so the 40 MHz center is derived from the HT operation
/// element's own primary-channel field + secondary offset instead.
fn vht_width_center(
    width: u8,
    c0: u8,
    c1: u8,
    has_c1: bool,
    ht: Option<(u8, u8)>, // (primary channel, secondary offset)
    base_freq: Option<u32>, // BSS primary frequency (fallback base)
    seg: impl Fn(u8) -> Option<u32>,
) -> (u32, Option<u32>, Option<u32>) {
    match width {
        1 => (80, seg(c0), None),
        2 => (160, seg(c0), None),
        3 if has_c1 => (160, seg(c0), seg(c1)), // 80+80 stored as 160 + second center
        3 => (160, seg(c0), None),
        // 0 = 20 or 40 MHz: the HT operation element decides.
        _ => {
            let Some((primary_ch, sec)) = ht else {
                return (20, None, None);
            };
            match sec {
                HT_SEC_ABOVE | HT_SEC_BELOW => {
                    let primary = if primary_ch > 0 {
                        seg(primary_ch).or(base_freq)
                    } else {
                        base_freq
                    };
                    let center = match sec {
                        HT_SEC_ABOVE => primary.map(|f| f + 10),
                        _ => primary.map(|f| f.saturating_sub(10)),
                    };
                    (40, center, None)
                }
                _ => (20, None, None),
            }
        }
    }
}

/// Center frequency segment (channel number) → MHz.
///
/// 6 GHz segments are 1–71 (`5955 + (n−1)·5`); 5 GHz segments are ≥ 34
/// (`5000 + n·5`). The band is picked from the BSS's primary frequency;
/// 2.4 GHz segments are not applicable (2.4 GHz-wide BSSes use the HT
/// secondary offset instead).
fn seg_to_mhz(seg: u8, primary_freq_mhz: Option<u32>) -> Option<u32> {
    let f = primary_freq_mhz?;
    if f >= 5925 {
        Some(5955 + seg.saturating_sub(1) as u32 * 5)
    } else if f >= 5000 {
        Some(5000 + seg as u32 * 5)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a TLV stream of single elements: (id, payload) pairs.
    fn ies(elements: &[(u8, &[u8])]) -> Vec<u8> {
        let mut v = Vec::new();
        for (id, p) in elements {
            v.push(*id);
            v.push(p.len() as u8);
            v.extend_from_slice(p);
        }
        v
    }

    /// A realistic 2.4 GHz HT op element (len 22): primary channel 6,
    /// secondary offset `sec`.
    fn ht_op(primary: u8, sec: u8) -> (u8, Vec<u8>) {
        let mut p = vec![0u8; 22];
        p[0] = primary;
        p[1] = sec;
        (61, p)
    }

    fn vht_op(width: u8, c0: u8, c1: u8, len: usize) -> (u8, Vec<u8>) {
        let mut p = vec![width, c0, c1];
        p.resize(len, 0);
        (192, p)
    }

    fn freq5180() -> Option<u32> {
        Some(5180)
    }

    #[test]
    fn no_operation_elements_yields_none() {
        let v = ies(&[(3, &[1, 2, 3]), (92, &[0])]); // id 92, not an op element
        assert_eq!(ie_width_center(&v, freq5180()), None);
        assert_eq!(ie_width_center(&[], freq5180()), None);
    }

    #[test]
    fn ht_20_mhz_is_none() {
        let (id, p) = ht_op(6, HT_SEC_NONE);
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, freq5180()), None);
    }

    #[test]
    fn ht_40_above_center_primary_plus_10() {
        let (id, p) = ht_op(36, HT_SEC_ABOVE);
        let v = ies(&[(id, &p)]);
        // ch36 (5180) + secondary above → 40 MHz, center 5190.
        assert_eq!(ie_width_center(&v, freq5180()), Some((40, Some(5190), None)));
    }

    #[test]
    fn ht_40_below_center_primary_minus_10() {
        let (id, p) = ht_op(38, HT_SEC_BELOW);
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, Some(5190)), Some((40, Some(5180), None)));
    }

    #[test]
    fn vht_80_mhz() {
        // 80 MHz on ch54: center segment 55 → 5000 + 55·5 = 5275.
        let (id, p) = vht_op(1, 55, 0, 5);
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, Some(5180)), Some((80, Some(5275), None)));
    }

    #[test]
    fn vht_160_mhz() {
        // 160 MHz, center segment 66 → 5330.
        let (id, p) = vht_op(2, 66, 0, 5);
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, Some(5180)), Some((160, Some(5330), None)));
    }

    #[test]
    fn vht_80plus80_two_centers() {
        // 80+80: c0 = 44 (5220), c1 = 132 (5660).
        let (id, p) = vht_op(3, 44, 132, 6);
        let v = ies(&[(id, &p)]);
        assert_eq!(
            ie_width_center(&v, Some(5180)),
            Some((160, Some(5220), Some(5660)))
        );
    }

    #[test]
    fn vht_20or40_resolves_via_ht_offset() {
        // VHT says "20 or 40", HT offset says above → 40 MHz. The center is
        // derived from the HT op's primary channel (36 → 5180) + 10 = 5190.
        let (vid, vp) = vht_op(0, 36, 0, 5);
        let (hid, hp) = ht_op(36, HT_SEC_ABOVE);
        let v = ies(&[(vid, &vp), (hid, &hp)]);
        assert_eq!(
            ie_width_center(&v, freq5180()),
            Some((40, Some(5190), None))
        );
    }

    #[test]
    fn vht_20or40_ignores_ambiguous_vht_segment() {
        // Real-world case (live-captured on the dev machine): VHT width 0
        // with center segment = 38 (the AP advertises the *center* channel,
        // not the primary), HT op primary 36, secondary above. Correct
        // center: 5180 + 10 = 5190 (not 5190 + 10 = 5200).
        let (vid, vp) = vht_op(0, 38, 0, 5);
        let (hid, hp) = ht_op(36, HT_SEC_ABOVE);
        let v = ies(&[(vid, &vp), (hid, &hp)]);
        assert_eq!(
            ie_width_center(&v, freq5180()),
            Some((40, Some(5190), None))
        );
    }

    #[test]
    fn vht_20or40_without_ht_is_20() {
        let (id, p) = vht_op(0, 36, 0, 5);
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, freq5180()), Some((20, None, None)));
    }

    /// HE op extension element: [255, len, 36, <params LE32>, <mcs LE16>,
    /// <optional…>].
    fn he_op(params: u32, optional: &[u8]) -> (u8, Vec<u8>) {
        let mut p = params.to_le_bytes().to_vec();
        p.extend_from_slice(&[0, 0]); // basic HE-MCS/NSS set
        p.extend_from_slice(optional);
        (255, vec![36].into_iter().chain(p).collect())
    }

    #[test]
    fn he_6ghz_80_mhz() {
        // 6 GHz op info: [primary, control, c0, c1, minrate], control & 3 = 2 (80).
        // Center segment 14 → 5955 + 13·5 = 6020.
        let optional = [13u8, 2, 14, 14, 80];
        let (id, p) = he_op(0x0002_0000, &optional);
        let v = ies(&[(id, &p)]);
        assert_eq!(
            ie_width_center(&v, Some(5955)),
            Some((80, Some(6020), None))
        );
    }

    #[test]
    fn he_6ghz_20_mhz() {
        let optional = [1u8, 0, 1, 1, 80]; // control & 3 = 0 → 20
        let (id, p) = he_op(0x0002_0000, &optional);
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, Some(5955)), Some((20, Some(5955), None)));
    }

    #[test]
    fn he_5ghz_vht_info_80() {
        // VHT op info present: [width, c0, c1], width = 1 (80).
        let optional = [1u8, 55, 0];
        let (id, p) = he_op(0x0000_4000, &optional);
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, freq5180()), Some((80, Some(5275), None)));
    }

    #[test]
    fn he_optional_fields_in_spec_order() {
        // VHT op info (3B) + co-hosted (1B) + 6 GHz op info (5B): the 6 GHz
        // field starts at offset 6+3+1 = 10.
        let mut optional = vec![1u8, 55, 0]; // VHT info (80 MHz — ignored, 6GHz wins)
        optional.push(1); // co-hosted max BSSID
        optional.extend_from_slice(&[13u8, 3, 40, 40, 80]); // 6 GHz: 160 MHz, c0 = 40
        let (id, p) = he_op(0x0002_0000 | 0x0000_8000 | 0x0000_4000, &optional);
        let v = ies(&[(id, &p)]);
        // 6 GHz info: control & 3 = 3 → 160 MHz, center = 5955 + 39·5 = 6150.
        assert_eq!(
            ie_width_center(&v, Some(5955)),
            Some((160, Some(6150), None))
        );
    }

    /// EHT op extension element: [255, len, 106, params, <mcs 4B>, <op info…>].
    fn eht_op(op_info: Option<&[u8]>) -> (u8, Vec<u8>) {
        let mut p = vec![0u8]; // params
        p.extend_from_slice(&[0; 4]); // basic MCS/NSS
        if let Some(info) = op_info {
            p[0] |= 1;
            p.extend_from_slice(info);
        }
        (255, vec![106].into_iter().chain(p).collect())
    }

    #[test]
    fn eht_320_mhz() {
        // control & 7 = 4 → 320 MHz, c0 = 44 → 5955 + 43·5 = 6170.
        let (id, p) = eht_op(Some(&[4u8, 44, 0]));
        let v = ies(&[(id, &p)]);
        assert_eq!(
            ie_width_center(&v, Some(5955)),
            Some((320, Some(6170), None))
        );
    }

    #[test]
    fn eht_40_mhz() {
        let (id, p) = eht_op(Some(&[1u8, 2, 0]));
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, Some(5955)), Some((40, Some(5960), None)));
    }

    #[test]
    fn eht_without_op_info_yields_none() {
        let (id, p) = eht_op(None);
        let v = ies(&[(id, &p)]);
        assert_eq!(ie_width_center(&v, Some(5955)), None);
    }

    #[test]
    fn eht_takes_priority_over_he() {
        let (hid, hp) = he_op(0x0002_0000, &[13u8, 2, 14, 14, 80]); // HE: 80
        let (eid, ep) = eht_op(Some(&[3u8, 40, 0])); // EHT: 160
        let v = ies(&[(hid, &hp), (eid, &ep)]);
        assert_eq!(
            ie_width_center(&v, Some(5955)),
            Some((160, Some(6150), None))
        );
    }

    #[test]
    fn malformed_ie_stream_is_handled() {
        // Truncated element, then a valid HT op.
        let mut v = vec![61u8, 100, 36, HT_SEC_ABOVE]; // claims len 100, has 2 bytes
        v.extend_from_slice(&[191, 5, 1, 55, 0, 0, 0]);
        // The TLV walker breaks on the truncated element, so nothing parses —
        // that is fine (returns None, no panic).
        assert_eq!(ie_width_center(&v, freq5180()), None);
    }

    #[test]
    fn bssid_to_str_format() {
        assert_eq!(bssid_to_str(&[0xba, 0xfb, 0xe4, 0x12, 0x14, 0xfa]), "ba:fb:e4:12:14:fa");
    }

    /// Live test (skipped unless WIFICHECKER_LIVE_DBUS=1): exercises the
    /// real netlink path on the dev machine.
    #[test]
    fn live_interface_channel() {
        if std::env::var("WIFICHECKER_LIVE_DBUS").is_err() {
            eprintln!("live_interface_channel: skipped (set WIFICHECKER_LIVE_DBUS=1)");
            return;
        }
        let devs = super::super::nm_dbus::query_wifi_device_names();
        if devs.is_empty() {
            eprintln!("live_interface_channel: skipped (no WiFi devices)");
            return;
        }
        for dev in &devs {
            let Some(ic) = interface_channel(dev) else {
                println!("  {dev}: no channel info");
                continue;
            };
            println!("  {dev}: {:#?}", ic.channel);
            let scan = scan_channels(ic.ifindex);
            for (bssid, ch) in scan.iter().take(5) {
                println!("    scan {bssid}: {ch:?}");
            }
        }
    }
}
