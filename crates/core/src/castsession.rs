//! CASTv2 session over TLS (caster-core stage 2, connection half).
//!
//! Chromecast devices ship self-signed certificates; the Python
//! stack (pychromecast) likewise skips verification for these
//! local-network devices. The verifier below accepts any cert —
//! do not reuse it for anything but Cast devices.

use crate::castwire::*;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaStatus {
    pub position: f64,
    pub duration: f64,
    pub state: String,
    /// The receiver's idle reason when idle ("FINISHED" when
    /// an item played to its end), else None.
    pub idle_reason: Option<String>,
    pub media_session_id: Option<i64>,
}

impl MediaStatus {
    /// The protocol-neutral snapshot (Python
    /// `_chromecast_status`): Chromecast is the one protocol
    /// whose status supports session detection.
    pub fn to_unified(&self) -> crate::cast::CastStatus {
        crate::cast::CastStatus {
            position_seconds: Some(self.position),
            duration_seconds: (self.duration > 0.0).then_some(self.duration),
            player_state: Some(self.state.clone()),
            connected: true,
            supports_session_detection: true,
        }
    }
}

pub fn parse_media_status(value: &serde_json::Value) -> Result<MediaStatus, String> {
    let status = value
        .get("status")
        .and_then(|v| v.as_array())
        .ok_or("Cast receiver returned no media status")?;
    let Some(media) = status.first() else {
        return Ok(MediaStatus {
            state: "IDLE".into(),
            ..Default::default()
        });
    };
    let position = media
        .get("currentTime")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let duration = media
        .pointer("/media/duration")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if !position.is_finite() || position < 0.0 || !duration.is_finite() || duration < 0.0 {
        return Err("Cast receiver returned an invalid playback time".into());
    }
    Ok(MediaStatus {
        position,
        duration,
        state: media
            .get("playerState")
            .and_then(|v| v.as_str())
            .unwrap_or("IDLE")
            .into(),
        idle_reason: media
            .get("idleReason")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        media_session_id: media.get("mediaSessionId").and_then(|v| v.as_i64()),
    })
}

/// Idle reasons that mean the OLD session ended, not that the
/// new load was rejected (Python `STALE_IDLE_REASONS`).
pub const STALE_IDLE_REASONS: [&str; 2] = ["INTERRUPTED", "CANCELLED"];

/// Build the LOAD payload (Python `_load` / pychromecast
/// `play_media`): the media carries the probed stream type, and
/// currentTime follows `_load`'s rule — 0 for a live relay's
/// buffered start, the resume position for a buffered load,
/// absent for a live load.
pub fn load_payload(
    url: &str,
    content_type: &str,
    title: &str,
    stream_type: &str,
    start_seconds: f64,
    live_relay: bool,
) -> serde_json::Value {
    let start = if start_seconds.is_finite() {
        start_seconds.max(0.0)
    } else {
        0.0
    };
    let mut payload = serde_json::json!({
        "type": "LOAD",
        "autoplay": true,
        "media": {
            "contentId": url,
            "streamType": stream_type,
            "contentType": content_type,
            "metadata": {
                "metadataType": 0,
                "title": title,
            }
        },
    });
    if live_relay {
        payload["currentTime"] = serde_json::json!(0.0);
    } else if start > 0.0 && stream_type == "BUFFERED" {
        payload["currentTime"] = serde_json::json!(start);
    }
    payload
}

/// How a load settle watch ended (Python `_await_playing`'s
/// boolean, with the failure's idle reason kept for the caller's
/// "did not start playing" message).
#[derive(Debug, Clone, PartialEq)]
pub enum SettleOutcome {
    Settled,
    /// The receiver rejected the load (idle with a real reason),
    /// or never settled before the deadline; the reason is the
    /// last idle reason seen, when any.
    Failed(Option<String>),
}

/// One status observation against the settle rule. `None` means
/// keep polling: statuses from the previous session are stale
/// news, and an idle report with no reason (or a stale reason)
/// is not a verdict.
pub fn settle_verdict(
    status: &MediaStatus,
    previous_session: Option<i64>,
) -> Option<SettleOutcome> {
    let stale = previous_session.is_some() && status.media_session_id == previous_session;
    if stale {
        return None;
    }
    match status.state.as_str() {
        "PLAYING" | "PAUSED" => Some(SettleOutcome::Settled),
        "IDLE" => match status.idle_reason.as_deref() {
            Some(reason) if !reason.is_empty() && !STALE_IDLE_REASONS.contains(&reason) => {
                Some(SettleOutcome::Failed(Some(reason.to_string())))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Drive the settle rule over a status source until it decides
/// or the deadline passes. The source is injected so the policy
/// is testable without a TLS session.
pub fn await_playing_from(
    source: &mut dyn FnMut() -> Option<MediaStatus>,
    previous_session: Option<i64>,
    timeout: Duration,
    poll: Duration,
) -> SettleOutcome {
    let deadline = Instant::now() + timeout;
    let mut last_reason: Option<String> = None;
    loop {
        if let Some(status) = source() {
            if status.media_session_id != previous_session
                && let Some(reason) = status.idle_reason.clone()
            {
                last_reason = Some(reason);
            }
            if let Some(verdict) = settle_verdict(&status, previous_session) {
                return verdict;
            }
        }
        if Instant::now() >= deadline {
            return SettleOutcome::Failed(last_reason);
        }
        std::thread::sleep(poll);
    }
}

fn seek_payload(session: i64, seconds: f64) -> Result<serde_json::Value, String> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err("Seek position must be a finite, nonnegative number".into());
    }
    Ok(serde_json::json!({"type": "SEEK", "mediaSessionId": session, "currentTime": seconds}))
}

fn volume_payload(level: f64) -> Result<serde_json::Value, String> {
    if !level.is_finite() || !(0.0..=1.0).contains(&level) {
        return Err("Volume must be between 0 and 1".into());
    }
    Ok(serde_json::json!({"type": "SET_VOLUME", "volume": {"level": level}}))
}

#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

pub struct CastSession {
    stream: TlsStream,
    buf: Vec<u8>,
    next_request: i64,
    pub app_transport: Option<String>,
    pub media_session_id: Option<i64>,
}

impl CastSession {
    /// Connect to a device's CASTv2 port and perform the virtual
    /// connection handshake with the receiver.
    pub fn connect(host: &str, port: u16) -> Result<Self, String> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| format!("server name: {e}"))?;
        let conn = rustls::ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| format!("tls: {e}"))?;
        let tcp = TcpStream::connect((host, port)).map_err(|e| format!("connect: {e}"))?;
        tcp.set_read_timeout(Some(Duration::from_millis(500)))
            .map_err(|e| format!("socket: {e}"))?;
        let mut session = CastSession {
            stream: rustls::StreamOwned::new(conn, tcp),
            buf: Vec::new(),
            next_request: 1,
            app_transport: None,
            media_session_id: None,
        };
        session.send(&json_message(
            RECEIVER_ID,
            NS_CONNECTION,
            serde_json::json!({"type": "CONNECT"}),
        ))?;
        Ok(session)
    }

    fn send(&mut self, msg: &CastMessage) -> Result<(), String> {
        self.stream
            .write_all(&encode_message(msg))
            .map_err(|e| format!("send: {e}"))
    }

    /// Read one message within `timeout`, answering heartbeats
    /// internally. Returns None on timeout.
    pub fn read_message(&mut self, timeout: Duration) -> Option<CastMessage> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some((msg, used)) = decode_frame(&self.buf) {
                self.buf.drain(..used);
                if msg.namespace == NS_HEARTBEAT && msg.payload_utf8.contains("\"PING\"") {
                    let _ = self.send(&json_message(
                        &msg.source_id,
                        NS_HEARTBEAT,
                        serde_json::json!({"type": "PONG"}),
                    ));
                    continue;
                }
                return Some(msg);
            }
            if Instant::now() >= deadline {
                return None;
            }
            let mut chunk = [0u8; 8192];
            match self.stream.read(&mut chunk) {
                Ok(0) => return None,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(_) => return None,
            }
        }
    }

    /// Send a JSON request and wait for a response whose payload
    /// type matches `expect_type`.
    fn request(
        &mut self,
        dest: &str,
        namespace: &str,
        mut payload: serde_json::Value,
        expect_type: &str,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_request;
        self.next_request += 1;
        payload["requestId"] = serde_json::json!(id);
        self.send(&json_message(dest, namespace, payload))?;
        let deadline = Instant::now() + timeout;
        loop {
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                return Err(format!("timeout waiting for {expect_type}"));
            }
            let Some(msg) = self.read_message(remain) else {
                return Err(format!("timeout waiting for {expect_type}"));
            };
            if msg.namespace != namespace {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.payload_utf8) else {
                continue;
            };
            if value.get("type").and_then(|t| t.as_str()) == Some(expect_type) {
                if value.get("requestId").and_then(|r| r.as_i64()) == Some(id)
                    || value.get("requestId").and_then(|r| r.as_i64()) == Some(0)
                {
                    return Ok(value);
                }
                // RECEIVER_STATUS arrives unsolicited with requestId
                // 0 before the real one; both are usable.
                return Ok(value);
            }
        }
    }

    /// Current receiver status (running apps, volume).
    pub fn receiver_status(&mut self) -> Result<serde_json::Value, String> {
        self.request(
            RECEIVER_ID,
            NS_RECEIVER,
            serde_json::json!({"type": "GET_STATUS"}),
            "RECEIVER_STATUS",
            Duration::from_secs(8),
        )
    }

    /// Launch the Default Media Receiver and connect to its
    /// transport. Returns the app session id.
    pub fn launch_default_receiver(&mut self) -> Result<String, String> {
        let status = self.request(
            RECEIVER_ID,
            NS_RECEIVER,
            serde_json::json!({"type": "LAUNCH", "appId": DEFAULT_RECEIVER_APP}),
            "RECEIVER_STATUS",
            Duration::from_secs(15),
        )?;
        let apps = status
            .get("status")
            .and_then(|s| s.get("applications"))
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        let app = apps
            .iter()
            .find(|a| a.get("appId").and_then(|x| x.as_str()) == Some(DEFAULT_RECEIVER_APP))
            .ok_or("default receiver did not start")?;
        let transport = app
            .get("transportId")
            .and_then(|t| t.as_str())
            .ok_or("receiver gave no transport id")?
            .to_string();
        self.send(&json_message(
            &transport,
            NS_CONNECTION,
            serde_json::json!({"type": "CONNECT"}),
        ))?;
        self.app_transport = Some(transport.clone());
        Ok(transport)
    }

    /// Load a stream URL in the running media receiver.
    pub fn load_media(&mut self, url: &str, content_type: &str, title: &str) -> Result<(), String> {
        self.load_media_at(url, content_type, title, 0.0)
    }

    /// Load from `start_seconds` (Python `_play_chromecast` /
    /// `_load`: the LOAD carries currentTime for a buffered
    /// stream, and a receiver that ignored it — reporting a
    /// position more than 5 s short of a start over 5 s — gets
    /// one bounded corrective seek).
    pub fn load_media_at(
        &mut self,
        url: &str,
        content_type: &str,
        title: &str,
        start_seconds: f64,
    ) -> Result<(), String> {
        let status =
            self.load_media_prepared(url, content_type, title, "BUFFERED", start_seconds, false)?;
        let start = if start_seconds.is_finite() {
            start_seconds.max(0.0)
        } else {
            0.0
        };
        if start > 5.0 && status.position < start - 5.0 {
            // The receiver ignored currentTime on load; one
            // bounded seek puts the resume where it belongs.
            let _ = self.seek(start);
        }
        Ok(())
    }

    /// Load with Caster's preparation applied (C01): the stream
    /// type comes from the probe (LIVE or BUFFERED), and the
    /// currentTime rule is `_load`'s — a live relay starts at 0,
    /// a buffered load carries the resume position, a live load
    /// carries none. Returns the status the LOAD answered with.
    pub fn load_media_prepared(
        &mut self,
        url: &str,
        content_type: &str,
        title: &str,
        stream_type: &str,
        start_seconds: f64,
        live_relay: bool,
    ) -> Result<MediaStatus, String> {
        let transport = self
            .app_transport
            .clone()
            .ok_or("launch the receiver first")?;
        let payload = load_payload(
            url,
            content_type,
            title,
            stream_type,
            start_seconds,
            live_relay,
        );
        let status = self.request(
            &transport,
            NS_MEDIA,
            payload,
            "MEDIA_STATUS",
            Duration::from_secs(15),
        )?;
        let parsed = parse_media_status(&status)?;
        if let Some(id) = parsed.media_session_id {
            self.media_session_id = Some(id);
        }
        Ok(parsed)
    }

    /// Wait for the receiver to settle on the load just sent
    /// (Python `_await_playing`): PLAYING or PAUSED on a session
    /// other than the previous one settles; an idle report with
    /// a real reason rejects; everything else keeps polling
    /// until LOAD_TIMEOUT. The previous session id is the one
    /// recorded before the load.
    pub fn await_playing(&mut self, previous_session: Option<i64>) -> SettleOutcome {
        let mut source = || self.media_status().ok();
        await_playing_from(
            &mut source,
            previous_session,
            Duration::from_secs_f64(crate::castmedia::LOAD_TIMEOUT),
            Duration::from_millis(150),
        )
    }

    /// Python's post-load resume correction, applied once the
    /// load has settled: a buffered start over 5 s that the
    /// receiver under-shot by more than 5 s gets one seek.
    pub fn correct_resume_seek(&mut self, start_seconds: f64) {
        if start_seconds > 5.0
            && let Ok(now) = self.media_status()
            && now.position < start_seconds - 5.0
        {
            let _ = self.seek(start_seconds);
        }
    }

    /// Play / pause / stop the loaded media.
    pub fn media_control(&mut self, command: &str) -> Result<(), String> {
        if !matches!(command, "PLAY" | "PAUSE" | "STOP") {
            return Err("Unknown Cast playback command".into());
        }
        let transport = self
            .app_transport
            .clone()
            .ok_or("launch the receiver first")?;
        let session = self.media_session_id.ok_or("no media loaded")?;
        self.request(
            &transport,
            NS_MEDIA,
            serde_json::json!({"type": command, "mediaSessionId": session}),
            "MEDIA_STATUS",
            Duration::from_secs(8),
        )?;
        Ok(())
    }

    pub fn media_status(&mut self) -> Result<MediaStatus, String> {
        let transport = self
            .app_transport
            .clone()
            .ok_or("launch the receiver first")?;
        let status = self.request(
            &transport,
            NS_MEDIA,
            serde_json::json!({"type":"GET_STATUS"}),
            "MEDIA_STATUS",
            Duration::from_secs(8),
        )?;
        parse_media_status(&status)
    }

    pub fn seek(&mut self, seconds: f64) -> Result<(), String> {
        let session = self.media_session_id.ok_or("no media loaded")?;
        let payload = seek_payload(session, seconds)?;
        let transport = self
            .app_transport
            .clone()
            .ok_or("launch the receiver first")?;
        self.request(
            &transport,
            NS_MEDIA,
            payload,
            "MEDIA_STATUS",
            Duration::from_secs(8),
        )?;
        Ok(())
    }

    pub fn set_volume(&mut self, level: f64) -> Result<(), String> {
        self.request(
            RECEIVER_ID,
            NS_RECEIVER,
            volume_payload(level)?,
            "RECEIVER_STATUS",
            Duration::from_secs(8),
        )?;
        Ok(())
    }

    /// Ping the device (heartbeat namespace).
    pub fn ping(&mut self) -> Result<(), String> {
        self.send(&json_message(
            RECEIVER_ID,
            NS_HEARTBEAT,
            serde_json::json!({"type": "PING"}),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cast_status_and_missing_media_as_idle() {
        let value = serde_json::json!({"type":"MEDIA_STATUS", "status":[{"mediaSessionId":7,"currentTime":42.5,"playerState":"PAUSED","media":{"duration":3600}}]});
        let status = parse_media_status(&value).unwrap();
        assert_eq!(status.position, 42.5);
        assert_eq!(status.duration, 3600.0);
        assert_eq!(status.state, "PAUSED");
        assert_eq!(
            parse_media_status(&serde_json::json!({"status":[]}))
                .unwrap()
                .state,
            "IDLE"
        );
    }

    #[test]
    fn cast_controls_reject_invalid_numbers_and_keep_protocol_fields() {
        assert!(seek_payload(7, -1.0).is_err());
        assert!(seek_payload(7, f64::NAN).is_err());
        assert!(volume_payload(f64::INFINITY).is_err());
        assert!(volume_payload(1.1).is_err());
        let seek = seek_payload(7, 42.5).unwrap();
        assert_eq!(seek["mediaSessionId"], 7);
        assert_eq!(seek["currentTime"], 42.5);
        assert_eq!(volume_payload(0.5).unwrap()["volume"]["level"], 0.5);
    }
}
