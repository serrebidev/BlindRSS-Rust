//! AirPlay audio casting: a RAOP v2 sender (the protocol the
//! YouTube TV-era AirPlay receivers speak on `_raop._tcp`),
//! implemented from the public RAOP design (shairplay /
//! philippe44's RAOP-Player lineage, as also implemented by
//! LinusU/rust-raop-player and pyatv, which Python's casting
//! uses). This replaces the batch-22 audit's "no Rust
//! equivalent" verdict: the protocol is implementable directly.
//!
//! Shape: RTSP handshake (OPTIONS, ANNOUNCE with an SDP whose
//! AES session key is RSA-OAEP-encrypted to the well-known
//! AirPort public key, SETUP, RECORD, SET_PARAMETER volume),
//! then ALAC frames over RTP/UDP paced in real time, with the
//! timing (NTP) responder, the per-second sync packets and
//! retransmit-on-request on the control channel. AirPlay 2
//! pairing-based devices are out of scope, exactly as in
//! Python (pyatv scans RAOP services that do not require
//! pairing).
//!
//! Audio in: interleaved s16le PCM at 44,100 Hz stereo from any
//! reader (the app pipes an ffmpeg decode). The equalizer, when
//! enabled, is applied to the PCM before encoding.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine;

use crate::equalizer::{EqConfig, EqProcessor};

pub const SAMPLE_RATE: u32 = 44_100;
pub const CHANNELS: u16 = 2;
/// Frames per RTP packet (the RAOP standard chunk).
pub const FRAMES_PER_PACKET: usize = 352;
const BACKLOG: usize = 512;
/// Receiver latency allowance on top of the device-reported
/// Audio-Latency (one second of audio + the 11,025-frame
/// minimum every RAOP sender adds).
const BASE_LATENCY: u64 = 44_100 + 11_025;

/// The AirPort RSA public modulus (the public half of the key
/// whose private half shairplay extracted; senders encrypt the
/// session key to it). Base64, from the public RAOP design.
const AIRPORT_MODULUS_B64: &str = "59dE8qLieItsH1WgjrcFRKj6eUWqi+bGLOX1HL3U3GhC/j0Qg90u3sG/1CUtwC5vOYvfDmFI6oSFXi5ELabWJmT2dKHzBJKa3k9ok+8t9ucRqMd6DZHJ2YCCLlDRKSKv6kDqnw4UwPdpOMXziC/AMj3Z/lUVX1G7WSHCAWKf1zNS1eLvqr+boEjXuBOitnZ/bDzPHrTOZz0Dew0uowxf/+sG+NCK3eQJVxqcaJ/vEHKIVd2M+5qL71yJQ+87X6oV3eaYvt3zWZYD6z5vYTcrtij2VZ9Zmni/UAaHqn9JdsBWLUEpVviYnhimNVvYFZeCXg/IdTQ+x4IRdiXNv5hEew==";

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    use rsa::rand_core::{OsRng, RngCore};
    OsRng.fill_bytes(&mut out);
    out
}

fn random_u32() -> u32 {
    u32::from_be_bytes(random_bytes::<4>())
}

/// NTP timestamp (seconds/fraction since 1900) for a wall time.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct NtpTime {
    pub seconds: u32,
    pub fraction: u32,
}

impl NtpTime {
    pub fn now() -> NtpTime {
        let unix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        NtpTime {
            seconds: (unix.as_secs() + 0x83AA_7E80) as u32,
            fraction: (((unix.subsec_micros() as u64) << 32) / 1_000_000) as u32,
        }
    }

    /// Sample-frame timestamp at 44.1 kHz for this NTP time.
    pub fn into_timestamp(self) -> u64 {
        let ntp = ((self.seconds as u64) << 32) | (self.fraction as u64);
        ((ntp >> 16) * SAMPLE_RATE as u64) >> 16
    }

    /// The NTP time at a sample-frame timestamp.
    pub fn from_timestamp(ts: u64) -> NtpTime {
        let ntp = ((ts << 16) / SAMPLE_RATE as u64) << 16;
        NtpTime {
            seconds: (ntp >> 32) as u32,
            fraction: ntp as u32,
        }
    }

    fn write_to(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.seconds.to_be_bytes());
        out.extend_from_slice(&self.fraction.to_be_bytes());
    }

    fn read_from(buf: &[u8], at: usize) -> Option<NtpTime> {
        Some(NtpTime {
            seconds: u32::from_be_bytes(buf.get(at..at + 4)?.try_into().ok()?),
            fraction: u32::from_be_bytes(buf.get(at + 4..at + 8)?.try_into().ok()?),
        })
    }
}

/// The SDP body for ANNOUNCE (ALAC + the encrypted-key lines).
pub fn build_sdp(
    local_ip: &str,
    remote_ip: &str,
    sid: &str,
    crypto: Option<(&str, &str)>,
) -> String {
    let mut sdp = format!(
        "v=0\r\no=iTunes {sid} 0 IN IP4 {local_ip}\r\ns=iTunes\r\nc=IN IP4 {remote_ip}\r\nt=0 0\r\n"
    );
    sdp.push_str(&format!(
        "m=audio 0 RTP/AVP 96\r\na=rtpmap:96 AppleLossless\r\na=fmtp:96 {FRAMES_PER_PACKET} 0 16 40 10 14 2 255 0 0 {SAMPLE_RATE}\r\n"
    ));
    if let Some((rsaaeskey, aesiv)) = crypto {
        sdp.push_str(&format!("a=rsaaeskey:{rsaaeskey}\r\na=aesiv:{aesiv}\r\n"));
    }
    sdp
}

/// RSA-OAEP(SHA-1)-encrypt the session key to the AirPort
/// public key; returns (base64 rsaaeskey, base64 aesiv), both
/// without padding, as the SDP wants.
pub fn encrypt_session_key(key: &[u8; 16], iv: &[u8; 16]) -> Result<(String, String), String> {
    let modulus_bytes = base64::engine::general_purpose::STANDARD
        .decode(AIRPORT_MODULUS_B64)
        .map_err(|e| format!("modulus decode: {e}"))?;
    let n = rsa::BigUint::from_bytes_be(&modulus_bytes);
    let e = rsa::BigUint::from(65_537u32);
    let pubkey = rsa::RsaPublicKey::new(n, e).map_err(|e| format!("bad AirPort key: {e}"))?;
    let encrypted = pubkey
        .encrypt(
            &mut rsa::rand_core::OsRng,
            rsa::Oaep::new::<sha1::Sha1>(),
            key,
        )
        .map_err(|e| format!("RSA encrypt failed: {e}"))?;
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD;
    Ok((b64.encode(encrypted), b64.encode(iv)))
}

/// AES-128-CBC encrypt one ALAC packet (PKCS7-padded, the
/// RAOP-Player design: each packet is encrypted independently
/// with the session IV).
pub fn encrypt_packet(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    type AesCbc = cbc::Encryptor<aes::Aes128>;
    AesCbc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
}

/// Parse the SETUP response's Transport header into
/// (audio_port, control_port, timing_port).
pub fn parse_setup_transport(transport: &str) -> Option<(u16, u16, u16)> {
    let (mut audio, mut control, mut timing) = (0u16, 0u16, 0u16);
    for token in transport.split(';') {
        if let Some((k, v)) = token.trim().split_once('=') {
            match k {
                "server_port" => audio = v.parse().ok()?,
                "control_port" => control = v.parse().ok()?,
                "timing_port" => timing = v.parse().ok()?,
                _ => {}
            }
        }
    }
    if audio == 0 || control == 0 {
        return None;
    }
    Some((audio, control, timing))
}

/// The sync packet sent on the control channel (first sync
/// carries the 0x10 extension bit). 20 bytes.
pub fn build_sync_packet(head_ts: u64, latency: u64, first: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(20);
    out.push(0x80 | if first { 0x10 } else { 0x00 });
    out.push(0x54 | 0x80);
    out.extend_from_slice(&7u16.to_be_bytes());
    let latency_ts = head_ts.saturating_sub(latency);
    out.extend_from_slice(&(latency_ts as u32).to_be_bytes());
    NtpTime::from_timestamp(head_ts).write_to(&mut out);
    out.extend_from_slice(&(head_ts as u32).to_be_bytes());
    out
}

/// Answer a timing request packet (32 bytes) with the timing
/// response. Returns None for anything else.
pub fn build_timing_response(request: &[u8]) -> Option<Vec<u8>> {
    if request.len() < 32 {
        return None;
    }
    let send_time = NtpTime::read_from(request, 24)?;
    let now = NtpTime::now();
    let mut out = Vec::with_capacity(32);
    out.push(request[0]);
    out.push(0x53 | 0x80);
    out.extend_from_slice(&request[2..4]);
    out.extend_from_slice(&0u32.to_be_bytes());
    send_time.write_to(&mut out); // ref_time = their send time
    now.write_to(&mut out); // recv_time
    now.write_to(&mut out); // send_time
    Some(out)
}

/// One parsed control-channel packet.
pub enum ControlPacket {
    Timing(Vec<u8>),
    Retransmit { seq: u16, count: u16 },
    Other,
}

pub fn parse_control_packet(packet: &[u8]) -> ControlPacket {
    if packet.len() >= 2 {
        // Timing requests also arrive on the control port on
        // some receivers; answer identically. (0xD2 = type 0x52
        // with the marker bit; 0xD6 = retransmit request 0x56.)
        if packet[1] == 0xD2 {
            if let Some(resp) = build_timing_response(packet) {
                return ControlPacket::Timing(resp);
            }
        } else if packet[1] == 0xD6 && packet.len() >= 8 {
            return ControlPacket::Retransmit {
                seq: u16::from_be_bytes([packet[4], packet[5]]),
                count: u16::from_be_bytes([packet[6], packet[7]]),
            };
        }
    }
    ControlPacket::Other
}

/// A minimal blocking RTSP client for the RAOP handshake.
struct Rtsp {
    reader: BufReader<TcpStream>,
    cseq: u32,
    sid: String,
    session: Option<String>,
    url: String,
}

impl Rtsp {
    fn connect(host: &str, port: u16) -> Result<Rtsp, String> {
        let stream = TcpStream::connect((host, port))
            .map_err(|e| format!("AirPlay connect to {host}:{port} failed: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
        let sid = format!("{:010}", random_u32() % 1_000_000_000);
        let url = format!("rtsp://{host}/{sid}");
        Ok(Rtsp {
            reader: BufReader::new(stream),
            cseq: 0,
            sid,
            session: None,
            url,
        })
    }

    fn local_ip(&self) -> String {
        self.reader
            .get_ref()
            .local_addr()
            .map(|a| a.ip().to_string())
            .unwrap_or_else(|_| "127.0.0.1".to_string())
    }

    /// Send one request; returns (status, headers).
    fn request(
        &mut self,
        method: &str,
        url_override: Option<&str>,
        extra: &[(&str, String)],
        body: Option<(&str, &str)>,
    ) -> Result<(u16, Vec<(String, String)>), String> {
        self.cseq += 1;
        let sci = format!(
            "{:016x}",
            u64::from(random_u32()) << 32 | u64::from(random_u32())
        );
        let mut req = format!(
            "{} {} RTSP/1.0\r\n",
            method,
            url_override.unwrap_or(&self.url)
        );
        req.push_str(&format!("CSeq: {}\r\n", self.cseq));
        req.push_str("User-Agent: iTunes/7.6.2 (Windows; N;)\r\n");
        req.push_str(&format!("Client-Instance: {sci}\r\n"));
        if let Some(session) = &self.session {
            req.push_str(&format!("Session: {session}\r\n"));
        }
        for (k, v) in extra {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some((ctype, text)) = body {
            req.push_str(&format!("Content-Type: {ctype}\r\n"));
            req.push_str(&format!("Content-Length: {}\r\n", text.len()));
        }
        req.push_str("\r\n");
        if let Some((_, text)) = body {
            req.push_str(text);
        }
        self.reader
            .get_mut()
            .write_all(req.as_bytes())
            .map_err(|e| format!("RTSP {method} send failed: {e}"))?;
        // Response: status line, headers, optional body.
        let mut status_line = String::new();
        self.reader
            .read_line(&mut status_line)
            .map_err(|e| format!("RTSP {method} read failed: {e}"))?;
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("RTSP {method}: bad response {status_line:?}"))?;
        let mut headers: Vec<(String, String)> = Vec::new();
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            self.reader
                .read_line(&mut line)
                .map_err(|e| format!("RTSP {method} header read failed: {e}"))?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                let (k, v) = (k.trim().to_string(), v.trim().to_string());
                if k.eq_ignore_ascii_case("content-length") {
                    content_length = v.parse().unwrap_or(0);
                }
                headers.push((k, v));
            }
        }
        if content_length > 0 {
            let mut body_buf = vec![0u8; content_length];
            self.reader
                .read_exact(&mut body_buf)
                .map_err(|e| format!("RTSP {method} body read failed: {e}"))?;
        }
        Ok((status, headers))
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// RAOP volume: percent (0-100) to the dB value SET_PARAMETER
/// carries (linear between -30 and 0; 0% is mute at -144).
pub fn volume_db(percent: u8) -> f32 {
    match percent {
        0 => -144.0,
        1..=99 => -30.0 + (30.0 * percent as f32) / 100.0,
        _ => 0.0,
    }
}

/// An active AirPlay cast. Dropping or stopping ends the
/// stream (TEARDOWN) on the streaming thread.
pub struct RaopCast {
    pub stop: Arc<AtomicBool>,
}

/// The shared control surface of one RAOP session (Python's
/// `_AirSession`, shared between its runner and the controls):
/// pause freezes the stream in place (the session and its
/// position stay), volume rides SET_PARAMETER, and the
/// position is the streamed-frame count over the start
/// offset — a RAOP receiver reports no position of its own.
/// A seek is a new stream from the target offset (the app
/// restarts its ffmpeg decode there), exactly as Python's
/// seek reopens its source at the pending seek.
#[derive(Debug)]
pub struct RaopHandle {
    pub stop: Arc<AtomicBool>,
    paused: AtomicBool,
    volume_percent: AtomicU8,
    position_ms: AtomicU64,
    finished: AtomicBool,
}

impl RaopHandle {
    pub fn new(volume_percent: u8) -> Arc<Self> {
        Arc::new(RaopHandle {
            stop: Arc::new(AtomicBool::new(false)),
            paused: AtomicBool::new(false),
            volume_percent: AtomicU8::new(volume_percent.min(100)),
            position_ms: AtomicU64::new(0),
            finished: AtomicBool::new(false),
        })
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn set_volume(&self, percent: u8) {
        self.volume_percent
            .store(percent.min(100), Ordering::Relaxed);
    }

    pub fn volume(&self) -> u8 {
        self.volume_percent.load(Ordering::Relaxed)
    }

    /// Position by the clock (start offset + frames streamed).
    pub fn position_seconds(&self) -> f64 {
        self.position_ms.load(Ordering::Relaxed) as f64 / 1000.0
    }

    /// The source played to its end (not a stop or an error).
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Relaxed)
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Stream PCM (s16le, 44.1 kHz, stereo) from `pcm` to the
/// device at host:port until EOF or `stop`. Blocking; run on a
/// worker thread. `volume_percent` follows the same 0-100
/// scale as the app's volume. Returns packets sent.
pub fn stream_pcm(
    host: &str,
    port: u16,
    pcm: &mut dyn Read,
    eq: Option<&EqConfig>,
    volume_percent: u8,
    stop: Arc<AtomicBool>,
) -> Result<u64, String> {
    let handle = Arc::new(RaopHandle {
        stop,
        paused: AtomicBool::new(false),
        volume_percent: AtomicU8::new(volume_percent.min(100)),
        position_ms: AtomicU64::new(0),
        finished: AtomicBool::new(false),
    });
    stream_pcm_with_handle(host, port, pcm, eq, &handle, 0.0)
}

/// Stream PCM under a shared `RaopHandle` (pause / volume /
/// position / finished for the unified cast controls), with
/// the source already decoded from `start_offset_seconds`
/// (a resume or a seek: the caller seeks its decoder, the
/// handle's clock starts at the offset).
pub fn stream_pcm_with_handle(
    host: &str,
    port: u16,
    pcm: &mut dyn Read,
    eq: Option<&EqConfig>,
    handle: &Arc<RaopHandle>,
    start_offset_seconds: f64,
) -> Result<u64, String> {
    let stop = handle.stop.clone();
    let volume_percent = handle.volume();
    handle.position_ms.store(
        (start_offset_seconds.max(0.0) * 1000.0) as u64,
        Ordering::Relaxed,
    );
    let mut rtsp = Rtsp::connect(host, port)?;
    rtsp.request("OPTIONS", Some("*"), &[], None)?;

    let local_ip = rtsp.local_ip();
    // UDP sockets, bound before SETUP so their ports can be
    // announced (the timing socket first, as Apple TV expects).
    let timing_sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("timing bind: {e}"))?;
    let control_sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("control bind: {e}"))?;
    let audio_sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("audio bind: {e}"))?;
    let local_timing = timing_sock.local_addr().map_err(|e| e.to_string())?.port();
    let local_control = control_sock.local_addr().map_err(|e| e.to_string())?.port();

    // Session key + IV; SDP carries the RSA-encrypted key.
    let key = random_bytes::<16>();
    let iv = random_bytes::<16>();
    let crypto = encrypt_session_key(&key, &iv).ok();
    let sdp = build_sdp(
        &local_ip,
        host,
        &rtsp.sid,
        crypto.as_ref().map(|(k, v)| (k.as_str(), v.as_str())),
    );
    let (status, _) = rtsp.request("ANNOUNCE", None, &[], Some(("application/sdp", &sdp)))?;
    if status != 200 {
        return Err(format!("AirPlay ANNOUNCE refused (RTSP {status})"));
    }

    let transport = format!(
        "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;control_port={local_control};timing_port={local_timing}"
    );
    let (status, headers) = rtsp.request("SETUP", None, &[("Transport", transport)], None)?;
    if status != 200 {
        return Err(format!("AirPlay SETUP refused (RTSP {status})"));
    }
    if let Some(session) = Rtsp::header(&headers, "Session") {
        rtsp.session = Some(session.to_string());
    }
    let transport_resp = Rtsp::header(&headers, "Transport")
        .ok_or("AirPlay SETUP response has no Transport header")?
        .to_string();
    let (audio_port, control_port, _timing_port) =
        parse_setup_transport(&transport_resp).ok_or("AirPlay SETUP ports unparsable")?;
    audio_sock
        .connect((host, audio_port))
        .map_err(|e| format!("audio connect: {e}"))?;
    control_sock
        .connect((host, control_port))
        .map_err(|e| format!("control connect: {e}"))?;
    if _timing_port != 0 {
        timing_sock.connect((host, _timing_port)).ok();
    }
    control_sock
        .set_read_timeout(Some(Duration::from_millis(5)))
        .ok();
    timing_sock
        .set_read_timeout(Some(Duration::from_millis(5)))
        .ok();

    // RECORD from the stream start.
    let ssrc = random_u32();
    let mut seq: u16 = random_u32() as u16;
    let start_ts = NtpTime::now().into_timestamp();
    let rtp_info = format!("seq={};rtptime={}", seq.wrapping_add(1), start_ts);
    let (status, headers) = rtsp.request(
        "RECORD",
        None,
        &[("Range", "npt=0-".to_string()), ("RTP-Info", rtp_info)],
        None,
    )?;
    if status != 200 {
        return Err(format!("AirPlay RECORD refused (RTSP {status})"));
    }
    let mut latency = BASE_LATENCY;
    if let Some(al) = Rtsp::header(&headers, "Audio-Latency")
        && let Ok(v) = al.trim().parse::<u64>()
    {
        latency = latency.max(v + 11_025);
    }

    // Volume.
    let db = volume_db(volume_percent);
    let _ = rtsp.request(
        "SET_PARAMETER",
        None,
        &[],
        Some(("text/parameters", &format!("volume: {db:.1}\r\n"))),
    );

    // Timing responder thread.
    let timing_stop = Arc::new(AtomicBool::new(false));
    let timing_thread = {
        let sock = timing_sock.try_clone().map_err(|e| e.to_string())?;
        let flag = timing_stop.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            while !flag.load(Ordering::Relaxed) {
                match sock.recv(&mut buf) {
                    Ok(n) => {
                        if let Some(resp) = build_timing_response(&buf[..n]) {
                            let _ = sock.send(&resp);
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        })
    };

    // ALAC encoder for 352-frame packets.
    let in_format =
        alac_encoder::FormatDescription::pcm::<i16>(SAMPLE_RATE as f64, CHANNELS as u32);
    let out_format = alac_encoder::FormatDescription::alac(
        SAMPLE_RATE as f64,
        FRAMES_PER_PACKET as u32,
        CHANNELS as u32,
    );
    let mut encoder = alac_encoder::AlacEncoder::new(&out_format);
    let mut eq_proc = eq
        .filter(|c| c.enabled)
        .map(|c| EqProcessor::new(SAMPLE_RATE as f64, c));

    let bytes_per_chunk = FRAMES_PER_PACKET * 4;
    let mut pcm_buf = vec![0u8; bytes_per_chunk];
    let mut head_ts = start_ts + latency;
    let mut first_pkt = true;
    let mut first_sync = true;
    let mut backlog: Vec<Option<(u16, Vec<u8>)>> = vec![None; BACKLOG];
    let mut packets_sent: u64 = 0;
    let stream_start = Instant::now();
    let mut frames_sent: u64 = 0;
    let mut last_sync = Instant::now() - Duration::from_secs(2);
    let mut last_keepalive = Instant::now();
    let mut read_err: Option<String> = None;

    let mut sent_volume = volume_percent;
    'stream: loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // Pause: hold the session open (keepalive OPTIONS so
        // the receiver keeps it) and freeze — no PCM is read
        // or sent, so the position clock stands still, as in
        // Python's pause. Play resumes right here.
        while handle.is_paused() && !stop.load(Ordering::Relaxed) {
            if last_keepalive.elapsed() >= Duration::from_secs(5) {
                let _ = rtsp.request("OPTIONS", Some("*"), &[], None);
                last_keepalive = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // A volume change rides SET_PARAMETER (Python's
        // AirPlay volume), applied on this thread which owns
        // the RTSP connection.
        let wanted_volume = handle.volume();
        if wanted_volume != sent_volume {
            sent_volume = wanted_volume;
            let db = volume_db(sent_volume);
            let _ = rtsp.request(
                "SET_PARAMETER",
                None,
                &[],
                Some(("text/parameters", &format!("volume: {db:.1}\r\n"))),
            );
        }
        // Fill one chunk; a short read at EOF is zero-padded,
        // then the loop ends after sending it.
        let mut filled = 0;
        let mut eof = false;
        while filled < bytes_per_chunk {
            match pcm.read(&mut pcm_buf[filled..]) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => filled += n,
                Err(e) => {
                    read_err = Some(format!("PCM read failed: {e}"));
                    eof = true;
                    break;
                }
            }
        }
        if filled == 0 && eof {
            if read_err.is_none() {
                handle.finished.store(true, Ordering::Relaxed);
            }
            break;
        }
        for b in &mut pcm_buf[filled..] {
            *b = 0;
        }

        if let Some(proc) = eq_proc.as_mut() {
            let mut samples: Vec<i16> = pcm_buf
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes(*c))
                .collect();
            proc.process_i16(&mut samples);
            for (i, s) in samples.iter().enumerate() {
                pcm_buf[i * 2..i * 2 + 2].copy_from_slice(&s.to_le_bytes());
            }
        }

        let max_size = pcm_buf.len() + in_format.max_packet_size();
        let mut encoded = vec![0u8; max_size];
        let size = encoder.encode(&in_format, &pcm_buf, &mut encoded);
        encoded.truncate(size);
        let payload = if crypto.is_some() {
            encrypt_packet(&key, &iv, &encoded)
        } else {
            encoded
        };

        seq = seq.wrapping_add(1);
        let mut packet = Vec::with_capacity(12 + payload.len());
        packet.push(0x80);
        packet.push(if first_pkt { 0xE0 } else { 0x60 });
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&(head_ts as u32).to_be_bytes());
        packet.extend_from_slice(&ssrc.to_be_bytes());
        packet.extend_from_slice(&payload);
        first_pkt = false;
        if audio_sock.send(&packet).is_err() {
            break 'stream;
        }
        backlog[(seq % (BACKLOG as u16)) as usize] = Some((seq, packet));
        packets_sent += 1;
        head_ts += FRAMES_PER_PACKET as u64;
        frames_sent += FRAMES_PER_PACKET as u64;
        handle.position_ms.store(
            (start_offset_seconds.max(0.0) * 1000.0) as u64
                + frames_sent * 1000 / SAMPLE_RATE as u64,
            Ordering::Relaxed,
        );

        // Control channel: sync every second, retransmits and
        // timing answers as they arrive.
        if last_sync.elapsed() >= Duration::from_secs(1) {
            let sync = build_sync_packet(head_ts, latency, first_sync);
            first_sync = false;
            let _ = control_sock.send(&sync);
            last_sync = Instant::now();
        }
        let mut cbuf = [0u8; 2048];
        if let Ok(n) = control_sock.recv(&mut cbuf) {
            match parse_control_packet(&cbuf[..n]) {
                ControlPacket::Timing(resp) => {
                    let _ = control_sock.send(&resp);
                }
                ControlPacket::Retransmit { seq: rseq, count } => {
                    for i in 0..count {
                        let want = rseq.wrapping_add(i);
                        if let Some(Some((s, pkt))) =
                            backlog.get((want % (BACKLOG as u16)) as usize)
                            && *s == want
                        {
                            let mut resend = Vec::with_capacity(4 + pkt.len());
                            resend.extend_from_slice(&[0x80, 0x56 | 0x80, 0, 1]);
                            resend.extend_from_slice(pkt);
                            let _ = audio_sock.send(&resend);
                        }
                    }
                }
                ControlPacket::Other => {}
            }
        }
        if last_keepalive.elapsed() >= Duration::from_secs(30) {
            let _ = rtsp.request("OPTIONS", Some("*"), &[], None);
            last_keepalive = Instant::now();
        }

        if eof {
            if read_err.is_none() {
                handle.finished.store(true, Ordering::Relaxed);
            }
            break;
        }
        // Pace to real time: packet N is due at
        // start + frames/rate (the latency head start is in
        // the timestamps, not the pacing).
        let due = stream_start + Duration::from_secs_f64(frames_sent as f64 / SAMPLE_RATE as f64);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
    }

    timing_stop.store(true, Ordering::Relaxed);
    let _ = timing_thread.join();
    let info = format!("seq={seq};rtptime={head_ts}");
    let _ = rtsp.request("FLUSH", None, &[("RTP-Info", info)], None);
    let _ = rtsp.request("TEARDOWN", None, &[], None);
    if let Some(e) = read_err {
        return Err(e);
    }
    Ok(packets_sent)
}
