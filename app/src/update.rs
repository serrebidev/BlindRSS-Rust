//! Updates: ship-shape against this repository's GitHub Releases,
//! verified with minisign (the shipfitter scheme, as in BlindRadio
//! and the Rust IPTV client).
//!
//! The public key is the second line of
//! `crates/core/assets/minisign.pub` — the BlindRadio keypair
//! (key ID 151E848B8939A63F); its secret half signs every release
//! asset and lives only in the `MINISIGN_KEY` repository secret,
//! never in git. ship-shape does the whole flow: version-ordered
//! check, update dialog, streamed download with progress and
//! cancellation, fail-closed signature verification before any
//! install, and a checked swap/restart through its platform
//! helper. Audit 8 O03/O04/O05/O17 were findings against the
//! hand-rolled staging updater this replaced; see docs/PARITY.md.

use std::sync::Arc;

use ship_shape::ui::{self, CheckTrigger};
use ship_shape::{InstallKind, UpdateChannel, UpdaterConfig};
use wxdragon::prelude::WxWidget;

/// The committed release-signing public key (minisign.pub format:
/// a comment line, then the base64 key).
const MINISIGN_PUB: &str = include_str!("../../crates/core/assets/minisign.pub");

/// The base64 key line ship-shape's `UpdaterConfig` expects.
fn public_key() -> &'static str {
    MINISIGN_PUB.lines().nth(1).map(str::trim).unwrap_or("")
}

pub fn config() -> Arc<UpdaterConfig> {
    Arc::new(
        UpdaterConfig::new(
            "serrebidev/BlindRSS-Rust",
            "BlindRSS",
            "BlindRSS",
            public_key(),
            env!("CARGO_PKG_VERSION"),
        )
        // BlindRSS ships portable zips on every platform; there is
        // no installer build for ship-shape to drive instead.
        .with_install_kind(InstallKind::Portable),
    )
}

/// Run one update check through ship-shape's UI flow. Manual (the
/// Check for Updates action) also reports up-to-date and errors;
/// Automatic (the launch check) only surfaces an available update.
/// ship-shape refuses to run two checks at once, so repeated or
/// overlapping triggers are safe.
pub fn check(parent: &dyn WxWidget, trigger: CheckTrigger) {
    ui::run_update_check(config(), parent, UpdateChannel::Stable, trigger);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::atomic::AtomicBool;

    /// The BlindRadio keypair's public half, as committed in
    /// `crates/core/assets/minisign.pub` (public release data).
    const EXPECTED_KEY: &str = "RWQ/pjmJi4QeFQf1jrQjt0KxMJgPAQo7rTMwLgyhNqdRjZtQnN9PLx3O";

    #[test]
    fn config_names_the_release_and_embeds_the_release_key() {
        let config = config();
        assert_eq!(config.github_repo, "serrebidev/BlindRSS-Rust");
        assert_eq!(config.app_name, "BlindRSS");
        assert_eq!(config.app_display_name, "BlindRSS");
        assert_eq!(config.current_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(config.install_kind, InstallKind::Portable);
        // The updater must verify against the key the release
        // workflow signs with; anything else fails every update
        // closed (or, worse, verifies against the wrong key).
        assert_eq!(config.minisign_public_key, EXPECTED_KEY);
        assert_eq!(public_key(), EXPECTED_KEY);
    }

    /// Serve `body` once on loopback for `path`; returns the URL.
    fn serve_once(path: &'static str, body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://127.0.0.1:{port}{path}")
    }

    /// A real signature made by the release key (BlindRadio
    /// v0.2.45's BlindRadio.tar.gz.minisig, public release data;
    /// same key ID as the embedded public key) — but over OTHER
    /// bytes, so verification must fail.
    const REAL_SIG: &str = concat!(
        "untrusted comment: signature from rsign secret key\n",
        "RUQ/pjmJi4QeFfDwFtbcmjzYhf6KKpipIoYxMcM1FwDONDjm+kZ8AOuBUR4GqZi+vg0tVeJMlwW7pf/A9o/7TAgr9TWhfNIOUw4=\n",
        "trusted comment: timestamp:1793660993\tfile:BlindRadio.tar.gz\thashed\n",
        "db1orUnRYjuxaADwHx2ksQ9dz7oABgGrFLxd87IyuEL1RHV8hF+N3k2SHcGjx7/PGRcmij+LMSj1E/7qtxBFCg==\n",
    );

    #[test]
    fn download_with_a_signature_over_other_bytes_does_not_install() {
        // ship-shape's download is the install gate: it fetches the
        // .minisig first, streams the asset, and only renames the
        // temp file into place after the signature verifies. The
        // key ID here matches the embedded key, so this exercises
        // the Ed25519 check itself through the production config.
        let payload_url = serve_once("/BlindRSS.tar.gz", b"not the signed archive".to_vec());
        let sig_url = serve_once("/BlindRSS.tar.gz.minisig", REAL_SIG.as_bytes().to_vec());
        let cancelled = AtomicBool::new(false);
        let err = ship_shape::download_update_file(
            &config(),
            &payload_url,
            &sig_url,
            &cancelled,
            |_, _| {},
        )
        .expect_err("a signature over different bytes must not install");
        assert!(
            matches!(err, ship_shape::UpdateError::Verification(_)),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn download_with_a_garbage_signature_does_not_install() {
        let payload_url = serve_once("/BlindRSS.tar.gz", b"payload".to_vec());
        let sig_url = serve_once("/BlindRSS.tar.gz.minisig", b"not a signature".to_vec());
        let cancelled = AtomicBool::new(false);
        let err = ship_shape::download_update_file(
            &config(),
            &payload_url,
            &sig_url,
            &cancelled,
            |_, _| {},
        )
        .expect_err("a garbage signature must not install");
        assert!(
            matches!(err, ship_shape::UpdateError::Verification(_)),
            "unexpected error: {err:?}"
        );
    }
}
