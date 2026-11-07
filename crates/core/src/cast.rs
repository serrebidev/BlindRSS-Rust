//! Casting: discovery (slice 8, staged per the Caster-engine plan).
//!
//! BlindRSS implements no casting of its own in Python; the engine
//! comes from Caster (`core/casting.py` is the adapter). This Rust
//! module is the first stage of the shared caster-core: SSDP
//! discovery for Chromecast (DIAL) devices and mDNS-style device
//! records, with the response parsing kept pure and tested. The
//! CASTv2 channel lands next in this module.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct CastDevice {
    pub name: String,
    pub host: String,
    pub port: u16,
    /// "chromecast" | "dlna" | "sonos" | "roku" | "kodi" |
    /// "airplay" (RAOP; host+port are the RTSP endpoint).
    pub kind: String,
    pub model: String,
    /// Protocol endpoint when discovery resolved one (UPnP
    /// AVTransport control URL, Roku base URL); empty when
    /// host+port are enough.
    pub endpoint: String,
    /// Content types the receiver said it accepts (UPnP
    /// ConnectionManager GetProtocolInfo sinks); empty when
    /// it did not say, which callers must treat as unknown,
    /// never as "nothing" (Caster's rule).
    pub sinks: Vec<String>,
}

impl CastDevice {
    /// Caster `Device.supports_video`: a published sink list
    /// is believed outright; silence falls back to the kind.
    pub fn supports_video(&self) -> bool {
        if !self.sinks.is_empty() {
            return self.sinks.iter().any(|m| m.starts_with("video/"));
        }
        matches!(self.kind.as_str(), "chromecast" | "dlna" | "roku" | "kodi")
    }
}

impl CastDevice {
    /// Python `CastDevice.display_name`: "{name} [{protocol}]".
    pub fn display_name(&self) -> String {
        format!("{} [{}]", self.name, protocol_label(&self.kind))
    }

    /// Python `CastDevice.unique_id`: "{protocol}:{identifier}".
    /// The identifier is the protocol endpoint when discovery
    /// resolved one, else the host:port pair.
    pub fn unique_id(&self) -> String {
        let id = if self.endpoint.is_empty() {
            format!("{}:{}", self.host, self.port)
        } else {
            self.endpoint.clone()
        };
        format!("{}:{id}", protocol_label(&self.kind))
    }
}

/// Python `CastProtocol` display values per device kind. DLNA
/// and generic UPnP share one engine device kind ("dlna" here,
/// "upnp" in Python's kind map); both label as Python does for
/// a renderer found through UPnP.
pub fn protocol_label(kind: &str) -> &'static str {
    match kind {
        "chromecast" => "Chromecast",
        "airplay" => "AirPlay",
        "sonos" => "Sonos",
        "roku" => "Roku",
        "kodi" => "Kodi",
        _ => "DLNA",
    }
}

/// Whether the protocol accepts a seek (Python `CastingManager
/// ._seek`: every kind but Roku, which has no seek over ECP).
pub fn supports_seek(kind: &str) -> bool {
    matches!(kind, "chromecast" | "airplay" | "dlna" | "sonos" | "kodi")
}

/// Whether the protocol accepts a volume level (Python
/// `set_volume`: every kind but Roku).
pub fn supports_volume(kind: &str) -> bool {
    matches!(kind, "chromecast" | "airplay" | "dlna" | "sonos" | "kodi")
}

/// Python `caster_devices.looks_like_sonos`: a renderer whose
/// name (or maker/model text) mentions Sonos or a RINCON id is a
/// Sonos, however it announced itself — driving it as plain DLNA
/// or RAOP misses its grouping and transport quirks, so generic
/// discovery paths exclude it and the Sonos path owns it.
pub fn looks_like_sonos(name: &str, extra: &str) -> bool {
    let blob = format!("{name} {extra}").to_lowercase();
    blob.contains("sonos") || blob.contains("rincon")
}

/// Python `_hms`: seconds as "H:MM:SS" (UPnP REL_TIME, Sonos).
pub fn format_hms(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as i64;
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// Python `_parse_hms`: "H:MM:SS[.fff]" back to seconds.
pub fn parse_hms(text: &str) -> Option<f64> {
    let mut parts = text.trim().split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(0..60).contains(&minutes) {
        return None;
    }
    Some(hours as f64 * 3600.0 + minutes as f64 * 60.0 + seconds)
}

/// A protocol-neutral cast status snapshot (Python
/// `CastingManager.get_status`): every protocol's status is
/// normalised into this shape, with the clock position filling
/// in where a receiver cannot report one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CastStatus {
    pub position_seconds: Option<f64>,
    pub duration_seconds: Option<f64>,
    pub player_state: Option<String>,
    pub connected: bool,
    /// Only Chromecast reports a media session, so only it can
    /// positively detect a dropped session (Python's flag).
    pub supports_session_detection: bool,
}

impl CastStatus {
    /// The item has ended: an idle/stopped report after the
    /// session was live. Live relays also idle between loads,
    /// so callers gate this on `was_active` (see CastQueue).
    pub fn is_finished(&self) -> bool {
        matches!(self.player_state.as_deref(), Some("IDLE" | "STOPPED"))
    }
}

/// Position by the clock, for receivers that cannot report one
/// (Python `_clock_set` / `_clock_position`, and the AirPlay
/// `_AirSession.position`): a base position plus the elapsed
/// monotonic time while playing; frozen while paused.
#[derive(Debug, Clone)]
pub struct CastClock {
    state: Option<String>,
    base: f64,
    since: Option<std::time::Instant>,
}

impl Default for CastClock {
    fn default() -> Self {
        Self::new()
    }
}

impl CastClock {
    pub fn new() -> Self {
        CastClock {
            state: None,
            base: 0.0,
            since: None,
        }
    }

    /// Python `_clock_set(state, position=None)`: keep the
    /// current (extrapolated) position unless one is given.
    pub fn set(&mut self, state: &str, position: Option<f64>) {
        let current = self.position();
        self.base = position.unwrap_or(current.unwrap_or(0.0));
        self.state = Some(state.to_string());
        self.since = (state == "playing").then(std::time::Instant::now);
    }

    pub fn clear(&mut self) {
        self.state = None;
        self.since = None;
    }

    pub fn state(&self) -> Option<&str> {
        self.state.as_deref()
    }

    pub fn position(&self) -> Option<f64> {
        self.state.as_ref()?;
        let mut pos = self.base;
        if self.state.as_deref() == Some("playing")
            && let Some(since) = self.since
        {
            pos += since.elapsed().as_secs_f64();
        }
        Some(pos)
    }
}

/// One item a cast session can play (a resolved stream).
#[derive(Debug, Clone, PartialEq)]
pub struct CastQueueItem {
    pub url: String,
    pub title: String,
    pub mime: String,
}

/// What a status poll decided about the cast queue.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueAdvance {
    /// Keep playing the current item.
    None,
    /// The current item ended; play this next one.
    Next(CastQueueItem),
    /// The current item ended and the queue is exhausted.
    Finished,
}

/// The cast play queue (the local player's media-finished
/// advance, applied to a cast session): when the receiver
/// reports the current item finished — an idle/stopped state
/// after the item was seen playing, paused or buffering — the
/// next queued item is handed back for the caller to load.
#[derive(Debug, Clone)]
pub struct CastQueue {
    items: Vec<CastQueueItem>,
    index: Option<usize>,
    was_active: bool,
}

impl CastQueue {
    pub fn new(items: Vec<CastQueueItem>) -> Self {
        CastQueue {
            items,
            index: None,
            was_active: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Start (or restart) at the first item.
    pub fn start(&mut self) -> Option<&CastQueueItem> {
        self.index = if self.items.is_empty() { None } else { Some(0) };
        self.was_active = false;
        self.current()
    }

    pub fn current(&self) -> Option<&CastQueueItem> {
        self.index.and_then(|i| self.items.get(i))
    }

    pub fn current_index(&self) -> Option<usize> {
        self.index
    }

    /// Skip to the next item on the user's say-so.
    pub fn skip_next(&mut self) -> Option<&CastQueueItem> {
        let next = self.index? + 1;
        if next >= self.items.len() {
            self.index = None;
            return None;
        }
        self.index = Some(next);
        self.was_active = false;
        self.current()
    }

    /// Feed one status poll; advances the queue when the
    /// current item is finished (Python's player advances its
    /// queue on media-finished — for a cast session the
    /// receiver's idle report is that event).
    pub fn note_status(&mut self, status: &CastStatus) -> QueueAdvance {
        if self.index.is_none() {
            return QueueAdvance::None;
        }
        if let Some("PLAYING" | "PAUSED" | "BUFFERING") = status.player_state.as_deref() {
            self.was_active = true;
            return QueueAdvance::None;
        }
        if status.position_seconds.is_some_and(|p| p > 0.0) {
            self.was_active = true;
        }
        if !self.was_active || !status.is_finished() {
            return QueueAdvance::None;
        }
        self.was_active = false;
        match self.skip_next() {
            Some(item) => QueueAdvance::Next(item.clone()),
            None => QueueAdvance::Finished,
        }
    }
}

/// Parse one SSDP response (or NOTIFY) packet into a device, when
/// it is a Chromecast (its LOCATION points at a DIAL description
/// and the server/ST mentions Cast). Pure; fixture-tested.
pub fn parse_ssdp_packet(text: &str) -> Option<CastDevice> {
    let mut headers: HashMap<String, String> = HashMap::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let location = headers.get("location")?;
    // Chromecast DIAL descriptions live on port 8008 at /ssdp/device-desc.xml
    if !location.contains(":8008") && !location.contains("device-desc") {
        return None;
    }
    let hostport = location
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()?
        .to_string();
    // The parsed port is the DIAL description port; the cast
    // port below is always 8009.
    let host = match hostport.rsplit_once(':') {
        Some((h, _dial_port)) => h.to_string(),
        None => hostport,
    };
    let name = headers
        .get("x-friendly-name")
        .cloned()
        .unwrap_or_else(|| host.clone());
    Some(CastDevice {
        name,
        host,
        port: 8009, // CASTv2 port; the SSDP port is the DIAL port.
        kind: "chromecast".to_string(),
        model: headers.get("x-model-name").cloned().unwrap_or_default(),
        endpoint: String::new(),
        sinks: Vec::new(),
    })
}

/// Broadcast an SSDP M-SEARCH for DIAL devices and collect
/// Chromecast responses for `timeout`.
pub fn discover_chromecasts(timeout: Duration) -> Vec<CastDevice> {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return Vec::new();
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = socket.set_broadcast(true);
    let search = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: urn:dial-multiscreen-org:service:dial:1\r\n\r\n";
    if socket
        .send_to(search.as_bytes(), "239.255.255.250:1900")
        .is_err()
    {
        return Vec::new();
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut resends = SearchResends::new(timeout);
    let mut out: Vec<CastDevice> = Vec::new();
    let mut buf = [0u8; 4096];
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Ok(text) = std::str::from_utf8(&buf[..n])
                    && let Some(dev) = parse_ssdp_packet(text)
                    && !out.iter().any(|d| d.host == dev.host)
                {
                    out.push(dev);
                }
            }
            Err(_) => {
                if resends.due() {
                    let _ = socket.send_to(search.as_bytes(), "239.255.255.250:1900");
                }
                continue;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The other Caster protocols (caster_devices.py / caster_extras.py):
// DLNA/UPnP MediaRenderers, Sonos zone players, Roku ECP, and
// Kodi (mDNS). Discovery shares the SSDP machinery above; Kodi
// answers mDNS, not SSDP.
// ---------------------------------------------------------------------------

/// Fetch a UPnP device description and pull the friendly name
/// plus the AVTransport control URL (resolved against the
/// description's base). Hand-parsed: the documents are small
/// and regular.
pub fn parse_upnp_description(xml: &str, location: &str) -> Option<(String, String, String)> {
    let desc = parse_upnp_description_full(xml, location)?;
    Some((desc.name, desc.av_transport_url, desc.model))
}

fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

/// The five predefined XML entities, unescaped in one pass
/// (Python's description parser applies `html.unescape` to the
/// friendly name and manufacturer).
pub fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// A UPnP device description's cast-relevant fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UpnpDescription {
    pub name: String,
    pub manufacturer: String,
    pub model: String,
    pub av_transport_url: String,
    /// ConnectionManager control URL (GetProtocolInfo sinks
    /// live behind it); empty for renderers that do not
    /// publish the service.
    pub connection_manager_url: String,
    pub rendering_control_url: String,
    /// ZoneGroupTopology control URL (Sonos group state lives
    /// behind it; empty for other renderers).
    pub zone_group_topology_url: String,
}

fn resolve_control_url(control: &str, location: &str) -> String {
    if control.starts_with("http") {
        control.to_string()
    } else if let Some(stripped) = control.strip_prefix('/') {
        let authority = location
            .split("://")
            .nth(1)
            .and_then(|r| r.split('/').next())
            .unwrap_or("");
        let scheme = location.split("://").next().unwrap_or("http");
        format!("{scheme}://{authority}/{stripped}")
    } else {
        let base = location
            .rsplit_once('/')
            .map(|(b, _)| b.to_string())
            .unwrap_or_default();
        format!("{base}/{control}")
    }
}

/// Full description parse: friendly name, manufacturer and the
/// AVTransport / RenderingControl / ZoneGroupTopology control
/// URLs, each resolved against the description location the way
/// `caster_extras._upnp_fetch_control` resolves them.
pub fn parse_upnp_description_full(xml: &str, location: &str) -> Option<UpnpDescription> {
    let name = xml_unescape(&xml_tag(xml, "friendlyName")?);
    let manufacturer = xml_tag(xml, "manufacturer")
        .map(|m| xml_unescape(&m))
        .unwrap_or_default();
    let model = xml_tag(xml, "modelName").unwrap_or_default();
    let mut desc = UpnpDescription {
        name,
        manufacturer,
        model,
        ..Default::default()
    };
    let mut rest = xml;
    while let Some(pos) = rest.find("<service>") {
        let Some(rel_end) = rest[pos..].find("</service>") else {
            break;
        };
        let block_end = pos + rel_end;
        let block = &rest[pos..block_end];
        if let Some(control) = xml_tag(block, "controlURL") {
            let resolved = resolve_control_url(&control, location);
            if block.contains("AVTransport") && desc.av_transport_url.is_empty() {
                desc.av_transport_url = resolved;
            } else if block.contains("ConnectionManager") && desc.connection_manager_url.is_empty()
            {
                desc.connection_manager_url = resolved;
            } else if block.contains("RenderingControl") && desc.rendering_control_url.is_empty() {
                desc.rendering_control_url = resolved;
            } else if block.contains("ZoneGroupTopology") && desc.zone_group_topology_url.is_empty()
            {
                desc.zone_group_topology_url = resolved;
            }
        }
        rest = &rest[block_end..];
    }
    if desc.av_transport_url.is_empty() {
        return None;
    }
    Some(desc)
}

/// Content types a renderer accepts, from its
/// ConnectionManager's GetProtocolInfo (Caster
/// `upnp_sink_mimes`). Empty means "it did not say" —
/// unknown, not "nothing".
pub fn upnp_sink_mimes(connection_manager_url: &str) -> Vec<String> {
    if connection_manager_url.is_empty() {
        return Vec::new();
    }
    let Ok(xml) = crate::castupnp::soap_call(
        connection_manager_url,
        "urn:schemas-upnp-org:service:ConnectionManager:1",
        "GetProtocolInfo",
        "",
    ) else {
        return Vec::new();
    };
    let Some(sink) = xml_tag(&xml, "Sink") else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for entry in sink.split(',') {
        // protocol:network:contentFormat:additionalInfo
        let parts: Vec<&str> = entry.trim().split(':').collect();
        if parts.len() >= 3 && parts[2].contains('/') {
            let mime = parts[2]
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if !mime.is_empty() && !out.contains(&mime) {
                out.push(mime);
            }
        }
    }
    out
}

/// Caster repeats each SSDP search across the window
/// (SSDP_SEARCHES = 3, spaced timeout / (3 + 1) apart):
/// SSDP is lossy UDP multicast — one lost reply is one
/// device missing from the list, and a device that
/// already answered just answers again and is deduped.
pub(crate) const SSDP_SEARCHES: u32 = 3;

pub(crate) struct SearchResends {
    sent: u32,
    next: std::time::Instant,
    spacing: Duration,
}

impl SearchResends {
    pub(crate) fn new(timeout: Duration) -> Self {
        let spacing = timeout / (SSDP_SEARCHES + 1);
        Self {
            sent: 1, // the caller sends the first search itself
            next: std::time::Instant::now() + spacing,
            spacing,
        }
    }

    /// True (at most SSDP_SEARCHES - 1 times) when the next
    /// repeat is due.
    pub(crate) fn due(&mut self) -> bool {
        if self.sent < SSDP_SEARCHES && std::time::Instant::now() >= self.next {
            self.sent += 1;
            self.next += self.spacing;
            true
        } else {
            false
        }
    }
}

/// Caster's cross-protocol discovery merge (`_discover_sync`):
/// devices keyed by NAME, protocols applied in the caller's
/// fixed order (chromecast, airplay, upnp/dlna, roku, kodi —
/// Sonos is handled separately and wins outright). A later
/// same-name device replaces the incumbent ONLY when it
/// published sinks proving video and the incumbent cannot
/// show video — a TV answering AirPlay (audio-only RAOP) and
/// DLNA keeps its picture path; ties keep the earlier
/// protocol. Replacement keeps the incumbent's position.
pub fn merge_discovery_by_name(devices: Vec<CastDevice>) -> Vec<CastDevice> {
    let mut out: Vec<CastDevice> = Vec::new();
    for dev in devices {
        match out.iter_mut().find(|d| d.name == dev.name) {
            None => out.push(dev),
            Some(existing) => {
                if !dev.sinks.is_empty() && dev.supports_video() && !existing.supports_video() {
                    *existing = dev;
                }
            }
        }
    }
    out
}

/// One SSDP sweep for several search targets; returns raw
/// (ST, LOCATION, headers) hits for the protocol parsers.
fn ssdp_sweep(targets: &[&str], timeout: Duration) -> Vec<(String, String)> {
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return Vec::new();
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = socket.set_broadcast(true);
    for st in targets {
        let search = format!(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
        );
        let _ = socket.send_to(search.as_bytes(), "239.255.255.250:1900");
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut resends = SearchResends::new(timeout);
    let mut out: Vec<(String, String)> = Vec::new();
    let mut buf = [0u8; 8192];
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                    out.push((text.to_string(), String::new()));
                }
            }
            Err(_) => {
                if resends.due() {
                    for st in targets {
                        let search = format!(
                            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
                        );
                        let _ = socket.send_to(search.as_bytes(), "239.255.255.250:1900");
                    }
                }
                continue;
            }
        }
    }
    out
}

fn ssdp_headers(text: &str) -> HashMap<String, String> {
    let mut headers: HashMap<String, String> = HashMap::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    headers
}

fn http_get_text(url: &str) -> Option<String> {
    let mut resp = crate::http::agent().get(url).call().ok()?;
    resp.body_mut().read_to_string().ok()
}

/// Discover every supported cast device kind on the LAN.
/// Chromecasts come from the DIAL sweep above; this adds
/// DLNA renderers, Sonos players, Roku (ECP), and Kodi
/// (mDNS + configured hosts).
pub fn discover_all(timeout: Duration) -> Vec<CastDevice> {
    // Chromecasts: the SSDP DIAL sweep merged with the explicit
    // `_googlecast._tcp` mDNS browse (C02) — each finds devices
    // the other misses.
    let chromecasts = merge_chromecast_discovery(
        discover_chromecasts(timeout),
        discover_chromecasts_mdns(timeout),
    );
    // Per-protocol buckets, merged by name at the end in
    // Caster's fixed order (chromecast, airplay, upnp, roku,
    // kodi; Sonos last and winning outright).
    let mut sonos: Vec<CastDevice> = Vec::new();
    let mut dlna: Vec<CastDevice> = Vec::new();
    let mut roku: Vec<CastDevice> = Vec::new();
    let hits = ssdp_sweep(
        &[
            "urn:schemas-upnp-org:device:MediaRenderer:1",
            "urn:schemas-upnp-org:device:ZonePlayer:1",
            "roku:ecp",
        ],
        timeout,
    );
    let mut seen_locations: Vec<String> = Vec::new();
    for (packet, _) in &hits {
        let headers = ssdp_headers(packet);
        let Some(location) = headers.get("location") else {
            continue;
        };
        if seen_locations.contains(location) {
            continue;
        }
        seen_locations.push(location.clone());
        let st = headers.get("st").cloned().unwrap_or_default();
        let host = location
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .split('/')
            .next()
            .unwrap_or("")
            .split(':')
            .next()
            .unwrap_or("")
            .to_string();
        if st.contains("ZonePlayer") {
            if let Some(xml) = http_get_text(location)
                && let Some((name, control, model)) = parse_upnp_description(&xml, location)
            {
                sonos.push(CastDevice {
                    name,
                    host: host.clone(),
                    port: 1400,
                    kind: "sonos".to_string(),
                    model,
                    endpoint: control,
                    sinks: Vec::new(),
                });
            }
        } else if st.contains("MediaRenderer") {
            if let Some(xml) = http_get_text(location)
                && let Some(desc) = parse_upnp_description_full(&xml, location)
            {
                // A Sonos also answers MediaRenderer; Caster
                // excludes it from the generic list outright
                // (C04) — the ZonePlayer hit owns it.
                if looks_like_sonos(&desc.name, &desc.manufacturer) {
                    continue;
                }
                // The renderer's published sinks ride along:
                // they are the merge's video proof and the
                // honest capability record (Caster fetches
                // them the same way, per renderer).
                let sinks = upnp_sink_mimes(&desc.connection_manager_url);
                dlna.push(CastDevice {
                    name: desc.name,
                    host: host.clone(),
                    port: 0,
                    kind: "dlna".to_string(),
                    model: desc.model,
                    endpoint: desc.av_transport_url,
                    sinks,
                });
            }
        } else if st == "roku:ecp" {
            // Caster tries the device-info name tags in order.
            let name = http_get_text(&format!("http://{host}:8060/query/device-info"))
                .and_then(|xml| {
                    [
                        "friendly-device-name",
                        "user-device-name",
                        "default-device-name",
                        "model-name",
                    ]
                    .iter()
                    .find_map(|tag| xml_tag(&xml, tag))
                })
                .unwrap_or_else(|| format!("Roku {host}"));
            roku.push(CastDevice {
                name,
                host: host.clone(),
                port: 8060,
                kind: "roku".to_string(),
                model: String::new(),
                endpoint: format!("http://{host}:8060"),
                sinks: Vec::new(),
            });
        }
    }
    // Caster's fixed merge order, keyed by name.
    let mut merged: Vec<CastDevice> = Vec::new();
    merged.extend(chromecasts);
    merged.extend(discover_airplay(timeout));
    merged.extend(dlna);
    merged.extend(roku);
    merged.extend(discover_kodi(timeout));
    let mut out = merge_discovery_by_name(merged);
    out.extend(sonos);
    // Sonos merges last and wins by name (C04): a generic entry
    // carrying a Sonos player's name is the same device seen
    // through a worse protocol, and is dropped.
    let sonos_names: Vec<String> = out
        .iter()
        .filter(|d| d.kind == "sonos")
        .map(|d| d.name.to_lowercase())
        .collect();
    if !sonos_names.is_empty() {
        out.retain(|d| d.kind == "sonos" || !sonos_names.contains(&d.name.to_lowercase()));
    }
    out
}

/// AirPlay (RAOP) discovery: mDNS for `_raop._tcp`. Instance
/// names are "{device-id}@{display name}". The port is the
/// RAOP RTSP port (5000 on classic AirPort, dynamic on newer
/// receivers). Announcements Caster would not keep — pairing-
/// required receivers and Sonos announcements — are filtered
/// by `raop_supported` (C05).
pub fn discover_airplay(timeout: Duration) -> Vec<CastDevice> {
    airplay_devices_from_records(&mdns_browse_records("_raop._tcp", timeout))
}

/// Kodi discovery: mDNS for the JSON-RPC service plus the
/// configured hosts (config cast_kodi_hosts, not read here —
/// the app appends those itself).
pub fn discover_kodi(timeout: Duration) -> Vec<CastDevice> {
    let mut out = Vec::new();
    // mDNS PTR query for _kodi-jsonrpc._tcp and the older
    // _xbmc-jsonrpc._tcp, then SRV/A follow-ups are folded
    // into the same response set by responders.
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return out;
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    for service in ["_kodi-jsonrpc._tcp.local", "_xbmc-jsonrpc._tcp.local"] {
        let query = mdns_ptr_query(service);
        let _ = socket.send_to(&query, "224.0.0.251:5353");
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = [0u8; 8192];
    let mut names: Vec<(String, String, u16)> = Vec::new(); // (instance, host, port)
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                for (instance, host, port) in parse_mdns_response(&buf[..n]) {
                    if !names.iter().any(|(i, _, _)| *i == instance) {
                        names.push((instance, host, port));
                    }
                }
            }
            Err(_) => continue,
        }
    }
    for (instance, host, port) in names {
        let label = instance.split("._").next().unwrap_or("Kodi").to_string();
        out.push(CastDevice {
            name: if label.is_empty() {
                "Kodi".to_string()
            } else {
                label
            },
            host,
            port: if port == 0 { 8080 } else { port },
            kind: "kodi".to_string(),
            model: String::new(),
            endpoint: String::new(),
            sinks: Vec::new(),
        });
    }
    out
}

fn mdns_name(name: &str, out: &mut Vec<u8>) {
    for part in name.split('.') {
        out.push(part.len() as u8);
        out.extend_from_slice(part.as_bytes());
    }
    out.push(0);
}

/// Build an mDNS PTR query packet for a service name.
pub fn mdns_ptr_query(service: &str) -> Vec<u8> {
    let mut pkt = vec![0u8; 12]; // id=0, flags=0, qd=1
    pkt[4] = 0;
    pkt[5] = 1;
    mdns_name(service, &mut pkt);
    pkt.extend_from_slice(&12u16.to_be_bytes()); // PTR
    pkt.extend_from_slice(&1u16.to_be_bytes()); // IN
    pkt
}

/// Parse an mDNS response into (instance, host, port) triples
/// by joining PTR -> SRV -> A records. Tolerant by design:
/// anything malformed is skipped.
pub fn parse_mdns_response(pkt: &[u8]) -> Vec<(String, String, u16)> {
    fn read_name(pkt: &[u8], mut off: usize) -> Option<(String, usize)> {
        let mut labels = Vec::new();
        let mut jumped = None;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 30 || off >= pkt.len() {
                return None;
            }
            let len = pkt[off];
            if len == 0 {
                return Some((labels.join("."), jumped.unwrap_or(off + 1)));
            }
            if len & 0xC0 == 0xC0 {
                if off + 1 >= pkt.len() {
                    return None;
                }
                let ptr = (((len & 0x3F) as usize) << 8) | pkt[off + 1] as usize;
                if jumped.is_none() {
                    jumped = Some(off + 2);
                }
                off = ptr;
                continue;
            }
            let start = off + 1;
            let end = start + len as usize;
            if end > pkt.len() {
                return None;
            }
            labels.push(String::from_utf8_lossy(&pkt[start..end]).to_string());
            off = end;
        }
    }
    fn u16_at(pkt: &[u8], off: usize) -> Option<u16> {
        Some(u16::from_be_bytes([*pkt.get(off)?, *pkt.get(off + 1)?]))
    }
    if pkt.len() < 12 {
        return Vec::new();
    }
    let qd = u16_at(pkt, 4).unwrap_or(0) as usize;
    let an = u16_at(pkt, 6).unwrap_or(0) as usize;
    let ns = u16_at(pkt, 8).unwrap_or(0) as usize;
    let ar = u16_at(pkt, 10).unwrap_or(0) as usize;
    let mut off = 12usize;
    for _ in 0..qd {
        match read_name(pkt, off) {
            Some((_, next)) => off = next + 4,
            None => return Vec::new(),
        }
    }
    let mut ptrs: Vec<String> = Vec::new();
    let mut srvs: Vec<(String, String, u16)> = Vec::new();
    let mut addrs: Vec<(String, String)> = Vec::new();
    for _ in 0..(an + ns + ar) {
        let Some((name, next)) = read_name(pkt, off) else {
            break;
        };
        off = next;
        let (Some(rtype), Some(rdlen)) = (u16_at(pkt, off), u16_at(pkt, off + 8)) else {
            break;
        };
        let rdata = off + 10;
        match rtype {
            12 => {
                // PTR
                if let Some((target, _)) = read_name(pkt, rdata) {
                    ptrs.push(target);
                }
            }
            33 => {
                // SRV: priority/weight/port + target
                if let Some(port) = u16_at(pkt, rdata + 4)
                    && let Some((target, _)) = read_name(pkt, rdata + 6) {
                        srvs.push((name.clone(), target, port));
                    }
            }
            1
                // A
                if rdlen == 4 && rdata + 4 <= pkt.len() => {
                    addrs.push((
                        name.clone(),
                        format!(
                            "{}.{}.{}.{}",
                            pkt[rdata],
                            pkt[rdata + 1],
                            pkt[rdata + 2],
                            pkt[rdata + 3]
                        ),
                    ));
                }
            _ => {}
        }
        off = rdata + rdlen as usize;
    }
    let mut out = Vec::new();
    for instance in ptrs {
        if let Some((_, target, port)) = srvs.iter().find(|(n, _, _)| *n == instance) {
            let host = addrs
                .iter()
                .find(|(n, _)| n == target)
                .map(|(_, ip)| ip.clone())
                .unwrap_or_else(|| target.clone());
            out.push((instance, host, *port));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Record-level mDNS (Audit 8 Task 28, C02/C05): Google Cast
// discovery browses `_googlecast._tcp` explicitly (Caster: "the
// browser in pychromecast misses TVs that only answer mDNS"), and
// the AirPlay filter reads RAOP TXT auth/capabilities — both need
// TXT records, which the triple parser above discards.
// ---------------------------------------------------------------------------

/// One mDNS service instance with its SRV target/port, A
/// addresses and TXT properties joined.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MdnsServiceRecord {
    /// Full instance name as the PTR carried it.
    pub instance: String,
    /// SRV target host name.
    pub target: String,
    /// SRV port (0 when no SRV record was seen).
    pub port: u16,
    /// A addresses for the target, in arrival order.
    pub addresses: Vec<String>,
    /// TXT key/value pairs (a bare key carries an empty value).
    pub txt: Vec<(String, String)>,
}

impl MdnsServiceRecord {
    /// The instance label: the part before the service suffix.
    pub fn label(&self) -> String {
        self.instance
            .split("._")
            .next()
            .unwrap_or(&self.instance)
            .to_string()
    }

    /// Python `caster_extras.mdns_host`: prefer an IPv4 address,
    /// fall back to any address, then to the SRV target name.
    pub fn host(&self) -> String {
        self.addresses
            .iter()
            .find(|a| !a.contains(':'))
            .cloned()
            .or_else(|| self.addresses.first().cloned())
            .unwrap_or_else(|| self.target.clone())
    }

    pub fn txt_get(&self, key: &str) -> Option<&str> {
        self.txt
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

fn parse_txt_rdata(rdata: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < rdata.len() {
        let len = rdata[off] as usize;
        let start = off + 1;
        let end = start + len;
        if end > rdata.len() {
            break;
        }
        let text = String::from_utf8_lossy(&rdata[start..end]).to_string();
        match text.split_once('=') {
            Some((k, v)) => out.push((k.trim().to_string(), v.to_string())),
            None => {
                if !text.is_empty() {
                    out.push((text, String::new()));
                }
            }
        }
        off = end;
    }
    out
}

/// Parse an mDNS packet into service records (PTR -> SRV/TXT/A
/// joined by name). Tolerant by design, like the triple parser.
pub fn parse_mdns_records(pkt: &[u8]) -> Vec<MdnsServiceRecord> {
    fn read_name(pkt: &[u8], mut off: usize) -> Option<(String, usize)> {
        let mut labels = Vec::new();
        let mut jumped = None;
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 30 || off >= pkt.len() {
                return None;
            }
            let len = pkt[off];
            if len == 0 {
                return Some((labels.join("."), jumped.unwrap_or(off + 1)));
            }
            if len & 0xC0 == 0xC0 {
                if off + 1 >= pkt.len() {
                    return None;
                }
                let ptr = (((len & 0x3F) as usize) << 8) | pkt[off + 1] as usize;
                if jumped.is_none() {
                    jumped = Some(off + 2);
                }
                off = ptr;
                continue;
            }
            let start = off + 1;
            let end = start + len as usize;
            if end > pkt.len() {
                return None;
            }
            labels.push(String::from_utf8_lossy(&pkt[start..end]).to_string());
            off = end;
        }
    }
    fn u16_at(pkt: &[u8], off: usize) -> Option<u16> {
        Some(u16::from_be_bytes([*pkt.get(off)?, *pkt.get(off + 1)?]))
    }
    if pkt.len() < 12 {
        return Vec::new();
    }
    let qd = u16_at(pkt, 4).unwrap_or(0) as usize;
    let an = u16_at(pkt, 6).unwrap_or(0) as usize;
    let ns = u16_at(pkt, 8).unwrap_or(0) as usize;
    let ar = u16_at(pkt, 10).unwrap_or(0) as usize;
    let mut off = 12usize;
    for _ in 0..qd {
        match read_name(pkt, off) {
            Some((_, next)) => off = next + 4,
            None => return Vec::new(),
        }
    }
    let mut ptrs: Vec<String> = Vec::new();
    let mut srvs: Vec<(String, String, u16)> = Vec::new();
    let mut txts: Vec<(String, Vec<(String, String)>)> = Vec::new();
    let mut addrs: Vec<(String, String)> = Vec::new();
    for _ in 0..(an + ns + ar) {
        let Some((name, next)) = read_name(pkt, off) else {
            break;
        };
        off = next;
        let (Some(rtype), Some(rdlen)) = (u16_at(pkt, off), u16_at(pkt, off + 8)) else {
            break;
        };
        let rdata = off + 10;
        if rdata + rdlen as usize > pkt.len() && rtype != 12 && rtype != 33 {
            break;
        }
        match rtype {
            12 => {
                if let Some((target, _)) = read_name(pkt, rdata) {
                    ptrs.push(target);
                }
            }
            33 => {
                if let Some(port) = u16_at(pkt, rdata + 4)
                    && let Some((target, _)) = read_name(pkt, rdata + 6)
                {
                    srvs.push((name.clone(), target, port));
                }
            }
            16 => {
                let end = (rdata + rdlen as usize).min(pkt.len());
                txts.push((name.clone(), parse_txt_rdata(&pkt[rdata..end])));
            }
            1 if rdlen == 4 && rdata + 4 <= pkt.len() => {
                addrs.push((
                    name.clone(),
                    format!(
                        "{}.{}.{}.{}",
                        pkt[rdata],
                        pkt[rdata + 1],
                        pkt[rdata + 2],
                        pkt[rdata + 3]
                    ),
                ));
            }
            _ => {}
        }
        off = rdata + rdlen as usize;
    }
    let mut out: Vec<MdnsServiceRecord> = Vec::new();
    for instance in ptrs {
        let Some((_, target, port)) = srvs.iter().find(|(n, _, _)| *n == instance) else {
            continue;
        };
        let mut record = MdnsServiceRecord {
            instance: instance.clone(),
            target: target.clone(),
            port: *port,
            addresses: addrs
                .iter()
                .filter(|(n, _)| n == target)
                .map(|(_, ip)| ip.clone())
                .collect(),
            txt: txts
                .iter()
                .find(|(n, _)| *n == instance)
                .map(|(_, t)| t.clone())
                .unwrap_or_default(),
        };
        if let Some(existing) = out.iter_mut().find(|r| r.instance == record.instance) {
            // Announcements arrive in fragments; merge them.
            for a in record.addresses.drain(..) {
                if !existing.addresses.contains(&a) {
                    existing.addresses.push(a);
                }
            }
            if existing.txt.is_empty() {
                existing.txt = record.txt;
            }
        } else {
            out.push(record);
        }
    }
    out
}

/// Browse one mDNS service for `timeout`, merging the records
/// from every response packet.
pub fn mdns_browse_records(service: &str, timeout: Duration) -> Vec<MdnsServiceRecord> {
    let mut merged: Vec<MdnsServiceRecord> = Vec::new();
    let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
        return merged;
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(400)));
    let query = mdns_ptr_query(&format!("{service}.local"));
    let _ = socket.send_to(&query, "224.0.0.251:5353");
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = [0u8; 8192];
    while std::time::Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                for record in parse_mdns_records(&buf[..n]) {
                    match merged.iter_mut().find(|r| r.instance == record.instance) {
                        Some(existing) => {
                            if existing.port == 0 {
                                existing.port = record.port;
                            }
                            if existing.target.is_empty() {
                                existing.target = record.target.clone();
                            }
                            for a in &record.addresses {
                                if !existing.addresses.contains(a) {
                                    existing.addresses.push(a.clone());
                                }
                            }
                            if existing.txt.is_empty() {
                                existing.txt = record.txt.clone();
                            }
                        }
                        None => merged.push(record),
                    }
                }
            }
            Err(_) => continue,
        }
    }
    merged
}

/// Google Cast devices from `_googlecast._tcp` records (C02).
/// The display name is the TXT `fn` friendly name when the
/// receiver publishes one, else the instance label; a name two
/// hosts both claim is disambiguated with the host, the way
/// Caster keys its found dict (`"{fn} ({host})"`).
pub fn chromecast_devices_from_records(records: &[MdnsServiceRecord]) -> Vec<CastDevice> {
    let mut out: Vec<CastDevice> = Vec::new();
    for record in records {
        let host = record.host();
        if host.is_empty() {
            continue;
        }
        let mut name = record
            .txt_get("fn")
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| record.label());
        if name.is_empty() {
            name = host.clone();
        }
        if out.iter().any(|d| d.name == name && d.host != host) {
            name = format!("{name} ({host})");
        }
        if out.iter().any(|d| d.host == host) {
            continue;
        }
        out.push(CastDevice {
            name,
            host,
            port: if record.port == 0 { 8009 } else { record.port },
            kind: "chromecast".to_string(),
            model: record
                .txt_get("md")
                .filter(|m| !m.is_empty())
                .unwrap_or("Chromecast")
                .to_string(),
            endpoint: String::new(),
            sinks: Vec::new(),
        });
    }
    out
}

/// Merge the SSDP DIAL results with the mDNS results (C02): one
/// entry per host, friendly names preserved — the mDNS TXT name
/// replaces an SSDP entry that only had its host to go by, and
/// the mDNS model fills an empty SSDP model.
pub fn merge_chromecast_discovery(ssdp: Vec<CastDevice>, mdns: Vec<CastDevice>) -> Vec<CastDevice> {
    let mut out = ssdp;
    for dev in mdns {
        match out.iter_mut().find(|d| d.host == dev.host) {
            Some(existing) => {
                if existing.name == existing.host && dev.name != dev.host {
                    existing.name = dev.name;
                }
                if existing.model.is_empty() {
                    existing.model = dev.model;
                }
            }
            None => out.push(dev),
        }
    }
    out
}

/// Chromecast discovery over mDNS only: browse
/// `_googlecast._tcp` explicitly, as Caster does — TVs that
/// never answer the SSDP DIAL sweep still announce here.
pub fn discover_chromecasts_mdns(timeout: Duration) -> Vec<CastDevice> {
    chromecast_devices_from_records(&mdns_browse_records("_googlecast._tcp", timeout))
}

/// Whether an RAOP announcement is a receiver Caster would keep
/// (C05): Python keeps an AirPlay config only when it carries an
/// RAOP service whose pairing requirement is NotNeeded, and the
/// name/device does not look like Sonos. Pairing itself stays
/// unsupported (Python does not pair either); the parity gap is
/// the filtering. The TXT signals read here are the RAOP `pw`
/// flag (password/pairing required) and the Sonos identity in
/// the name or the `am` model string.
pub fn raop_supported(record: &MdnsServiceRecord) -> bool {
    let label = record.label();
    let name = label
        .split_once('@')
        .map(|(_, n)| n.trim())
        .unwrap_or(&label);
    let model = record.txt_get("am").unwrap_or("");
    if looks_like_sonos(name, model) {
        return false;
    }
    if record
        .txt_get("pw")
        .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1")
    {
        return false;
    }
    true
}

/// AirPlay devices from `_raop._tcp` records, filtered by
/// [`raop_supported`].
pub fn airplay_devices_from_records(records: &[MdnsServiceRecord]) -> Vec<CastDevice> {
    let mut out = Vec::new();
    for record in records {
        if !raop_supported(record) {
            continue;
        }
        let label = record.label();
        let name = label
            .split_once('@')
            .map(|(_, n)| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| label.clone());
        let host = record.host();
        if host.is_empty() {
            continue;
        }
        out.push(CastDevice {
            name,
            host,
            port: if record.port == 0 { 5000 } else { record.port },
            kind: "airplay".to_string(),
            model: record.txt_get("am").unwrap_or("").to_string(),
            endpoint: String::new(),
            sinks: Vec::new(),
        });
    }
    out
}
