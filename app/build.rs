//! Build script, via shipfitter (same pattern as BlindFB): on
//! Windows embed the application manifest and the Explorer version
//! resource. Without the manifest, wxWidgets shows the "doesn't
//! use a correct manifest specifying the use of Common Controls
//! Library v6" warning at startup and the app falls back to the
//! old common controls.

fn main() {
    // Compile the gettext catalogs (po/*.po) into locale/ for
    // patois::embed_domain! to embed. Without the gettext
    // tools on PATH this warns and builds without translations
    // (patois-build's contract); CI installs them.
    patois_build::compile_translations("po", "locale");
    // wxdragon-sys 0.9.24 link gap (Linux, media-ctrl feature): the wx
    // GStreamer media backend calls gst_player_* but the sys crate does
    // not emit the gstplayer link. Windows/macOS backends don't need it.
    // Remove this when the sys crate links gstreamer-player-1.0 itself.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=gstplayer-1.0");
    }
    // wxWidgets debug symbols exceed the default MSVC PDB page limit.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg=/PDBPAGESIZE:8192");
    }
    shipfitter::windows::embed_manifest("BlindRSS").expect("embed manifest");
    shipfitter::windows::VersionInfo {
        product_name: "BlindRSS",
        company: "Brandon Bracey",
        copyright: "Copyright (c) 2026 Brandon Bracey",
        original_filename: "blindrss.exe",
        product_version: None,
        icon: None,
    }
    .embed()
    .expect("embed version info");
}
