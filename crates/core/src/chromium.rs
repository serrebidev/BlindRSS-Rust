//! Chromium cookie import (port of Python core/chromium_cookies.py,
//! the v10 half). Lists installed Chromium-family profiles, loads
//! each profile's v10 key (Windows: DPAPI-unwrap of the Local State
//! key, then AES-256-GCM; macOS: Keychain password + PBKDF2 into
//! AES-128-CBC; Linux: the "peanuts" pre-keyring default), and reads
//! decrypted cookies out of a temp copy of the profile's SQLite DB.
//!
//! NOT ported: the v20 app-bound path. Python resolves it through
//! SYSTEM impersonation, a CNG machine key and a UAC-elevated helper
//! process; the Rust app ships no elevation machinery, so v20 rows
//! are counted as skipped instead of decrypted. v10 remains the
//! default cookie scheme for non-domain-joined installs.

use crate::cookies::Cookie;
use std::path::{Path, PathBuf};

const NT_TO_UNIX_OFFSET: i64 = 11_644_473_600;
const DOMAIN_HASH_LEN: usize = 32;

#[derive(Debug, Clone)]
pub struct ChromiumProfile {
    pub browser: String,
    pub profile: String,
    pub cookie_db: PathBuf,
    pub local_state: PathBuf,
    pub mtime: f64,
}

fn data_roots() -> Vec<(String, PathBuf)> {
    let mut specs: Vec<(String, String, Vec<String>)> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let roaming = std::env::var("APPDATA").unwrap_or_default();
        let table: &[(&str, &str, &[&str])] = &[
            ("Google Chrome", "L", &["Google", "Chrome", "User Data"]),
            (
                "Google Chrome Beta",
                "L",
                &["Google", "Chrome Beta", "User Data"],
            ),
            (
                "Google Chrome Dev",
                "L",
                &["Google", "Chrome Dev", "User Data"],
            ),
            (
                "Google Chrome Canary",
                "L",
                &["Google", "Chrome SxS", "User Data"],
            ),
            ("Microsoft Edge", "L", &["Microsoft", "Edge", "User Data"]),
            (
                "Microsoft Edge Beta",
                "L",
                &["Microsoft", "Edge Beta", "User Data"],
            ),
            (
                "Microsoft Edge Dev",
                "L",
                &["Microsoft", "Edge Dev", "User Data"],
            ),
            (
                "Microsoft Edge Canary",
                "L",
                &["Microsoft", "Edge SxS", "User Data"],
            ),
            (
                "Brave",
                "L",
                &["BraveSoftware", "Brave-Browser", "User Data"],
            ),
            (
                "Brave Beta",
                "L",
                &["BraveSoftware", "Brave-Browser-Beta", "User Data"],
            ),
            (
                "Brave Dev",
                "L",
                &["BraveSoftware", "Brave-Browser-Dev", "User Data"],
            ),
            (
                "Brave Nightly",
                "L",
                &["BraveSoftware", "Brave-Browser-Nightly", "User Data"],
            ),
            ("Chromium", "L", &["Chromium", "User Data"]),
            ("Vivaldi", "L", &["Vivaldi", "User Data"]),
            ("Vivaldi Snapshot", "L", &["Vivaldi Snapshot", "User Data"]),
            ("Opera", "R", &["Opera Software", "Opera Stable"]),
            ("Opera Beta", "R", &["Opera Software", "Opera Next"]),
            (
                "Opera Developer",
                "R",
                &["Opera Software", "Opera Developer"],
            ),
            ("Opera GX", "R", &["Opera Software", "Opera GX Stable"]),
        ];
        for (label, which, parts) in table {
            let base = if *which == "L" { &local } else { &roaming };
            specs.push((
                label.to_string(),
                base.clone(),
                parts.iter().map(|s| s.to_string()).collect(),
            ));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let base = std::env::var("HOME")
            .map(|h| format!("{h}/Library/Application Support"))
            .unwrap_or_default();
        let table: &[(&str, &[&str])] = &[
            ("Google Chrome", &["Google", "Chrome"]),
            ("Google Chrome Beta", &["Google", "Chrome Beta"]),
            ("Google Chrome Dev", &["Google", "Chrome Dev"]),
            ("Google Chrome Canary", &["Google", "Chrome Canary"]),
            ("Microsoft Edge", &["Microsoft Edge"]),
            ("Microsoft Edge Beta", &["Microsoft Edge Beta"]),
            ("Microsoft Edge Dev", &["Microsoft Edge Dev"]),
            ("Brave", &["BraveSoftware", "Brave-Browser"]),
            ("Brave Beta", &["BraveSoftware", "Brave-Browser-Beta"]),
            ("Brave Nightly", &["BraveSoftware", "Brave-Browser-Nightly"]),
            ("Chromium", &["Chromium"]),
            ("Vivaldi", &["Vivaldi"]),
            ("Opera", &["com.operasoftware.Opera"]),
            ("Opera GX", &["com.operasoftware.OperaGX"]),
        ];
        for (label, parts) in table {
            specs.push((
                label.to_string(),
                base.clone(),
                parts.iter().map(|s| s.to_string()).collect(),
            ));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| format!("{h}/.config"))
                .unwrap_or_default()
        });
        let table: &[(&str, &[&str])] = &[
            ("Google Chrome", &["google-chrome"]),
            ("Google Chrome Beta", &["google-chrome-beta"]),
            ("Google Chrome Unstable", &["google-chrome-unstable"]),
            ("Chromium", &["chromium"]),
            ("Microsoft Edge", &["microsoft-edge"]),
            ("Microsoft Edge Beta", &["microsoft-edge-beta"]),
            ("Brave", &["BraveSoftware", "Brave-Browser"]),
            ("Brave Beta", &["BraveSoftware", "Brave-Browser-Beta"]),
            ("Brave Nightly", &["BraveSoftware", "Brave-Browser-Nightly"]),
            ("Vivaldi", &["vivaldi"]),
            ("Vivaldi Snapshot", &["vivaldi-snapshot"]),
            ("Opera", &["opera"]),
        ];
        for (label, parts) in table {
            specs.push((
                label.to_string(),
                base.clone(),
                parts.iter().map(|s| s.to_string()).collect(),
            ));
        }
    }
    let mut out = Vec::new();
    for (label, base, parts) in specs {
        if base.is_empty() {
            continue;
        }
        let mut root = PathBuf::from(base);
        for part in parts {
            root.push(part);
        }
        if root.is_dir() {
            out.push((label, root));
        }
    }
    out
}

fn find_cookie_dbs(root: &Path) -> Vec<PathBuf> {
    let mut dbs = Vec::new();
    for cand in [root.join("Network").join("Cookies"), root.join("Cookies")] {
        if cand.is_file() {
            dbs.push(cand);
        }
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            for cand in [dir.join("Network").join("Cookies"), dir.join("Cookies")] {
                if cand.is_file() {
                    dbs.push(cand);
                }
            }
        }
    }
    dbs
}

/// Importable Chromium profiles, newest cookie DB first
/// (Python's list_chromium_profiles).
pub fn list_profiles() -> Vec<ChromiumProfile> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (label, root) in data_roots() {
        let local_state = root.join("Local State");
        for db in find_cookie_dbs(&root) {
            let key = db.to_string_lossy().to_lowercase();
            if !seen.insert(key) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&db) else {
                continue;
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            let mut profile_dir = db.parent().map(|p| p.to_path_buf()).unwrap_or_default();
            if profile_dir
                .file_name()
                .map(|n| n.eq_ignore_ascii_case("network"))
                .unwrap_or(false)
            {
                profile_dir = profile_dir
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
            }
            let profile = profile_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            found.push(ChromiumProfile {
                browser: label.clone(),
                profile,
                cookie_db: db,
                local_state: local_state.clone(),
                mtime,
            });
        }
    }
    found.sort_by(|a, b| {
        b.mtime
            .partial_cmp(&a.mtime)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    found
}

/// The Keychain/keyring account name for a browser label
/// (Python's _browser_key_for_label).
pub fn browser_key_for_label(label: &str) -> &'static str {
    let low = label.to_ascii_lowercase();
    if low.contains("edge") {
        "Microsoft Edge"
    } else if low.contains("brave") {
        "Brave"
    } else if low.contains("vivaldi") {
        "Vivaldi"
    } else if low.contains("chromium") {
        "Chromium"
    } else if low.contains("opera") {
        "Opera"
    } else {
        "Chrome"
    }
}

/// A profile's v10 key: 32-byte AES-GCM key (Windows) or 16-byte
/// AES-CBC key (macOS/Linux PBKDF2 derivations).
#[derive(Debug, Clone)]
pub enum V10Key {
    Gcm([u8; 32]),
    Cbc([u8; 16]),
}

pub fn load_v10_key(local_state: &Path, browser_key: &str) -> Option<V10Key> {
    #[cfg(target_os = "windows")]
    {
        let _ = browser_key;
        load_v10_key_windows(local_state)
    }
    #[cfg(target_os = "macos")]
    {
        let password = macos_keychain_password(browser_key)?;
        let mut key = [0u8; 16];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(&password, b"saltysalt", 1003, &mut key);
        Some(V10Key::Cbc(key))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = (local_state, browser_key);
        let mut key = [0u8; 16];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(b"peanuts", b"saltysalt", 1, &mut key);
        Some(V10Key::Cbc(key))
    }
}

#[cfg(target_os = "macos")]
fn macos_keychain_password(browser_key: &str) -> Option<Vec<u8>> {
    let service = format!("{browser_key} Safe Storage");
    let out = std::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-w",
            "-a",
            browser_key,
            "-s",
            &service,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let pw = text.trim();
    if pw.is_empty() {
        None
    } else {
        Some(pw.as_bytes().to_vec())
    }
}

#[cfg(target_os = "windows")]
fn load_v10_key_windows(local_state: &Path) -> Option<V10Key> {
    use base64::Engine;
    let text = std::fs::read_to_string(local_state).ok()?;
    let state: serde_json::Value = serde_json::from_str(&text).ok()?;
    let enc_b64 = state.get("os_crypt")?.get("encrypted_key")?.as_str()?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(enc_b64)
        .ok()?;
    if raw.len() <= 5 || &raw[..5] != b"DPAPI" {
        return None;
    }
    let plain = dpapi_unprotect(&raw[5..])?;
    let key: [u8; 32] = plain.try_into().ok()?;
    Some(V10Key::Gcm(key))
}

#[cfg(target_os = "windows")]
fn dpapi_unprotect(blob: &[u8]) -> Option<Vec<u8>> {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    #[repr(C)]
    struct DataBlob {
        size: u32,
        data: *mut u8,
    }
    type CryptUnprotectData = unsafe extern "system" fn(
        *const DataBlob,
        *mut *mut u16,
        *const DataBlob,
        *const c_void,
        *const c_void,
        u32,
        *mut DataBlob,
    ) -> i32;
    type LocalFree = unsafe extern "system" fn(*mut c_void) -> *mut c_void;

    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> usize;
        fn GetProcAddress(module: usize, name: *const u8) -> usize;
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
    static CRYPT32: OnceLock<usize> = OnceLock::new();
    static KERNEL32: OnceLock<usize> = OnceLock::new();
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let crypt = *CRYPT32.get_or_init(|| unsafe { LoadLibraryW(wide("crypt32.dll").as_ptr()) });
    let kern = *KERNEL32.get_or_init(|| unsafe { LoadLibraryW(wide("kernel32.dll").as_ptr()) });
    if crypt == 0 || kern == 0 {
        return None;
    }
    let unprotect: CryptUnprotectData = proc(crypt, "CryptUnprotectData")?;
    let local_free: LocalFree = proc(kern, "LocalFree")?;
    let input = DataBlob {
        size: blob.len() as u32,
        data: blob.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        size: 0,
        data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        unprotect(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut output,
        )
    };
    if ok == 0 || output.data.is_null() {
        return None;
    }
    let out = unsafe { std::slice::from_raw_parts(output.data, output.size as usize).to_vec() };
    unsafe {
        local_free(output.data as *mut c_void);
    }
    Some(out)
}

/// Decrypt one Chromium encrypted_value (Python's
/// decrypt_cookie_value for the v10 scheme). v20 returns None —
/// counted as skipped by callers.
pub fn decrypt_cookie_value(
    encrypted: &[u8],
    key: Option<&V10Key>,
    strip_domain_hash: bool,
) -> Option<Vec<u8>> {
    if encrypted.is_empty() {
        return Some(Vec::new());
    }
    let prefix = &encrypted[..3.min(encrypted.len())];
    if prefix == b"v20" {
        return None;
    }
    if prefix != b"v10" {
        return None;
    }
    let key = key?;
    let plain = match key {
        V10Key::Gcm(k) => {
            if encrypted.len() < 3 + 12 + 16 + 1 {
                return None;
            }
            decrypt_gcm(k, &encrypted[3..15], &encrypted[15..])?
        }
        V10Key::Cbc(k) => {
            if encrypted.len() < 3 + 16 + 1 {
                return None;
            }
            decrypt_cbc(k, &encrypted[3..19], &encrypted[19..])?
        }
    };
    if strip_domain_hash && plain.len() > DOMAIN_HASH_LEN {
        return Some(plain[DOMAIN_HASH_LEN..].to_vec());
    }
    Some(plain)
}

fn decrypt_gcm(key: &[u8; 32], nonce: &[u8], ciphertext_and_tag: &[u8]) -> Option<Vec<u8>> {
    use ring::aead::{Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
    let unbound = UnboundKey::new(&ring::aead::AES_256_GCM, key).ok()?;
    let key = LessSafeKey::new(unbound);
    let nonce_array: [u8; NONCE_LEN] = nonce.try_into().ok()?;
    let nonce = Nonce::try_assume_unique_for_key(&nonce_array).ok()?;
    let mut data = ciphertext_and_tag.to_vec();
    let plain = key.open_in_place(nonce, Aad::empty(), &mut data).ok()?;
    Some(plain.to_vec())
}

fn decrypt_cbc(key: &[u8; 16], iv: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;
    let iv: [u8; 16] = iv.try_into().ok()?;
    let mut buf = ciphertext.to_vec();
    let plain = Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .ok()?;
    Some(plain.to_vec())
}

/// One decrypted cookie row in jar terms.
#[derive(Debug, Clone)]
pub struct JarRecord {
    pub host: String,
    pub http_only: bool,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    pub expires: i64,
    pub name: String,
    pub value: String,
}

impl JarRecord {
    fn jar_line(&self) -> String {
        let domain = if self.http_only {
            format!("#HttpOnly_{}", self.host)
        } else {
            self.host.clone()
        };
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            domain,
            if self.include_subdomains {
                "TRUE"
            } else {
                "FALSE"
            },
            self.path,
            if self.secure { "TRUE" } else { "FALSE" },
            self.expires,
            self.name,
            self.value
        )
    }

    fn key(&self) -> (String, String, String) {
        (
            self.host.trim_start_matches('.').to_lowercase(),
            self.name.clone(),
            self.path.clone(),
        )
    }
}

pub struct ReadOutcome {
    pub records: Vec<JarRecord>,
    pub skipped: usize,
}

/// Read + decrypt one profile's cookie DB (Python's read_cookie_db).
pub fn read_cookie_db(db_path: &Path, key: Option<&V10Key>) -> Result<ReadOutcome, String> {
    if !db_path.is_file() {
        return Err("cookies database not found".to_string());
    }
    let tmp_dir = std::env::temp_dir().join(format!("blindrss-chromium-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
    let result = read_cookie_db_inner(db_path, key, &tmp_dir);
    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

fn read_cookie_db_inner(
    db_path: &Path,
    key: Option<&V10Key>,
    tmp_dir: &Path,
) -> Result<ReadOutcome, String> {
    let dst = tmp_dir.join("Cookies");
    std::fs::copy(db_path, &dst).map_err(|e| e.to_string())?;
    for suffix in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{suffix}", db_path.display()));
        if side.is_file() {
            let _ = std::fs::copy(&side, format!("{}{suffix}", dst.display()));
        }
    }
    let conn = rusqlite::Connection::open(&dst).map_err(|e| e.to_string())?;
    let strip_hash: bool =
        match conn.query_row("SELECT value FROM meta WHERE key = 'version'", [], |r| {
            r.get::<_, String>(0)
        }) {
            Ok(v) => v.parse::<i64>().map(|n| n >= 24).unwrap_or(true),
            Err(_) => true,
        };
    let mut stmt = conn
        .prepare(
            "SELECT host_key, path, is_secure, is_httponly, expires_utc, name, value, \
             encrypted_value FROM cookies",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Vec<u8>>(7)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut records = Vec::new();
    let mut skipped = 0usize;
    for row in rows {
        let (host, path, secure, http_only, expires_utc, name, value, encrypted) =
            row.map_err(|e| e.to_string())?;
        let host = host.trim().to_string();
        if host.is_empty() || name.is_empty() {
            continue;
        }
        let value = if !value.is_empty() && encrypted.is_empty() {
            value
        } else {
            match decrypt_cookie_value(&encrypted, key, strip_hash) {
                Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                None => {
                    skipped += 1;
                    continue;
                }
            }
        };
        let expires = if expires_utc <= 0 {
            0
        } else {
            expires_utc / 1_000_000 - NT_TO_UNIX_OFFSET
        };
        records.push(JarRecord {
            include_subdomains: host.starts_with('.'),
            host,
            http_only: http_only != 0,
            path: if path.is_empty() {
                "/".to_string()
            } else {
                path
            },
            secure: secure != 0,
            expires,
            name,
            value,
        });
    }
    Ok(ReadOutcome { records, skipped })
}

/// Import one profile: load its key, read its DB.
pub fn read_profile_cookies(profile: &ChromiumProfile) -> ReadOutcome {
    let key = load_v10_key(
        &profile.local_state,
        browser_key_for_label(&profile.browser),
    );
    match read_cookie_db(&profile.cookie_db, key.as_ref()) {
        Ok(outcome) => outcome,
        Err(_) => ReadOutcome {
            records: Vec::new(),
            skipped: 0,
        },
    }
}

/// Merge imported records into existing jar text: an imported
/// cookie replaces an existing line with the same
/// (domain, name, path); everything else is kept verbatim.
pub fn merge_into_jar(existing: &str, records: &[JarRecord], now: i64) -> String {
    let new_keys: std::collections::HashSet<(String, String, String)> =
        records.iter().map(|r| r.key()).collect();
    let mut out: Vec<String> = Vec::new();
    for line in existing.lines() {
        let trimmed = line.trim_end_matches('\r');
        let parsed = parse_jar_line(trimmed);
        match parsed {
            Some(rec) if new_keys.contains(&rec.key()) => { /* replaced below */ }
            _ => {
                if !trimmed.is_empty() {
                    out.push(trimmed.to_string());
                }
            }
        }
    }
    for rec in records {
        if rec.expires != 0 && rec.expires < now {
            continue;
        }
        out.push(rec.jar_line());
    }
    let mut text = out.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

fn parse_jar_line(line: &str) -> Option<JarRecord> {
    let (http_only, rest) = match line.strip_prefix("#HttpOnly_") {
        Some(r) => (true, r),
        None => {
            if line.starts_with('#') {
                return None;
            }
            (false, line)
        }
    };
    let f: Vec<&str> = rest.split('\t').collect();
    if f.len() < 7 {
        return None;
    }
    let c = Cookie {
        domain: f[0].trim().trim_start_matches('.').to_lowercase(),
        include_subdomains: f[1].eq_ignore_ascii_case("TRUE"),
        path: f[2].to_string(),
        secure: f[3].eq_ignore_ascii_case("TRUE"),
        expires: f[4].trim().parse().unwrap_or(0),
        name: f[5].to_string(),
        value: f[6..].join("\t"),
    };
    Some(JarRecord {
        host: c.domain,
        http_only,
        include_subdomains: c.include_subdomains,
        path: c.path,
        secure: c.secure,
        expires: c.expires,
        name: c.name,
        value: c.value,
    })
}
