//! Local MP4/M4A/M4B embedded chapters: the `chpl` atom
//! parser behind `embedded_media_chapters` (Python
//! `_embedded_media_chapters` reads the same atom through
//! mutagen). Fixtures are synthetic box trees built in
//! the test — no binary media files in the repo.

use blindrss_core::chapters::{embedded_media_chapters, mp4_chapters};

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

fn chpl(chapters: &[(u64, &str)]) -> Vec<u8> {
    let mut payload = vec![1u8, 0, 0, 0]; // version + flags
    payload.extend_from_slice(&0u32.to_be_bytes()); // reserved
    payload.push(0); // reserved
    payload.extend_from_slice(&(chapters.len() as u32).to_be_bytes());
    for (start_100ns, title) in chapters {
        payload.extend_from_slice(&start_100ns.to_be_bytes());
        payload.push(title.len() as u8);
        payload.extend_from_slice(title.as_bytes());
    }
    bx(b"chpl", &payload)
}

fn write_temp(name: &str, bytes: &[u8]) -> String {
    let dir =
        std::env::temp_dir().join(format!("blindrss-mp4-test-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn chpl_at_moov_level_parses_starts_and_titles() {
    let mut file = bx(b"ftyp", b"M4A \0\0\0\0M4A ");
    let moov = bx(
        b"moov",
        &chpl(&[(0, "Intro"), (900_000_000, "Main feature")]), // 90 s
    );
    file.extend_from_slice(&moov);
    let path = write_temp("flat.m4b", &file);
    let found = mp4_chapters(&path);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].start, 0.0);
    assert_eq!(found[0].title.as_deref(), Some("Intro"));
    assert_eq!(found[1].start, 90.0);
    assert_eq!(found[1].title.as_deref(), Some("Main feature"));
}

#[test]
fn chpl_nested_in_udta_is_found_and_routed_by_suffix() {
    let udta = bx(b"udta", &chpl(&[(15_000_000, "Chapter 1.5s")]));
    let moov = bx(b"moov", &udta);
    let path = write_temp("nested.m4a", &moov);
    let found = embedded_media_chapters(&path, Some("audio/mp4"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].start, 1.5);
    assert_eq!(found[0].title.as_deref(), Some("Chapter 1.5s"));
}

#[test]
fn mp4_without_chpl_and_missing_files_yield_no_chapters() {
    let moov = bx(b"moov", &bx(b"free", b"nothing here"));
    let path = write_temp("none.mp4", &moov);
    assert!(mp4_chapters(&path).is_empty());
    assert!(mp4_chapters("/nonexistent/definitely-missing.m4b").is_empty());
    // A non-MP4 local file still yields nothing from this path.
    let other = write_temp("plain.bin", b"not media at all");
    assert!(embedded_media_chapters(&other, None).is_empty());
}

#[test]
fn truncated_chpl_stops_cleanly() {
    let mut payload = chpl(&[(0, "Ok")]);
    payload.truncate(payload.len() - 2); // cut mid-title
    let moov = bx(b"moov", &payload);
    let path = write_temp("trunc.m4b", &moov);
    assert!(mp4_chapters(&path).is_empty() || mp4_chapters(&path).len() == 1);
}
