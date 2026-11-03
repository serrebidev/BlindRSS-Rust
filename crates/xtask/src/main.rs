//! Release tooling (same pattern as BlindRadio's xtask).
//!
//! `cargo xtask sign <file>...` writes `<file>.minisig` beside
//! each file, the minisign signature the updater verifies a
//! download against (crates/core/assets/minisign.pub). The
//! secret key comes from the `MINISIGN_KEY` environment
//! variable — in CI that is the repository secret of the same
//! name; it is never stored in git. `MINISIGN_PASSWORD`
//! unlocks the key when it is encrypted.

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("sign") => sign_assets(&args[1..]),
        _ => Err("usage: cargo xtask sign <file>...".to_string()),
    };
    if let Err(e) = result {
        eprintln!("xtask: {e}");
        std::process::exit(1);
    }
}

/// `cargo xtask sign <file>...`: writes `<file>.minisig`
/// beside each.
fn sign_assets(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err("sign needs at least one file".to_string());
    }
    for file in args {
        if !sign_file(Path::new(file))? {
            return Err("MINISIGN_KEY is not set".to_string());
        }
        println!("xtask: signed {file}");
    }
    Ok(())
}

/// Writes `<file>.minisig` beside `file`. Returns false when
/// there is no key, so a build without one still runs.
fn sign_file(path: &Path) -> Result<bool, String> {
    let Ok(key) = std::env::var("MINISIGN_KEY") else {
        return Ok(false);
    };
    let password = std::env::var("MINISIGN_PASSWORD").ok();
    sign_file_with(path, &key, password.as_deref())?;
    Ok(true)
}

/// The signing itself, parameterised so tests can drive it
/// without the environment (BlindRadio's sign_file inlines
/// the env reads; the split is the only deviation).
fn sign_file_with(path: &Path, key_text: &str, password: Option<&str>) -> Result<(), String> {
    let boxed =
        minisign::SecretKeyBox::from_string(key_text).map_err(|e| format!("MINISIGN_KEY: {e}"))?;
    let key = match password {
        Some(password) => boxed.into_secret_key(Some(password.to_string())),
        None => boxed.into_unencrypted_secret_key(),
    }
    .map_err(|e| format!("MINISIGN_KEY: {e}"))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let trusted = format!("timestamp:{timestamp}\tfile:{name}\thashed");
    let reader = std::io::BufReader::new(
        std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?,
    );
    let signature = minisign::sign(None, &key, reader, Some(&trusted), None)
        .map_err(|e| format!("signing {}: {e}", path.display()))?;
    let mut out = path.as_os_str().to_owned();
    out.push(".minisig");
    std::fs::write(&out, signature.into_string())
        .map_err(|e| format!("{}: {e}", Path::new(&out).display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sign a scratch file with a throwaway keypair and verify
    /// the .minisig exactly the way the updater does
    /// (minisign-verify against the public half). The owner's
    /// real key is never involved.
    #[test]
    fn sign_round_trips_through_verify() {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let key_text = kp.sk.to_box(None).unwrap().into_string();
        let pub_box = kp.pk.to_box().unwrap().into_string();
        let pub_b64 = pub_box.lines().nth(1).unwrap().trim();

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file = std::env::temp_dir().join(format!(
            "blindrss_xtask_sign_{}_{nanos}.bin",
            std::process::id()
        ));
        std::fs::write(&file, b"release bytes").unwrap();
        sign_file_with(&file, &key_text, None).unwrap();

        let mut sig_path = file.as_os_str().to_owned();
        sig_path.push(".minisig");
        let sig_text = std::fs::read_to_string(&sig_path).unwrap();
        let pk = minisign_verify::PublicKey::from_base64(pub_b64).unwrap();
        let sig = minisign_verify::Signature::decode(&sig_text).unwrap();
        pk.verify(b"release bytes", &sig, false)
            .expect("signature must verify");
        assert!(pk.verify(b"tampered", &sig, false).is_err());

        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_file(&sig_path);
    }

    /// An encrypted key signs only with its password: the
    /// release workflow passes MINISIGN_PASSWORD for exactly
    /// this case.
    #[test]
    fn encrypted_key_needs_its_password() {
        let kp =
            minisign::KeyPair::generate_encrypted_keypair(Some("test-pw".to_string())).unwrap();
        let key_text = kp.sk.to_box(None).unwrap().into_string();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let file = std::env::temp_dir().join(format!(
            "blindrss_xtask_sign_enc_{}_{nanos}.bin",
            std::process::id()
        ));
        std::fs::write(&file, b"release bytes").unwrap();
        assert!(sign_file_with(&file, &key_text, None).is_err());
        sign_file_with(&file, &key_text, Some("test-pw")).unwrap();
        let _ = std::fs::remove_file(&file);
        let mut sig_path = file.as_os_str().to_owned();
        sig_path.push(".minisig");
        let _ = std::fs::remove_file(&sig_path);
    }
}
