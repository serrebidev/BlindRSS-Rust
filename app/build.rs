//! Build script, via shipfitter (same pattern as BlindFB): on
//! Windows embed the application manifest and the Explorer version
//! resource. Without the manifest, wxWidgets shows the "doesn't
//! use a correct manifest specifying the use of Common Controls
//! Library v6" warning at startup and the app falls back to the
//! old common controls.

fn main() {
    // The changelog ships embedded in the binary so Help >
    // View Changelog reads the full history offline (the
    // BlindRadio pattern); app/src/help.rs includes it.
    println!("cargo:rerun-if-changed=../CHANGELOG.md");
    // The changelog as an HTML page, converted once here and
    // inflated at run time (the BlindRadio pattern): Help >
    // View Changelog writes it to the temp folder and opens
    // it in the default browser. No table of contents: one
    // entry per release would bury the newest under a long
    // list.
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    shipfitter::docs::convert_deflated(
        std::path::Path::new("../CHANGELOG.md"),
        &out_dir.join("changelog.html"),
        &shipfitter::docs::Page {
            title: "BlindRSS Changelog",
            toc_depth: 0,
            ..Default::default()
        },
    )
    .expect("the changelog converts");
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
