fn main() {
    // wxdragon-sys 0.9.24 link gap (Linux, media-ctrl feature): the wx
    // GStreamer media backend calls gst_player_* but the sys crate does
    // not emit the gstplayer link. Windows/macOS backends don't need it.
    // Remove this when the sys crate links gstreamer-player-1.0 itself.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-lib=gstplayer-1.0");
    }
}
