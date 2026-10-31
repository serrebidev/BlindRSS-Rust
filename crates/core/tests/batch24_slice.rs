//! Batch 24: equalizer model + DSP, RAOP (AirPlay) wire
//! formats — the residual-audit follow-ups Brandon pointed at
//! (Luna EQ's shape, the Rust RAOP implementations).

use blindrss_core::equalizer::*;
use blindrss_core::raop::*;

#[test]
fn eq_config_normalizes_like_python() {
    let v = serde_json::json!({
        "enabled": true,
        "preamp": 99.0,
        "bands": [30.0, -30.0, 5.55, 1],
        "preset": "  Rock  ",
    });
    let c = EqConfig::from_value(Some(&v));
    assert!(c.enabled);
    assert_eq!(c.preamp, 20.0);
    assert_eq!(c.bands[0], 20.0);
    assert_eq!(c.bands[1], -20.0);
    assert_eq!(c.bands[2], 5.6);
    assert_eq!(c.bands[3], 1.0);
    assert_eq!(c.bands[9], 0.0);
    assert_eq!(c.preset.as_deref(), Some("Rock"));
    assert_eq!(EqConfig::from_value(None), EqConfig::default());
    assert!(EqConfig::default().is_flat());
}

#[test]
fn eq_presets_match_libvlc_table() {
    assert_eq!(PRESETS.len(), 18);
    let flat = PRESETS.iter().find(|p| p.0 == "Flat").unwrap();
    assert_eq!(flat.1, 0.0);
    assert!(flat.2.iter().all(|g| *g == 0.0));
    let rock = PRESETS.iter().find(|p| p.0 == "Rock").unwrap();
    // libVLC rock: preamp 5.0 stored -> -7 effective; bands as read.
    assert_eq!(rock.1, -7.0);
    assert_eq!(
        rock.2,
        [8.0, 4.8, -5.6, -8.0, -3.2, 4.0, 8.8, 11.2, 11.2, 11.2]
    );
    assert_eq!(format_band_label(31.25), "31.25 Hz");
    assert_eq!(format_band_label(16000.0), "16 kHz");
}

#[test]
fn eq_user_presets_roundtrip() {
    let raw = serde_json::json!([
        {"name": "Mine", "preamp": 3.0, "bands": [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]},
        {"name": "mine", "preamp": 9.0, "bands": [2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0, 2.0]},
        {"name": "", "preamp": 0.0, "bands": []},
    ]);
    let list = normalize_user_presets(Some(&raw));
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].preamp, 3.0);
    let up = upsert_user_preset(Some(&raw), "Mine", -2.0, [4.0; BAND_COUNT]);
    let list2 = normalize_user_presets(Some(&serde_json::Value::Array(up)));
    assert_eq!(list2.len(), 1);
    assert_eq!(list2[0].preamp, -2.0);
    assert_eq!(list2[0].bands[0], 4.0);
}

#[test]
fn eq_processor_gain_and_preamp_convention() {
    // 1 kHz sine, 0.25 s at 44.1k.
    let n = 11_025;
    let mut base: Vec<i16> = (0..n * 2)
        .map(|i| {
            let t = (i / 2) as f64 / 44_100.0;
            (f64::sin(2.0 * std::f64::consts::PI * 1000.0 * t) * 10_000.0) as i16
        })
        .collect();
    let rms = |v: &[i16]| {
        (v.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / v.len() as f64).sqrt()
    };
    let before = rms(&base);

    // Flat config but enabled with preamp 12 (libVLC unity):
    // identity.
    let mut cfg = EqConfig {
        enabled: true,
        preamp: 12.0,
        bands: [0.0; BAND_COUNT],
        preset: None,
    };
    let mut p = EqProcessor::new(44_100.0, &cfg);
    let mut v = base.clone();
    p.process_i16(&mut v);
    assert!((rms(&v) - before).abs() < 1.0);

    // Preamp 0 in stored units = -12 dB effective.
    cfg.preamp = 0.0;
    let mut p = EqProcessor::new(44_100.0, &cfg);
    let mut v = base.clone();
    p.process_i16(&mut v);
    assert!((rms(&v) / before - 0.251).abs() < 0.01);

    // +12 dB on the 1 kHz band boosts a 1 kHz sine clearly.
    cfg.preamp = 12.0;
    cfg.bands[5] = 12.0;
    let mut p = EqProcessor::new(44_100.0, &cfg);
    let mut v = base.clone();
    p.process_i16(&mut v);
    assert!(rms(&v) > before * 2.0, "boosted rms {}", rms(&v));
    base.clear();
}

#[test]
fn eq_ffmpeg_chain_shape() {
    let live = PRESETS.iter().find(|p| p.0 == "Live").unwrap();
    let cfg = EqConfig {
        enabled: true,
        preamp: 7.0, // stored units; effective -5
        bands: live.2,
        preset: Some("Live".into()),
    };
    let chain = ffmpeg_filter_chain(&cfg);
    assert!(chain.starts_with("volume=-5.0dB"), "{chain}");
    assert!(chain.contains("equalizer=f=125:width_type=o:width=1:g=4.0"));
    assert!(!chain.contains("f=62.5"), "zero band leaked: {chain}");
}

#[test]
fn raop_sdp_and_transport() {
    let sdp = build_sdp("192.168.1.5", "192.168.1.9", "1234567890", None);
    assert!(sdp.contains("a=fmtp:96 352 0 16 40 10 14 2 255 0 0 44100"));
    assert!(!sdp.contains("rsaaeskey"));
    let sdp2 = build_sdp(
        "192.168.1.5",
        "192.168.1.9",
        "1234567890",
        Some(("KEY", "IV")),
    );
    assert!(sdp2.contains("a=rsaaeskey:KEY\r\na=aesiv:IV"));

    let t = "RTP/AVP/UDP;unicast;interleaved=0-1;mode=record;server_port=6001;control_port=6002;timing_port=6003";
    assert_eq!(parse_setup_transport(t), Some((6001, 6002, 6003)));
    assert_eq!(parse_setup_transport("garbage"), None);
}

#[test]
fn raop_crypto_roundtrip_shapes() {
    let key = [7u8; 16];
    let iv = [3u8; 16];
    let (rsakey, aesiv) = encrypt_session_key(&key, &iv).unwrap();
    // RSA-2048 OAEP block = 256 bytes -> 342 base64 chars unpadded.
    assert_eq!(rsakey.len(), 342);
    assert_eq!(aesiv.len(), 22);
    let enc = encrypt_packet(&key, &iv, &[1u8; 100]);
    assert_eq!(enc.len() % 16, 0);
    assert!(enc.len() > 100);
}

#[test]
fn raop_sync_and_timing_packets() {
    let sync = build_sync_packet(100_000, 55_125, true);
    assert_eq!(sync.len(), 20);
    assert_eq!(sync[0], 0x90);
    assert_eq!(sync[1], 0xD4);
    assert_eq!(u32::from_be_bytes(sync[4..8].try_into().unwrap()), 44_875);
    assert_eq!(
        u32::from_be_bytes(sync[16..20].try_into().unwrap()),
        100_000
    );
    let sync2 = build_sync_packet(100_000, 55_125, false);
    assert_eq!(sync2[0], 0x80);

    // A timing request: header + dummy + ref + recv + send at 24.
    let mut req = vec![0u8; 32];
    req[0] = 0x80;
    req[1] = 0xD2;
    req[24..28].copy_from_slice(&12345u32.to_be_bytes());
    req[28..32].copy_from_slice(&6789u32.to_be_bytes());
    let resp = build_timing_response(&req).unwrap();
    assert_eq!(resp.len(), 32);
    assert_eq!(resp[1], 0xD3);
    // ref_time echoes the request's send_time.
    assert_eq!(&resp[8..16], &req[24..32]);
    assert_eq!(build_timing_response(&[0u8; 10]), None);
}

#[test]
fn raop_volume_mapping() {
    assert_eq!(volume_db(0), -144.0);
    assert_eq!(volume_db(50), -15.0);
    assert_eq!(volume_db(100), 0.0);
}
