//! Playback for the non-Cast protocols (Caster's
//! caster_extras.py): DLNA/UPnP AVTransport (also Sonos, whose
//! renderer is an AVTransport with the Sonos service type),
//! Roku External Control Protocol, and Kodi JSON-RPC.

use crate::cast::CastDevice;

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

fn soap_action(
    control_url: &str,
    service_type: &str,
    action: &str,
    args: &str,
) -> Result<String, String> {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <s:Body><u:{action} xmlns:u=\"{service_type}\">{args}</u:{action}></s:Body>\
         </s:Envelope>"
    );
    let mut resp = crate::http::agent()
        .post(control_url)
        .header("Content-Type", "text/xml; charset=\"utf-8\"")
        .header("SOAPACTION", &format!("\"{service_type}#{action}\""))
        .send(body)
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

const AVTRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:1";
const SONOS_AVTRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:1";

/// Play on a DLNA MediaRenderer or a Sonos player:
/// SetAVTransportURI then Play.
pub fn play_upnp(device: &CastDevice, url: &str, title: &str, mime: &str) -> Result<(), String> {
    if device.endpoint.is_empty() {
        return Err(format!("{} has no AVTransport endpoint", device.name));
    }
    let service = if device.kind == "sonos" {
        SONOS_AVTRANSPORT
    } else {
        AVTRANSPORT
    };
    let metadata = didl_metadata(url, title, mime);
    soap_action(
        &device.endpoint,
        service,
        "SetAVTransportURI",
        &format!(
            "<InstanceID>0</InstanceID><CurrentURI>{}</CurrentURI>\
             <CurrentURIMetaData>{}</CurrentURIMetaData>",
            xml_escape(url),
            xml_escape(&metadata)
        ),
    )?;
    soap_action(
        &device.endpoint,
        service,
        "Play",
        "<InstanceID>0</InstanceID><Speed>1</Speed>",
    )?;
    Ok(())
}

/// Play on a Roku through the External Control Protocol: the
/// Roku Media Player channel (15985) accepts the media URL as
/// launch parameters.
pub fn play_roku(device: &CastDevice, url: &str, title: &str) -> Result<(), String> {
    let base = if device.endpoint.is_empty() {
        format!("http://{}:8060", device.host)
    } else {
        device.endpoint.clone()
    };
    let launch = format!(
        "{base}/launch/15985?url={}&title={}&contentType=mp4",
        url_encode(url),
        url_encode(title)
    );
    crate::http::agent()
        .post(&launch)
        .send_empty()
        .map_err(|e| format!("Roku launch failed: {e}"))?;
    Ok(())
}

/// Play on Kodi through its JSON-RPC API (Player.Open).
/// Credentials come from config (cast_kodi_user /
/// cast_kodi_password) via the caller.
pub fn play_kodi(device: &CastDevice, url: &str, user: &str, password: &str) -> Result<(), String> {
    let base = format!("http://{}:{}/jsonrpc", device.host, device.port);
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "Player.Open",
        "params": {"item": {"file": url}},
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
    if text.contains("\"error\"") {
        return Err(format!("Kodi rejected the request: {text}"));
    }
    Ok(())
}

/// Dispatch a cast by device kind. Chromecast is handled by
/// castsession (CASTv2); everything else lands here.
pub fn play(
    device: &CastDevice,
    url: &str,
    title: &str,
    mime: &str,
    kodi_user: &str,
    kodi_password: &str,
) -> Result<(), String> {
    match device.kind.as_str() {
        "dlna" | "sonos" => play_upnp(device, url, title, mime),
        "roku" => play_roku(device, url, title),
        "kodi" => play_kodi(device, url, kodi_user, kodi_password),
        other => Err(format!("unsupported cast device kind: {other}")),
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
        assert_eq!(url_encode("a b&c"), "a%20b%26c");
    }
}
