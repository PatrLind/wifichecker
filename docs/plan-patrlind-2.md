# WiFi Checker — feature plan `0.2.2-patrlind-2`

Date: 2026-08-28
Status: **approved in principle — start on confirmation**

## Goals (user requests)

| # | Request |
|---|---------|
| 1 | When the wanted SSID is disconnected but the WiFi card is on: still do a scan and record all networks (incl. the wanted SSID if detected) on "no signal" points. |
| 2 | Record the bandwidth (channel width) of connected *and* scanned signals. |
| 4 | Report channel width + **center frequency** (the technical term; e.g. 40 MHz on ch36/ch38 → center 5190 MHz, the "channel 37"). |
| 3 | The "All networks" popup should also include the network(s) we are looking for. |
| 5 | Channel report feature: per-point channel/AP/signal list + site survey across measurements to find free channels, with per-band spectrum view (bars). **Separate step.** |

## Verified findings (checked on the dev machine)

- **NM D-Bus does not expose channel width / center frequency.** `org.freedesktop.NetworkManager.AccessPoint` properties are only: Ssid, HwAddress, Frequency (primary), Strength, MaxBitrate, LastSeen, Mode, Flags. → netlink is required.
- **The kernel exposes everything via nl80211 generic netlink:**
  - `NL80211_CMD_GET_INTERFACE` → `NL80211_ATTR_WIPHY_FREQ` (primary), `NL80211_ATTR_CHANNEL_WIDTH` (0=20, 1=40, 2=80, 3=160, 4=80+80, 5=320), `NL80211_ATTR_CENTER_FREQ1`, `NL80211_ATTR_CENTER_FREQ2`. No privileges needed.
  - `NL80211_CMD_GET_SCAN` (cached scan — what `iw scan dump` reads) → BSS entries with `BSSID`, `FREQ`, `SIGNAL_MBM`, and raw `IE`. Neighboring-AP width/center is derived from the **HT op (id 61)**, **VHT op (id 192)**, **HE op (extension id 36)**, **EHT op (extension id 106)** information elements (see exact layouts below).
- **Unprivileged access works here**: `iw link` / `iw scan dump` succeed as uid 1000 with `CapEff=0` (kernel 7.0, Intel iwlwifi, iw 6.17).
- **Flatpak**: `AF_NETLINK` is in the default seccomp allowlist — **confirmed in the installed flatpak 1.16.6 source** (`common/flatpak-run.c`, `socket_family_allowlist[]` contains `{ AF_NETLINK, 0 }`, i.e. allowed unconditionally, no manifest permission); the sandbox shares the host network namespace by default (`--share=network`). → the native-netlink approach needs **no manifest change and no bundled binary**.
- **Scans work without association** whenever the radio is on → the no-signal scan is feasible.
- The "wl-nl80211" crate mentioned in external AI notes **does exist** (initial check was a false negative — the crates.io API requires a User-Agent header). Evaluated and live-tested: it covers both needed queries with typed attributes (see architecture section).
- No netlink crates were in the local cargo cache at planning time — irrelevant now that we adopt the crate: the Flatpak build regenerates `cargo-sources.json` from `Cargo.lock` (build script), and all new deps are pure-Rust (no C build step).

## Architecture decision: `wl-nl80211` crate (native netlink, no `iw`)

Use the **`wl-nl80211` crate** (https://crates.io/crates/wl-nl80211, rust-netlink org — the maintainers of the mature `netlink-*` crate family, MIT, actively maintained: v0.7.0 released 2026-08-18, ~18k downloads/30d) instead of hand-rolled libc netlink.

**Live-verified on this machine** (scratch project, rustc 1.98, `wlp14s0`):

- `handle.interface().get(...)` → typed attributes, incl. exactly what we need:
  - `WiphyFreq(5180)` (primary), `ChannelWidth(Mhz(40))` (also `Mhz80Plus80` for 80+80), `CenterFreq1(5190)`, `CenterFreq2`, `WiphyChannelType(Ht40Plus)`, `IfName("wlp14s0")`, `IfIndex`, `Ssid(…)`. No privileges needed.
- `handle.scan().dump(if_index)` → per-BSS `Bssid`, `Frequency(u32 MHz)`, `SignalMbm(i32)`, `RawInformationElements(Vec<u8>)` (cached scan = what `iw scan dump` reads — no new scan triggered).
- `Ieee80211Elements::parse(&ies)` → typed IEs (Ssid, HT/VHT/HE capability, RSN, …); **unknown element ids land in `Other(id, Vec<u8>)`**, so the HT/VHT/HE/EHT *operation* elements (which the crate does not specifically parse) are available as raw payloads and we decode the few fields we need. **Layouts verified against iw 6.17 source (`scan.c`, `util.c`) and Linux v6.6 kernel headers (`include/linux/ieee80211.h`) 2026-08-28:**
  - **HT op (id 61)**, min len 2: `p[0]` = primary channel, `p[1] & 3` = secondary offset (0 = none, 1 = above, 3 = below) → 40 MHz, center = primary ± 10 MHz
  - **VHT op (id 192 — 191 is the VHT *capability* element)**, min len 3 (5/6 in practice): `p[0] & 3` = ch_width (0 = 20/40 → resolve via HT op, see below; 1 = 80; 2 = 160; 3 = 80+80), center seg1 = `p[1]`, seg2 = `p[2]` (80+80, len ≥ 6). **Width-0 caveat (live-verified 2026-08-28):** APs commonly put the *center* channel — not the primary — in the VHT center segment for 20/40 (e.g. our own AP: VHT seg = 38 = center, while HT op says primary 36 + secondary above). So for width 0 the 40 MHz center is derived from the **HT op's own primary-channel field + secondary offset** (falling back to the BSS frequency as base); the VHT segment is ignored in that case. Live result for the connected AP: 40 MHz, center 5190 — matching the interface dump and `iw`.
  - **HE op (extension id 36 — *not* 141)**, crate yields `Other(255, p)` with `p[0] == 36`, fields `f = p[1..]`: `params = LE32(f[0..4])`; optional fields at `f[6]`: if `params & 0x0000_4000` (VHT op info): 3 bytes `[width, ccfs0, ccfs1]` (VHT encoding); if `params & 0x0000_8000`: 1 byte co-hosted; if `params & 0x0002_0000` (6 GHz op info): 5 bytes `[primary, control, ccfs0, ccfs1, minrate]`, `control & 3` = 0/1/2/3 → 20/40/80/160 MHz
  - **EHT op (extension id 106 — *not* 144)**, `Other(255, p)` with `p[0] == 106`, fields `f = p[1..]`: `params = f[0]` (bit 0 = op info present); op info at `f[5]`: `[control, ccfs0, ccfs1]`, `control & 7` = 0–4 → 20/40/80/160/320 MHz
  - **Priority** EHT op → HE op → VHT op → HT op → default 20 MHz (center = primary)
  - **Crate BSS variants**: `RawInformationElements` (last frame IEs) *and* `RawBeaconInformationElements` (beacon IEs) — try beacon first, then probe, use the first that yields width info
- Center channel number → MHz: n < 34 → 6 GHz `5955 + (n−1)·5`; n ≥ 34 → 5 GHz `5000 + n·5` (chosen by the AP's primary frequency).

**API notes** (from the crate's own examples, which we mirror):

```rust
let (connection, handle, _) = wl_nl80211::new_connection()?; // io::Result
tokio::spawn(connection);                                   // background task
let mut s = handle.interface().get(Vec::new()).execute().await;  // TryStream
let mut s = handle.scan().dump(if_index).execute().await;
```

- API is async (futures TryStream + tokio). **tokio is already an app dependency**; we run each query in the existing background measurement thread via `tokio::runtime::Builder::new_current_thread().enable_io()` + `block_on` (same pattern as the crate's examples). Short-lived connection per query.
- `new_connection()` is behind the **default** `tokio_socket` feature — no feature flags needed. `netlink_packet_core` is re-exported as `wl_nl80211::packet_core` (for the `Parseable` trait) — only one new direct dependency: `wl-nl80211 = "0.7"`.
- Scan BSS data arrives as a flat attribute list per BSS message (`Nl80211Attr::Bss(Vec<Nl80211BssInfo>)`) — group per message.
- All best-effort: any failure → debug log + `None`/empty; the rest of the app works identically.

**Cost:** ~9 new pure-Rust crates in the tree (`wl-nl80211`, `genetlink`, `netlink-sys`, `netlink-proto`, `netlink-packet-core`, `netlink-packet-generic`, `futures-timer`?, `bitflags`, `thiserror`; anyhow/log/tokio/futures already present). No C compilation → **no Flatpak manifest change**; the offline Flatpak build just needs `cargo-sources.json` regeneration, which `build_flatpak_local.sh`/`publish_flatpak.sh` already do.

**Width model** (new `Option<u32>` fields, `#[serde(default)]` → old projects load unchanged):
- `channel_width_mhz` — 20/40/80/160/320; **80+80 stored as 160** (displayed "80+80" when `center_freq2` is present)
- `center_freq_mhz` — center 1 (MHz)
- `center_freq2_mhz` — center 2 (only for 80+80)

No fallback to `iw` — if netlink is unavailable, fields simply stay unknown.

## Part 1 (this step): requests 1–4

### 1. No-signal points record a full scan
- `nm_dbus.rs`: new `query_wifi_device_names() -> Vec<String>` — all NM-managed WiFi device interface names (connected or not).
- Measurement background thread (window.rs): when there is **no active connection**:
  - pick card = preferred card (if present) else first WiFi device;
  - fresh scan exactly as today (RequestScan + wait ≤ 6 s, cached-list fallback);
  - store scan list (same `MAX_SCAN_RESULTS = 40` cap) in the no-signal measurement.
  - no WiFi device at all (radio off) → plain no-signal point, empty list.
- Confirm dialog body adds "N APs in range".
- **Selected Measurement details** for a no-signal point: "Networks in range: N", then for each SSID this floor is measuring: its strongest detected signal (ch/band), or "not detected".
- New `Measurement.device: String` (serde default) — records which card was used.

### 2 + 4. Channel width + center frequency, recorded and displayed
- New fields on `WifiInfo`, `ScanEntry`, `Measurement` (see model above).
- Enrichment at measurement time: `get_interface_channel()` for the connected AP; `get_scan_channels()` per BSSID for the scan list.
- Live tick (1.5 s): `get_interface_channel()` while a card is connected → live "Current Signal" line.
- Display in Current Signal, Selected Measurement, scan-list section and the popup, e.g.:
  - `Ch: 36 | 5180 MHz | 40 MHz | center 5190 MHz`
  - 80+80: `80+80 MHz | center 5190 + 5690`
  - unknown: line stays as today (no "unknown" noise)

### 3. "All networks" popup includes the wanted network
- "Other networks (N APs)" → **"All networks in range (N APs)"** — all recorded APs, including the target SSID's APs; connected AP marked `●` (for no-signal points: APs of the floor's measured SSIDs get the `●` mark).
- Button also appears for no-signal points with a recorded scan.
- Table gains a width column (e.g. `36 · 40M`), still monospace column-aligned.

### Version
- `0.2.2-patrlind-1` → **`0.2.2-patrlind-2`** (VERSION file + Cargo.toml — build.rs enforces the match).

## Part 2 (next step): request 5 — channel report

- New header-bar button → modal window.
- **Point view**: pick one measurement → per band (2.4 / 5 / 6 GHz):
  - table: channel, SSID, AP (alias), signal, width
  - **bar chart**: x = channel, y = dBm, one semi-transparent bar per AP on that channel
- **Site survey view**: select all or a subset of measurements (checkbox list) → per band:
  - bar chart per channel + table (AP count, signal)
  - **aggregation selectable** (dropdown, default strongest): *strongest (max across points)* or *average*
  - free/clear channels highlighted
- Charts drawn with cairo (existing dependency, same approach as the floor-plan canvas).
- Pure post-processing of recorded scan data — no new radio work.
- New file: `src/widgets/channel_report.rs` + window.rs wiring.

## Order of work (Part 1)

1. `cargo add wl-nl80211@0.7` + `src/services/nl80211.rs` thin wrapper (interface channel info, scan channels, HT/VHT/HE/EHT op IE decoding) + unit tests
   - IE op decoder tested against captured byte vectors (incl. this machine's live capture)
   - live test gated on env var (same pattern as `WIFICHECKER_LIVE_DBUS`)
2. Model changes (fields + serde default tests for old project JSON)
3. `nm_dbus` device list + no-signal scan flow (window.rs)
4. UI: Selected Measurement details, popup, Current Signal (measurement_panel.rs)
5. Verification
6. Version bump

## Verification plan (results 2026-08-28)

- ✅ `cargo test` — **167 tests pass** (incl. IE-decoding unit tests with synthetic vectors, backward-compat deserialization test, and the live netlink test gated behind `WIFICHECKER_LIVE_DBUS=1`)
- ✅ Native live run on this machine (40 MHz ch36/ch38 connection): interface query and scan-cache enrichment **both** report `5180 MHz / 40 MHz / center 5190 MHz` for the connected AP — matching `iw dev wlp14s0 link` / `iw scan dump`
- ⏳ Disconnect WiFi → measure a no-signal point → confirm scan list + wanted-SSID reporting (needs the user's network toggle — left as a manual step; code path reuses the same verified `scan_channels()`)
- ✅ Flatpak sandbox AF_NETLINK — **confirmed via flatpak 1.16.6 source** (`socket_family_allowlist` in `common/flatpak-run.c`); sandbox also shares the host netns, so no further probe needed
- ✅ Load an old project file — covered by unit test `test_measurement_json_without_new_fields_uses_defaults`
- ✅ Version bump `0.2.2-patrlind-1` → `0.2.2-patrlind-2` (`VERSION` + `Cargo.toml`, `build.rs` sync check passes)

## Part 2b: channel width/center for all scanned APs (2026-08-28)

### Empirical findings (dev machine, iwlwifi, kernel 7.0)

| Path | Privilege | Result |
|------|-----------|--------|
| `GET_INTERFACE` (connected AP) | none | width/center of the associated BSS |
| `GET_SCAN` (driver scan cache) | none | **full AP list** (67–98 BSS with width/center) right after any scan completes; shrinks to just the associated BSS while the connection sits idle |
| Trigger own scan (`NL80211_CMD_SCAN`) | CAP_NET_ADMIN | accepted with privilege; per-BSS results unicast to the triggering socket only |

The app already triggers an NM scan (`request_fresh_scan`, the same
unprivileged D-Bus call `nmcli device wifi rescan` uses) right before the
scan list is recorded, so `scan_channels()` (GET_SCAN) read immediately
afterwards sees the full, fresh cache. **No privilege is needed at all.**

### Root cause of the originally all-null records

`merge_nl_scan` never matched: NM reports BSSIDs **uppercase**
(`B4:FB:E4:12:14:FA`), netlink keys them **lowercase**. Fixed with a
case-insensitive lookup — that single fix made the full width/center data
appear in the saved scan lists.

### What was tried and removed

A privileged own-scan enrichment (`trigger_scan_channels` + `setcap`
capability + one-time hint dialog) was implemented and then found to be
unnecessary (the cache is already fresh at read time) and its
`NEW_SCAN_RESULTS` event collection was unreliable. **Removed:** the
trigger code, the `setcap` dialog, and the related `MeasureResult` flags.

### Chart rework (same step)
- Bar placement = physical spectrum: centered on the recorded center
  frequency (40 MHz @ 5180/center 5190 → 5170–5210), derived from base
  frequency when unknown; 80+80 → two 80 MHz lobes (`bar_lobes()`,
  unit-tested).
- Bar color = **signal strength** (green → yellow → orange → red,
  `signal_color()`); table dots match.
- Spotlight highlight: hovering a bar or a table row dims all other
  bars (alpha 0.15), draws a black triangle marker under the
  highlighted bar's channel (drawn last, always on top), and highlights
  the matching row (shared `BandSync` hover state; CSS in `main.rs`).
- Tables sorted **strongest signal first**; dedicated BSSID column
  (dot + MAC) + separate AP (alias) column.
- `merge_nl_scan` matches BSSIDs case-insensitively.

## Part 2c: remembered UI + last/recent projects (2026-08-28)

- **Window size + measurement-panel width** remembered in `settings.json`
  (`window_width/height`, `sidebar_width`); debounced save on
  resize/pane-drag + immediate save on window close; restored on start
  (`save_ui_geometry_now` / `remember_ui_geometry`).
- **Start with the last opened project** instead of the default project
  (`last_project_path`, restored in `Window::new`, falls back to the
  default project file).
- **Recent Projects** section in the hamburger menu (up to 10, file
  names, most recent first; `recent_projects`). Updated on new/open/
  save-as/open-recent via `remember_project_path`; menu rebuilt in place
  via `rebuild_project_menu` (same `gio::Menu` object, so the
  MenuButton picks up changes without re-setting the model).

## Part 2d: theme-aware channel charts (2026-08-28)

The charts previously painted a hard-coded white background with dark
text/lines — fine in light mode, glaring in dark mode. Now every chart
color resolves from the active theme via the widget's style context
(`view_bg_color` / `view_fg_color`, verified live against libadwaita
1.x in both schemes): background, grid lines, axis labels, plot frame,
bar outlines, hover banner, spotlight outline and the channel marker
triangle all follow the theme (verified names resolve in both light
and dark; safe light-theme fallbacks if a token is missing).

## Risks / open items
## Part 2c: remembered UI + last/recent projects (2026-08-28)

- **Window size + measurement-panel width** remembered in `settings.json`
  (`window_width/height`, `sidebar_width`); debounced save on
  resize/pane-drag + immediate save on window close; restored on start
  (`save_ui_geometry_now` / `remember_ui_geometry`).
- **Start with the last opened project** instead of the default project
  (`last_project_path`, restored in `Window::new`, falls back to the
  default project file).
- **Recent Projects** section in the hamburger menu (up to 10, file
  names, most recent first; `recent_projects`). Updated on new/open/
  save-as/open-recent via `remember_project_path`; menu rebuilt in place
  via `rebuild_project_menu` (same `gio::Menu` object, so the
  MenuButton picks up changes without re-setting the model).

## Part 2d: theme-aware channel charts (2026-08-28)

The charts previously painted a hard-coded white background with dark
text/lines — fine in light mode, glaring in dark mode. Now every chart
color resolves from the active theme via the widget's style context
(`view_bg_color` / `view_fg_color`, verified live against libadwaita
1.x in both schemes): background, grid lines, axis labels, plot frame,
bar outlines, hover banner, spotlight outline and the channel marker
triangle all follow the theme (verified names resolve in both light
and dark; safe light-theme fallbacks if a token is missing).

## Risks / open items

| Risk | Mitigation |
|------|------------|
| New dependency in the tree (`wl-nl80211` + netlink stack) | rust-netlink org (maintainers of the mature `netlink-*` crates), MIT, very active (v0.7.0 ten days old), ~18k downloads/30d; we use a tiny surface (2 queries + IE parse); pin to `0.7` |
| IE element layouts (HT/VHT/HE/EHT op) | **verified 2026-08-28** against iw 6.17 source (downloaded `iw_6.17.orig.tar.xz`) and Linux v6.6 `include/linux/ieee80211.h`; unit-tested with synthetic vectors; VHT width-0 ambiguity handled via HT op (live-verified against the dev machine's own AP) |
| Sandbox AF_NETLINK | **resolved 2026-08-28**: allowed unconditionally in flatpak 1.16.6's default seccomp profile (`socket_family_allowlist`), sandbox shares host netns → no manifest change, no fallback path needed |
| Older kernels may require `CAP_NET_ADMIN` for `GET_SCAN` | graceful `None`; connected-AP width via interface dump still works (unprivileged) |
| Some drivers don't cache scan IEs / cache frame varies over time (VHT op may be absent in the cached frame) | per-AP width stays `None`; interface path (connected AP) is unaffected |

## Files touched

**Part 1**
- `Cargo.toml` / `Cargo.lock`: `wl-nl80211 = "0.7"` (+ transitive netlink crates); `cargo-sources.json` regenerated
- new: `src/services/nl80211.rs`
- `src/models/measurement.rs` (ScanEntry + Measurement fields, `device`)
- `src/services/wifi_scanner.rs` (facade), `src/services/nm_dbus.rs` (device list)
- `src/window.rs` (measurement flow, no-signal flow, live tick, dialog)
- `src/widgets/measurement_panel.rs` (details text, popup, Current Signal)
- `VERSION`, `Cargo.toml`

**Part 2**
- new: `src/widgets/channel_report.rs`
**Part 2b**
- `src/services/nl80211.rs` (`bss_channel_info` refactor, cache docs;
  privileged trigger code tried and removed)
- `src/window.rs` (case-insensitive `merge_nl_scan`)
- `src/widgets/channel_report.rs` (`bar_lobes`, `signal_color`,
  `BandSync`, spotlight/marker drawing, signal-sorted tables)
- `src/main.rs` (row-hover CSS)

**Part 2c**
- `src/models/settings.rs` (window/sidebar size, last + recent projects)
- `src/window.rs` (startup project restore, `remember_project_path`,
  `rebuild_project_menu`, `open-recent` action, geometry save)
