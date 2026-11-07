//! Chromium v20 App-Bound Encryption (Audit 8 B17, port of
//! Python core/chromium_cookies.py's v20 half).
//!
//! Chromium 127+ encrypts cookies with a key bound to the
//! browser and the machine: the `app_bound_encrypted_key`
//! in Local State is wrapped twice with DPAPI — once under
//! the SYSTEM account, once under the user — and on Chrome
//! the inner blob is wrapped once more (flag 1/2/3 below).
//! Deriving the master key therefore needs one SYSTEM
//! DPAPI step, which Windows only allows from an elevated
//! process; derivation happens once per key rotation in a
//! short-lived elevated helper and the result is cached
//! re-encrypted under the user's DPAPI (see the cache
//! section), so later imports prompt never.
//!
//! This module is layered like Python's: the pure crypto
//! (blob parsing, master-key derivation) touches no OS API
//! and is unit-tested against Python's vectors; the OS
//! steps (SYSTEM DPAPI, the flag-3 CNG key) are injected
//! as closures. The Windows plumbing — impersonation,
//! elevation transports, the VSS copy — lives in the
//! `win` submodule and in the helper entry points at the
//! bottom of this file.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Pure crypto (no OS calls)
// ---------------------------------------------------------------------------

const fn hex32(s: &str) -> [u8; 32] {
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        let hi = (bytes[2 * i] as char).to_digit(16).expect("hex") as u8;
        let lo = (bytes[2 * i + 1] as char).to_digit(16).expect("hex") as u8;
        out[i] = (hi << 4) | lo;
        i += 1;
    }
    out
}

/// Static keys from Chromium's elevation_service
/// PostProcessData (Python's module constants).
const AES_GCM_KEY: [u8; 32] =
    hex32("B31C6E241AC846728DA9C1FAC4936651CFFB944D143AB816276BCC6DA0284787");
const CHACHA20_KEY: [u8; 32] =
    hex32("E98F37D7F4E1FA433D19304DC2258042090E2D1D7EEA7670D41F738D08729660");
const XOR_KEY: [u8; 32] = hex32("CCF8A1CEC56605B8517552BA1A2D061C03A29E90274FB2FCF59BA4B75C392390");

/// The CNG key that unwraps a flag-3 encrypted AES key.
pub const CNG_KEY_NAME: &str = "Google Chromekey1";
pub const CNG_PROVIDER_NAME: &str = "Microsoft Software Key Storage Provider";

/// A parsed app-bound key blob (Python
/// `parse_app_bound_key_blob`'s dict, typed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAppBoundKey {
    /// 0 = raw key (non-Chrome Chromium), 1/2/3 = Chrome
    /// PostProcessData generations.
    pub flag: u8,
    pub raw_key: Option<[u8; 32]>,
    pub iv: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub tag: Vec<u8>,
    /// Flag 3 only: the AES key wrapped by the machine
    /// CNG key.
    pub encrypted_aes_key: Option<Vec<u8>>,
}

/// Parse a double-DPAPI-decrypted app-bound key blob.
/// The wire format (Chromium elevation_service) is
/// `[u32 path_len][path][u32 content_len][content]`.
/// Errors mirror Python's ValueError cases so a future
/// Chromium format change fails loudly instead of
/// producing garbage.
pub fn parse_app_bound_key_blob(blob: &[u8]) -> Result<ParsedAppBoundKey, String> {
    if blob.len() < 8 {
        return Err("app-bound key blob too short".to_string());
    }
    let header_len = u32::from_le_bytes(blob[0..4].try_into().unwrap()) as usize;
    if header_len > blob.len() - 8 {
        return Err("app-bound key blob header length overflows".to_string());
    }
    let content_off = 4 + header_len;
    let content_len =
        u32::from_le_bytes(blob[content_off..content_off + 4].try_into().unwrap()) as usize;
    if header_len + content_len + 8 != blob.len() {
        return Err("app-bound key blob length mismatch".to_string());
    }
    let content = &blob[content_off + 4..content_off + 4 + content_len];

    let mut parsed = ParsedAppBoundKey {
        flag: 0,
        raw_key: None,
        iv: Vec::new(),
        ciphertext: Vec::new(),
        tag: Vec::new(),
        encrypted_aes_key: None,
    };
    if content.len() == 32 {
        // Non-Google-Chrome Chromium: the master key itself.
        parsed.raw_key = Some(content.try_into().unwrap());
        return Ok(parsed);
    }
    if content.is_empty() {
        return Err("app-bound key blob has empty content".to_string());
    }
    let flag = content[0];
    parsed.flag = flag;
    match flag {
        1 | 2 => {
            if content.len() != 61 {
                return Err(format!(
                    "app-bound key flag {flag} content is {} bytes, expected 61",
                    content.len()
                ));
            }
            parsed.iv = content[1..13].to_vec();
            parsed.ciphertext = content[13..45].to_vec();
            parsed.tag = content[45..61].to_vec();
        }
        3 => {
            if content.len() != 93 {
                return Err(format!(
                    "app-bound key flag 3 content is {} bytes, expected 93",
                    content.len()
                ));
            }
            parsed.encrypted_aes_key = Some(content[1..33].to_vec());
            parsed.iv = content[33..45].to_vec();
            parsed.ciphertext = content[45..77].to_vec();
            parsed.tag = content[77..93].to_vec();
        }
        _ => return Err(format!("unsupported app-bound key flag {flag}")),
    }
    Ok(parsed)
}

fn decrypt_aead(
    algorithm: &'static ring::aead::Algorithm,
    key: &[u8; 32],
    nonce: &[u8],
    ciphertext_and_tag: &[u8],
) -> Option<Vec<u8>> {
    use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey};
    let unbound = UnboundKey::new(algorithm, key).ok()?;
    let key = LessSafeKey::new(unbound);
    let nonce_array: [u8; ring::aead::NONCE_LEN] = nonce.try_into().ok()?;
    let nonce = Nonce::try_assume_unique_for_key(&nonce_array).ok()?;
    let mut data = ciphertext_and_tag.to_vec();
    let plain = key.open_in_place(nonce, Aad::empty(), &mut data).ok()?;
    Some(plain.to_vec())
}

/// Derive the 32-byte v20 master key from a parsed blob.
/// `cng_decrypt` decrypts the flag-3 `encrypted_aes_key`
/// with the machine CNG key (must run as SYSTEM); it is
/// only required for flag 3.
/// The injected flag-3 CNG step: decrypts the wrapped
/// AES key with the machine key (runs as SYSTEM).
pub type CngDecrypt<'a> = dyn Fn(&[u8]) -> Option<Vec<u8>> + 'a;

pub fn derive_v20_master_key(
    parsed: &ParsedAppBoundKey,
    cng_decrypt: Option<&CngDecrypt<'_>>,
) -> Result<[u8; 32], String> {
    if parsed.flag == 0 {
        let key = parsed
            .raw_key
            .ok_or_else(|| "raw app-bound master key is not 32 bytes".to_string())?;
        return Ok(key);
    }
    if parsed.iv.is_empty() || parsed.ciphertext.is_empty() || parsed.tag.is_empty() {
        return Err("incomplete app-bound key blob".to_string());
    }
    let mut ct_and_tag = parsed.ciphertext.clone();
    ct_and_tag.extend_from_slice(&parsed.tag);
    let plain = match parsed.flag {
        1 => decrypt_aead(
            &ring::aead::AES_256_GCM,
            &AES_GCM_KEY,
            &parsed.iv,
            &ct_and_tag,
        ),
        2 => decrypt_aead(
            &ring::aead::CHACHA20_POLY1305,
            &CHACHA20_KEY,
            &parsed.iv,
            &ct_and_tag,
        ),
        3 => {
            let cng = cng_decrypt.ok_or_else(|| {
                "CNG decryption is required for flag-3 app-bound keys".to_string()
            })?;
            let encrypted_aes_key = parsed
                .encrypted_aes_key
                .as_deref()
                .ok_or_else(|| "missing encrypted_aes_key".to_string())?;
            let decrypted = cng(encrypted_aes_key)
                .ok_or_else(|| "CNG decryption of the app-bound AES key failed".to_string())?;
            let xored: Vec<u8> = decrypted
                .iter()
                .zip(XOR_KEY.iter())
                .map(|(a, b)| a ^ b)
                .collect();
            let xored: [u8; 32] = xored.try_into().map_err(|v: Vec<u8>| {
                format!("CNG-derived AES key is {} bytes, expected 32", v.len())
            })?;
            decrypt_aead(&ring::aead::AES_256_GCM, &xored, &parsed.iv, &ct_and_tag)
        }
        _ => return Err(format!("unsupported app-bound key flag {}", parsed.flag)),
    };
    let plain = plain.ok_or_else(|| "app-bound key unwrap failed (tag mismatch)".to_string())?;
    plain
        .try_into()
        .map_err(|v: Vec<u8>| format!("derived master key is {} bytes, expected 32", v.len()))
}

/// The Local State `os_crypt.app_bound_encrypted_key`
/// (base64), or "" when the browser predates App-Bound
/// Encryption / the state cannot be read.
pub fn app_bound_key_b64(local_state: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(local_state) else {
        return String::new();
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&text) else {
        return String::new();
    };
    state
        .get("os_crypt")
        .and_then(|c| c.get("app_bound_encrypted_key"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Cache fingerprint of an app-bound key (Python: SHA-256
/// of the base64 string, hex).
pub fn v20_fingerprint(app_bound_b64: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(app_bound_b64.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Master-key cache (DPAPI-protected at rest on Windows)
// ---------------------------------------------------------------------------

/// The cache file inside the app's data directory.
pub const KEYS_FILENAME: &str = "chromium_v20_keys.json";

fn keys_path(data_dir: &Path) -> PathBuf {
    data_dir.join(KEYS_FILENAME)
}

fn read_keys_file(data_dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(keys_path(data_dir)) else {
        return serde_json::Map::new();
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The cached master key for a fingerprint, or None.
/// Stored base64; on Windows the stored bytes are
/// DPAPI-protected under the user, elsewhere (Python's
/// non-Windows behavior) they are the key itself.
pub fn load_cached_v20_key(data_dir: &Path, fingerprint: &str) -> Option<[u8; 32]> {
    use base64::Engine;
    let data = read_keys_file(data_dir);
    let entry = data.get(fingerprint)?.as_str()?;
    let protected = base64::engine::general_purpose::STANDARD
        .decode(entry)
        .ok()?;
    let key = platform_unprotect(&protected)?;
    key.try_into().ok()
}

/// Cache a derived master key (atomic replace, like
/// Python's temp-file + os.replace write).
pub fn store_cached_v20_key(data_dir: &Path, fingerprint: &str, key: &[u8; 32]) {
    use base64::Engine;
    let protected = platform_protect(key);
    let mut data = read_keys_file(data_dir);
    data.insert(
        fingerprint.to_string(),
        serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(protected)),
    );
    let Ok(text) = serde_json::to_string(&serde_json::Value::Object(data)) else {
        return;
    };
    let _ = std::fs::create_dir_all(data_dir);
    let tmp = data_dir.join(format!(".v20-keys-{}.tmp", uuid::Uuid::new_v4().simple()));
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, keys_path(data_dir)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(target_os = "windows")]
fn platform_protect(key: &[u8; 32]) -> Vec<u8> {
    win::dpapi_protect(key, "BlindRSS Chromium v20 cookie key").unwrap_or_else(|| key.to_vec())
}

#[cfg(target_os = "windows")]
fn platform_unprotect(blob: &[u8]) -> Option<Vec<u8>> {
    win::dpapi_unprotect(blob)
}

#[cfg(not(target_os = "windows"))]
fn platform_protect(key: &[u8; 32]) -> Vec<u8> {
    key.to_vec()
}

#[cfg(not(target_os = "windows"))]
fn platform_unprotect(blob: &[u8]) -> Option<Vec<u8>> {
    Some(blob.to_vec())
}

// ---------------------------------------------------------------------------
// Resolution: cache first, one batched elevation for the rest
// ---------------------------------------------------------------------------

/// The outcome of resolving a batch of app-bound keys.
#[derive(Debug, Default)]
pub struct MasterKeyResolution {
    /// fingerprint -> master key, for every key that
    /// resolved (cache or elevation).
    pub keys: std::collections::HashMap<String, [u8; 32]>,
    /// The elevation attempt failed outright.
    pub elevation_failed: bool,
    /// The user cancelled the UAC prompt.
    pub elevation_cancelled: bool,
}

/// Resolve v20 master keys for `(fingerprint, base64)`
/// pairs (Python `resolve_master_keys`). Cached keys are
/// used when present; the rest are derived in a single
/// elevation and cached. Off Windows, or with
/// `elevate == false`, only the cache speaks.
pub fn resolve_master_keys(
    app_bound_map: &[(String, String)],
    data_dir: &Path,
    elevate: bool,
) -> MasterKeyResolution {
    let mut resolution = MasterKeyResolution::default();
    if app_bound_map.is_empty() {
        return resolution;
    }
    let mut missing: Vec<(String, String)> = Vec::new();
    for (fingerprint, b64) in app_bound_map {
        match load_cached_v20_key(data_dir, fingerprint) {
            Some(key) => {
                resolution.keys.insert(fingerprint.clone(), key);
            }
            None => missing.push((fingerprint.clone(), b64.clone())),
        }
    }
    if missing.is_empty() || !elevate || !cfg!(target_os = "windows") {
        return resolution;
    }
    #[cfg(target_os = "windows")]
    {
        let b64s: Vec<String> = missing.iter().map(|(_, b64)| b64.clone()).collect();
        match win::run_elevated_helper_batch(&b64s) {
            Ok(keys) => {
                for ((fingerprint, _), key) in missing.iter().zip(keys) {
                    if let Some(key) = key {
                        store_cached_v20_key(data_dir, fingerprint, &key);
                        resolution.keys.insert(fingerprint.clone(), key);
                    }
                }
            }
            Err(e) => {
                resolution.elevation_failed = true;
                resolution.elevation_cancelled = win::error_is_cancelled(&e);
            }
        }
    }
    resolution
}

// ---------------------------------------------------------------------------
// Helper-process entry points (the app binary dispatches
// these flags before any GUI starts; Python's main.py
// dispatch is the model). Off Windows they are inert:
// the flags are never produced there.
// ---------------------------------------------------------------------------

pub const KEY_HELPER_FLAG: &str = "--blindrss-chromium-key-helper";
pub const VSS_HELPER_FLAG: &str = "--blindrss-chromium-vss-copy";
pub const ELEVATED_HELPER_FLAG: &str = "--blindrss-elevated-helper";

/// The elevated key helper (Python `run_key_helper_cli`):
/// reads base64 app-bound keys, one per line, from
/// `--input`; writes one base64 master key (or
/// `ERROR:...`) per line to `--output`.
pub fn run_key_helper_cli(args: &[String]) -> i32 {
    let (Some(input), Some(output)) = (arg_value(args, "--input"), arg_value(args, "--output"))
    else {
        return 2;
    };
    let text = match std::fs::read_to_string(&input) {
        Ok(text) => text,
        Err(e) => {
            let _ = std::fs::write(&output, format!("ERROR:{e}"));
            return 1;
        }
    };
    let mut results: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        #[cfg(target_os = "windows")]
        {
            use base64::Engine;
            match win::derive_master_key_from_app_bound_key(line) {
                Ok(key) => results.push(base64::engine::general_purpose::STANDARD.encode(key)),
                Err(e) => results.push(format!("ERROR:{e}")),
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            results.push("ERROR:App-Bound Encryption is Windows-only".to_string());
        }
    }
    match std::fs::write(&output, results.join("\n")) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

/// The elevated VSS copy helper (Python
/// `run_vss_copy_cli`): source paths, one per line, from
/// `--input`; copies via Volume Shadow Copy into
/// `--output-dir`; writes the staged path per line
/// (empty = failure) to `--manifest`.
pub fn run_vss_copy_cli(args: &[String]) -> i32 {
    let (Some(input), Some(output_dir), Some(manifest)) = (
        arg_value(args, "--input"),
        arg_value(args, "--output-dir"),
        arg_value(args, "--manifest"),
    ) else {
        return 2;
    };
    let text = match std::fs::read_to_string(&input) {
        Ok(text) => text,
        Err(e) => {
            let _ = std::fs::write(&manifest, format!("ERROR:{e}"));
            return 1;
        }
    };
    let sources: Vec<String> = text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    #[cfg(target_os = "windows")]
    {
        match win::vss_copy_files(&sources, Path::new(&output_dir)) {
            Ok(staged) => {
                let _ = std::fs::write(&manifest, staged.join("\n"));
                0
            }
            Err(e) => {
                let _ = std::fs::write(&manifest, format!("ERROR:{e}"));
                1
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (&output_dir, &sources);
        let _ = std::fs::write(&manifest, "ERROR:VSS copy is Windows-only");
        1
    }
}

/// The unified elevated helper behind the scheduled task
/// (Python `run_elevated_helper_cli`): reads the request
/// JSON from the fixed request path, writes the response
/// JSON to the fixed response path. Handles v20 key
/// derivation and VSS file copies in one elevation.
pub fn run_elevated_helper_cli() -> i32 {
    #[cfg(target_os = "windows")]
    {
        win::run_elevated_helper()
    }
    #[cfg(not(target_os = "windows"))]
    {
        1
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

/// True when `path` exists but cannot be opened for
/// reading — the exclusive lock a running Chromium
/// browser holds on its Cookies database (Python
/// `_is_locked_file`; a sharing violation surfaces as a
/// permission error).
pub fn is_locked_file(path: &Path) -> bool {
    use std::io::Read;
    if path.as_os_str().is_empty() {
        return false;
    }
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut buf = [0u8; 1];
            // read(1) in Python: success (even at EOF)
            // means readable; a permission failure is
            // the lock.
            f.read_exact(&mut buf)
                .map(|_| false)
                .unwrap_or_else(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        }
        // A sharing violation (the browser's exclusive
        // lock) surfaces as PermissionDenied or OS error
        // 32/33 on Windows; other errors mean a missing
        // or unreadable-for-other-reasons file, which
        // Python treats as not locked.
        Err(e) => {
            e.kind() == std::io::ErrorKind::PermissionDenied
                || matches!(e.raw_os_error(), Some(32) | Some(33))
        }
    }
}

/// Stage locked files through the elevated VSS path
/// (Python `_run_elevated_vss_copy`): returns
/// (index -> staged path, staging dir to clean up).
/// The silent scheduled task is tried first; the
/// prompting runas helper is the fallback; a cancelled
/// prompt propagates as an error whose message contains
/// "cancelled" (see `win::error_is_cancelled`).
#[cfg(target_os = "windows")]
pub fn run_elevated_vss_copy(
    copy_paths: &[PathBuf],
) -> Result<(std::collections::HashMap<usize, PathBuf>, Option<PathBuf>), String> {
    win::run_elevated_vss_copy(copy_paths)
}

// ---------------------------------------------------------------------------
// Windows plumbing (DPAPI is shared with chromium.rs; here:
// SYSTEM impersonation, the CNG machine key, elevation)
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub(crate) mod win {
    use super::*;
    use std::ffi::c_void;

    pub fn dpapi_unprotect(blob: &[u8]) -> Option<Vec<u8>> {
        crate::chromium::dpapi_unprotect(blob)
    }

    pub fn dpapi_protect(blob: &[u8], description: &str) -> Option<Vec<u8>> {
        crate::chromium::dpapi_protect(blob, description)
    }

    /// Python: cancellation is winerror 1223 or a
    /// "cancel" in the message (its OSError text for the
    /// runas refusal reads "User cancelled the permission
    /// prompt").
    pub fn error_is_cancelled(message: &str) -> bool {
        message.to_lowercase().contains("cancel")
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> usize;
        fn GetProcAddress(module: usize, name: *const u8) -> usize;
    }

    fn library(name: &str) -> Option<usize> {
        let addr = unsafe { LoadLibraryW(wide(name).as_ptr()) };
        if addr == 0 { None } else { Some(addr) }
    }

    fn proc<T>(module: usize, name: &str) -> Option<T> {
        let mut buf = name.as_bytes().to_vec();
        buf.push(0);
        let addr = unsafe { GetProcAddress(module, buf.as_ptr()) };
        if addr == 0 {
            None
        } else {
            Some(unsafe { std::mem::transmute_copy(&addr) })
        }
    }

    // --- Token / process FFI ---------------------------------------------

    type Handle = usize;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Luid {
        low: u32,
        high: i32,
    }

    #[repr(C)]
    struct LuidAndAttributes {
        luid: Luid,
        attributes: u32,
    }

    #[repr(C)]
    struct TokenPrivileges {
        count: u32,
        privileges: [LuidAndAttributes; 1],
    }

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        cnt_usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        cnt_threads: u32,
        parent_process_id: u32,
        pri_class_base: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_DUPLICATE: u32 = 0x0002;
    const TOKEN_IMPERSONATE: u32 = 0x0004;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0000_0002;
    const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
    const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

    struct Win32 {
        // kernel32
        get_current_process: unsafe extern "system" fn() -> Handle,
        open_process: unsafe extern "system" fn(u32, i32, u32) -> Handle,
        close_handle: unsafe extern "system" fn(Handle) -> i32,
        create_toolhelp32_snapshot: unsafe extern "system" fn(u32, u32) -> Handle,
        process32_first_w: unsafe extern "system" fn(Handle, *mut ProcessEntry32W) -> i32,
        process32_next_w: unsafe extern "system" fn(Handle, *mut ProcessEntry32W) -> i32,
        wait_for_single_object: unsafe extern "system" fn(Handle, u32) -> u32,
        local_free: unsafe extern "system" fn(*mut c_void) -> *mut c_void,
        // advapi32
        open_process_token: unsafe extern "system" fn(Handle, u32, *mut Handle) -> i32,
        lookup_privilege_value_w:
            unsafe extern "system" fn(*const u16, *const u16, *mut Luid) -> i32,
        adjust_token_privileges: unsafe extern "system" fn(
            Handle,
            i32,
            *const TokenPrivileges,
            u32,
            *mut c_void,
            *mut u32,
        ) -> i32,
        duplicate_token_ex:
            unsafe extern "system" fn(Handle, u32, *const c_void, u32, u32, *mut Handle) -> i32,
        set_thread_token: unsafe extern "system" fn(*const c_void, Handle) -> i32,
        get_token_information:
            unsafe extern "system" fn(Handle, u32, *mut c_void, u32, *mut u32) -> i32,
        convert_sid_to_string_sid_w: unsafe extern "system" fn(*const c_void, *mut *mut u16) -> i32,
    }

    fn win32() -> Option<Win32> {
        let k = library("kernel32.dll")?;
        let a = library("advapi32.dll")?;
        Some(Win32 {
            get_current_process: proc(k, "GetCurrentProcess")?,
            open_process: proc(k, "OpenProcess")?,
            close_handle: proc(k, "CloseHandle")?,
            create_toolhelp32_snapshot: proc(k, "CreateToolhelp32Snapshot")?,
            process32_first_w: proc(k, "Process32FirstW")?,
            process32_next_w: proc(k, "Process32NextW")?,
            wait_for_single_object: proc(k, "WaitForSingleObject")?,
            local_free: proc(k, "LocalFree")?,
            open_process_token: proc(a, "OpenProcessToken")?,
            lookup_privilege_value_w: proc(a, "LookupPrivilegeValueW")?,
            adjust_token_privileges: proc(a, "AdjustTokenPrivileges")?,
            duplicate_token_ex: proc(a, "DuplicateTokenEx")?,
            set_thread_token: proc(a, "SetThreadToken")?,
            get_token_information: proc(a, "GetTokenInformation")?,
            convert_sid_to_string_sid_w: proc(a, "ConvertSidToStringSidW")?,
        })
    }

    fn list_processes(w: &Win32) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        unsafe {
            let snap = (w.create_toolhelp32_snapshot)(TH32CS_SNAPPROCESS, 0);
            if snap == 0 || snap == usize::MAX {
                return out;
            }
            let mut entry = ProcessEntry32W {
                size: std::mem::size_of::<ProcessEntry32W>() as u32,
                cnt_usage: 0,
                process_id: 0,
                default_heap_id: 0,
                module_id: 0,
                cnt_threads: 0,
                parent_process_id: 0,
                pri_class_base: 0,
                flags: 0,
                exe_file: [0; 260],
            };
            if (w.process32_first_w)(snap, &mut entry) != 0 {
                loop {
                    let end = entry
                        .exe_file
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.exe_file.len());
                    out.push((
                        entry.process_id,
                        String::from_utf16_lossy(&entry.exe_file[..end]),
                    ));
                    entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;
                    if (w.process32_next_w)(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            (w.close_handle)(snap);
        }
        out
    }

    fn token_is_system(w: &Win32, token: Handle) -> bool {
        unsafe {
            let mut needed: u32 = 0;
            (w.get_token_information)(token, 1, std::ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                return false;
            }
            let mut buf = vec![0u8; needed as usize];
            if (w.get_token_information)(
                token,
                1,
                buf.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            ) == 0
            {
                return false;
            }
            // TOKEN_USER begins with the SID pointer.
            let sid = *(buf.as_ptr() as *const *const c_void);
            let mut text: *mut u16 = std::ptr::null_mut();
            if (w.convert_sid_to_string_sid_w)(sid, &mut text) == 0 || text.is_null() {
                return false;
            }
            let mut len = 0;
            while *text.add(len) != 0 {
                len += 1;
            }
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
            (w.local_free)(text as *mut c_void);
            value == "S-1-5-18"
        }
    }

    /// A duplicated impersonation token for a SYSTEM
    /// process (Python `_open_system_dup_token`):
    /// non-protected SYSTEM processes are preferred;
    /// every candidate's SID is verified before
    /// duplicating.
    fn open_system_dup_token(w: &Win32) -> Result<Handle, String> {
        const PRIORITY: [&str; 8] = [
            "winlogon.exe",
            "services.exe",
            "spoolsv.exe",
            "svchost.exe",
            "lsass.exe",
            "csrss.exe",
            "wininit.exe",
            "fontdrvhost.exe",
        ];
        let procs = list_processes(w);
        let mut by_name: std::collections::HashMap<String, Vec<u32>> =
            std::collections::HashMap::new();
        for (pid, name) in &procs {
            by_name.entry(name.to_lowercase()).or_default().push(*pid);
        }
        let mut ordered: Vec<(String, u32)> = Vec::new();
        for name in PRIORITY {
            if let Some(pids) = by_name.remove(name) {
                ordered.extend(pids.into_iter().map(|pid| (name.to_string(), pid)));
            }
        }
        for (pid, name) in &procs {
            let lower = name.to_lowercase();
            if by_name.contains_key(&lower) {
                ordered.push((lower, *pid));
            }
        }

        let mut errors: Vec<String> = Vec::new();
        for (name, pid) in ordered {
            unsafe {
                let process = (w.open_process)(PROCESS_QUERY_INFORMATION, 0, pid);
                if process == 0 {
                    errors.push(format!("{name}:{pid} open err {}", last_error()));
                    continue;
                }
                let mut token: Handle = 0;
                let opened = (w.open_process_token)(
                    process,
                    TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_IMPERSONATE,
                    &mut token,
                );
                let result = if opened == 0 {
                    errors.push(format!("{name}:{pid} token err {}", last_error()));
                    None
                } else if !token_is_system(w, token) {
                    errors.push(format!("{name}:{pid} not SYSTEM"));
                    None
                } else {
                    let mut dup: Handle = 0;
                    if (w.duplicate_token_ex)(
                        token,
                        MAXIMUM_ALLOWED,
                        std::ptr::null(),
                        2, // SecurityImpersonation
                        2, // TokenImpersonation
                        &mut dup,
                    ) == 0
                    {
                        errors.push(format!("{name}:{pid} dup err {}", last_error()));
                        None
                    } else {
                        Some(dup)
                    }
                };
                if token != 0 {
                    (w.close_handle)(token);
                }
                (w.close_handle)(process);
                if let Some(dup) = result {
                    return Ok(dup);
                }
            }
        }
        Err(format!(
            "Could not obtain a SYSTEM token from any process: {}",
            errors
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        ))
    }

    fn last_error() -> u32 {
        std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32
    }

    /// RAII SYSTEM impersonation (Python
    /// `_SystemImpersonation`): enables SeDebugPrivilege,
    /// sets a duplicated SYSTEM token on the thread, and
    /// always reverts — a failure mid-dance must not leave
    /// the thread impersonating SYSTEM.
    pub struct SystemImpersonation {
        w: Win32,
        dup: Handle,
        current: Handle,
        active: bool,
    }

    impl SystemImpersonation {
        pub fn enter() -> Result<Self, String> {
            let w = win32().ok_or_else(|| "advapi32/kernel32 unavailable".to_string())?;
            let mut imp = SystemImpersonation {
                w,
                dup: 0,
                current: 0,
                active: false,
            };
            if let Err(e) = imp.enter_impl() {
                imp.revert();
                return Err(e);
            }
            imp.active = true;
            Ok(imp)
        }

        fn enter_impl(&mut self) -> Result<(), String> {
            unsafe {
                let process = (self.w.get_current_process)();
                if (self.w.open_process_token)(
                    process,
                    TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                    &mut self.current,
                ) == 0
                {
                    return Err(format!("OpenProcessToken failed ({})", last_error()));
                }
                let mut luid = Luid { low: 0, high: 0 };
                if (self.w.lookup_privilege_value_w)(
                    std::ptr::null(),
                    wide("SeDebugPrivilege").as_ptr(),
                    &mut luid,
                ) != 0
                {
                    let tp = TokenPrivileges {
                        count: 1,
                        privileges: [LuidAndAttributes {
                            luid,
                            attributes: SE_PRIVILEGE_ENABLED,
                        }],
                    };
                    (self.w.adjust_token_privileges)(
                        self.current,
                        0,
                        &tp,
                        0,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    );
                }
                self.dup = open_system_dup_token(&self.w)?;
                if (self.w.set_thread_token)(std::ptr::null(), self.dup) == 0 {
                    return Err(format!("SetThreadToken failed ({})", last_error()));
                }
            }
            Ok(())
        }

        fn revert(&mut self) {
            unsafe {
                (self.w.set_thread_token)(std::ptr::null(), 0);
                if self.dup != 0 {
                    (self.w.close_handle)(self.dup);
                }
                if self.current != 0 {
                    (self.w.close_handle)(self.current);
                }
            }
            self.active = false;
        }
    }

    impl Drop for SystemImpersonation {
        fn drop(&mut self) {
            if self.active {
                self.revert();
            }
        }
    }

    /// Decrypt with the machine CNG key (Python
    /// `_cng_decrypt`): the caller runs it under SYSTEM
    /// impersonation. Two-call size pattern,
    /// NCRYPT_SILENT_FLAG so no UI can appear.
    pub fn cng_decrypt(encrypted_aes_key: &[u8]) -> Option<Vec<u8>> {
        type OpenProvider = unsafe extern "system" fn(*mut Handle, *const u16, u32) -> i32;
        type OpenKey = unsafe extern "system" fn(Handle, *mut Handle, *const u16, u32, u32) -> i32;
        type Decrypt = unsafe extern "system" fn(
            Handle,
            *const u8,
            u32,
            *const c_void,
            *mut u8,
            u32,
            *mut u32,
            u32,
        ) -> i32;
        type FreeObject = unsafe extern "system" fn(Handle) -> i32;
        let ncrypt = library("ncrypt.dll")?;
        let open_provider: OpenProvider = proc(ncrypt, "NCryptOpenStorageProvider")?;
        let open_key: OpenKey = proc(ncrypt, "NCryptOpenKey")?;
        let decrypt: Decrypt = proc(ncrypt, "NCryptDecrypt")?;
        let free_object: FreeObject = proc(ncrypt, "NCryptFreeObject")?;
        unsafe {
            let mut provider: Handle = 0;
            if open_provider(&mut provider, wide(CNG_PROVIDER_NAME).as_ptr(), 0) != 0 {
                return None;
            }
            let mut key: Handle = 0;
            let opened = open_key(provider, &mut key, wide(CNG_KEY_NAME).as_ptr(), 0, 0);
            let result = if opened != 0 {
                None
            } else {
                let mut result_len: u32 = 0;
                let sized = decrypt(
                    key,
                    encrypted_aes_key.as_ptr(),
                    encrypted_aes_key.len() as u32,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    0,
                    &mut result_len,
                    0x40,
                );
                if sized != 0 || result_len == 0 {
                    None
                } else {
                    let mut out = vec![0u8; result_len as usize];
                    let status = decrypt(
                        key,
                        encrypted_aes_key.as_ptr(),
                        encrypted_aes_key.len() as u32,
                        std::ptr::null(),
                        out.as_mut_ptr(),
                        result_len,
                        &mut result_len,
                        0x40,
                    );
                    if status != 0 {
                        None
                    } else {
                        out.truncate(result_len as usize);
                        Some(out)
                    }
                }
            };
            if key != 0 {
                free_object(key);
            }
            free_object(provider);
            result
        }
    }

    /// The full derivation for one app-bound key (Python
    /// `derive_master_key_from_app_bound_key`): the
    /// SYSTEM DPAPI step and any flag-3 CNG run under
    /// SYSTEM impersonation; the user DPAPI step runs
    /// under the caller's own token.
    pub fn derive_master_key_from_app_bound_key(
        app_bound_key_b64: &str,
    ) -> Result<[u8; 32], String> {
        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD
            .decode(app_bound_key_b64)
            .map_err(|e| format!("app-bound key is not valid base64: {e}"))?;
        if raw.get(..4) != Some(b"APPB") {
            return Err("app-bound key does not begin with the APPB marker".to_string());
        }
        let encrypted = &raw[4..];
        let system_decrypted = {
            let _imp = SystemImpersonation::enter()?;
            dpapi_unprotect(encrypted).ok_or_else(|| "SYSTEM DPAPI step failed".to_string())?
        };
        let user_decrypted = dpapi_unprotect(&system_decrypted)
            .ok_or_else(|| "user DPAPI step failed".to_string())?;
        let parsed = parse_app_bound_key_blob(&user_decrypted)?;
        let cng = |data: &[u8]| -> Option<Vec<u8>> {
            let _imp = SystemImpersonation::enter().ok()?;
            cng_decrypt(data)
        };
        derive_v20_master_key(&parsed, Some(&cng))
    }

    // --- Elevation transports --------------------------------------------

    #[repr(C)]
    struct ShellExecuteInfoW {
        cb_size: u32,
        f_mask: u32,
        hwnd: Handle,
        lp_verb: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show: i32,
        h_inst_app: Handle,
        lp_id_list: *mut c_void,
        lp_class: *const u16,
        hkey_class: Handle,
        dw_hot_key: u32,
        h_icon: Handle,
        h_process: Handle,
    }

    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const ERROR_CANCELLED: u32 = 1223;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Launch `exe` elevated via the runas verb; returns
    /// the process handle (0 when Windows does not hand
    /// one back, which happens on some versions).
    fn runas_launch(exe: &str, params: &str) -> Result<Handle, String> {
        type ShellExecuteExW = unsafe extern "system" fn(*mut ShellExecuteInfoW) -> i32;
        let shell32 = library("shell32.dll").ok_or("shell32.dll unavailable")?;
        let shell_execute: ShellExecuteExW =
            proc(shell32, "ShellExecuteExW").ok_or("ShellExecuteExW unavailable")?;
        let verb = wide("runas");
        let file = wide(exe);
        let parameters = wide(params);
        let mut sei = ShellExecuteInfoW {
            cb_size: std::mem::size_of::<ShellExecuteInfoW>() as u32,
            f_mask: SEE_MASK_NOCLOSEPROCESS,
            hwnd: 0,
            lp_verb: verb.as_ptr(),
            lp_file: file.as_ptr(),
            lp_parameters: parameters.as_ptr(),
            lp_directory: std::ptr::null(),
            n_show: 0, // SW_HIDE
            h_inst_app: 0,
            lp_id_list: std::ptr::null_mut(),
            lp_class: std::ptr::null(),
            hkey_class: 0,
            dw_hot_key: 0,
            h_icon: 0,
            h_process: 0,
        };
        let ok = unsafe { shell_execute(&mut sei) };
        if ok == 0 {
            let code = last_error();
            if code == ERROR_CANCELLED {
                return Err("User cancelled the permission prompt (1223)".to_string());
            }
            return Err(format!("ShellExecuteEx runas failed ({code})"));
        }
        Ok(sei.h_process)
    }

    fn close_process_handle(handle: Handle) {
        if handle != 0
            && let Some(w) = win32()
        {
            unsafe {
                (w.close_handle)(handle);
            }
        }
    }

    fn process_signaled(handle: Handle) -> bool {
        if handle == 0 {
            return false;
        }
        match win32() {
            Some(w) => unsafe { (w.wait_for_single_object)(handle, 0) == 0 },
            None => false,
        }
    }

    /// Launch `exe` elevated via the runas verb and wait
    /// for it to exit (Python `_runas_and_wait`).
    pub fn runas_and_wait(exe: &str, params: &str, timeout_secs: u64) -> Result<(), String> {
        let handle = runas_launch(exe, params)?;
        let w = win32().ok_or("kernel32 unavailable")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        if handle != 0 {
            unsafe {
                while std::time::Instant::now() < deadline {
                    if (w.wait_for_single_object)(handle, 1000) == 0 {
                        break;
                    }
                }
                (w.close_handle)(handle);
            }
        }
        Ok(())
    }

    /// subprocess.list2cmdline equivalent for the arg
    /// shapes this app builds (paths and flags; quote
    /// when a space or quote is present).
    fn list2cmdline(args: &[String]) -> String {
        let mut out = String::new();
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
                out.push_str(arg);
                continue;
            }
            out.push('"');
            let mut backslashes = 0usize;
            for ch in arg.chars() {
                match ch {
                    '\\' => {
                        backslashes += 1;
                        out.push('\\');
                    }
                    '"' => {
                        for _ in 0..backslashes {
                            out.push('\\');
                        }
                        backslashes = 0;
                        out.push_str("\\\"");
                    }
                    _ => {
                        backslashes = 0;
                        out.push(ch);
                    }
                }
            }
            for _ in 0..backslashes {
                out.push('\\');
            }
            out.push('"');
        }
        out
    }

    fn current_exe_string() -> Result<String, String> {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("current_exe failed: {e}"))
    }

    fn temp_file(prefix: &str, suffix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}{}{suffix}", uuid::Uuid::new_v4().simple()))
    }

    fn decode_key_lines(lines: &[String], expected: usize) -> Vec<Option<[u8; 32]>> {
        use base64::Engine;
        let mut results: Vec<Option<[u8; 32]>> = lines
            .iter()
            .map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with("ERROR:") {
                    return None;
                }
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(line)
                    .ok()?;
                decoded.try_into().ok()
            })
            .collect();
        while results.len() < expected {
            results.push(None);
        }
        results.truncate(expected);
        results
    }

    /// Runas fallback batch (Python
    /// `_run_elevated_helper_batch_runas`): one UAC
    /// prompt runs this same binary as the key helper.
    fn run_elevated_helper_batch_runas(
        app_bound_keys_b64: &[String],
    ) -> Result<Vec<Option<[u8; 32]>>, String> {
        if app_bound_keys_b64.is_empty() {
            return Ok(Vec::new());
        }
        let exe = current_exe_string()?;
        let input_path = temp_file("blindrss-abk-", ".txt");
        let output_path = temp_file("blindrss-v20-", ".txt");
        let result = (|| {
            std::fs::write(&input_path, app_bound_keys_b64.join("\n"))
                .map_err(|e| format!("helper input write failed: {e}"))?;
            std::fs::write(&output_path, "").map_err(|e| e.to_string())?;
            let args = vec![
                KEY_HELPER_FLAG.to_string(),
                "--input".to_string(),
                input_path.to_string_lossy().into_owned(),
                "--output".to_string(),
                output_path.to_string_lossy().into_owned(),
            ];
            // Python's wait: up to 120 s, breaking on the
            // process exiting OR the output file becoming
            // non-empty (some Windows versions return no
            // usable handle for runas). The prompt itself
            // can sit unanswered a while, so the deadline
            // covers the whole dance.
            let handle = runas_launch(&exe, &list2cmdline(&args))?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            let mut text = String::new();
            while std::time::Instant::now() < deadline {
                if process_signaled(handle) {
                    text = std::fs::read_to_string(&output_path).unwrap_or_default();
                    break;
                }
                text = std::fs::read_to_string(&output_path).unwrap_or_default();
                if !text.trim().is_empty() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            close_process_handle(handle);
            if text.trim().is_empty() {
                return Err("Elevated helper produced no result".to_string());
            }
            let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
            Ok(decode_key_lines(&lines, app_bound_keys_b64.len()))
        })();
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&output_path);
        result
    }

    // --- Silent elevation via a pre-registered scheduled task -----------

    pub const ELEVATED_TASK_PREFIX: &str = "BlindRSS Elevated Helper";

    fn elevated_request_path() -> PathBuf {
        std::env::temp_dir().join("blindrss-elevated-request.json")
    }

    fn elevated_response_path() -> PathBuf {
        std::env::temp_dir().join("blindrss-elevated-response.json")
    }

    /// Per-executable task name (Python
    /// `_elevated_task_name`): a dev checkout and the
    /// installed app own different tasks and never steal
    /// each other's registration.
    fn elevated_task_name() -> Result<String, String> {
        use sha2::Digest;
        let exe = current_exe_string()?;
        let digest = sha2::Sha256::digest(exe.as_bytes());
        let hex: String = digest[..5].iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("{ELEVATED_TASK_PREFIX} {hex}"))
    }

    fn schtasks(args: &[&str]) -> Result<String, String> {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("schtasks")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("schtasks failed to start: {e}"))?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if output.status.success() {
            Ok(text)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "schtasks {} failed: {}",
                args.first().copied().unwrap_or(""),
                stderr.trim()
            ))
        }
    }

    /// Python `_task_registered`: registered AND not
    /// stale (the task's action must point at this exe).
    fn task_registered(name: &str) -> bool {
        let Ok(xml) = schtasks(&["/query", "/tn", name, "/xml"]) else {
            return false;
        };
        let Ok(exe) = current_exe_string() else {
            return false;
        };
        xml.to_lowercase().contains(&exe.to_lowercase())
    }

    /// Register the silent task (Python
    /// `_register_elevated_task`): one UAC prompt, via a
    /// PowerShell registration script run elevated.
    /// Other per-executable tasks are unregistered first;
    /// the legacy fixed-name task is never touched.
    fn register_elevated_task(name: &str) -> Result<(), String> {
        let exe = current_exe_string()?;
        let ps_escape = |s: &str| s.replace('\'', "''");
        let script = format!(
            "$ErrorActionPreference = 'Stop'\n\
             $taskName = '{name}'\n\
             Get-ScheduledTask | Where-Object {{ $_.TaskName -match '^{prefix} [0-9a-f]{{10}}$' -and $_.TaskName -ne $taskName }} | Unregister-ScheduledTask -Confirm:$false | Out-Null\n\
             $action = New-ScheduledTaskAction -Execute '{exe}' -Argument '{flag}'\n\
             $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Highest\n\
             Register-ScheduledTask -TaskName $taskName -Action $action -Principal $principal -Force | Out-Null\n",
            name = ps_escape(name),
            prefix = ELEVATED_TASK_PREFIX,
            exe = ps_escape(&exe),
            flag = ELEVATED_HELPER_FLAG,
        );
        let script_path = temp_file("blindrss-register-task-", ".ps1");
        std::fs::write(&script_path, script)
            .map_err(|e| format!("task script write failed: {e}"))?;
        let params = format!(
            "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File {}",
            list2cmdline(&[script_path.to_string_lossy().into_owned()])
        );
        let result = runas_and_wait("powershell.exe", &params, 180);
        let _ = std::fs::remove_file(&script_path);
        result
    }

    enum TaskFail {
        /// Transport problem (no/invalid response): the
        /// caller may fall back to the prompting helper.
        Transport(String),
        /// The helper ran and reported an error: hard
        /// failure, no fallback (Python parity).
        Response(String),
    }

    fn write_json_atomic(path: &Path, value: &serde_json::Value) -> Result<(), String> {
        let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
        let text = serde_json::to_string(value).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        })
    }

    /// Run a request through the silent task (Python
    /// `_run_elevated_task`): registers first if needed,
    /// polls for the response up to 300 s.
    fn run_elevated_task(request: &serde_json::Value) -> Result<serde_json::Value, TaskFail> {
        let name = elevated_task_name().map_err(TaskFail::Transport)?;
        let mut just_registered = false;
        if !task_registered(&name) {
            register_elevated_task(&name).map_err(TaskFail::Transport)?;
            just_registered = true;
        }
        let request_path = elevated_request_path();
        let response_path = elevated_response_path();
        let _ = std::fs::remove_file(&response_path);
        write_json_atomic(&request_path, request).map_err(TaskFail::Transport)?;

        let attempts = if just_registered { 5 } else { 1 };
        let mut run_error = String::new();
        for attempt in 0..attempts {
            match schtasks(&["/run", "/tn", &name]) {
                Ok(_) => {
                    run_error.clear();
                    break;
                }
                Err(e) => {
                    run_error = e;
                    if attempt + 1 < attempts {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                    }
                }
            }
        }
        let result = (|| {
            if !run_error.is_empty() {
                return Err(TaskFail::Transport(run_error.clone()));
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                if response_path.is_file() {
                    match std::fs::read_to_string(&response_path) {
                        Ok(text) => {
                            let value: serde_json::Value =
                                serde_json::from_str(&text).map_err(|e| {
                                    TaskFail::Transport(format!(
                                        "elevated response is not JSON: {e}"
                                    ))
                                })?;
                            if let Some(error) = value.get("error").and_then(|v| v.as_str()) {
                                return Err(TaskFail::Response(error.to_string()));
                            }
                            return Ok(value);
                        }
                        Err(_) => {
                            // Mid-write by the helper; keep
                            // polling.
                        }
                    }
                }
                if std::time::Instant::now() >= deadline {
                    return Err(TaskFail::Transport(
                        "Timed out waiting for the elevated helper".to_string(),
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        })();
        let _ = std::fs::remove_file(&request_path);
        result
    }

    /// The batch entry point used by resolve_master_keys
    /// (Python `_run_elevated_helper_batch`): silent
    /// task first; runas fallback only when the task
    /// cannot be used; cancellation propagates.
    pub fn run_elevated_helper_batch(
        app_bound_keys_b64: &[String],
    ) -> Result<Vec<Option<[u8; 32]>>, String> {
        if app_bound_keys_b64.is_empty() {
            return Ok(Vec::new());
        }
        let request = serde_json::json!({ "keys": app_bound_keys_b64 });
        match run_elevated_task(&request) {
            Ok(response) => {
                let raw = response
                    .get("keys")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let lines: Vec<String> = raw
                    .iter()
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect();
                Ok(decode_key_lines(&lines, app_bound_keys_b64.len()))
            }
            Err(TaskFail::Response(e)) => Err(e),
            Err(TaskFail::Transport(e)) => {
                if error_is_cancelled(&e) {
                    return Err(e);
                }
                run_elevated_helper_batch_runas(app_bound_keys_b64)
            }
        }
    }

    /// The unified helper body (Python
    /// `run_elevated_helper_cli`), run elevated by the
    /// scheduled task: fixed request/response paths, so
    /// the task needs no dynamic arguments.
    pub fn run_elevated_helper() -> i32 {
        use base64::Engine;
        let request_path = elevated_request_path();
        let response_path = elevated_response_path();
        let request: serde_json::Value = match std::fs::read_to_string(&request_path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
            Err(e) => {
                let _ = write_json_atomic(
                    &response_path,
                    &serde_json::json!({ "error": format!("could not read request: {e}") }),
                );
                return 1;
            }
        };
        let mut keys_out: Vec<String> = Vec::new();
        if let Some(keys) = request.get("keys").and_then(|v| v.as_array()) {
            for item in keys {
                let Some(b64) = item.as_str() else {
                    keys_out.push("ERROR:request key is not a string".to_string());
                    continue;
                };
                match derive_master_key_from_app_bound_key(b64) {
                    Ok(key) => keys_out.push(base64::engine::general_purpose::STANDARD.encode(key)),
                    Err(e) => keys_out.push(format!("ERROR:{e}")),
                }
            }
        }
        let mut copies_out = serde_json::Map::new();
        let copy_paths: Vec<String> = request
            .get("copy")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if !copy_paths.is_empty() {
            let dest = request
                .get("copy_dir")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::temp_dir().join(format!(
                        "blindrss-vss-stage-{}",
                        uuid::Uuid::new_v4().simple()
                    ))
                });
            match vss_copy_files(&copy_paths, &dest) {
                Ok(staged) => {
                    for (index, path) in staged.iter().enumerate() {
                        if !path.is_empty() {
                            copies_out
                                .insert(index.to_string(), serde_json::Value::String(path.clone()));
                        }
                    }
                }
                Err(e) => {
                    let _ = write_json_atomic(&response_path, &serde_json::json!({ "error": e }));
                    return 1;
                }
            }
        }
        let response = serde_json::json!({ "keys": keys_out, "copies": copies_out });
        match write_json_atomic(&response_path, &response) {
            Ok(()) => 0,
            Err(_) => 1,
        }
    }

    // --- Volume Shadow Copy (locked cookie DBs) ---------------------------

    /// Runas VSS fallback (Python
    /// `_run_elevated_vss_copy_runas`): one prompt runs
    /// this binary as the VSS copy helper; 180 s
    /// deadline, manifest poll included.
    fn run_elevated_vss_copy_runas(
        copy_paths: &[PathBuf],
    ) -> Result<(std::collections::HashMap<usize, PathBuf>, Option<PathBuf>), String> {
        let exe = current_exe_string()?;
        let staging_dir = std::env::temp_dir().join(format!(
            "blindrss-vss-stage-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&staging_dir).map_err(|e| format!("staging dir failed: {e}"))?;
        let manifest_path = staging_dir.join("manifest.txt");
        let input_path = temp_file("blindrss-vss-src-", ".txt");
        let cleanup = |dir: &PathBuf, input: &PathBuf| {
            let _ = std::fs::remove_file(input);
            let _ = std::fs::remove_dir_all(dir);
        };
        let lines: Vec<String> = copy_paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if let Err(e) = std::fs::write(&input_path, lines.join("\n")) {
            cleanup(&staging_dir, &input_path);
            return Err(format!("VSS input write failed: {e}"));
        }
        let args = vec![
            VSS_HELPER_FLAG.to_string(),
            "--input".to_string(),
            input_path.to_string_lossy().into_owned(),
            "--output-dir".to_string(),
            staging_dir.to_string_lossy().into_owned(),
            "--manifest".to_string(),
            manifest_path.to_string_lossy().into_owned(),
        ];
        // Python's wait: up to 180 s, breaking on the
        // process exiting OR the manifest appearing.
        let handle = match runas_launch(&exe, &list2cmdline(&args)) {
            Ok(handle) => handle,
            Err(e) => {
                cleanup(&staging_dir, &input_path);
                return Err(e);
            }
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
        let mut text = String::new();
        while std::time::Instant::now() < deadline {
            if process_signaled(handle) {
                text = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                break;
            }
            text = std::fs::read_to_string(&manifest_path).unwrap_or_default();
            if !text.trim().is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        close_process_handle(handle);
        if text.trim().is_empty() {
            cleanup(&staging_dir, &input_path);
            return Err("Elevated VSS helper produced no result".to_string());
        }
        let _ = std::fs::remove_file(&input_path);
        let mut staged = std::collections::HashMap::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with("ERROR:") {
                staged.insert(index, PathBuf::from(line));
            }
        }
        Ok((staged, Some(staging_dir)))
    }

    /// Parent VSS copy (Python `_run_elevated_vss_copy`):
    /// silent task first, runas fallback; returns
    /// (staged map, staging dir the caller must remove).
    pub fn run_elevated_vss_copy(
        copy_paths: &[PathBuf],
    ) -> Result<(std::collections::HashMap<usize, PathBuf>, Option<PathBuf>), String> {
        if copy_paths.is_empty() {
            return Ok((std::collections::HashMap::new(), None));
        }
        let staging_dir = std::env::temp_dir().join(format!(
            "blindrss-vss-stage-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&staging_dir).map_err(|e| format!("staging dir failed: {e}"))?;
        let lines: Vec<String> = copy_paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let request = serde_json::json!({
            "copy": lines,
            "copy_dir": staging_dir.to_string_lossy(),
        });
        match run_elevated_task(&request) {
            Ok(response) => {
                let mut staged = std::collections::HashMap::new();
                if let Some(copies) = response.get("copies").and_then(|v| v.as_object()) {
                    for (key, value) in copies {
                        if let (Ok(index), Some(path)) = (key.parse::<usize>(), value.as_str()) {
                            staged.insert(index, PathBuf::from(path));
                        }
                    }
                }
                Ok((staged, Some(staging_dir)))
            }
            Err(TaskFail::Response(e)) => {
                let _ = std::fs::remove_dir_all(&staging_dir);
                Err(e)
            }
            Err(TaskFail::Transport(e)) => {
                let _ = std::fs::remove_dir_all(&staging_dir);
                if error_is_cancelled(&e) {
                    return Err(e);
                }
                run_elevated_vss_copy_runas(copy_paths)
            }
        }
    }

    /// The elevated-side copy itself (Python
    /// `_vss_copy_files`): one ClientAccessible snapshot
    /// per source drive via the WMI COM API (deliberately
    /// NOT PowerShell — security products flag hidden
    /// PowerShell creating shadow copies), files copied
    /// out from the snapshot device, ACL relaxed for the
    /// non-elevated caller, snapshots deleted at the
    /// end. Returns one staged path per source (""
    /// when a source produced no copy).
    pub fn vss_copy_files(source_paths: &[String], dest_dir: &Path) -> Result<Vec<String>, String> {
        std::fs::create_dir_all(dest_dir).map_err(|e| format!("VSS dest dir failed: {e}"))?;
        let mut staged = vec![String::new(); source_paths.len()];
        if source_paths.is_empty() {
            return Ok(staged);
        }
        let wmi = Wmi::connect()?;
        let mut shadows: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        let copy_result = (|| {
            let shadow_class = wmi.get("Win32_ShadowCopy")?;
            for (index, source) in source_paths.iter().enumerate() {
                let source = source.trim();
                if source.is_empty() {
                    continue;
                }
                // ntpath.splitdrive: "C:\..." -> ("C:", "\...")
                let bytes = source.as_bytes();
                if bytes.len() < 2 || bytes[1] != b':' {
                    return Err(format!("VSS source is not on a local volume: {source}"));
                }
                let drive = &source[..2];
                let root = format!("{drive}\\");
                let relative = source[2..].trim_start_matches(['\\', '/']);
                let shadow_key = root.to_lowercase();
                if !shadows.contains_key(&shadow_key) {
                    let method =
                        shadow_class.call_method("Methods_", &[Variant::bstr("Create")])?;
                    let in_params_class = method.get_property("InParameters")?;
                    let in_params = in_params_class.call_method("SpawnInstance_", &[])?;
                    in_params.put_property("Volume", Variant::bstr(&root))?;
                    in_params.put_property("Context", Variant::bstr("ClientAccessible"))?;
                    let class_path = shadow_class
                        .get_property("Path_")?
                        .get_property("Path")?
                        .as_string()
                        .ok_or("Win32_ShadowCopy path missing")?;
                    let result = wmi.exec_method(&class_path, "Create", &in_params)?;
                    let return_value = result
                        .get_property("ReturnValue")?
                        .as_i32()
                        .ok_or("VSS Create returned no status")?;
                    if return_value != 0 {
                        return Err(format!("VSS Create returned {return_value}"));
                    }
                    let shadow_id = result
                        .get_property("ShadowID")?
                        .as_string()
                        .filter(|s| !s.is_empty())
                        .ok_or("VSS Create returned no shadow ID")?;
                    let shadow = wmi.get(&format!(
                        "Win32_ShadowCopy.ID='{}'",
                        shadow_id.replace('\'', "''")
                    ))?;
                    let device = shadow
                        .get_property("DeviceObject")?
                        .as_string()
                        .filter(|s| !s.is_empty())
                        .ok_or("VSS shadow copy has no device object")?;
                    shadows.insert(shadow_key.clone(), (shadow_id, device));
                }
                let (_id, device) = shadows.get(&shadow_key).unwrap();
                let shadow_source =
                    format!("{}\\{}", device.trim_end_matches(['\\', '/']), relative);
                let destination = dest_dir.join(format!("{index}.cookies"));
                for suffix in ["", "-wal", "-shm"] {
                    let candidate = format!("{shadow_source}{suffix}");
                    if Path::new(&candidate).is_file() {
                        let dest = format!("{}{suffix}", destination.display());
                        std::fs::copy(&candidate, &dest)
                            .map_err(|e| format!("VSS copy failed: {e}"))?;
                    }
                }
            }
            // The elevated helper's copies can carry the
            // protected-admin DACL; grant the interactive
            // user's group full control so the caller can
            // read (and later delete) them.
            use std::os::windows::process::CommandExt;
            let dest_string = dest_dir.to_string_lossy().into_owned();
            // Python logs a warning when this fails and
            // continues — the copies may still be
            // readable — so the result is ignored here.
            let _ = std::process::Command::new("icacls")
                .args([
                    dest_string.as_str(),
                    "/grant",
                    "*S-1-5-32-545:(OI)(CI)F",
                    "/T",
                    "/Q",
                ])
                .creation_flags(CREATE_NO_WINDOW)
                .output();
            for (index, slot) in staged.iter_mut().enumerate() {
                let destination = dest_dir.join(format!("{index}.cookies"));
                if destination.is_file() {
                    *slot = destination.to_string_lossy().into_owned();
                }
            }
            Ok::<(), String>(())
        })();
        // Snapshots are explicitly deleted after the copy
        // (and die with this process regardless).
        for (shadow_id, _device) in shadows.values() {
            if let Ok(shadow) = wmi.get(&format!(
                "Win32_ShadowCopy.ID='{}'",
                shadow_id.replace('\'', "''")
            )) {
                let _ = shadow.call_method("Delete_", &[]);
            }
        }
        copy_result?;
        Ok(staged)
    }

    // --- Minimal IDispatch over WMI (SWbemLocator) -------------------------
    //
    // Python drives WMI through pywin32's late binding;
    // this is the same late-bound path hand-rolled: just
    // enough COM to connect, get objects, and invoke
    // methods / properties by name.

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        d1: u32,
        d2: u16,
        d3: u16,
        d4: [u8; 8],
    }

    const CLSID_SWEM_LOCATOR: Guid = Guid {
        d1: 0x4590_F811,
        d2: 0x1D3A,
        d3: 0x11D0,
        d4: [0x89, 0x1F, 0x00, 0xAA, 0x00, 0x4B, 0x2E, 0x24],
    };
    const IID_IDISPATCH: Guid = Guid {
        d1: 0x0002_0400,
        d2: 0x0000,
        d3: 0x0000,
        d4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };

    /// A VARIANT as this code uses it: 24 bytes, the
    /// value union at offset 8 (BSTR pointer, IDispatch
    /// pointer, or a 4-byte integer in the low half).
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct VariantRaw {
        vt: u16,
        r1: u16,
        r2: u16,
        r3: u16,
        data: usize,
        tail: u64,
    }

    const VT_EMPTY: u16 = 0;
    const VT_I4: u16 = 3;
    const VT_BSTR: u16 = 8;
    const VT_DISPATCH: u16 = 9;
    const DISPATCH_METHOD: u16 = 1;
    const DISPATCH_PROPERTYGET: u16 = 2;
    const DISPATCH_PROPERTYPUT: u16 = 4;
    const DISPID_PROPERTYPUT: i32 = -3;

    #[repr(C)]
    struct DispParams {
        rgvarg: *mut VariantRaw,
        rgdispid_named_args: *mut i32,
        c_args: u32,
        c_named_args: u32,
    }

    #[repr(C)]
    struct DispatchVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        get_type_info_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
        get_type_info: unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> i32,
        get_ids_of_names: unsafe extern "system" fn(
            *mut c_void,
            *const Guid,
            *const *mut u16,
            u32,
            u32,
            *mut i32,
        ) -> i32,
        invoke: unsafe extern "system" fn(
            *mut c_void,
            i32,
            *const Guid,
            u32,
            u16,
            *mut DispParams,
            *mut VariantRaw,
            *mut c_void,
            *mut u32,
        ) -> i32,
    }

    struct OleAut {
        sys_alloc_string: unsafe extern "system" fn(*const u16) -> *mut u16,
        variant_clear: unsafe extern "system" fn(*mut VariantRaw) -> i32,
    }

    fn oleaut() -> Option<OleAut> {
        let lib = library("oleaut32.dll")?;
        Some(OleAut {
            sys_alloc_string: proc(lib, "SysAllocString")?,
            variant_clear: proc(lib, "VariantClear")?,
        })
    }

    enum Variant {
        Bstr(String),
        /// A borrowed IDispatch pointer: Invoke AddRefs
        /// it, VariantClear releases that ref; the enum
        /// itself owns nothing.
        DispatchRef(*mut c_void),
    }

    impl Variant {
        fn bstr(s: &str) -> Variant {
            Variant::Bstr(s.to_string())
        }
    }

    /// An owned IDispatch pointer; released on drop.
    struct Dispatch {
        ptr: *mut c_void,
    }

    impl Dispatch {
        fn vtbl(&self) -> &DispatchVtbl {
            unsafe { &*(*(self.ptr as *const *const DispatchVtbl)) }
        }

        fn dispid(&self, name: &str) -> Result<i32, String> {
            let mut id: i32 = 0;
            let name_wide = wide(name);
            let names: [*mut u16; 1] = [name_wide.as_ptr() as *mut u16];
            let hr = unsafe {
                (self.vtbl().get_ids_of_names)(
                    self.ptr,
                    &IID_IDISPATCH,
                    names.as_ptr(),
                    1,
                    0,
                    &mut id,
                )
            };
            if hr != 0 {
                return Err(format!("WMI member {name} not found ({hr:#x})"));
            }
            Ok(id)
        }

        fn invoke(&self, name: &str, flags: u16, args: &[Variant]) -> Result<VariantRaw, String> {
            let ole = oleaut().ok_or("oleaut32.dll unavailable")?;
            let id = self.dispid(name)?;
            // COM passes arguments in reverse order.
            let mut raw_args: Vec<VariantRaw> = args.iter().rev().map(|a| a.to_raw(&ole)).collect();
            let mut named: [i32; 1] = [DISPID_PROPERTYPUT];
            let mut params = DispParams {
                rgvarg: if raw_args.is_empty() {
                    std::ptr::null_mut()
                } else {
                    raw_args.as_mut_ptr()
                },
                rgdispid_named_args: if flags == DISPATCH_PROPERTYPUT {
                    named.as_mut_ptr()
                } else {
                    std::ptr::null_mut()
                },
                c_args: raw_args.len() as u32,
                c_named_args: if flags == DISPATCH_PROPERTYPUT { 1 } else { 0 },
            };
            let mut result = VariantRaw {
                vt: VT_EMPTY,
                r1: 0,
                r2: 0,
                r3: 0,
                data: 0,
                tail: 0,
            };
            let hr = unsafe {
                (self.vtbl().invoke)(
                    self.ptr,
                    id,
                    &IID_IDISPATCH,
                    0,
                    flags,
                    &mut params,
                    &mut result,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            for raw in raw_args.iter_mut() {
                unsafe {
                    (ole.variant_clear)(raw);
                }
            }
            if hr != 0 {
                return Err(format!("WMI call {name} failed ({hr:#x})"));
            }
            Ok(result)
        }

        fn get_property(&self, name: &str) -> Result<RawValue, String> {
            let raw = self.invoke(name, DISPATCH_PROPERTYGET, &[])?;
            Ok(RawValue(raw))
        }

        fn put_property(&self, name: &str, value: Variant) -> Result<(), String> {
            self.invoke(name, DISPATCH_PROPERTYPUT, &[value])?;
            Ok(())
        }

        fn call_method(&self, name: &str, args: &[Variant]) -> Result<RawValue, String> {
            let raw = self.invoke(name, DISPATCH_METHOD | DISPATCH_PROPERTYGET, args)?;
            Ok(RawValue(raw))
        }
    }

    impl Drop for Dispatch {
        fn drop(&mut self) {
            unsafe {
                (self.vtbl().release)(self.ptr);
            }
        }
    }

    /// A VARIANT returned from Invoke; cleared on drop.
    struct RawValue(VariantRaw);

    impl RawValue {
        fn as_string(&self) -> Option<String> {
            if self.0.vt == VT_BSTR && self.0.data != 0 {
                let ptr = self.0.data as *const u16;
                unsafe {
                    // BSTR: length prefix sits 4 bytes
                    // before the data.
                    let len_ptr = (ptr as *const u8).sub(4) as *const u32;
                    let len = *len_ptr as usize;
                    Some(String::from_utf16_lossy(std::slice::from_raw_parts(
                        ptr, len,
                    )))
                }
            } else {
                None
            }
        }

        /// The held IDispatch pointer, borrowed (the
        /// variant keeps its reference).
        fn dispatch_ptr(&self) -> Option<*mut c_void> {
            if self.0.vt == VT_DISPATCH && self.0.data != 0 {
                Some(self.0.data as *mut c_void)
            } else {
                None
            }
        }

        fn as_i32(&self) -> Option<i32> {
            if self.0.vt == VT_I4 {
                Some(self.0.data as i32)
            } else {
                None
            }
        }

        /// Borrow the held IDispatch for one call
        /// (the variant keeps its reference; the
        /// borrowed owner is forgotten, never dropped).
        fn with_dispatch<R>(
            &self,
            f: impl FnOnce(&Dispatch) -> Result<R, String>,
        ) -> Result<R, String> {
            if self.0.vt == VT_DISPATCH && self.0.data != 0 {
                let borrowed = Dispatch {
                    ptr: self.0.data as *mut c_void,
                };
                let out = f(&borrowed);
                std::mem::forget(borrowed);
                out
            } else {
                Err("WMI value is not an object".to_string())
            }
        }

        fn get_property(&self, name: &str) -> Result<RawValue, String> {
            self.with_dispatch(|d| d.get_property(name))
        }

        fn put_property(&self, name: &str, value: Variant) -> Result<(), String> {
            self.with_dispatch(|d| d.put_property(name, value))
        }

        fn call_method(&self, name: &str, args: &[Variant]) -> Result<RawValue, String> {
            self.with_dispatch(|d| d.call_method(name, args))
        }

        /// Take ownership of the held IDispatch (the
        /// variant is emptied so its drop clears
        /// nothing).
        fn into_dispatch(mut self) -> Result<Dispatch, String> {
            if self.0.vt == VT_DISPATCH && self.0.data != 0 {
                let ptr = self.0.data as *mut c_void;
                self.0.data = 0;
                self.0.vt = VT_EMPTY;
                Ok(Dispatch { ptr })
            } else {
                Err("WMI value is not an object".to_string())
            }
        }
    }

    impl Drop for RawValue {
        fn drop(&mut self) {
            if let Some(ole) = oleaut() {
                unsafe {
                    (ole.variant_clear)(&mut self.0);
                }
            }
        }
    }

    impl Variant {
        fn to_raw(&self, ole: &OleAut) -> VariantRaw {
            match self {
                Variant::Bstr(s) => VariantRaw {
                    vt: VT_BSTR,
                    r1: 0,
                    r2: 0,
                    r3: 0,
                    data: unsafe { (ole.sys_alloc_string)(wide(s).as_ptr()) } as usize,
                    tail: 0,
                },
                Variant::DispatchRef(ptr) => {
                    let vtbl = unsafe { &*(*(*ptr as *const *const DispatchVtbl)) };
                    unsafe {
                        (vtbl.add_ref)(*ptr);
                    }
                    VariantRaw {
                        vt: VT_DISPATCH,
                        r1: 0,
                        r2: 0,
                        r3: 0,
                        data: *ptr as usize,
                        tail: 0,
                    }
                }
            }
        }
    }

    /// A connected WMI service (root\cimv2), mirroring
    /// the pywin32 connection in Python's _vss_copy_files.
    struct Wmi {
        service: Dispatch,
    }

    impl Wmi {
        fn connect() -> Result<Wmi, String> {
            type CoInitializeEx = unsafe extern "system" fn(*const c_void, u32) -> i32;
            type CoCreateInstance = unsafe extern "system" fn(
                *const Guid,
                *mut c_void,
                u32,
                *const Guid,
                *mut *mut c_void,
            ) -> i32;
            let ole32 = library("ole32.dll").ok_or("ole32.dll unavailable")?;
            let co_init: CoInitializeEx =
                proc(ole32, "CoInitializeEx").ok_or("CoInitializeEx unavailable")?;
            let co_create: CoCreateInstance =
                proc(ole32, "CoCreateInstance").ok_or("CoCreateInstance unavailable")?;
            unsafe {
                // COINIT_MULTITHREADED; RPC_E_CHANGED_MODE
                // (already initialized differently) is
                // fine — the apartment already exists.
                let hr = co_init(std::ptr::null(), 0);
                // RPC_E_CHANGED_MODE (0x80010106): the
                // apartment already exists; proceed.
                if hr < 0 && hr != -2_147_417_850 {
                    return Err(format!("CoInitializeEx failed ({hr:#x})"));
                }
                let mut locator_ptr: *mut c_void = std::ptr::null_mut();
                // CLSCTX_INPROC_SERVER | CLSCTX_LOCAL_SERVER
                let hr = co_create(
                    &CLSID_SWEM_LOCATOR,
                    std::ptr::null_mut(),
                    0x1 | 0x4,
                    &IID_IDISPATCH,
                    &mut locator_ptr,
                );
                if hr != 0 || locator_ptr.is_null() {
                    return Err(format!("SWbemLocator create failed ({hr:#x})"));
                }
                let locator = Dispatch { ptr: locator_ptr };
                let service_raw = locator.call_method(
                    "ConnectServer",
                    &[Variant::bstr("."), Variant::bstr("root\\cimv2")],
                )?;
                let service = service_raw.into_dispatch()?;
                Ok(Wmi { service })
            }
        }

        fn get(&self, path: &str) -> Result<Dispatch, String> {
            let raw = self.service.call_method("Get", &[Variant::bstr(path)])?;
            raw.into_dispatch()
        }

        fn exec_method(
            &self,
            class_path: &str,
            method: &str,
            in_params: &RawValue,
        ) -> Result<RawValue, String> {
            let ptr = in_params
                .dispatch_ptr()
                .ok_or("VSS in-params are not an object")?;
            self.service.call_method(
                "ExecMethod_",
                &[
                    Variant::bstr(class_path),
                    Variant::bstr(method),
                    Variant::DispatchRef(ptr),
                ],
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Batch preparation for profile reads
// ---------------------------------------------------------------------------

/// Everything a caller needs to read a set of profiles
/// with v20 support: the resolved master keys (by
/// fingerprint) and the VSS-staged copies of locked
/// databases (by position in the caller's slice).
pub struct V20ReadPrep {
    pub keys: std::collections::HashMap<String, [u8; 32]>,
    pub staged: std::collections::HashMap<usize, PathBuf>,
    staging_dir: Option<PathBuf>,
}

impl V20ReadPrep {
    /// The master key for one profile, if it resolved.
    pub fn key_for(&self, profile: &crate::chromium::ChromiumProfile) -> Option<&[u8; 32]> {
        let b64 = app_bound_key_b64(&profile.local_state);
        if b64.is_empty() {
            None
        } else {
            self.keys.get(&v20_fingerprint(&b64))
        }
    }

    /// Remove the VSS staging directory (the caller's
    /// contract once the staged copies are consumed).
    pub fn cleanup(&self) {
        if let Some(dir) = &self.staging_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Resolve master keys for a batch of profiles in one
/// elevation and stage their locked databases (Python's
/// import-level batching in `import_chromium_cookies`).
/// `data_dir` holds the key cache. Profiles without an
/// app-bound key simply get no key.
pub fn prepare_v20_reads(
    profiles: &[&crate::chromium::ChromiumProfile],
    data_dir: &Path,
) -> V20ReadPrep {
    let mut app_bound_map: Vec<(String, String)> = Vec::new();
    for profile in profiles {
        let b64 = app_bound_key_b64(&profile.local_state);
        if !b64.is_empty() {
            app_bound_map.push((v20_fingerprint(&b64), b64));
        }
    }
    let resolution = resolve_master_keys(&app_bound_map, data_dir, true);
    // Mutated only by the Windows VSS stage below.
    #[allow(unused_mut)]
    let mut prep = V20ReadPrep {
        keys: resolution.keys,
        staged: std::collections::HashMap::new(),
        staging_dir: None,
    };
    #[cfg(target_os = "windows")]
    {
        let locked: Vec<PathBuf> = profiles
            .iter()
            .filter(|p| is_locked_file(&p.cookie_db))
            .map(|p| p.cookie_db.clone())
            .collect();
        if !locked.is_empty()
            && let Ok((map, dir)) = run_elevated_vss_copy(&locked)
        {
            let locked_positions: Vec<usize> = profiles
                .iter()
                .enumerate()
                .filter(|(_, p)| is_locked_file(&p.cookie_db))
                .map(|(i, _)| i)
                .collect();
            for (locked_pos, staged_path) in map {
                if let Some(position) = locked_positions.get(locked_pos) {
                    prep.staged.insert(*position, staged_path);
                }
            }
            prep.staging_dir = dir;
        }
    }
    prep
}
