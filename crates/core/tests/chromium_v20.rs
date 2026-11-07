//! B17: the Chromium v20 App-Bound pure-crypto port,
//! tested against Python's ground truth
//! (blindrss-work tests/test_chromium_cookies.py — the
//! flag-1 vector is a real Chrome 130 blob from the
//! chrome_v20_decryption write-up Python cites).
//! The Windows plumbing (impersonation, elevation, VSS)
//! is exercised on the owner's machine; everything that
//! can be proven offline is proven here.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use blindrss_core::chromium::decrypt_cookie_value_with_v20;
use blindrss_core::chromium_v20::{
    ParsedAppBoundKey, derive_v20_master_key, load_cached_v20_key, parse_app_bound_key_blob,
    resolve_master_keys, store_cached_v20_key, v20_fingerprint,
};
use chacha20poly1305::ChaCha20Poly1305;

fn hex(bytes: &str) -> Vec<u8> {
    (0..bytes.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&bytes[i..i + 2], 16).unwrap())
        .collect()
}

/// Python's `_wrap`: blob with an empty header.
fn wrap(content: &[u8]) -> Vec<u8> {
    let mut blob = Vec::new();
    blob.extend_from_slice(&0u32.to_le_bytes());
    blob.extend_from_slice(&(content.len() as u32).to_le_bytes());
    blob.extend_from_slice(content);
    blob
}

fn parsed(flag: u8) -> ParsedAppBoundKey {
    ParsedAppBoundKey {
        flag,
        raw_key: None,
        iv: Vec::new(),
        ciphertext: Vec::new(),
        tag: Vec::new(),
        encrypted_aes_key: None,
    }
}

const FLAG1_IV: &str = "cabf17e5f2f447b0e81b641b";
const FLAG1_CT: &str = "f27c224966e25ffcedd2e0cfc04e1f21f61bc2daa2eb6f532c47d39e7b50e67f";
const FLAG1_TAG: &str = "4d5c343fe6eed94358919ed23ad89630";
const FLAG1_KEY: &str = "6d296ee57a29256e745e262515971e66c198cd322ca69ffd57de15738bedcd6c";

#[test]
fn parse_raw_32_byte_key() {
    let key: Vec<u8> = (0..32).collect();
    let parsed = parse_app_bound_key_blob(&wrap(&key)).unwrap();
    assert_eq!(parsed.flag, 0);
    assert_eq!(parsed.raw_key.unwrap().to_vec(), key);
}

#[test]
fn parse_flag1_real_chrome_vector() {
    let mut header = vec![0x02u8];
    header.extend_from_slice(b"C:\\Program Files\\Google\\Chrome");
    assert_eq!(header.len(), 31);
    let mut content = vec![0x01u8];
    content.extend_from_slice(&hex(FLAG1_IV));
    content.extend_from_slice(&hex(FLAG1_CT));
    content.extend_from_slice(&hex(FLAG1_TAG));
    let mut blob = Vec::new();
    blob.extend_from_slice(&(header.len() as u32).to_le_bytes());
    blob.extend_from_slice(&header);
    blob.extend_from_slice(&(content.len() as u32).to_le_bytes());
    blob.extend_from_slice(&content);

    let parsed = parse_app_bound_key_blob(&blob).unwrap();
    assert_eq!(parsed.flag, 1);
    assert_eq!(parsed.iv, hex(FLAG1_IV));
    assert_eq!(parsed.ciphertext, hex(FLAG1_CT));
    assert_eq!(parsed.tag, hex(FLAG1_TAG));
}

#[test]
fn parse_flag2_and_flag3_layouts() {
    let mut content2 = vec![0x02u8];
    content2.extend_from_slice(&[0u8; 12]);
    content2.extend_from_slice(&[0u8; 32]);
    content2.extend_from_slice(&[0u8; 16]);
    let parsed2 = parse_app_bound_key_blob(&wrap(&content2)).unwrap();
    assert_eq!(parsed2.flag, 2);
    assert_eq!(parsed2.iv.len(), 12);
    assert_eq!(parsed2.ciphertext.len(), 32);
    assert_eq!(parsed2.tag.len(), 16);

    let mut content3 = vec![0x03u8];
    content3.extend_from_slice(&[0u8; 32]);
    content3.extend_from_slice(&[0u8; 12]);
    content3.extend_from_slice(&[0u8; 32]);
    content3.extend_from_slice(&[0u8; 16]);
    let parsed3 = parse_app_bound_key_blob(&wrap(&content3)).unwrap();
    assert_eq!(parsed3.flag, 3);
    assert_eq!(parsed3.encrypted_aes_key.unwrap().len(), 32);
    assert_eq!(parsed3.iv.len(), 12);
}

#[test]
fn parse_rejects_unknown_shapes() {
    let mut bad = Vec::new();
    bad.extend_from_slice(&0u32.to_le_bytes());
    bad.extend_from_slice(&2u32.to_le_bytes());
    bad.push(0x04);
    bad.extend_from_slice(b"abcd");
    assert!(parse_app_bound_key_blob(&bad).is_err());

    let mut unknown_flag = vec![0x09u8];
    unknown_flag.extend_from_slice(&[0u8; 60]);
    assert!(parse_app_bound_key_blob(&wrap(&unknown_flag)).is_err());

    let mut short_flag1 = vec![0x01u8];
    short_flag1.extend_from_slice(&[0u8; 40]);
    assert!(parse_app_bound_key_blob(&wrap(&short_flag1)).is_err());
}

#[test]
fn derive_flag1_real_vector() {
    let mut p = parsed(1);
    p.iv = hex(FLAG1_IV);
    p.ciphertext = hex(FLAG1_CT);
    p.tag = hex(FLAG1_TAG);
    let key = derive_v20_master_key(&p, None).unwrap();
    assert_eq!(key.to_vec(), hex(FLAG1_KEY));
}

#[test]
fn derive_flag2_roundtrip() {
    // Python seals with the static ChaCha key; mirror its
    // vector construction with the same static key bytes.
    const CHACHA20_KEY_HEX: &str =
        "E98F37D7F4E1FA433D19304DC2258042090E2D1D7EEA7670D41F738D08729660";
    let master: Vec<u8> = (0..32).collect();
    let iv: Vec<u8> = (0..12).collect();
    let cipher = ChaCha20Poly1305::new(hex(CHACHA20_KEY_HEX).as_slice().into());
    let wrapped = cipher
        .encrypt(
            iv.as_slice().into(),
            Payload {
                msg: &master,
                aad: &[],
            },
        )
        .unwrap();
    let mut p = parsed(2);
    p.iv = iv;
    p.ciphertext = wrapped[..wrapped.len() - 16].to_vec();
    p.tag = wrapped[wrapped.len() - 16..].to_vec();
    let key = derive_v20_master_key(&p, None).unwrap();
    assert_eq!(key.to_vec(), master);
}

#[test]
fn derive_flag3_roundtrip() {
    const XOR_KEY_HEX: &str = "CCF8A1CEC56605B8517552BA1A2D061C03A29E90274FB2FCF59BA4B75C392390";
    let master: Vec<u8> = (0..32).collect();
    let aes_key: Vec<u8> = (1..33).collect();
    let iv: Vec<u8> = (11..23).collect();
    // The CNG output is the real aes_key XORed with the
    // static XOR key (Python's simulation).
    let xor_key = hex(XOR_KEY_HEX);
    let cng_output: Vec<u8> = aes_key
        .iter()
        .zip(xor_key.iter())
        .map(|(a, b)| a ^ b)
        .collect();
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&aes_key));
    let wrapped = cipher
        .encrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: &master,
                aad: &[],
            },
        )
        .unwrap();
    let mut p = parsed(3);
    p.encrypted_aes_key = Some(b"fake-cng-ciphertext".to_vec());
    p.iv = iv;
    p.ciphertext = wrapped[..wrapped.len() - 16].to_vec();
    p.tag = wrapped[wrapped.len() - 16..].to_vec();
    let cng = move |_data: &[u8]| Some(cng_output.clone());
    let key = derive_v20_master_key(&p, Some(&cng)).unwrap();
    assert_eq!(key.to_vec(), master);
}

#[test]
fn derive_raw_key_passthrough() {
    let mut p = parsed(0);
    let key: [u8; 32] = (0..32).collect::<Vec<u8>>().try_into().unwrap();
    p.raw_key = Some(key);
    assert_eq!(derive_v20_master_key(&p, None).unwrap(), key);
}

#[test]
fn derive_flag3_requires_cng() {
    let mut p = parsed(3);
    p.encrypted_aes_key = Some(vec![b'x'; 32]);
    p.iv = vec![b'1'; 12];
    p.ciphertext = vec![b'2'; 32];
    p.tag = vec![b'3'; 16];
    assert!(derive_v20_master_key(&p, None).is_err());
}

#[test]
fn decrypt_v20_strips_domain_hash() {
    let key: [u8; 32] = (0..32).collect::<Vec<u8>>().try_into().unwrap();
    let mut plain = vec![0x11u8; 32];
    plain.extend_from_slice(b"the-value");
    let iv = [7u8; 12];
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let wrapped = cipher
        .encrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: &plain,
                aad: &[],
            },
        )
        .unwrap();
    let mut value = b"v20".to_vec();
    value.extend_from_slice(&iv);
    value.extend_from_slice(&wrapped);
    assert_eq!(
        decrypt_cookie_value_with_v20(&value, None, Some(&key), true),
        Some(b"the-value".to_vec())
    );
    // Without the key the row is undecryptable.
    assert_eq!(
        decrypt_cookie_value_with_v20(&value, None, None, true),
        None
    );
    // A bad tag returns None, never garbage.
    let mut corrupted = value.clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0xFF;
    assert_eq!(
        decrypt_cookie_value_with_v20(&corrupted, None, Some(&key), true),
        None
    );
}

fn temp_data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blindrss-v20-test-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn key_cache_roundtrip() {
    let dir = temp_data_dir("cache");
    let key: [u8; 32] = (0..32).collect::<Vec<u8>>().try_into().unwrap();
    store_cached_v20_key(&dir, "abc123", &key);
    assert_eq!(load_cached_v20_key(&dir, "abc123"), Some(key));
    assert_eq!(load_cached_v20_key(&dir, "missing"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fingerprint_is_sha256_of_the_base64_string() {
    // Python: hashlib.sha256(b64.encode()).hexdigest().
    // sha256("") is the well-known empty digest.
    assert_eq!(
        v20_fingerprint(""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn resolve_uses_the_cache_without_elevation() {
    let dir = temp_data_dir("resolve");
    let b64 = "QUFBQQ=="; // arbitrary stand-in app-bound value
    let fingerprint = v20_fingerprint(b64);
    let key: [u8; 32] = (0..32).collect::<Vec<u8>>().try_into().unwrap();
    store_cached_v20_key(&dir, &fingerprint, &key);
    let map = vec![(fingerprint.clone(), b64.to_string())];
    let resolution = resolve_master_keys(&map, &dir, false);
    assert_eq!(resolution.keys.get(&fingerprint), Some(&key));
    assert!(!resolution.elevation_failed);
    // An unknown fingerprint resolves to nothing, and
    // (elevate=false) never attempts an elevation.
    let unknown = vec![("deadbeef".to_string(), "QUFBQg==".to_string())];
    let resolution = resolve_master_keys(&unknown, &dir, false);
    assert!(resolution.keys.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn locked_file_probe_treats_plain_files_as_unlocked() {
    let dir = temp_data_dir("lock");
    let file = dir.join("Cookies");
    std::fs::write(&file, b"sqlite").unwrap();
    assert!(!blindrss_core::chromium_v20::is_locked_file(&file));
    assert!(!blindrss_core::chromium_v20::is_locked_file(
        &dir.join("missing")
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
