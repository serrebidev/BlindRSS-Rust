// Copyright (c) serrebidev and contributors
// This file is part of BlindRSS
// SPDX-License-Identifier: MIT

//! YouTube audio streams without yt-dlp.
//!
//! Port of the player-resolve half of lib_YTPYP (`ytpipe_core`), the core
//! of foo_ytpyp (<https://github.com/amanKG777/foo_ytpyp>, MIT, (c) 2026
//! Aman Kumar Gupta), same as Python `core/youtube_innertube.py`. YouTube's
//! app clients (Vision Pro, iPhone, Android VR) get plain googlevideo URLs
//! from InnerTube `player`: no cipher, no Deno, one ~0.1 s request instead
//! of a 4-7 s yt-dlp run. yt-dlp stays the fallback and keeps every other
//! site, video downloads, full text and channel listings.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VISIONOS_UA: &str =
    "com.google.visionos.youtube/1.02(RealityDevice14,1; U; CPU visionOS 25_6_0 like Mac OS X; GB)";
const IOS_UA: &str =
    "com.google.ios.youtube/21.03.2 (iPhone16,2; U; CPU iOS 18_7_2 like Mac OS X; en_US)";
const GOOGLEAPIS_PLAYER: &str =
    "https://youtubei.googleapis.com/youtubei/v1/player?prettyPrint=false";
const ANDROID_VR_PLAYER: &str =
    "https://www.youtube.com/youtubei/v1/player?key=AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const MAX_CACHE_AGE: Duration = Duration::from_secs(3 * 3600);
const EXPIRY_MARGIN_SECS: u64 = 600;
const MAX_CACHE: usize = 256;
const CHUNK: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct AudioStream {
    pub id: String,
    pub url: String,
    pub title: String,
    pub duration_seconds: u64,
    /// "webm" (Opus) or "m4a" (AAC).
    pub ext: String,
}

struct Cached {
    stream: AudioStream,
    at: Instant,
    expires: u64,
}

static CACHE: Mutex<Option<HashMap<String, Cached>>> = Mutex::new(None);
static VISITOR: Mutex<Option<(String, Instant)>> = Mutex::new(None);

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A random nonce (time + counter through splitmix64, as lib_YTPYP does).
fn nonce(len: usize) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut state = nanos
        ^ COUNTER
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (0..len)
        .map(|_| {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ALPHABET[((z ^ (z >> 31)) % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}

fn visionos_client() -> Value {
    serde_json::json!({
        "clientName": "VISIONOS", "clientVersion": "1.02", "clientScreen": "WATCH",
        "platform": "MOBILE", "deviceMake": "Apple", "deviceModel": "RealityDevice14,1",
        "osName": "visionOS", "osVersion": "25.6.0.23O471", "hl": "en-GB", "gl": "GB",
        "utcOffsetMinutes": 0
    })
}

fn post(url: &str, body: &Value, ua: &str) -> Result<Value, String> {
    let mut response = crate::http::agent()
        .post(url)
        .header("Content-Type", "application/json")
        .header("User-Agent", ua)
        .header("X-Goog-Api-Format-Version", "2")
        .send(body.to_string())
        .map_err(|e| e.to_string())?;
    if response.status() != 200 {
        return Err(format!("HTTP {}", response.status()));
    }
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn visitor_data() -> String {
    {
        let guard = VISITOR.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((value, at)) = guard.as_ref()
            && at.elapsed() < Duration::from_secs(24 * 3600)
        {
            return value.clone();
        }
    }
    let body = serde_json::json!({"context": {"client": visionos_client()}});
    let value = post(
        "https://www.youtube.com/youtubei/v1/visitor_id?prettyPrint=false",
        &body,
        VISIONOS_UA,
    )
    .ok()
    .and_then(|data| {
        data["responseContext"]["visitorData"]
            .as_str()
            .map(str::to_string)
    })
    .unwrap_or_default();
    if !value.is_empty() {
        *VISITOR.lock().unwrap_or_else(|p| p.into_inner()) = Some((value.clone(), Instant::now()));
    }
    value
}

/// One player request; returns the response and the cpn it sent.
fn request(client: &str, video_id: &str) -> Result<(Value, String), String> {
    let mut body =
        serde_json::json!({"videoId": video_id, "contentCheckOk": true, "racyCheckOk": true});
    match client {
        "VISIONOS" => {
            let cpn = nonce(16);
            let mut ctx = visionos_client();
            let visitor = visitor_data();
            if !visitor.is_empty() {
                ctx["visitorData"] = Value::String(visitor);
            }
            body["context"] = serde_json::json!({
                "client": ctx,
                "request": {"internalExperimentFlags": [], "useSsl": true},
                "user": {"lockedSafetyMode": false}
            });
            body["cpn"] = Value::String(cpn.clone());
            let url = format!("{GOOGLEAPIS_PLAYER}&t={}&id={video_id}", nonce(12));
            Ok((post(&url, &body, VISIONOS_UA)?, cpn))
        }
        "IOS" => {
            let cpn = nonce(16);
            body["context"] = serde_json::json!({"client": {
                "clientName": "IOS", "clientVersion": "21.03.2", "deviceMake": "Apple",
                "deviceModel": "iPhone16,2", "osName": "iOS", "osVersion": "18.7.2.22H124",
                "hl": "en", "gl": "US"
            }});
            body["cpn"] = Value::String(cpn.clone());
            Ok((post(GOOGLEAPIS_PLAYER, &body, IOS_UA)?, cpn))
        }
        _ => {
            body["context"] = serde_json::json!({"client": {
                "clientName": "ANDROID_VR", "clientVersion": "1.56.21", "hl": "en", "gl": "US"
            }});
            Ok((
                post(
                    ANDROID_VR_PLAYER,
                    &body,
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64)",
                )?,
                String::new(),
            ))
        }
    }
}

/// Original-language track over AI dubs, normal over DRC audio, then bitrate.
fn format_rank(f: &Value) -> (bool, bool, u64) {
    let track = &f["audioTrack"];
    let original = if track.is_object() {
        track["audioIsDefault"].as_bool().unwrap_or(false)
            || track["displayName"]
                .as_str()
                .is_some_and(|n| n.to_ascii_lowercase().contains("original"))
    } else {
        true
    };
    (
        original,
        !f["isDrc"].as_bool().unwrap_or(false),
        f["bitrate"].as_u64().unwrap_or(0),
    )
}

fn has_query_key(url: &str, key: &str) -> bool {
    query_value(url, key).is_some()
}

fn query_value<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
    })
}

/// Turn one InnerTube player response into the best audio stream.
pub fn parse_player_response(
    video_id: &str,
    cpn: &str,
    data: &Value,
) -> Result<AudioStream, String> {
    let status = data["playabilityStatus"]["status"]
        .as_str()
        .unwrap_or("UNKNOWN");
    if status != "OK" {
        let reason = data["playabilityStatus"]["reason"]
            .as_str()
            .unwrap_or("video unplayable");
        return Err(format!("{status}: {reason}"));
    }
    let details = &data["videoDetails"];
    if let Some(returned) = details["videoId"].as_str()
        && returned != video_id
    {
        return Err(format!("player returned a different video ({returned})"));
    }
    let best = data["streamingData"]["adaptiveFormats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| {
            f["mimeType"].as_str().unwrap_or("").starts_with("audio/") && f["url"].is_string()
        })
        .max_by_key(|f| format_rank(f))
        .ok_or("no direct audio streams in response")?;
    let mut url = best["url"].as_str().unwrap_or_default().to_string();
    // Without the playback nonce googlevideo throttles or cuts the stream off.
    if !cpn.is_empty() && !has_query_key(&url, "cpn") {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&format!("cpn={cpn}"));
    }
    let mime = best["mimeType"].as_str().unwrap_or("audio/webm");
    Ok(AudioStream {
        id: video_id.to_string(),
        url,
        title: details["title"].as_str().unwrap_or("").to_string(),
        duration_seconds: details["lengthSeconds"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        ext: if mime.contains("webm") { "webm" } else { "m4a" }.to_string(),
    })
}

/// One player request for (client, video id): the response and its cpn.
pub type PlayerFetch<'a> = dyn Fn(&str, &str) -> Result<(Value, String), String> + 'a;

/// Best audio stream for a YouTube video URL, or None when every client fails.
pub fn resolve_audio(url: &str) -> Option<AudioStream> {
    resolve_audio_with(url, &request)
}

/// `resolve_audio` with the player request injected (the test seam).
pub fn resolve_audio_with(url: &str, fetch: &PlayerFetch<'_>) -> Option<AudioStream> {
    let video_id = crate::ytfull::video_id_from_url(url)?;
    {
        let guard = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(hit) = guard.as_ref().and_then(|c| c.get(&video_id))
            && hit.at.elapsed() < MAX_CACHE_AGE
            && (hit.expires == 0 || unix_now() + EXPIRY_MARGIN_SECS < hit.expires)
        {
            return Some(hit.stream.clone());
        }
    }
    let mut errors = Vec::new();
    for client in ["VISIONOS", "IOS", "ANDROID_VR"] {
        match fetch(client, &video_id)
            .and_then(|(data, cpn)| parse_player_response(&video_id, &cpn, &data))
        {
            Ok(stream) => {
                let expires = query_value(&stream.url, "expire")
                    .and_then(|e| e.parse().ok())
                    .unwrap_or(0);
                let mut guard = CACHE.lock().unwrap_or_else(|p| p.into_inner());
                let cache = guard.get_or_insert_with(HashMap::new);
                if cache.len() >= MAX_CACHE
                    && let Some(oldest) = cache
                        .iter()
                        .min_by_key(|(_, c)| c.at)
                        .map(|(k, _)| k.clone())
                {
                    cache.remove(&oldest);
                }
                cache.insert(
                    video_id,
                    Cached {
                        stream: stream.clone(),
                        at: Instant::now(),
                        expires,
                    },
                );
                return Some(stream);
            }
            Err(e) => errors.push(format!("{client}: {e}")),
        }
    }
    eprintln!(
        "[innertube] could not resolve {video_id}: {}",
        errors.join("; ")
    );
    None
}

/// Drop a cached URL (it was rejected), so the next resolve is fresh.
pub fn invalidate(url: &str) {
    if let Some(id) = crate::ytfull::video_id_from_url(url)
        && let Some(cache) = CACHE.lock().unwrap_or_else(|p| p.into_inner()).as_mut()
    {
        cache.remove(&id);
    }
}

/// Download a video's audio to `dir/<id>.<ext>` in 8 MB ranges
/// (googlevideo throttles one long GET; YouTube's own clients range).
pub fn download_audio(url: &str, dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    invalidate(url);
    let stream = resolve_audio(url).ok_or("InnerTube could not resolve the video")?;
    let final_path = dir.join(format!("{}.{}", stream.id, stream.ext));
    let part = dir.join(format!("{}.{}.part", stream.id, stream.ext));
    let result = (|| -> Result<(), String> {
        use std::io::Write as _;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut start = 0u64;
        loop {
            let mut response = crate::http::agent()
                .get(&stream.url)
                .header("Range", &format!("bytes={start}-{}", start + CHUNK - 1))
                .call()
                .map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            if status != 200 && status != 206 {
                return Err(format!("HTTP {status}"));
            }
            let total = response
                .headers()
                .get("Content-Range")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit_once('/'))
                .and_then(|(_, t)| t.trim().parse::<u64>().ok());
            let bytes = response
                .body_mut()
                .with_config()
                .limit(CHUNK + 1024)
                .read_to_vec()
                .map_err(|e| e.to_string())?;
            if bytes.is_empty() {
                return Err("empty range response".into());
            }
            out.write_all(&bytes).map_err(|e| e.to_string())?;
            start += bytes.len() as u64;
            let done = match (status, total) {
                (200, _) => true,
                (_, Some(total)) => start >= total,
                _ => (bytes.len() as u64) < CHUNK,
            };
            if done {
                return Ok(());
            }
        }
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&part, &final_path).map_err(|e| e.to_string())?;
            Ok(final_path)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            invalidate(url);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(formats: Value, id: &str) -> Value {
        serde_json::json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"videoId": id, "title": "T", "lengthSeconds": "213"},
            "streamingData": {"adaptiveFormats": formats}
        })
    }

    #[test]
    fn picks_original_non_drc_highest_bitrate_and_adds_cpn() {
        let formats = serde_json::json!([
            {"mimeType": "video/mp4", "url": "https://v/video?x=1", "bitrate": 9_000_000},
            {"mimeType": "audio/webm", "url": "https://v/dub?x=1", "bitrate": 200_000,
             "audioTrack": {"audioIsDefault": false, "displayName": "French"}},
            {"mimeType": "audio/webm", "url": "https://v/drc?x=1", "bitrate": 300_000, "isDrc": true},
            {"mimeType": "audio/webm; codecs=\"opus\"", "url": "https://v/best?x=1", "bitrate": 160_000},
            {"mimeType": "audio/mp4", "signatureCipher": "s=abc", "bitrate": 999_999}
        ]);
        let s =
            parse_player_response("dQw4w9WgXcQ", "N", &response(formats, "dQw4w9WgXcQ")).unwrap();
        assert_eq!(s.url, "https://v/best?x=1&cpn=N");
        assert_eq!((s.duration_seconds, s.ext.as_str()), (213, "webm"));
    }

    #[test]
    fn rejects_unplayable_and_mismatched() {
        let blocked = serde_json::json!({"playabilityStatus": {"status": "LOGIN_REQUIRED"}});
        assert!(parse_player_response("dQw4w9WgXcQ", "", &blocked).is_err());
        let other = response(
            serde_json::json!([{"mimeType": "audio/mp4", "url": "u"}]),
            "otherVideo1",
        );
        assert!(parse_player_response("dQw4w9WgXcQ", "", &other).is_err());
    }

    #[test]
    fn falls_through_clients_then_caches() {
        let calls = std::cell::RefCell::new(Vec::new());
        let fetch = |client: &str, _id: &str| {
            calls.borrow_mut().push(client.to_string());
            if client != "ANDROID_VR" {
                return Err("blocked".to_string());
            }
            Ok((
                response(
                    serde_json::json!([{"mimeType": "audio/mp4", "url": "https://v/a"}]),
                    "jNQXAC9IVRw",
                ),
                String::new(),
            ))
        };
        let url = "https://www.youtube.com/watch?v=jNQXAC9IVRw&list=RDjNQXAC9IVRw";
        assert_eq!(resolve_audio_with(url, &fetch).unwrap().ext, "m4a");
        assert_eq!(resolve_audio_with(url, &fetch).unwrap().url, "https://v/a");
        assert_eq!(*calls.borrow(), ["VISIONOS", "IOS", "ANDROID_VR"]);
        assert!(resolve_audio_with("https://example.com/watch?v=jNQXAC9IVRw", &fetch).is_none());
        invalidate(url);
    }

    #[test]
    fn nonces_differ() {
        assert_ne!(nonce(16), nonce(16));
        assert_eq!(nonce(12).len(), 12);
    }
}
