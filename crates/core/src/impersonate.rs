//! TLS-impersonated fetch (Audit 8 B16): one GET on a real
//! browser's TLS/HTTP fingerprint, for pages whose WAF
//! resets or challenges the plain ureq/rustls handshake
//! (Python `core/article_extractor.py::_download_via_impersonation`
//! over curl_cffi, issue #29 there).
//!
//! Transport: wreq (BoringSSL) driven through a blocking
//! bridge — one OS thread per call running a current-thread
//! tokio runtime — because core is synchronous throughout.
//! The spike that proved this shape (BoringSSL builds, the
//! emulated profile's User-Agent crosses a real socket,
//! HTTPS completes through a proxy) is recorded in PARITY
//! B16. Certificate validation is never weakened: the
//! client keeps wreq's default root store, and the system
//! proxy configuration is honored the way requests honors
//! it in Python.
//!
//! The emulation profile supplies the request's identity
//! set (User-Agent, Sec-CH-UA, Sec-Fetch) self-consistently
//! with the handshake — the same division of labor as
//! Python's `_impersonated_headers`, which forwards only
//! the caller's functional headers and lets curl_cffi own
//! the identity set. The functional headers sent here are
//! Python's `_HTML_ACCEPT_HEADERS` plus the caller's cookie
//! header when one was supplied.

use std::time::Duration;

/// The wreq-util emulation profile for a Python
/// impersonation target key. `""` and `"chrome"` are
/// Python's default target (`utils.IMPERSONATE_TARGET =
/// "chrome"`, curl_cffi's current Chrome) and map to the
/// newest Chrome profile wreq-util ships; `"safari184"`
/// is curl_cffi's Safari 18.4 and maps to the nearest
/// Safari 18 profile (18.5, same fingerprint family);
/// `"firefox"` maps to the newest Firefox profile.
/// Unknown keys fall back to Chrome, mirroring Python's
/// `impersonate_target or IMPERSONATE_TARGET`. (The
/// variants live on wreq-util's `Emulation` const table;
/// their type is `Profile`.)
pub fn profile_for_target(target: &str) -> wreq_util::Profile {
    match target {
        "safari184" => wreq_util::Emulation::Safari18_5,
        "firefox" => wreq_util::Emulation::Firefox151,
        _ => wreq_util::Emulation::Chrome149,
    }
}

/// One impersonated GET. Returns the body only on a
/// 200..400 status (Python's success rule; the caller
/// additionally rejects empty and interstitial bodies).
/// Every failure — transport, TLS, a panicked bridge
/// thread — is None: this stage is a fallback in a cascade,
/// never a new error surface.
pub fn fetch(url: &str, target: &str, cookie_header: &str, timeout: Duration) -> Option<String> {
    let url = url.to_string();
    let target = target.to_string();
    let cookie_header = cookie_header.to_string();
    let handle = std::thread::spawn(move || fetch_blocking(&url, &target, &cookie_header, timeout));
    handle.join().ok().flatten()
}

/// One impersonated GET returning the raw body bytes —
/// the feed-refresh stage (Python providers/local.py's
/// per-feed impersonation policy, issue #29): same bridge
/// and success rule as `fetch`, but the body stays bytes
/// so the feed decoder sees the document exactly as
/// served, and the Accept header is the feed one Python's
/// shared HEADERS carry.
/// ponytail: the bridge boilerplate is duplicated rather
/// than generalized — `fetch`'s text decoding (charset
/// aware) is behavior the extractor depends on; two small
/// copies beat a shared core that changes either.
pub fn fetch_bytes(
    url: &str,
    target: &str,
    cookie_header: &str,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let url = url.to_string();
    let target = target.to_string();
    let cookie_header = cookie_header.to_string();
    let handle =
        std::thread::spawn(move || fetch_blocking_bytes(&url, &target, &cookie_header, timeout));
    handle.join().ok().flatten()
}

fn fetch_blocking_bytes(
    url: &str,
    target: &str,
    cookie_header: &str,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async move {
        let client = wreq::Client::builder()
            .emulation(profile_for_target(target))
            .timeout(timeout)
            .build()
            .ok()?;
        let mut req = client
            .get(url)
            .header(
                "Accept",
                "application/rss+xml,application/xml,application/atom+xml,text/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "en-US,en;q=0.9");
        if !cookie_header.is_empty() {
            req = req.header("Cookie", cookie_header);
        }
        let resp = req.send().await.ok()?;
        let status = resp.status().as_u16();
        if !(200..400).contains(&status) {
            return None;
        }
        resp.bytes().await.ok().map(|b| b.to_vec())
    })
}

fn fetch_blocking(
    url: &str,
    target: &str,
    cookie_header: &str,
    timeout: Duration,
) -> Option<String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    rt.block_on(async move {
        let client = wreq::Client::builder()
            .emulation(profile_for_target(target))
            .timeout(timeout)
            .build()
            .ok()?;
        let mut req = client
            .get(url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header("Accept-Language", "en-US,en;q=0.9");
        if !cookie_header.is_empty() {
            req = req.header("Cookie", cookie_header);
        }
        let resp = req.send().await.ok()?;
        let status = resp.status().as_u16();
        if !(200..400).contains(&status) {
            return None;
        }
        resp.text().await.ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_map_to_the_newest_profile_of_their_family() {
        // The mapping is the contract ("" and unknown keys
        // are Python's Chrome default).
        assert_eq!(profile_for_target(""), wreq_util::Emulation::Chrome149);
        assert_eq!(
            profile_for_target("chrome"),
            wreq_util::Emulation::Chrome149
        );
        assert_eq!(
            profile_for_target("safari184"),
            wreq_util::Emulation::Safari18_5
        );
        assert_eq!(
            profile_for_target("firefox"),
            wreq_util::Emulation::Firefox151
        );
        assert_eq!(profile_for_target("opera"), wreq_util::Emulation::Chrome149);
    }
}
