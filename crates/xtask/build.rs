//! wxdragon-sys 0.9.24 link gap (Linux, media-ctrl feature): wx's GStreamer
//! media backend calls gst_player_* but the sys crate does not link
//! gstreamer-player-1.0. Workspace builds unify media-ctrl into xtask's
//! wxdragon (via ship-shape), so its binaries need the link too; app/build.rs
//! does the same for blindrss. Remove when the sys crate links it itself.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=gstplayer-1.0");
    }
}
