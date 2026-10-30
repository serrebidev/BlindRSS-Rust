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
        let transport = self
            .app_transport
            .clone()
            .ok_or("launch the receiver first")?;
        let payload = serde_json::json!({
            "type": "LOAD",
            "autoplay": true,
            "media": {
                "contentId": url,
                "streamType": "BUFFERED",
                "contentType": content_type,
                "metadata": {
                    "metadataType": 0,
                    "title": title,
                }
            }
        });
        let status = self.request(
            &transport,
            NS_MEDIA,
            payload,
            "MEDIA_STATUS",
            Duration::from_secs(15),
        )?;
        if let Some(id) = status
            .get("status")
            .and_then(|s| s.as_array())
            .and_then(|a| a.first())
            .and_then(|m| m.get("mediaSessionId"))
            .and_then(|v| v.as_i64())
        {
            self.media_session_id = Some(id);
        }
        Ok(())
    }

    /// Play / pause / stop the loaded media.
    pub fn media_control(&mut self, command: &str) -> Result<(), String> {
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

    /// Ping the device (heartbeat namespace).
    pub fn ping(&mut self) -> Result<(), String> {
        self.send(&json_message(
            RECEIVER_ID,
            NS_HEARTBEAT,
            serde_json::json!({"type": "PING"}),
        ))
    }
}
