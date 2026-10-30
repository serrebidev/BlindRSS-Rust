//! CASTv2 wire round-trip tests.

use blindrss_core::castwire::{NS_HEARTBEAT, decode_frame, encode_message, json_message};

#[test]
fn round_trip() {
    let msg = json_message(
        "receiver-0",
        NS_HEARTBEAT,
        serde_json::json!({"type": "PING"}),
    );
    let bytes = encode_message(&msg);
    // Framing: 4-byte big-endian length of the remainder.
    let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    assert_eq!(len, bytes.len() - 4);
    let (decoded, consumed) = decode_frame(&bytes).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(decoded, msg);
}

#[test]
fn partial_frame_rejected() {
    let msg = json_message(
        "receiver-0",
        NS_HEARTBEAT,
        serde_json::json!({"type": "PONG"}),
    );
    let bytes = encode_message(&msg);
    assert!(decode_frame(&bytes[..bytes.len() - 2]).is_none());
    assert!(decode_frame(&[]).is_none());
}
