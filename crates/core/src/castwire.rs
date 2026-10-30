//! CASTv2 wire format (caster-core stage 2).
//!
//! Chromecast speaks length-prefixed protobuf CastMessages over
//! TLS on port 8009. The message has seven fields; this module
//! hand-encodes/decodes exactly that schema (no protobuf runtime)
//! and the 4-byte big-endian framing. Pure and fixture-tested —
//! the TLS connection lives in `cast.rs`.

pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";
pub const DEFAULT_RECEIVER_APP: &str = "CC1AD845";
pub const SENDER_ID: &str = "sender-0";
pub const RECEIVER_ID: &str = "receiver-0";

#[derive(Debug, Clone, PartialEq)]
pub struct CastMessage {
    pub source_id: String,
    pub destination_id: String,
    pub namespace: String,
    pub payload_utf8: String,
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

fn write_tag(out: &mut Vec<u8>, field: u32, wire: u32) {
    write_varint(out, ((field << 3) | wire) as u64);
}

fn write_len_field(out: &mut Vec<u8>, field: u32, bytes: &[u8]) {
    write_tag(out, field, 2);
    write_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut result: u64 = 0;
    let mut shift = 0;
    loop {
        let byte = *data.get(*pos)?;
        *pos += 1;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some(result);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// Encode one CastMessage (always CASTV2_1_0 + STRING payload).
pub fn encode_message(msg: &CastMessage) -> Vec<u8> {
    let mut body = Vec::new();
    write_tag(&mut body, 1, 0);
    write_varint(&mut body, 0); // protocol_version
    write_len_field(&mut body, 2, msg.source_id.as_bytes());
    write_len_field(&mut body, 3, msg.destination_id.as_bytes());
    write_len_field(&mut body, 4, msg.namespace.as_bytes());
    write_tag(&mut body, 5, 0);
    write_varint(&mut body, 0); // payload_type STRING
    write_len_field(&mut body, 6, msg.payload_utf8.as_bytes());
    let mut framed = Vec::with_capacity(4 + body.len());
    framed.extend_from_slice(&(body.len() as u32).to_be_bytes());
    framed.extend_from_slice(&body);
    framed
}

/// Decode a framed buffer holding exactly one message (frame
/// header included). Returns the message and bytes consumed.
pub fn decode_frame(data: &[u8]) -> Option<(CastMessage, usize)> {
    if data.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if data.len() < 4 + len {
        return None;
    }
    let body = &data[4..4 + len];
    let mut msg = CastMessage {
        source_id: String::new(),
        destination_id: String::new(),
        namespace: String::new(),
        payload_utf8: String::new(),
    };
    let mut pos = 0;
    while pos < body.len() {
        let tag = read_varint(body, &mut pos)?;
        let field = (tag >> 3) as u32;
        let wire = (tag & 0x7) as u32;
        match wire {
            0 => {
                read_varint(body, &mut pos)?;
            }
            2 => {
                let n = read_varint(body, &mut pos)? as usize;
                let bytes = body.get(pos..pos + n)?;
                let text = String::from_utf8_lossy(bytes).to_string();
                match field {
                    2 => msg.source_id = text,
                    3 => msg.destination_id = text,
                    4 => msg.namespace = text,
                    6 => msg.payload_utf8 = text,
                    _ => {}
                }
                pos += n;
            }
            1 => pos += 8,
            5 => pos += 4,
            _ => return None,
        }
    }
    Some((msg, 4 + len))
}

/// Convenience constructor for a JSON payload message.
pub fn json_message(dest: &str, namespace: &str, payload: serde_json::Value) -> CastMessage {
    CastMessage {
        source_id: SENDER_ID.to_string(),
        destination_id: dest.to_string(),
        namespace: namespace.to_string(),
        payload_utf8: payload.to_string(),
    }
}
