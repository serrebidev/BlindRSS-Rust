//! Playback and transport control for the non-Cast protocols
//! (Caster's caster_extras.py / caster_devices.py): DLNA/UPnP
//! AVTransport (also Sonos, whose renderer is an AVTransport),
//! Roku External Control Protocol, and Kodi JSON-RPC.
//!
//! The control surface mirrors Python's `CastingManager`:
//! every protocol answers pause / resume / stop / seek /
//! volume / status through the same functions here, with the
//! protocol's real limits kept (Roku has no seek or volume
//! over ECP — its play/pause is the Play keypress, toggled by
//! the caller's clock state, exactly as Python gates it).

use crate::cast::{CastDevice, CastStatus, format_hms, parse_hms};

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A DIDL-Lite metadata document for one media item (what
/// AVTransport renderers expect alongside the URI).
fn didl_metadata(url: &str, title: &str, mime: &str) -> String {
    let class = if mime.starts_with("video/") {
        "object.item.videoItem"
    } else {
        "object.item.audioItem.musicTrack"
    };
    format!(
        "<DIDL-Lite xmlns=\"urn:schemas-microsoft-com:metadata-1-0/DIDL-Lite/\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:upnp=\"urn:schemas-upnp-org:metadata-1-0/upnp/\">\
         <item id=\"1\" parentID=\"0\" restricted=\"1\">\
         <dc:title>{}</dc:title><upnp:class>{}</upnp:class>\
         <res protocolInfo=\"http-get:*:{}:DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000\">{}</res>\
         </item></DIDL-Lite>",
        xml_escape(title),
        class,
        xml_escape(mime),
        xml_escape(url)
    )
}

fn soap_envelope(service_type: &str, action: &str, args: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body><u:{action} xmlns:u=\"{service_type}\">{args}</u:{action}></s:Body>\
         </s:Envelope>"
    )
}

fn soap_call(
    control_url: &str,
    service_type: &str,
    action: &str,
    args: &str,
) -> Result<String, String> {
    let mut resp = crate::http::agent()
        .post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPACTION", &format!("\"{service_type}#{action}\""))
        .send(soap_envelope(service_type, action, args))
        .map_err(|e| format!("{action} failed: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("read failed: {e}"))?;
    if text.contains("Fault") || text.contains("errorCode") {
        return Err(format!("renderer rejected {action}"));
    }
    Ok(text)
}

fn soap_action(
    control_url: &str,
    service_type: &str,
    action: &str,
    args: &str,
) -> Result<String, String> {
    soap_call(control_url, service_type, action, args)
}

const AVTRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const RENDERING_CONTROL: &str = "urn:schemas-upnp-org:service:RenderingControl:1";

fn endpoint_of(device: &CastDevice) -> Result<&str, String> {
    if device.endpoint.is_empty() {
        Err(format!("{} has no AVTransport endpoint", device.name))
    } else {
        Ok(&device.endpoint)
    }
}

fn avt(device: &CastDevice, action: &str, args: &str) -> Result<String, String> {
    soap_action(endpoint_of(device)?, AVTRANSPORT, action, args)
}

/// The RenderingControl URL for a renderer (Python
/// `upnp_set_volume`: the AVTransport path with the service
/// name swapped).
pub fn rendering_control_url(avtransport_url: &str) -> String {
    avtransport_url.replace("AVTransport", "RenderingControl")
}

/// Map a UPnP CurrentTransportState onto the protocol-neutral
/// player states (Python `_upnp_status`).
pub fn map_upnp_state(raw: &str) -> String {
    match raw {
        "PAUSED_PLAYBACK" => "PAUSED",
        "TRANSITIONING" => "BUFFERING",
        "STOPPED" | "NO_MEDIA_PRESENT" => "IDLE",
        other => other,
    }
    .to_string()
}

fn xml_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

/// Parse a GetPositionInfo reply: (position, duration) in
/// seconds from its RelTime / TrackDuration HMS fields.
pub fn parse_position_info(xml: &str) -> (Option<f64>, Option<f64>) {
    (
        xml_text(xml, "RelTime").and_then(|t| parse_hms(&t)),
        xml_text(xml, "TrackDuration").and_then(|t| parse_hms(&t)),
    )
}

/// Parse a GetTransportInfo reply into the neutral state.
pub fn parse_transport_state(xml: &str) -> Option<String> {
    xml_text(xml, "CurrentTransportState").map(|s| map_upnp_state(&s))
}

/// Play on a DLNA MediaRenderer or a Sonos player:
/// SetAVTransportURI then Play. A MusicCast receiver is first
/// switched to its network/server input (C06, Python
/// `_upnp_push`); a Sonos member is routed to its group
/// coordinator (C04).
pub fn play_upnp(device: &CastDevice, url: &str, title: &str, mime: &str) -> Result<(), String> {
    let effective = sonos_effective(device);
    play_upnp_on(&effective, url, title, mime)
}

fn play_upnp_on(device: &CastDevice, url: &str, title: &str, mime: &str) -> Result<(), String> {
    if device.kind == "dlna" {
        musiccast_prepare(device);
    }
    let metadata = didl_metadata(url, title, mime);
    avt(
        device,
        "SetAVTransportURI",
        &format!(
            "<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI>\
             <CurrentURIMetaData>{}</CurrentURIMetaData>",
            xml_escape(url),
            xml_escape(&metadata)
        ),
    )?;
    avt(device, "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>")?;
    Ok(())
}

/// Play from `start_seconds` (Python `_play_upnp`): renderers
/// take no start position with the URL, so once the renderer
/// reports PLAYING (polled up to 8 s) a resume seek follows;
/// a refused seek is not a playback failure.
pub fn play_upnp_at(
    device: &CastDevice,
    url: &str,
    title: &str,
    mime: &str,
    start_seconds: f64,
) -> Result<(), String> {
    let effective = sonos_effective(device);
    play_upnp_on(&effective, url, title, mime)?;
    if start_seconds > 0.0 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            match upnp_status(&effective) {
                Ok(s) if s.player_state.as_deref() == Some("PLAYING") => break,
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(500)),
                Err(_) => break,
            }
        }
        let _ = seek_upnp(&effective, start_seconds);
    }
    Ok(())
}

pub fn pause_upnp(device: &CastDevice) -> Result<(), String> {
    avt(device, "Pause", "<InstanceID>0</InstanceID>").map(|_| ())
}

pub fn resume_upnp(device: &CastDevice) -> Result<(), String> {
    avt(device, "Play", "<InstanceID>0</InstanceID><Speed>1</Speed>").map(|_| ())
}

pub fn stop_upnp(device: &CastDevice) -> Result<(), String> {
    // Sonos rejects stop sent to a group member that is not the
    // coordinator, so stop routes like play does (C04).
    let effective = sonos_effective(device);
    avt(&effective, "Stop", "<InstanceID>0</InstanceID>").map(|_| ())
}

/// Seek to an absolute position (AVTransport Seek, REL_TIME).
pub fn seek_upnp(device: &CastDevice, seconds: f64) -> Result<(), String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Seek position must be a finite, nonnegative number".into());
    }
    avt(
        device,
        "Seek",
        &format!(
            "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>{}</Target>",
            format_hms(seconds)
        ),
    )
    .map(|_| ())
}

/// Set volume 0..=100 through RenderingControl (Python
/// `upnp_set_volume`; Sonos goes through the same service).
pub fn set_volume_upnp(device: &CastDevice, percent: u32) -> Result<(), String> {
    let url = rendering_control_url(endpoint_of(device)?);
    soap_call(
        &url,
        RENDERING_CONTROL,
        "SetVolume",
        &format!(
            "<InstanceID>0</InstanceID><Channel>Master</Channel>\
             <DesiredVolume>{}</DesiredVolume>",
            percent.min(100)
        ),
    )
    .map(|_| ())
}

/// The renderer's status: GetPositionInfo for the position,
/// GetTransportInfo for the state (Python `_upnp_status`;
/// Sonos answers the same AVTransport actions).
pub fn upnp_status(device: &CastDevice) -> Result<CastStatus, String> {
    let info = avt(device, "GetPositionInfo", "<InstanceID>0</InstanceID>")?;
    let (position, duration) = parse_position_info(&info);
    let state = avt(device, "GetTransportInfo", "<InstanceID>0</InstanceID>")
        .ok()
        .and_then(|xml| parse_transport_state(&xml));
    Ok(CastStatus {
        position_seconds: position,
        duration_seconds: duration,
        player_state: state,
        connected: true,
        supports_session_detection: false,
    })
}

/// The Roku Media Player channel id (Caster
/// `ROKU_MEDIA_PLAYER`).
pub const ROKU_MEDIA_PLAYER: &str = "2213";

/// Form-encode one query value the way Python's
/// `urllib.parse.urlencode` (quote_plus) does: alphanumerics and
/// `_.-~` pass, a space becomes `+`, everything else is
/// percent-escaped.
pub fn form_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn roku_audio_format(mime: &str) -> &'static str {
    match mime {
        "audio/wav" => "wav",
        "audio/aac" => "aac",
        "audio/flac" => "flac",
        _ => "mp3",
    }
}

fn roku_video_format(mime: &str) -> &'static str {
    match mime {
        "application/vnd.apple.mpegurl" => "hls",
        _ => "mp4",
    }
}

/// The exact ECP launch path Caster sends (C03,
/// `caster_devices.roku_play`): launch the Roku Media Player
/// with `t` (a/v), `u` (the URL) and the audio or video
/// name/format pair for the media's probed MIME. An HTTP success
/// alone never proved media loaded — the query IS the contract.
pub fn roku_launch_path(url: &str, title: &str, mime: &str) -> String {
    let mut params = String::new();
    if mime.starts_with("audio/") {
        params.push_str("t=a");
        params.push_str(&format!("&u={}", form_encode(url)));
        params.push_str(&format!("&audioName={}", form_encode(title)));
        params.push_str(&format!("&audioFormat={}", roku_audio_format(mime)));
    } else {
        params.push_str("t=v");
        params.push_str(&format!("&u={}", form_encode(url)));
        params.push_str(&format!("&videoName={}", form_encode(title)));
        params.push_str(&format!("&videoFormat={}", roku_video_format(mime)));
    }
    format!("/launch/{ROKU_MEDIA_PLAYER}?{params}")
}

/// Play on a Roku through the External Control Protocol.
pub fn play_roku(device: &CastDevice, url: &str, title: &str, mime: &str) -> Result<(), String> {
    let launch = format!(
        "{}{}",
        roku_base(device),
        roku_launch_path(url, title, mime)
    );
    crate::http::agent()
        .post(&launch)
        .send_empty()
        .map_err(|e| format!("Roku launch failed: {e}"))?;
    Ok(())
}

fn roku_base(device: &CastDevice) -> String {
    if device.endpoint.is_empty() {
        format!("http://{}:8060", device.host)
    } else {
        device.endpoint.clone()
    }
}

/// One ECP keypress (Python `roku_key`).
pub fn roku_key(device: &CastDevice, key: &str) -> Result<(), String> {
    crate::http::agent()
        .post(&format!("{}/keypress/{key}", roku_base(device)))
        .send_empty()
        .map_err(|e| format!("Roku keypress failed: {e}"))?;
    Ok(())
}

/// Stop on a Roku: ECP has no stop, going Home leaves the
/// media player (Python `roku_stop`).
pub fn stop_roku(device: &CastDevice) -> Result<(), String> {
    roku_key(device, "Home")
}

fn kodi_base(device: &CastDevice) -> String {
    if device.endpoint.is_empty() {
        format!("http://{}:{}", device.host, device.port)
    } else {
        device.endpoint.trim_end_matches('/').to_string()
    }
}

/// One Kodi JSON-RPC call (Python `_kodi_rpc`): a JSON-RPC
/// error inside an HTTP 200 is a refusal, not a result.
pub fn kodi_rpc(
    device: &CastDevice,
    method: &str,
    params: serde_json::Value,
    user: &str,
    password: &str,
) -> Result<serde_json::Value, String> {
    let base = format!("{}/jsonrpc", kodi_base(device));
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let req = crate::http::agent()
        .post(&base)
        .header("Content-Type", "application/json");
    let req = if !user.is_empty() {
        let token = base64_encode(&format!("{user}:{password}"));
        req.header("Authorization", &format!("Basic {token}"))
    } else {
        req
    };
    let mut resp = req
        .send(body.to_string())
        .map_err(|e| format!("Kodi request failed: {e}"))?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("read failed: {e}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Kodi reply unparsable: {e}"))?;
    if let Some(error) = value.get("error") {
        let detail = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(format!("Kodi refused {method}: {detail}"));
    }
    Ok(value)
}

fn kodi_player_id(device: &CastDevice, user: &str, password: &str) -> Result<Option<i64>, String> {
    let reply = kodi_rpc(
        device,
        "Player.GetActivePlayers",
        serde_json::json!({}),
        user,
        password,
    )?;
    Ok(reply
        .get("result")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .and_then(|p| p.get("playerid"))
        .and_then(|v| v.as_i64()))
}

/// Play on Kodi through its JSON-RPC API (Player.Open).
/// Credentials come from config (cast_kodi_user /
/// cast_kodi_password) via the caller.
pub fn play_kodi(device: &CastDevice, url: &str, user: &str, password: &str) -> Result<(), String> {
    kodi_rpc(
        device,
        "Player.Open",
        serde_json::json!({"item": {"file": url}}),
        user,
        password,
    )
    .map(|_| ())
}

/// Play on Kodi from `start_seconds` (Python `_play_kodi`:
/// once a player exists — polled up to 8 s — seek to it).
pub fn play_kodi_at(
    device: &CastDevice,
    url: &str,
    start_seconds: f64,
    user: &str,
    password: &str,
) -> Result<(), String> {
    play_kodi(device, url, user, password)?;
    if start_seconds > 0.0 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        while std::time::Instant::now() < deadline {
            match kodi_player_id(device, user, password) {
                Ok(Some(_)) => {
                    let _ = seek_kodi(device, start_seconds, user, password);
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(500)),
                Err(_) => break,
            }
        }
    }
    Ok(())
}

fn kodi_play_pause(
    device: &CastDevice,
    play: bool,
    user: &str,
    password: &str,
) -> Result<(), String> {
    let Some(player) = kodi_player_id(device, user, password)? else {
        return Ok(());
    };
    kodi_rpc(
        device,
        "Player.PlayPause",
        serde_json::json!({"playerid": player, "play": play}),
        user,
        password,
    )
    .map(|_| ())
}

pub fn pause_kodi(device: &CastDevice, user: &str, password: &str) -> Result<(), String> {
    kodi_play_pause(device, false, user, password)
}

pub fn resume_kodi(device: &CastDevice, user: &str, password: &str) -> Result<(), String> {
    kodi_play_pause(device, true, user, password)
}

pub fn stop_kodi(device: &CastDevice, user: &str, password: &str) -> Result<(), String> {
    let Some(player) = kodi_player_id(device, user, password)? else {
        return Ok(());
    };
    kodi_rpc(
        device,
        "Player.Stop",
        serde_json::json!({"playerid": player}),
        user,
        password,
    )
    .map(|_| ())
}

/// Seek on Kodi (Python `_kodi_seek`: an h/m/s/ms time value).
pub fn seek_kodi(
    device: &CastDevice,
    seconds: f64,
    user: &str,
    password: &str,
) -> Result<(), String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Seek position must be a finite, nonnegative number".into());
    }
    let Some(player) = kodi_player_id(device, user, password)? else {
        return Ok(());
    };
    let total = seconds as i64;
    kodi_rpc(
        device,
        "Player.Seek",
        serde_json::json!({
            "playerid": player,
            "value": {"time": {
                "hours": total / 3600,
                "minutes": (total % 3600) / 60,
                "seconds": total % 60,
                "milliseconds": ((seconds - total as f64) * 1000.0) as i64,
            }},
        }),
        user,
        password,
    )
    .map(|_| ())
}

/// Set Kodi's volume 0..=100 (Python `kodi_set_volume`:
/// Application.SetVolume).
pub fn set_volume_kodi(
    device: &CastDevice,
    percent: u32,
    user: &str,
    password: &str,
) -> Result<(), String> {
    kodi_rpc(
        device,
        "Application.SetVolume",
        serde_json::json!({"volume": percent.min(100)}),
        user,
        password,
    )
    .map(|_| ())
}

fn kodi_time_seconds(clock: &serde_json::Value) -> f64 {
    let get = |k: &str| clock.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    get("hours") * 3600.0 + get("minutes") * 60.0 + get("seconds") + get("milliseconds") / 1000.0
}

/// Parse a Player.GetProperties result into the neutral
/// status (Python `_kodi_status`: speed 0 is paused).
pub fn parse_kodi_status(result: &serde_json::Value) -> CastStatus {
    let position = result.get("time").map(kodi_time_seconds);
    let duration = result
        .get("totaltime")
        .map(kodi_time_seconds)
        .filter(|d| *d > 0.0);
    let state = match result.get("speed").and_then(|v| v.as_f64()) {
        Some(0.0) => "PAUSED",
        Some(_) => "PLAYING",
        None => "IDLE",
    };
    CastStatus {
        position_seconds: position,
        duration_seconds: duration,
        player_state: Some(state.to_string()),
        connected: true,
        supports_session_detection: false,
    }
}

pub fn kodi_status(device: &CastDevice, user: &str, password: &str) -> Result<CastStatus, String> {
    let Some(player) = kodi_player_id(device, user, password)? else {
        return Ok(CastStatus {
            player_state: Some("IDLE".into()),
            connected: true,
            ..Default::default()
        });
    };
    let reply = kodi_rpc(
        device,
        "Player.GetProperties",
        serde_json::json!({"playerid": player, "properties": ["time", "totaltime", "speed"]}),
        user,
        password,
    )?;
    Ok(parse_kodi_status(
        reply.get("result").unwrap_or(&serde_json::Value::Null),
    ))
}

// ---------------------------------------------------------------------------
// MusicCast input preparation (C06; caster_devices.py's YXC
// family, called from core/casting.py's `_upnp_push`)
// ---------------------------------------------------------------------------

/// One YXC call. `None` on any transport failure or a non-zero
/// MusicCast response code (Python `yxc_try` semantics: callers
/// decorate a cast that works regardless, so failure is an empty
/// answer, never an error).
fn yxc_try(host: &str, path: &str, timeout: std::time::Duration) -> Option<serde_json::Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into();
    let mut resp = agent
        .get(&format!("http://{host}/YamahaExtendedControl/v1/{path}"))
        .header("User-Agent", crate::castmedia::CASTER_UA)
        .call()
        .ok()?;
    let text = resp.body_mut().read_to_string().ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    match value.get("response_code").and_then(|c| c.as_i64()) {
        Some(0) | None => Some(value),
        Some(_) => None,
    }
}

/// True if the device speaks MusicCast (Python `yxc_available`:
/// a real `main/getStatus` answer within 2 s).
pub fn yxc_available(host: &str) -> bool {
    yxc_try(host, "main/getStatus", std::time::Duration::from_secs(2)).is_some()
}

/// Whether the main zone advertises a function in its feature
/// list (Python `yxc_can`).
fn yxc_can(host: &str, func: &str) -> bool {
    let Some(features) = yxc_try(
        host,
        "system/getFeatures",
        std::time::Duration::from_secs(8),
    ) else {
        return false;
    };
    features
        .get("zone")
        .and_then(|z| z.as_array())
        .and_then(|zones| {
            zones
                .iter()
                .find(|z| z.get("id").and_then(|i| i.as_str()) == Some("main"))
        })
        .and_then(|zone| zone.get("func_list"))
        .and_then(|f| f.as_array())
        .is_some_and(|funcs| funcs.iter().any(|f| f.as_str() == Some(func)))
}

/// Switch a zone's input, preparing it first when the unit says
/// preparation is required (Python `yxc_set_input` /
/// `yxc_prepare_input`).
pub fn yxc_set_input(host: &str, input: &str) -> bool {
    if yxc_can(host, "prepare_input_change") {
        let _ = yxc_try(
            host,
            &format!("main/prepareInputChange?input={input}"),
            std::time::Duration::from_secs(4),
        );
    }
    yxc_try(
        host,
        &format!("main/setInput?input={input}"),
        std::time::Duration::from_secs(4),
    )
    .is_some()
}

/// The `_upnp_push` preparation for one host: when the renderer
/// is a MusicCast unit, switch it to the network/server input
/// before the transport push. Best-effort by design.
pub fn musiccast_prepare_host(host: &str) {
    if yxc_available(host) {
        let _ = yxc_set_input(host, "server");
    }
}

/// MusicCast preparation for a renderer device (Python
/// `upnp_host(control_url)`: the control URL's host part —
/// YXC listens on the device's plain HTTP port).
pub fn musiccast_prepare(device: &CastDevice) {
    let host = url::Url::parse(&device.endpoint)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| device.host.clone());
    if !host.is_empty() {
        musiccast_prepare_host(&host);
    }
}

// ---------------------------------------------------------------------------
// Sonos group-coordinator routing (C04; caster_devices.py's
// `_sonos_coordinator` / `sonos_play`)
// ---------------------------------------------------------------------------

const ZONE_GROUP_TOPOLOGY: &str = "urn:schemas-upnp-org:service:ZoneGroupTopology:1";

fn xml_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

/// Parse a GetZoneGroupState reply: the description Location of
/// the coordinator of the group this device belongs to, or
/// `None` when this device IS its group's coordinator (or the
/// state does not mention it). `self_description_url` is this
/// device's own description Location — members are matched by
/// it exactly, falling back to a host match. The state document
/// arrives entity-escaped inside the SOAP element.
pub fn parse_zone_group_coordinator(
    state_reply: &str,
    self_description_url: &str,
) -> Option<String> {
    let raw = xml_text(state_reply, "ZoneGroupState")?;
    let state = crate::cast::xml_unescape(&raw);
    let mut rest = state.as_str();
    while let Some(pos) = rest.find("<ZoneGroup ") {
        let Some(rel_end) = rest[pos..].find("</ZoneGroup>") else {
            break;
        };
        let block = &rest[pos..pos + rel_end];
        rest = &rest[pos + rel_end..];
        let Some(group_tag_end) = block.find('>') else {
            continue;
        };
        let Some(coordinator_uuid) = xml_attr(&block[..group_tag_end], "Coordinator") else {
            continue;
        };
        let mut members: Vec<(String, String)> = Vec::new(); // (uuid, location)
        let mut mrest = block;
        while let Some(mpos) = mrest.find("<ZoneGroupMember ") {
            let Some(mend) = mrest[mpos..].find('>') else {
                break;
            };
            let mtag = &mrest[mpos..mpos + mend];
            if let (Some(uuid), Some(location)) =
                (xml_attr(mtag, "UUID"), xml_attr(mtag, "Location"))
            {
                members.push((uuid, location));
            }
            mrest = &mrest[mpos + mend..];
        }
        // Match this device's member entry: by its exact
        // description Location first, then by host — taking
        // care that 192.168.1.5 does not match 192.168.1.50.
        let self_host = self_description_url
            .split("://")
            .nth(1)
            .and_then(|r| r.split([':', '/']).next())
            .unwrap_or("");
        let Some((self_uuid, _)) = members
            .iter()
            .find(|(_, location)| location == self_description_url)
            .or_else(|| {
                members.iter().find(|(_, location)| {
                    location.contains(&format!("://{self_host}:"))
                        || location.contains(&format!("://{self_host}/"))
                })
            })
        else {
            continue;
        };
        if *self_uuid == coordinator_uuid {
            return None; // this device coordinates its own group
        }
        return members
            .iter()
            .find(|(uuid, _)| *uuid == coordinator_uuid)
            .map(|(_, location)| location.clone());
    }
    None
}

/// The AVTransport endpoint playback should actually go to for
/// a Sonos device: its group coordinator's, when it is a group
/// member (Sonos rejects play and stop sent to a non-coordinator
/// member). Any failure along the way keeps the device's own
/// endpoint — a standalone player is its own coordinator.
pub fn sonos_coordinator_endpoint(device: &CastDevice) -> String {
    let own = device.endpoint.clone();
    let description_url = format!("http://{}:1400/xml/device_description.xml", device.host);
    let Some(desc_xml) = crate::http::agent()
        .get(&description_url)
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().read_to_string().ok())
    else {
        return own;
    };
    let Some(desc) = crate::cast::parse_upnp_description_full(&desc_xml, &description_url) else {
        return own;
    };
    if desc.zone_group_topology_url.is_empty() {
        return own;
    }
    let Ok(state) = soap_action(
        &desc.zone_group_topology_url,
        ZONE_GROUP_TOPOLOGY,
        "GetZoneGroupState",
        "",
    ) else {
        return own;
    };
    let Some(coordinator_desc_url) = parse_zone_group_coordinator(&state, &description_url) else {
        return own;
    };
    let Some(coord_xml) = crate::http::agent()
        .get(&coordinator_desc_url)
        .call()
        .ok()
        .and_then(|mut r| r.body_mut().read_to_string().ok())
    else {
        return own;
    };
    crate::cast::parse_upnp_description_full(&coord_xml, &coordinator_desc_url)
        .map(|d| d.av_transport_url)
        .filter(|u| !u.is_empty())
        .unwrap_or(own)
}

/// The device playback should target: for a Sonos group member,
/// a clone whose endpoint is the group coordinator's
/// AVTransport URL; every other kind passes through unchanged.
pub fn sonos_effective(device: &CastDevice) -> CastDevice {
    if device.kind != "sonos" {
        return device.clone();
    }
    let mut effective = device.clone();
    effective.endpoint = sonos_coordinator_endpoint(device);
    effective
}

// ---------------------------------------------------------------------------
// Unified dispatch (Python CastingManager's per-kind branches)
// ---------------------------------------------------------------------------

/// Kodi credentials from config, threaded through the calls
/// that need them.
#[derive(Debug, Clone, Default)]
pub struct KodiAuth {
    pub user: String,
    pub password: String,
}

/// Play by device kind, from `start_seconds`. Chromecast is
/// handled by castsession (CASTv2); everything else lands
/// here. Roku takes no start position (Python `_play_roku`
/// has none either).
pub fn play_at(
    device: &CastDevice,
    url: &str,
    title: &str,
    mime: &str,
    start_seconds: f64,
    auth: &KodiAuth,
) -> Result<(), String> {
    match device.kind.as_str() {
        "dlna" | "sonos" => play_upnp_at(device, url, title, mime, start_seconds),
        "roku" => play_roku(device, url, title, mime),
        "kodi" => play_kodi_at(device, url, start_seconds, &auth.user, &auth.password),
        other => Err(format!("unsupported cast device kind: {other}")),
    }
}

/// Dispatch a cast by device kind (start at the beginning).
pub fn play(
    device: &CastDevice,
    url: &str,
    title: &str,
    mime: &str,
    kodi_user: &str,
    kodi_password: &str,
) -> Result<(), String> {
    play_at(
        device,
        url,
        title,
        mime,
        0.0,
        &KodiAuth {
            user: kodi_user.to_string(),
            password: kodi_password.to_string(),
        },
    )
}

/// Pause the active item. `clock_state` gates Roku's toggle:
/// Python only sends the Play keypress when its clock says
/// the item is playing (pause) / paused (resume).
pub fn pause(
    device: &CastDevice,
    auth: &KodiAuth,
    clock_state: Option<&str>,
) -> Result<(), String> {
    match device.kind.as_str() {
        "dlna" | "sonos" => pause_upnp(device),
        "kodi" => pause_kodi(device, &auth.user, &auth.password),
        "roku" if clock_state == Some("playing") => roku_key(device, "Play"),
        "roku" => Ok(()),
        other => Err(format!("unsupported cast device kind: {other}")),
    }
}

pub fn resume(
    device: &CastDevice,
    auth: &KodiAuth,
    clock_state: Option<&str>,
) -> Result<(), String> {
    match device.kind.as_str() {
        "dlna" | "sonos" => resume_upnp(device),
        "kodi" => resume_kodi(device, &auth.user, &auth.password),
        "roku" if clock_state == Some("paused") => roku_key(device, "Play"),
        "roku" => Ok(()),
        other => Err(format!("unsupported cast device kind: {other}")),
    }
}

pub fn stop(device: &CastDevice, auth: &KodiAuth) -> Result<(), String> {
    match device.kind.as_str() {
        "dlna" | "sonos" => stop_upnp(device),
        "kodi" => stop_kodi(device, &auth.user, &auth.password),
        "roku" => stop_roku(device),
        other => Err(format!("unsupported cast device kind: {other}")),
    }
}

/// Seek by kind. Roku errors: no seek over ECP (Python's
/// `_seek` returns without acting; the unified surface says
/// so instead of silently dropping the user's seek).
pub fn seek(device: &CastDevice, seconds: f64, auth: &KodiAuth) -> Result<(), String> {
    match device.kind.as_str() {
        "dlna" | "sonos" => seek_upnp(device, seconds),
        "kodi" => seek_kodi(device, seconds, &auth.user, &auth.password),
        "roku" => Err("Roku does not support seeking".into()),
        other => Err(format!("unsupported cast device kind: {other}")),
    }
}

/// Set volume from a 0.0..=1.0 level (the unified scale).
pub fn set_volume(device: &CastDevice, level: f64, auth: &KodiAuth) -> Result<(), String> {
    if !level.is_finite() || !(0.0..=1.0).contains(&level) {
        return Err("Volume must be between 0 and 1".into());
    }
    let percent = (level * 100.0).round() as u32;
    match device.kind.as_str() {
        "dlna" | "sonos" => set_volume_upnp(device, percent),
        "kodi" => set_volume_kodi(device, percent, &auth.user, &auth.password),
        "roku" => Err("Roku does not support volume control".into()),
        other => Err(format!("unsupported cast device kind: {other}")),
    }
}

/// Status by kind. Roku has no position report Python uses;
/// its status is the caller's clock (handled one layer up),
/// so this returns an unconnected-shaped snapshot with no
/// position for Roku rather than inventing one.
pub fn status(device: &CastDevice, auth: &KodiAuth) -> Result<CastStatus, String> {
    match device.kind.as_str() {
        "dlna" | "sonos" => upnp_status(device),
        "kodi" => kodi_status(device, &auth.user, &auth.password),
        "roku" => Ok(CastStatus {
            connected: true,
            ..Default::default()
        }),
        other => Err(format!("unsupported cast device kind: {other}")),
    }
}

fn base64_encode(data: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = data.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0] as u32,
            *chunk.get(1).unwrap_or(&0) as u32,
            *chunk.get(2).unwrap_or(&0) as u32,
        ];
        let n = (b[0] << 16) | (b[1] << 8) | b[2];
        out.push(CHARS[(n >> 18) as usize & 63] as char);
        out.push(CHARS[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            CHARS[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn didl_and_base64() {
        let didl = didl_metadata("http://x.test/a.mp3", "A & B", "audio/mpeg");
        assert!(didl.contains("A &amp; B"));
        assert!(didl.contains("audioItem"));
        assert_eq!(base64_encode("user:pass"), "dXNlcjpwYXNz");
        // Python urlencode (quote_plus): a space is `+`, not %20.
        assert_eq!(form_encode("a b&c"), "a+b%26c");
    }

    #[test]
    fn upnp_replies_parse_to_neutral_status() {
        let pos = "<s:Envelope><s:Body><u:GetPositionInfoResponse>\
                   <TrackDuration>0:42:10</TrackDuration>\
                   <RelTime>0:07:05</RelTime></u:GetPositionInfoResponse></s:Body></s:Envelope>";
        assert_eq!(parse_position_info(pos), (Some(425.0), Some(2530.0)));
        assert_eq!(
            parse_transport_state("<CurrentTransportState>PAUSED_PLAYBACK</CurrentTransportState>"),
            Some("PAUSED".into())
        );
        assert_eq!(map_upnp_state("NO_MEDIA_PRESENT"), "IDLE");
        assert_eq!(
            rendering_control_url("http://h:1400/MediaRenderer/AVTransport/Control"),
            "http://h:1400/MediaRenderer/RenderingControl/Control"
        );
    }

    #[test]
    fn kodi_properties_parse_to_neutral_status() {
        let st = parse_kodi_status(&serde_json::json!({
            "time": {"hours": 0, "minutes": 7, "seconds": 5, "milliseconds": 500},
            "totaltime": {"hours": 0, "minutes": 42, "seconds": 10, "milliseconds": 0},
            "speed": 0,
        }));
        assert_eq!(st.position_seconds, Some(425.5));
        assert_eq!(st.duration_seconds, Some(2530.0));
        assert_eq!(st.player_state.as_deref(), Some("PAUSED"));
    }
}
