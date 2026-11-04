//! Audit 8 Task 23 (O06, O13, O14, O15, F20): dependency and
//! subprocess parity with Python `core/dependency_check.py` and
//! `core/discovery.py`.
//!
//! Python validates tools with the tool-correct version flag
//! (`ffmpeg -version`, `yt-dlp --version`) under a finite timeout,
//! resolves executables from known package locations when PATH is
//! minimal, keeps the managed yt-dlp on the NIGHTLY channel with a
//! 24 h success-only throttle, terminates timed-out subprocesses,
//! and ships a JavaScript runtime (Deno) plus a packaged runtime
//! self-test. The fake-executable tests are Unix-only (shell
//! scripts); the pure policy tests run everywhere.

use blindrss_core::dependency::*;
#[cfg(unix)]
use std::io::Write;
#[cfg(not(windows))]
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

fn unique_dir(tag: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "blindrss-dep23-{}-{}-{}-{}",
        tag,
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(unix)]
fn write_exe(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

// ---------------------------------------------------------------------------
// O13: correct version flags, bounded probing, broken-tool repair signal.
// ---------------------------------------------------------------------------

#[test]
fn version_arg_matches_python() {
    assert_eq!(version_arg_for_tool("ffmpeg"), "-version");
    assert_eq!(version_arg_for_tool("ffprobe"), "-version");
    assert_eq!(version_arg_for_tool("ffmpeg.exe"), "-version");
    assert_eq!(version_arg_for_tool("yt-dlp"), "--version");
    assert_eq!(version_arg_for_tool("deno"), "--version");
}

#[cfg(unix)]
#[test]
fn probe_uses_single_dash_version_for_ffmpeg() {
    // A faithful ffmpeg stand-in: real ffmpeg accepts -version;
    // this fake rejects the double-dash form the old Rust probe
    // used, so the old code reported a working ffmpeg as broken.
    let dir = unique_dir("ffmpegarg");
    let ffmpeg = write_exe(
        &dir,
        "ffmpeg",
        "#!/bin/sh\nif [ \"$1\" = \"-version\" ]; then echo \"ffmpeg version 7.1.1\"; exit 0; fi\nexit 1\n",
    );
    assert!(validate_executable(&ffmpeg, "ffmpeg"));
    assert_eq!(
        probe_version(&ffmpeg, "ffmpeg").as_deref(),
        Some("ffmpeg version 7.1.1")
    );
}

#[cfg(unix)]
#[test]
fn probe_rejects_broken_executable() {
    let dir = unique_dir("broken");
    let ffmpeg = write_exe(&dir, "ffmpeg", "#!/bin/sh\nexit 3\n");
    assert!(!validate_executable(&ffmpeg, "ffmpeg"));
    assert_eq!(probe_version(&ffmpeg, "ffmpeg"), None);
}

#[cfg(unix)]
#[test]
fn probe_of_hanging_executable_times_out() {
    let dir = unique_dir("hang");
    let ffmpeg = write_exe(&dir, "ffmpeg", "#!/bin/sh\nexec sleep 60\n");
    let started = std::time::Instant::now();
    assert!(!validate_executable_with_timeout(
        &ffmpeg,
        "ffmpeg",
        Duration::from_millis(300)
    ));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "probe must be bounded, took {:?}",
        started.elapsed()
    );
}

#[cfg(unix)]
#[test]
fn check_tools_reports_broken_tool_as_missing() {
    // O13's repair path: the app installs whatever check_tools
    // reports as path-less, so a present-but-broken ffmpeg must
    // surface as missing, not as found.
    let dir = unique_dir("checktools");
    let broken = write_exe(&dir, "ffmpeg", "#!/bin/sh\nexit 3\n");
    let good_probe = write_exe(
        &dir,
        "ffprobe",
        "#!/bin/sh\necho \"ffprobe version 7.1.1\"\nexit 0\n",
    );
    let good_ytdlp = write_exe(
        &dir,
        "yt-dlp",
        "#!/bin/sh\necho \"2026.11.03.111\"\nexit 0\n",
    );
    let mut mapping = std::collections::HashMap::new();
    mapping.insert("ffmpeg".to_string(), broken.to_string_lossy().to_string());
    mapping.insert(
        "ffprobe".to_string(),
        good_probe.to_string_lossy().to_string(),
    );
    mapping.insert(
        "yt-dlp".to_string(),
        good_ytdlp.to_string_lossy().to_string(),
    );
    set_user_tool_paths(&mapping);
    let statuses = check_tools();
    let ffmpeg = statuses.iter().find(|s| s.name == "ffmpeg").unwrap();
    assert_eq!(ffmpeg.path, None, "broken ffmpeg must read as missing");
    assert_eq!(ffmpeg.version, None);
    let ytdlp = statuses.iter().find(|s| s.name == "yt-dlp").unwrap();
    assert_eq!(ytdlp.version.as_deref(), Some("2026.11.03.111"));
    // Resolution precedence (shared policy): a user-configured
    // path beats the managed runtime copy.
    let managed_dir = unique_dir("precedence");
    seed_managed_ytdlp(&managed_dir, "2020.01.01.000");
    set_managed_ytdlp_dir(&managed_dir);
    assert_eq!(find_tool("yt-dlp"), Some(good_ytdlp.clone()));
}

// ---------------------------------------------------------------------------
// O14: known package locations beyond PATH.
// ---------------------------------------------------------------------------

#[test]
fn known_package_dirs_cover_python_locations() {
    for tool in ["ffmpeg", "ffprobe", "yt-dlp", "deno"] {
        let dirs = known_package_dirs(tool);
        assert!(!dirs.is_empty(), "no package dirs for {tool}");
    }
    #[cfg(not(windows))]
    {
        let ffmpeg_dirs = known_package_dirs("ffmpeg");
        assert!(ffmpeg_dirs.iter().any(|d| d == Path::new("/usr/local/bin")));
        assert!(
            ffmpeg_dirs
                .iter()
                .any(|d| d == Path::new("/opt/homebrew/bin"))
        );
        if let Some(home) = std::env::home_dir() {
            assert!(ffmpeg_dirs.contains(&home.join(".local").join("bin")));
        }
    }
}

#[cfg(unix)]
#[test]
fn find_in_dirs_locates_package_copy() {
    let dir = unique_dir("pkgdir");
    let ffmpeg = write_exe(&dir, "ffmpeg", "#!/bin/sh\nexit 0\n");
    assert_eq!(
        find_in_dirs(&["ffmpeg"], std::slice::from_ref(&dir)),
        Some(ffmpeg)
    );
    assert_eq!(find_in_dirs(&["ffprobe"], &[dir]), None);
}

// ---------------------------------------------------------------------------
// F20: timed-out subprocesses are killed and reaped, output bounded.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn run_tool_kills_and_reaps_on_timeout() {
    let dir = unique_dir("kill");
    let pidfile = dir.join("pid");
    let sleeper = write_exe(
        &dir,
        "sleeper",
        "#!/bin/sh\necho $$ > \"$1\"\nexec sleep 60\n",
    );
    let pid_arg = pidfile.to_string_lossy().to_string();
    let started = std::time::Instant::now();
    let result = run_tool(&sleeper, &[pid_arg.as_str()], Duration::from_millis(300));
    assert!(result.is_err(), "sleeping tool must time out");
    assert!(started.elapsed() < Duration::from_secs(10));
    let pid: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The child was killed AND reaped: the pid is fully gone.
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(!alive, "timed-out child {pid} survived");
}

#[cfg(unix)]
#[test]
fn run_tool_captures_output_and_bounds_it() {
    let dir = unique_dir("output");
    let chatty = write_exe(
        &dir,
        "chatty",
        "#!/bin/sh\necho hello-version\nhead -c 3000000 /dev/zero | tr '\\0' 'a'\n",
    );
    let run = run_tool(&chatty, &[], Duration::from_secs(30)).unwrap();
    assert!(run.success);
    assert!(run.stdout.starts_with("hello-version"));
    assert!(
        run.stdout.len() <= MAX_TOOL_OUTPUT_BYTES,
        "stdout must be capped, got {} bytes",
        run.stdout.len()
    );
}

#[cfg(unix)]
#[test]
fn run_tool_executes_freshly_written_files_repeatedly() {
    // Regression (Audit 8 Task 23 flake, root-caused 2026-11-04):
    // executing a file that was written and closed moments
    // earlier can fail with ETXTBSY ("Text file busy") for tens
    // to a few hundred milliseconds under load. That made the
    // nightly tests fail intermittently (a just-downloaded
    // yt-dlp staging copy probed as "did not run", a healthy
    // managed copy probed as broken) and failed one CI run.
    // run_tool's spawn absorbs the busy window; write-then-run
    // must simply work, fifty fresh files in a row.
    let dir = unique_dir("freshx");
    for i in 0..50 {
        let exe = write_exe(
            &dir,
            &format!("fresh{i}"),
            "#!/bin/sh\necho fresh-version\n",
        );
        let run = run_tool(&exe, &[], Duration::from_secs(10))
            .unwrap_or_else(|e| panic!("fresh file {i} must run: {e}"));
        assert!(run.success, "fresh file {i} must exit 0");
        assert_eq!(run.stdout.trim(), "fresh-version");
    }
}

// ---------------------------------------------------------------------------
// O06: nightly yt-dlp maintenance — current/stale/throttle/failure-safe.
// ---------------------------------------------------------------------------

#[test]
fn ytdlp_throttle_marks_only_success_and_lasts_24h() {
    let dir = unique_dir("throttle");
    let marker = dir.join("marker.txt");
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    assert!(ytdlp_check_due_at(&marker, now), "no marker = due");
    mark_ytdlp_checked_at(&marker, now);
    assert!(!ytdlp_check_due_at(&marker, now), "just checked = not due");
    assert!(
        !ytdlp_check_due_at(&marker, now + Duration::from_secs(3600)),
        "one hour later = not due"
    );
    assert!(
        ytdlp_check_due_at(&marker, now + Duration::from_secs(86_400 + 1)),
        "after 24h = due again"
    );
}

/// Loopback HTTP fixture: serves the nightly release API JSON on
/// /api and the fake yt-dlp asset on /asset (or a 500), counting
/// asset hits so tests can prove a current copy is not re-fetched.
#[cfg(unix)]
struct NightlyServer {
    base: String,
    asset_hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(unix)]
fn nightly_server(latest: &str, asset_body: Option<Vec<u8>>) -> NightlyServer {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let asset_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let hits = asset_hits.clone();
    let api_body = format!("{{\"tag_name\":\"{latest}\"}}");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let (status, body): (&str, Vec<u8>) = if request.contains("/asset") {
                hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                match &asset_body {
                    Some(bytes) => ("200 OK", bytes.clone()),
                    None => ("500 Internal Server Error", b"nope".to_vec()),
                }
            } else {
                ("200 OK", api_body.clone().into_bytes())
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    NightlyServer {
        base: format!("http://{addr}"),
        asset_hits,
    }
}

/// Seed a managed yt-dlp copy the way an installed one exists on
/// disk: right bytes AND the executable bit (a non-executable
/// file is exactly the broken-install case O13 repairs).
#[cfg(unix)]
fn seed_managed_ytdlp(dir: &Path, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dest = managed_ytdlp(dir);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, fake_ytdlp_script(version)).unwrap();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
    dest
}

#[cfg(unix)]
fn fake_ytdlp_script(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo \"{version}\"; fi\nexit 0\n")
        .into_bytes()
}

#[cfg(unix)]
#[test]
fn nightly_current_copy_is_not_redownloaded() {
    let dir = unique_dir("current");
    seed_managed_ytdlp(&dir, "2026.11.03.111");
    let server = nightly_server("2026.11.03.111", Some(fake_ytdlp_script("2026.11.03.111")));
    let report = update_managed_ytdlp(
        &dir,
        &format!("{}/asset", server.base),
        &format!("{}/api", server.base),
    )
    .unwrap();
    assert_eq!(report.action, YtdlpUpdateAction::AlreadyCurrent);
    assert_eq!(report.version.as_deref(), Some("2026.11.03.111"));
    assert_eq!(
        server.asset_hits.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a current nightly must not be re-downloaded"
    );
}

#[cfg(unix)]
#[test]
fn nightly_stale_copy_falls_back_to_verified_download() {
    // The fake's --update-to is a no-op (exit 0, version unchanged),
    // mirroring Python's "the exit code is not trusted" case: the
    // version is re-read and the full binary fetched instead.
    let dir = unique_dir("stale");
    let dest = seed_managed_ytdlp(&dir, "2026.10.01.000");
    let server = nightly_server("2026.11.03.111", Some(fake_ytdlp_script("2026.11.03.111")));
    let report = update_managed_ytdlp(
        &dir,
        &format!("{}/asset", server.base),
        &format!("{}/api", server.base),
    )
    .unwrap();
    assert_eq!(report.action, YtdlpUpdateAction::Downloaded);
    assert_eq!(
        probe_version(&dest, "yt-dlp").as_deref(),
        Some("2026.11.03.111")
    );
}

#[cfg(unix)]
#[test]
fn nightly_failed_download_keeps_working_copy() {
    let dir = unique_dir("keep");
    let dest = seed_managed_ytdlp(&dir, "2026.10.01.000");
    let original = fake_ytdlp_script("2026.10.01.000");
    let server = nightly_server("2026.11.03.111", None);
    let report = update_managed_ytdlp(
        &dir,
        &format!("{}/asset", server.base),
        &format!("{}/api", server.base),
    )
    .unwrap();
    assert_eq!(report.action, YtdlpUpdateAction::KeptExistingAfterFailure);
    assert_eq!(std::fs::read(&dest).unwrap(), original);
    assert_eq!(
        probe_version(&dest, "yt-dlp").as_deref(),
        Some("2026.10.01.000")
    );
}

#[cfg(unix)]
#[test]
fn nightly_missing_copy_installs_and_validates() {
    let dir = unique_dir("fresh");
    let server = nightly_server("2026.11.03.111", Some(fake_ytdlp_script("2026.11.03.111")));
    let report = update_managed_ytdlp(
        &dir,
        &format!("{}/asset", server.base),
        &format!("{}/api", server.base),
    )
    .unwrap();
    assert_eq!(report.action, YtdlpUpdateAction::InstalledNew);
    assert!(managed_ytdlp(&dir).is_file());

    let dir2 = unique_dir("freshfail");
    let server2 = nightly_server("2026.11.03.111", None);
    assert!(
        update_managed_ytdlp(
            &dir2,
            &format!("{}/asset", server2.base),
            &format!("{}/api", server2.base),
        )
        .is_err(),
        "with no working copy, a failed download is an error"
    );
    assert!(!managed_ytdlp(&dir2).is_file());
}

// ---------------------------------------------------------------------------
// O15: JavaScript runtime resolution + packaged smoke support.
// ---------------------------------------------------------------------------

#[test]
fn js_runtime_is_a_resolvable_tool() {
    // Resolution itself is environment-dependent; the contract is
    // that deno participates in the same policy as the other tools
    // (user override, PATH, known package locations).
    let dirs = known_package_dirs("deno");
    assert!(!dirs.is_empty());
    let _ = find_js_runtime();
}

#[test]
fn smoke_extraction_check_passes_on_sample_article() {
    assert_eq!(smoke_extraction_error(), None);
}
