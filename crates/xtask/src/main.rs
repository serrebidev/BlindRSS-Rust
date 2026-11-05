//! Release tooling (same pattern as BlindRadio's xtask).
//!
//! `cargo xtask sign <file>...` writes `<file>.minisig` beside
//! each file, the minisign signature the updater verifies a
//! download against (crates/core/assets/minisign.pub). The
//! secret key comes from the `MINISIGN_KEY` environment
//! variable — in CI that is the repository secret of the same
//! name; it is never stored in git. `MINISIGN_PASSWORD`
//! unlocks the key when it is encrypted.
//!
//! `cargo xtask bundle-app <staging> <out_dir> <version>`
//! assembles the macOS `BlindRSS.app` from a staging folder
//! (the built `blindrss` executable plus the release support
//! files), with the bundle metadata of the Python app's
//! `portable.spec` BUNDLE step (Audit 8 B14).

use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("sign") => sign_assets(&args[1..]),
        Some("bundle-app") => bundle_app(&args[1..]),
        _ => Err(
            "usage: cargo xtask sign <file>... | cargo xtask bundle-app <staging> <out_dir> <version>"
                .to_string(),
        ),
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
    // GitHub passes an unset secret as an EMPTY variable, and
    // an empty password here would take the encrypted-key
    // branch and fail ("Key is not encrypted") on this
    // unencrypted key — the v0.1.50 release died exactly
    // there. Empty means absent.
    let password = std::env::var("MINISIGN_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty());
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

/// The bundle metadata, ported from the Python build's
/// `portable.spec` BUNDLE step (Audit 8 B14): Finder and
/// VoiceOver read the name, identifier, version and category
/// from the bundle — a raw executable directory has none.
pub const BUNDLE_IDENTIFIER: &str = "com.serrebi.BlindRSS";
pub const BUNDLE_NAME: &str = "BlindRSS";
pub const BUNDLE_CATEGORY: &str = "public.app-category.news";
pub const BUNDLE_EXECUTABLE: &str = "blindrss";

fn bundle_app(args: &[String]) -> Result<(), String> {
    let (Some(staging), Some(out_dir), Some(version)) = (args.first(), args.get(1), args.get(2))
    else {
        return Err("usage: cargo xtask bundle-app <staging> <out_dir> <version>".to_string());
    };
    let app = build_app_bundle(Path::new(staging), Path::new(out_dir), version)?;
    println!("xtask: built {}", app.display());
    Ok(())
}

/// Assemble `<out_dir>/BlindRSS.app` from `staging` (the
/// release staging folder: the built executable plus support
/// files). Layout:
///
/// - `Contents/MacOS/blindrss` — the executable.
/// - `Contents/Resources/…` — every other staging entry
///   (locale/, sounds/, docs/, vlc/, README.md, PARITY.md,
///   minisign.pub); the app resolves bundled resources from
///   `Contents/Resources` when it runs inside a bundle.
/// - `Contents/Info.plist` + `Contents/PkgInfo`.
///
/// An existing bundle at the target is replaced (it is this
/// tool's own artifact). Returns the bundle path.
pub fn build_app_bundle(
    staging: &Path,
    out_dir: &Path,
    version: &str,
) -> Result<std::path::PathBuf, String> {
    let exe_src = staging.join(BUNDLE_EXECUTABLE);
    if !exe_src.is_file() {
        return Err(format!(
            "staging folder {} has no {BUNDLE_EXECUTABLE} executable",
            staging.display()
        ));
    }
    let app = out_dir.join(format!("{BUNDLE_NAME}.app"));
    if app.exists() {
        std::fs::remove_dir_all(&app).map_err(|e| format!("removing {}: {e}", app.display()))?;
    }
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    std::fs::create_dir_all(&macos).map_err(|e| format!("creating {}: {e}", macos.display()))?;
    std::fs::create_dir_all(&resources)
        .map_err(|e| format!("creating {}: {e}", resources.display()))?;

    let exe_dst = macos.join(BUNDLE_EXECUTABLE);
    std::fs::copy(&exe_src, &exe_dst).map_err(|e| format!("copying {}: {e}", exe_src.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe_dst, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", exe_dst.display()))?;
    }

    let entries =
        std::fs::read_dir(staging).map_err(|e| format!("reading {}: {e}", staging.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading {}: {e}", staging.display()))?;
        if entry.file_name() == BUNDLE_EXECUTABLE {
            continue;
        }
        copy_tree(&entry.path(), &resources.join(entry.file_name()))?;
    }

    std::fs::write(contents.join("Info.plist"), info_plist(version))
        .map_err(|e| format!("writing Info.plist: {e}"))?;
    std::fs::write(contents.join("PkgInfo"), "APPL????")
        .map_err(|e| format!("writing PkgInfo: {e}"))?;
    Ok(app)
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if meta.is_dir() {
        std::fs::create_dir_all(dst).map_err(|e| format!("creating {}: {e}", dst.display()))?;
        let entries =
            std::fs::read_dir(src).map_err(|e| format!("reading {}: {e}", src.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("reading {}: {e}", src.display()))?;
            copy_tree(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map_err(|e| format!("copying {}: {e}", src.display()))?;
        Ok(())
    }
}

/// The bundle's Info.plist: Python's `portable.spec`
/// info_plist entries plus the standard keys PyInstaller's
/// BUNDLE writes (executable, package type, versions).
pub fn info_plist(version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>CFBundleDisplayName</key>\n\
         \t<string>{BUNDLE_NAME}</string>\n\
         \t<key>CFBundleExecutable</key>\n\
         \t<string>{BUNDLE_EXECUTABLE}</string>\n\
         \t<key>CFBundleIdentifier</key>\n\
         \t<string>{BUNDLE_IDENTIFIER}</string>\n\
         \t<key>CFBundleName</key>\n\
         \t<string>{BUNDLE_NAME}</string>\n\
         \t<key>CFBundlePackageType</key>\n\
         \t<string>APPL</string>\n\
         \t<key>CFBundleShortVersionString</key>\n\
         \t<string>{version}</string>\n\
         \t<key>CFBundleVersion</key>\n\
         \t<string>{version}</string>\n\
         \t<key>LSApplicationCategoryType</key>\n\
         \t<string>{BUNDLE_CATEGORY}</string>\n\
         \t<key>NSPrincipalClass</key>\n\
         \t<string>NSApplication</string>\n\
         </dict>\n\
         </plist>\n"
    )
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

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "blindrss_xtask_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// B14: the bundle carries Python's metadata and layout —
    /// executable under Contents/MacOS, support files under
    /// Contents/Resources, and an Info.plist Finder/VoiceOver
    /// can read name, identifier, version and category from.
    #[test]
    fn bundle_app_layout_and_metadata() {
        let staging = scratch("bundle_staging");
        std::fs::write(staging.join("blindrss"), b"fake-exe").unwrap();
        std::fs::write(staging.join("README.md"), "readme").unwrap();
        std::fs::create_dir_all(staging.join("locale/en")).unwrap();
        std::fs::write(staging.join("locale/en/messages.po"), "po").unwrap();
        let out = scratch("bundle_out");

        let app = build_app_bundle(&staging, &out, "0.1.58").unwrap();
        assert_eq!(app, out.join("BlindRSS.app"));

        let exe = app.join("Contents/MacOS/blindrss");
        assert_eq!(std::fs::read(&exe).unwrap(), b"fake-exe");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "bundle executable must be runnable");
        }
        assert_eq!(
            std::fs::read_to_string(app.join("Contents/Resources/README.md")).unwrap(),
            "readme"
        );
        assert!(
            app.join("Contents/Resources/locale/en/messages.po")
                .exists(),
            "support trees land in Resources"
        );
        assert!(
            !app.join("Contents/Resources/blindrss").exists(),
            "the executable is not duplicated into Resources"
        );

        let plist = std::fs::read_to_string(app.join("Contents/Info.plist")).unwrap();
        for needle in [
            "com.serrebi.BlindRSS",
            "public.app-category.news",
            "NSApplication",
            "<string>BlindRSS</string>",
            "<string>blindrss</string>",
            "<string>0.1.58</string>",
            "CFBundleShortVersionString",
            "CFBundleVersion",
        ] {
            assert!(plist.contains(needle), "Info.plist missing {needle}");
        }
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn bundle_app_refuses_staging_without_executable() {
        let staging = scratch("bundle_empty");
        let out = scratch("bundle_out2");
        let err = build_app_bundle(&staging, &out, "1.0").unwrap_err();
        assert!(err.contains("blindrss"), "error names the gap: {err}");
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_dir_all(&out);
    }
}
