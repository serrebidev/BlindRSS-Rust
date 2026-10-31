//! Batch 28 (v0.1.27): automatic ffmpeg/ffprobe resolution —
//! the managed-install layout detection and zip extraction that
//! dependency::install_ffmpeg relies on.

use blindrss_core::dependency::{extract_ffmpeg_zip, managed_ffmpeg_bin_dir};

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blindrss-batch28-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn touch_pair(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    for name in ["ffmpeg", "ffmpeg.exe", "ffprobe", "ffprobe.exe"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
}

#[test]
fn managed_dir_finds_gyan_bin_layout() {
    let data = temp_dir("gyan");
    let bin = data
        .join("ffmpeg")
        .join("extract")
        .join("ffmpeg-7.1-essentials_build")
        .join("bin");
    touch_pair(&bin);
    assert_eq!(managed_ffmpeg_bin_dir(&data), Some(bin));
}

#[test]
fn managed_dir_finds_static_top_level_layout() {
    let data = temp_dir("static");
    let top = data
        .join("ffmpeg")
        .join("extract")
        .join("ffmpeg-7.0-amd64-static");
    touch_pair(&top);
    assert_eq!(managed_ffmpeg_bin_dir(&data), Some(top));
}

#[test]
fn managed_dir_none_when_incomplete() {
    let data = temp_dir("none");
    let bin = data
        .join("ffmpeg")
        .join("extract")
        .join("partial")
        .join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("ffmpeg"), b"x").unwrap();
    assert_eq!(managed_ffmpeg_bin_dir(&data), None);
}

#[test]
fn extract_zip_locates_the_pair() {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        for path in [
            "ffmpeg-7.1-essentials_build/bin/ffmpeg.exe",
            "ffmpeg-7.1-essentials_build/bin/ffprobe.exe",
            "ffmpeg-7.1-essentials_build/bin/ffplay.exe",
            "ffmpeg-7.1-essentials_build/doc/readme.txt",
        ] {
            zw.start_file(path, opts).unwrap();
            std::io::Write::write_all(&mut zw, b"binary").unwrap();
        }
        zw.finish().unwrap();
    }
    let dest = temp_dir("zip").join("extract");
    let bin = extract_ffmpeg_zip(&buf.into_inner(), &dest).unwrap();
    assert!(bin.ends_with("bin"));
    assert!(bin.join("ffmpeg.exe").is_file());
    assert!(bin.join("ffprobe.exe").is_file());
}

#[test]
fn extract_zip_rejects_archive_without_the_pair() {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zw.start_file("hello.txt", opts).unwrap();
        std::io::Write::write_all(&mut zw, b"hi").unwrap();
        zw.finish().unwrap();
    }
    let dest = temp_dir("badzip").join("extract");
    assert!(extract_ffmpeg_zip(&buf.into_inner(), &dest).is_err());
}
