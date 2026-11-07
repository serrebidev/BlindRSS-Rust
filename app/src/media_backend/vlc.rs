//! Dynamically loaded libVLC 3 engine. All calls stay on the UI thread.

use blindrss_core::{config::Config, equalizer::EqConfig};
use libloading::Library;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;

type Handle = *mut c_void;

#[repr(C)]
struct AudioDevice {
    next: *mut AudioDevice,
    device: *const c_char,
    description: *const c_char,
}

macro_rules! vlc_api {
    ($($(#[$meta:meta])* $name:ident: $ty:ty),+ $(,)?) => {
        struct Api {
            $($(#[$meta])* $name: $ty,)+
            _library: Library,
            _core: Option<Library>,
        }
        impl Api {
            unsafe fn from_library(library: Library, core: Option<Library>) -> Result<Self, String> {
                // libVLC 4 changes several signatures; never call it through the v3 ABI.
                let version = unsafe { library.get::<unsafe extern "C" fn() -> *const c_char>(b"libvlc_get_version\0") }
                    .map_err(|e| e.to_string())?;
                let version = unsafe { CStr::from_ptr(version()) }.to_string_lossy();
                if !version.starts_with("3.") {
                    return Err(format!("Unsupported libVLC version: {version}"));
                }
                Ok(Self {
                    $($name: *unsafe { library.get::<$ty>(concat!("libvlc_", stringify!($name), "\0").as_bytes()) }
                        .map_err(|e| e.to_string())?,)+
                    _library: library,
                    _core: core,
                })
            }
        }
    }
}

vlc_api! {
    new: unsafe extern "C" fn(i32, *const *const c_char) -> Handle,
    release: unsafe extern "C" fn(Handle),
    media_player_new: unsafe extern "C" fn(Handle) -> Handle,
    media_player_release: unsafe extern "C" fn(Handle),
    media_new_location: unsafe extern "C" fn(Handle, *const c_char) -> Handle,
    media_new_path: unsafe extern "C" fn(Handle, *const c_char) -> Handle,
    media_add_option: unsafe extern "C" fn(Handle, *const c_char),
    media_release: unsafe extern "C" fn(Handle),
    media_player_set_media: unsafe extern "C" fn(Handle, Handle),
    media_player_play: unsafe extern "C" fn(Handle) -> i32,
    media_player_set_pause: unsafe extern "C" fn(Handle, i32),
    media_player_stop: unsafe extern "C" fn(Handle),
    media_player_get_state: unsafe extern "C" fn(Handle) -> i32,
    media_player_get_time: unsafe extern "C" fn(Handle) -> i64,
    media_player_set_time: unsafe extern "C" fn(Handle, i64),
    media_player_get_length: unsafe extern "C" fn(Handle) -> i64,
    media_player_is_seekable: unsafe extern "C" fn(Handle) -> i32,
    media_player_set_rate: unsafe extern "C" fn(Handle, f32) -> i32,
    audio_get_volume: unsafe extern "C" fn(Handle) -> i32,
    audio_set_volume: unsafe extern "C" fn(Handle, i32) -> i32,
    // Windows switches to the mmdevice module before choosing a soundcard;
    // other platforms keep libVLC's default module, so this entry is unread there.
    #[cfg_attr(not(windows), allow(dead_code))]
    audio_output_set: unsafe extern "C" fn(Handle, *const c_char) -> i32,
    audio_output_device_set: unsafe extern "C" fn(Handle, *const c_char, *const c_char),
    audio_output_device_list_get: unsafe extern "C" fn(Handle, *const c_char) -> *mut AudioDevice,
    audio_output_device_list_release: unsafe extern "C" fn(*mut AudioDevice),
    audio_equalizer_new: unsafe extern "C" fn() -> Handle,
    audio_equalizer_release: unsafe extern "C" fn(Handle),
    audio_equalizer_set_preamp: unsafe extern "C" fn(Handle, f32) -> i32,
    audio_equalizer_set_amp_at_index: unsafe extern "C" fn(Handle, f32, u32) -> i32,
    media_player_set_equalizer: unsafe extern "C" fn(Handle, Handle) -> i32,
}

fn library_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("BLINDRSS_VLC") {
        let path = PathBuf::from(path);
        paths.push(if path.is_dir() {
            path.join(library_name())
        } else {
            path
        });
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        // Bundled layouts, in the order Python's portable.spec ships them:
        // vlc/lib/<lib> (macOS/Linux portable), then vlc/<lib>, then beside
        // the executable itself.
        paths.push(dir.join("vlc").join("lib").join(library_name()));
        paths.push(dir.join("vlc").join(library_name()));
        paths.push(dir.join(library_name()));
        #[cfg(target_os = "macos")]
        paths.push(dir.join("../Frameworks/libvlc.dylib"));
    }
    #[cfg(windows)]
    for key in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(dir) = std::env::var_os(key) {
            paths.push(PathBuf::from(dir).join("VideoLAN/VLC/libvlc.dll"));
        }
    }
    #[cfg(target_os = "macos")]
    paths.push(PathBuf::from(
        "/Applications/VLC.app/Contents/MacOS/lib/libvlc.dylib",
    ));
    #[cfg(target_os = "linux")]
    for dir in [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
    ] {
        // The same directories Python's add_linux_vlc_bundle() searches.
        paths.push(PathBuf::from(dir).join(library_name()));
        paths.push(PathBuf::from(dir).join("libvlc.so"));
    }
    paths.push(PathBuf::from(library_name()));
    paths
}

/// The libvlccore file that must be loaded before a sibling libvlc, or
/// `None` on platforms whose loader finds it unaided. A libvlc dropped
/// next to the executable (the bundled layout) has its core beside it,
/// not on the system search path, on every platform — not just Windows.
fn core_library_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["libvlccore.dll"]
    } else if cfg!(target_os = "macos") {
        &["libvlccore.dylib"]
    } else {
        // VLC 3.0.x ships libvlccore.so.8 (VideoLAN builds)
        // or libvlccore.so.9 (Debian/Ubuntu builds); a
        // bundled copy must preload whichever sits beside
        // libvlc, so try both, then the bare name.
        &["libvlccore.so.9", "libvlccore.so.8", "libvlccore.so"]
    }
}

/// Plugin directory of a bundled libvlc, mirroring Python's portable
/// layout (`<exe>/vlc/plugins` beside `vlc/lib/<lib>` or `vlc/<lib>`).
/// `None` for a system-installed libvlc, whose compiled-in plugin path
/// is correct. Pure path logic; existence is checked by the caller.
fn bundled_plugin_path(lib_path: &Path, exe_dir: &Path) -> Option<PathBuf> {
    if !lib_path.starts_with(exe_dir) {
        return None;
    }
    let vlc_dir = exe_dir.join("vlc");
    let plugins = vlc_dir.join("plugins");
    if lib_path.starts_with(&vlc_dir) && plugins.is_dir() {
        Some(plugins)
    } else {
        None
    }
}

fn library_name() -> &'static str {
    if cfg!(windows) {
        "libvlc.dll"
    } else if cfg!(target_os = "macos") {
        "libvlc.dylib"
    } else {
        "libvlc.so.5"
    }
}

/// Loads libVLC and reports which candidate won, so the caller can
/// recognise a bundled copy (plugin path, see `bundled_plugin_path`).
fn load_api() -> Result<(Api, PathBuf), String> {
    let mut last_error = "libVLC was not found".to_owned();
    for path in library_candidates() {
        // A loader does not search a loaded library's own directory for
        // its dependencies, so preload the sibling core first (previously
        // Windows-only; a bundled Linux/macOS copy needs it too).
        let core = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .and_then(|p| {
                core_library_names()
                    .iter()
                    .find_map(|name| unsafe { Library::new(p.join(name)) }.ok())
            });
        match unsafe { Library::new(&path) }
            .map_err(|e| e.to_string())
            .and_then(|lib| unsafe { Api::from_library(lib, core) })
        {
            Ok(api) => return Ok((api, path)),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

/// Mirror of Python's `core/vlc_options.py::build_vlc_instance_args`:
/// `--no-plugins-cache` always first, duplicates and blanks dropped.
fn build_instance_args(args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for arg in std::iter::once("--no-plugins-cache".to_owned()).chain(args.iter().cloned()) {
        let value = arg.trim().to_owned();
        if !value.is_empty() && !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

/// Mirror of Python's `core/vlc_instance.py::_build_args`, with one
/// deliberate divergence: Python passes `--no-video` (its player is
/// audio-only and never passes a window handle), while this backend
/// owns a video surface on Windows/macOS/Linux, so video stays enabled.
/// The Windows-only mmdevice/directx options stay Windows-only:
/// libvlc_new() returns NULL for options no loaded plugin registers,
/// which is why Python gates them, and the same failure would hit here.
fn instance_args(cfg: &Config, windows: bool) -> Vec<String> {
    let cache_ms = cfg.get_i64("vlc_network_caching_ms", 500).max(0);
    let file_cache_ms = cache_ms.max(500);
    let mut args = vec![
        "--quiet".to_owned(),
        "--no-video-title-show".to_owned(),
        "--no-snapshot-preview".to_owned(),
        "--input-fast-seek".to_owned(),
    ];
    if windows {
        let volume = cfg.get_i64("volume", 100).clamp(0, 100);
        args.push("--aout=mmdevice".to_owned());
        args.push(format!("--directx-volume={:.4}", volume as f64 / 100.0));
        args.push(format!("--mmdevice-volume={:.4}", volume as f64 / 100.0));
    }
    args.push(format!("--network-caching={cache_ms}"));
    args.push(format!("--file-caching={file_cache_ms}"));
    args.push("--http-reconnect".to_owned());
    build_instance_args(&args)
}

/// Python's retry when libvlc_new() rejects the configured options:
/// `build_vlc_instance_args("--no-video")`. Audio-only beats no VLC at
/// all; the MediaCtrl fallback stays behind this when even the minimal
/// set fails or libVLC is absent.
fn minimal_instance_args() -> Vec<String> {
    build_instance_args(&["--no-video".to_owned(), "--quiet".to_owned()])
}

pub fn media_options(
    cfg: &Config,
    url: &str,
    user_agent: &str,
    referer: &str,
    cookie: &str,
) -> Vec<String> {
    // Python's player.py gate: the local caching keys apply only to the
    // range-cache proxy's /media?id= URLs, not to every loopback URL
    // (the /stream and /play transcoders take the normal caching keys).
    let local_proxy = url.starts_with("http://127.0.0.1:") && url.contains("/media?id=");
    let network = cfg
        .get_i64(
            if local_proxy {
                "vlc_local_proxy_network_caching_ms"
            } else {
                "vlc_network_caching_ms"
            },
            1000,
        )
        .max(0);
    let file = if local_proxy {
        cfg.get_i64("vlc_local_proxy_file_caching_ms", 1000).max(0)
    } else {
        network.max(500)
    };
    let mut options = vec![
        format!(":network-caching={network}"),
        format!(":file-caching={file}"),
        ":http-reconnect".to_owned(),
    ];
    for (name, value) in [
        ("user-agent", user_agent),
        ("referrer", referer),
        ("cookie", cookie),
    ] {
        if !value.is_empty() && !value.contains(['\r', '\n', '\0']) {
            options.push(format!(":http-{name}={value}"));
        }
    }
    options
}

pub struct VlcPlayer {
    api: Api,
    instance: Handle,
    player: Handle,
}

impl VlcPlayer {
    pub fn new(cfg: &Config) -> Result<Self, String> {
        let (api, lib_path) = load_api()?;
        let mut args = instance_args(cfg, cfg!(windows));
        if let Some(exe_dir) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            && let Some(plugins) = bundled_plugin_path(&lib_path, &exe_dir)
        {
            // A bundled libvlc cannot know where its plugins were copied
            // to; a system libvlc's compiled-in path is left alone.
            args.push(format!("--plugin-path={}", plugins.to_string_lossy()));
        }
        // Python's vlc_instance retry: a NULL from libvlc_new() means an
        // option was unknown to the loaded plugin set, so retry once
        // with the minimal safe set before giving up to MediaCtrl.
        let (instance, player) = Self::make_handles(&api, &args)
            .or_else(|| Self::make_handles(&api, &minimal_instance_args()))
            .ok_or_else(|| "Could not initialize libVLC".to_owned())?;
        let engine = Self {
            api,
            instance,
            player,
        };
        engine.set_output(cfg.get_str("preferred_soundcard", ""));
        engine.apply_equalizer(&EqConfig::from_config(cfg));
        Ok(engine)
    }

    fn make_handles(api: &Api, args: &[String]) -> Option<(Handle, Handle)> {
        let args: Vec<CString> = args
            .iter()
            .map(|s| CString::new(s.as_str()).unwrap())
            .collect();
        let argv: Vec<_> = args.iter().map(|s| s.as_ptr()).collect();
        let instance = unsafe { (api.new)(argv.len() as i32, argv.as_ptr()) };
        if instance.is_null() {
            return None;
        }
        let player = unsafe { (api.media_player_new)(instance) };
        if player.is_null() {
            unsafe { (api.release)(instance) };
            return None;
        }
        Some((instance, player))
    }

    /// Test-only constructor for the installed-libVLC smoke test, which
    /// needs dummy audio/video outputs the production args never set.
    #[cfg(test)]
    fn with_args(cfg: &Config, args: &[&str]) -> Result<Self, String> {
        let (api, _) = load_api()?;
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let (instance, player) = Self::make_handles(&api, &args)
            .ok_or_else(|| "Could not initialize libVLC".to_owned())?;
        let engine = Self {
            api,
            instance,
            player,
        };
        engine.set_output(cfg.get_str("preferred_soundcard", ""));
        engine.apply_equalizer(&EqConfig::from_config(cfg));
        Ok(engine)
    }

    pub fn load(&self, url: &str, options: &[String]) -> bool {
        let Ok(location) = CString::new(url) else {
            return false;
        };
        let media = unsafe {
            if Path::new(url).is_file() {
                (self.api.media_new_path)(self.instance, location.as_ptr())
            } else {
                (self.api.media_new_location)(self.instance, location.as_ptr())
            }
        };
        if media.is_null() {
            return false;
        }
        self.stop();
        for option in options {
            if let Ok(option) = CString::new(option.as_str()) {
                unsafe { (self.api.media_add_option)(media, option.as_ptr()) };
            }
        }
        unsafe {
            (self.api.media_player_set_media)(self.player, media);
            (self.api.media_release)(media);
        }
        true
    }

    pub fn play(&self) -> bool {
        unsafe { (self.api.media_player_play)(self.player) == 0 }
    }
    pub fn pause(&self) {
        unsafe { (self.api.media_player_set_pause)(self.player, 1) }
    }
    pub fn stop(&self) {
        unsafe { (self.api.media_player_stop)(self.player) }
    }
    pub fn state(&self) -> i32 {
        unsafe { (self.api.media_player_get_state)(self.player) }
    }
    pub fn time(&self) -> i64 {
        unsafe { (self.api.media_player_get_time)(self.player) }.max(0)
    }
    pub fn length(&self) -> i64 {
        unsafe { (self.api.media_player_get_length)(self.player) }.max(0)
    }
    pub fn seek(&self, time: i64) {
        unsafe { (self.api.media_player_set_time)(self.player, time.max(0)) }
    }
    /// Whether the current input reports itself seekable
    /// (Python's `is_seekable()` in the resume/seek guards).
    pub fn is_seekable(&self) -> bool {
        unsafe { (self.api.media_player_is_seekable)(self.player) != 0 }
    }
    pub fn volume(&self) -> f64 {
        unsafe { (self.api.audio_get_volume)(self.player) }.max(0) as f64 / 100.0
    }
    pub fn set_volume(&self, volume: f64) -> bool {
        unsafe {
            (self.api.audio_set_volume)(
                self.player,
                (volume.clamp(0.0, 1.0) * 100.0).round() as i32,
            ) == 0
        }
    }
    pub fn set_rate(&self, rate: f64) -> bool {
        rate.is_finite()
            && rate > 0.0
            && unsafe { (self.api.media_player_set_rate)(self.player, rate as f32) == 0 }
    }

    pub fn set_output(&self, device: &str) {
        if let Ok(device) = CString::new(device) {
            unsafe {
                #[cfg(windows)]
                if !device.as_bytes().is_empty() {
                    (self.api.audio_output_set)(self.player, c"mmdevice".as_ptr());
                }
                (self.api.audio_output_device_set)(
                    self.player,
                    ptr::null(),
                    if device.as_bytes().is_empty() {
                        ptr::null()
                    } else {
                        device.as_ptr()
                    },
                );
            }
        }
    }
}

/// The audio output device list of one libVLC instance
/// (label, id) pairs, deduplicated by id.
fn device_list(api: &Api, instance: Handle) -> Vec<(String, String)> {
    let module = if cfg!(windows) {
        c"mmdevice"
    } else if cfg!(target_os = "macos") {
        c"auhal"
    } else {
        c"pulse"
    };
    let head = unsafe { (api.audio_output_device_list_get)(instance, module.as_ptr()) };
    {
        let mut next = head;
        // (kept in a block so `head` is released before return)
        let mut devices = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..512 {
            if next.is_null() {
                break;
            }
            let device = unsafe { &*next };
            if !device.device.is_null() {
                let id = unsafe { CStr::from_ptr(device.device) }
                    .to_string_lossy()
                    .trim()
                    .to_string();
                if !id.is_empty() && seen.insert(id.clone()) {
                    let label = if device.description.is_null() {
                        id.clone()
                    } else {
                        unsafe { CStr::from_ptr(device.description) }
                            .to_string_lossy()
                            .trim()
                            .to_string()
                    };
                    devices.push((if label.is_empty() { id.clone() } else { label }, id));
                }
            }
            next = device.next;
        }
        if !head.is_null() {
            unsafe { (api.audio_output_device_list_release)(head) };
        }
        devices
    }
}

/// Enumerate soundcards through a throwaway MINIMAL libVLC
/// instance (Python `gui/dialogs.py::_build_soundcard_choices`:
/// `Instance("--no-video", "--aout=mmdevice")` — no media
/// player, no output setting, no equalizer). Settings used to
/// build a full VlcPlayer just to list devices: creating a
/// libVLC instance takes seconds, so Settings took seconds to
/// open on the UI thread, and churning full player instances
/// in process destabilized the app (Brandon, 2026-11-07).
/// Callers run this OFF the UI thread (Python enumerates on a
/// background thread and populates the choice afterwards).
pub fn enumerate_soundcards() -> Vec<(String, String)> {
    let Ok((api, _lib_path)) = load_api() else {
        return Vec::new();
    };
    let aout = if cfg!(windows) {
        "--aout=mmdevice"
    } else if cfg!(target_os = "macos") {
        "--aout=auhal"
    } else {
        "--aout=pulse"
    };
    let args = build_instance_args(&[
        "--no-video".to_owned(),
        "--quiet".to_owned(),
        aout.to_owned(),
    ]);
    let cargs: Vec<CString> = args
        .iter()
        .map(|s| CString::new(s.as_str()).unwrap())
        .collect();
    let argv: Vec<_> = cargs.iter().map(|s| s.as_ptr()).collect();
    let instance = unsafe { (api.new)(argv.len() as i32, argv.as_ptr()) };
    if instance.is_null() {
        return Vec::new();
    }
    let devices = device_list(&api, instance);
    unsafe { (api.release)(instance) };
    devices
}

impl VlcPlayer {
    pub fn apply_equalizer(&self, config: &EqConfig) -> bool {
        if !config.enabled {
            return unsafe {
                (self.api.media_player_set_equalizer)(self.player, ptr::null_mut()) == 0
            };
        }
        let eq = unsafe { (self.api.audio_equalizer_new)() };
        if eq.is_null() {
            return false;
        }
        unsafe {
            (self.api.audio_equalizer_set_preamp)(eq, config.preamp as f32);
            for (index, amp) in config.bands.iter().enumerate() {
                (self.api.audio_equalizer_set_amp_at_index)(eq, *amp as f32, index as u32);
            }
            let ok = (self.api.media_player_set_equalizer)(self.player, eq) == 0;
            (self.api.audio_equalizer_release)(eq);
            ok
        }
    }

    /// Attach only while the native window is alive; the UI closes playback before destroying it.
    pub fn attach_video(&self, drawable: Handle) {
        #[cfg(any(windows, target_os = "macos"))]
        if !drawable.is_null() {
            #[cfg(windows)]
            let symbol = b"libvlc_media_player_set_hwnd\0";
            #[cfg(target_os = "macos")]
            let symbol = b"libvlc_media_player_set_nsobject\0";
            if let Ok(set_window) = unsafe {
                self.api
                    ._library
                    .get::<unsafe extern "C" fn(Handle, Handle)>(symbol)
            } {
                unsafe { set_window(self.player, drawable) };
            }
        }
        #[cfg(target_os = "linux")]
        if !drawable.is_null() {
            // libvlc_media_player_set_xwindow takes an X11 window id,
            // not a widget pointer; resolve it from the wxGTK handle.
            // A `None` (unrealized widget, Wayland session, missing
            // GTK/GDK) leaves libVLC on its own video output, exactly
            // the pre-embedding behaviour; the caller re-attaches on
            // every load, by which time the widget is realized.
            if let Some(xid) = linux::xid_for_widget(drawable)
                && let Ok(set_window) = unsafe {
                    self.api._library.get::<unsafe extern "C" fn(Handle, u32)>(
                        b"libvlc_media_player_set_xwindow\0",
                    )
                }
            {
                unsafe { set_window(self.player, xid) };
            }
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        let _ = drawable;
    }
}

/// X11 window-id resolution for a wxGTK widget handle, through the
/// GTK/GDK libraries the wxWidgets build already links. Everything is
/// loaded dynamically and every step fails closed to `None`: under a
/// Wayland-only session there is no XID to hand libVLC, and guessing
/// one would hand it a foreign window.
#[cfg(target_os = "linux")]
mod linux {
    use super::Handle;
    use libloading::Library;
    use std::ffi::{CStr, c_char, c_void};

    fn load(name: &str, fallback: &str) -> Option<Library> {
        unsafe { Library::new(name) }
            .ok()
            .or_else(|| unsafe { Library::new(fallback) }.ok())
    }

    pub fn xid_for_widget(widget: Handle) -> Option<u32> {
        if widget.is_null() {
            return None;
        }
        let gtk = load("libgtk-3.so.0", "libgtk-3.so")?;
        let gdk = load("libgdk-3.so.0", "libgdk-3.so")?;
        let gobject = load("libgobject-2.0.so.0", "libgobject-2.0.so")?;
        let get_window = unsafe {
            gtk.get::<unsafe extern "C" fn(Handle) -> *mut c_void>(b"gtk_widget_get_window\0")
        }
        .ok()?;
        let window = unsafe { get_window(widget) };
        if window.is_null() {
            return None;
        }
        // Refuse non-X11 GDK backends (Wayland) by the display's type
        // name instead of letting an X11-only call guess.
        let get_display = unsafe {
            gdk.get::<unsafe extern "C" fn(*mut c_void) -> *mut c_void>(b"gdk_window_get_display\0")
        }
        .ok()?;
        let type_name = unsafe {
            gobject.get::<unsafe extern "C" fn(*mut c_void) -> *const c_char>(
                b"g_type_name_from_instance\0",
            )
        }
        .ok()?;
        let display = unsafe { get_display(window) };
        if display.is_null() {
            return None;
        }
        let name = unsafe { type_name(display) };
        if name.is_null() || unsafe { CStr::from_ptr(name) }.to_bytes() != b"GdkX11Display" {
            return None;
        }
        let get_xid = unsafe {
            gdk.get::<unsafe extern "C" fn(*mut c_void) -> u32>(b"gdk_x11_window_get_xid\0")
        }
        .ok()?;
        match unsafe { get_xid(window) } {
            0 => None,
            xid => Some(xid),
        }
    }
}

impl Drop for VlcPlayer {
    fn drop(&mut self) {
        unsafe {
            (self.api.media_player_stop)(self.player);
            (self.api.media_player_release)(self.player);
            (self.api.release)(self.instance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blindrss_core::config::Config;

    #[test]
    fn media_options_preserve_python_cache_defaults_and_headers() {
        let cfg = Config::default();
        let options = media_options(
            &cfg,
            "https://media.example/episode.mp3",
            "Reader",
            "https://example/",
            "session=secret",
        );
        assert!(options.contains(&":network-caching=1000".to_owned()));
        assert!(options.contains(&":file-caching=1000".to_owned()));
        assert!(options.contains(&":http-user-agent=Reader".to_owned()));
        assert!(options.contains(&":http-referrer=https://example/".to_owned()));
        assert!(options.contains(&":http-cookie=session=secret".to_owned()));
    }

    #[test]
    fn local_proxy_uses_its_own_caches_and_negative_values_clamp() {
        let mut cfg = Config::default();
        cfg.set_path("vlc_network_caching_ms", serde_json::json!(-10));
        cfg.set_path("vlc_local_proxy_network_caching_ms", serde_json::json!(75));
        cfg.set_path("vlc_local_proxy_file_caching_ms", serde_json::json!(125));
        let local = media_options(&cfg, "http://127.0.0.1:1234/media?id=x", "", "", "");
        assert!(local.contains(&":network-caching=75".to_owned()));
        assert!(local.contains(&":file-caching=125".to_owned()));
        let remote = media_options(&cfg, "https://example.com/file", "", "", "");
        assert!(remote.contains(&":network-caching=0".to_owned()));
        assert!(remote.contains(&":file-caching=500".to_owned()));
        // Python's gate: the /stream and /play loopback transcoders are
        // not the range-cache proxy, so they take the normal keys.
        let stream = media_options(&cfg, "http://127.0.0.1:1234/stream?url=x", "", "", "");
        assert!(stream.contains(&":network-caching=0".to_owned()));
        assert!(stream.contains(&":file-caching=500".to_owned()));
    }

    #[test]
    fn instance_args_mirror_python_off_windows() {
        // Python test_shared_instance_omits_windows_only_options_off_windows
        // plus test_vlc_instance_args_disable_plugin_cache, ported.
        let args = instance_args(&Config::default(), false);
        assert_eq!(args[0], "--no-plugins-cache");
        assert_eq!(
            args.iter().filter(|a| *a == "--no-plugins-cache").count(),
            1
        );
        assert!(args.contains(&"--input-fast-seek".to_owned()));
        assert!(args.contains(&"--http-reconnect".to_owned()));
        assert!(args.contains(&"--network-caching=500".to_owned()));
        assert!(args.contains(&"--file-caching=500".to_owned()));
        assert!(!args.iter().any(|a| a.contains("mmdevice")));
        assert!(!args.iter().any(|a| a.contains("directx")));
        // Deliberate divergence: Python is audio-only (--no-video); the
        // Rust backend owns a video surface, so video stays enabled.
        assert!(!args.contains(&"--no-video".to_owned()));
    }

    #[test]
    fn instance_args_seed_windows_output_volume_like_python() {
        // Python test_shared_instance_uses_wasapi_not_directsound, ported.
        let mut cfg = Config::default();
        cfg.set_path("volume", serde_json::json!(25));
        let args = instance_args(&cfg, true);
        assert!(args.contains(&"--aout=mmdevice".to_owned()));
        assert!(args.contains(&"--mmdevice-volume=0.2500".to_owned()));
        assert!(args.contains(&"--directx-volume=0.2500".to_owned()));
        assert!(!args.iter().any(|a| a.contains("directsound")));
        // An out-of-range saved volume clamps instead of reaching libVLC.
        cfg.set_path("volume", serde_json::json!(400));
        let args = instance_args(&cfg, true);
        assert!(args.contains(&"--mmdevice-volume=1.0000".to_owned()));
    }

    #[test]
    fn instance_args_clamp_caching_like_python() {
        let mut cfg = Config::default();
        cfg.set_path("vlc_network_caching_ms", serde_json::json!(-10));
        let args = instance_args(&cfg, false);
        assert!(args.contains(&"--network-caching=0".to_owned()));
        assert!(args.contains(&"--file-caching=500".to_owned()));
        cfg.set_path("vlc_network_caching_ms", serde_json::json!(2000));
        let args = instance_args(&cfg, false);
        assert!(args.contains(&"--network-caching=2000".to_owned()));
        assert!(args.contains(&"--file-caching=2000".to_owned()));
    }

    #[test]
    fn build_instance_args_dedups_and_minimal_retry_matches_python() {
        // Python: build_vlc_instance_args("--no-video", "--no-video", ...)
        let args = build_instance_args(&["--no-video".to_owned(), "--no-video".to_owned()]);
        assert_eq!(args[0], "--no-plugins-cache");
        assert_eq!(args.iter().filter(|a| *a == "--no-video").count(), 1);
        assert_eq!(
            minimal_instance_args(),
            vec!["--no-plugins-cache", "--no-video", "--quiet"]
        );
    }

    #[test]
    fn bundled_plugin_path_only_for_bundled_layouts() {
        let base = std::env::temp_dir().join(format!(
            "blindrss-vlc-bundle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let exe_dir = base.clone();
        let lib = exe_dir.join("vlc/lib/libvlc.so.5");
        // No plugins directory yet: not a usable bundle.
        assert_eq!(bundled_plugin_path(&lib, &exe_dir), None);
        std::fs::create_dir_all(exe_dir.join("vlc/plugins")).unwrap();
        std::fs::create_dir_all(exe_dir.join("vlc/lib")).unwrap();
        assert_eq!(
            bundled_plugin_path(&lib, &exe_dir),
            Some(exe_dir.join("vlc/plugins"))
        );
        // A system library is never treated as bundled.
        assert_eq!(
            bundled_plugin_path(Path::new("/usr/lib/libvlc.so.5"), &exe_dir),
            None
        );
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn library_candidates_cover_bundled_and_linux_system_layouts() {
        let candidates = library_candidates();
        assert!(
            candidates
                .iter()
                .any(|p| p.ends_with(Path::new("vlc/lib").join(library_name())))
        );
        #[cfg(target_os = "linux")]
        assert!(
            candidates
                .iter()
                .any(|p| p == Path::new("/usr/lib/x86_64-linux-gnu/libvlc.so.5"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_xid_resolution_fails_closed_without_a_widget() {
        assert_eq!(super::linux::xid_for_widget(ptr::null_mut()), None);
    }

    #[test]
    fn headers_cannot_inject_vlc_options_or_newlines() {
        let options = media_options(
            &Config::default(),
            "https://example.org/",
            "UA\r\nInjected: yes",
            "bad\0value",
            "",
        );
        assert!(!options.iter().any(|o| o.contains(['\r', '\n', '\0'])));
        assert!(!options.iter().any(|o| o.starts_with(":http-referrer=")));
    }

    #[test]
    #[ignore = "requires installed libVLC 3; uses silent dummy audio output"]
    fn installed_vlc_plays_and_seeks_a_silent_wave() {
        let player = VlcPlayer::with_args(
            &Config::default(),
            &["--quiet", "--aout=dummy", "--vout=dummy"],
        )
        .unwrap();
        let path =
            std::env::temp_dir().join(format!("blindrss-vlc-smoke-{}.wav", std::process::id()));
        let samples = 8_000_u32 * 4;
        let mut wave = Vec::new();
        wave.extend_from_slice(b"RIFF");
        wave.extend_from_slice(&(36 + samples * 2).to_le_bytes());
        wave.extend_from_slice(b"WAVEfmt ");
        wave.extend_from_slice(&16_u32.to_le_bytes());
        wave.extend_from_slice(&1_u16.to_le_bytes());
        wave.extend_from_slice(&1_u16.to_le_bytes());
        wave.extend_from_slice(&8_000_u32.to_le_bytes());
        wave.extend_from_slice(&16_000_u32.to_le_bytes());
        wave.extend_from_slice(&2_u16.to_le_bytes());
        wave.extend_from_slice(&16_u16.to_le_bytes());
        wave.extend_from_slice(b"data");
        wave.extend_from_slice(&(samples * 2).to_le_bytes());
        wave.resize(wave.len() + samples as usize * 2, 0);
        std::fs::write(&path, wave).unwrap();
        assert!(player.load(path.to_str().unwrap(), &[]));
        assert!(player.play());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while player.state() != 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(player.state(), 3);
        assert_eq!(player.length(), 4000);
        assert!(player.set_rate(1.25));
        assert!(player.apply_equalizer(&EqConfig::default()));
        let devices = device_list(&player.api, player.instance);
        let ids: std::collections::HashSet<_> = devices.iter().map(|(_, id)| id).collect();
        assert_eq!(ids.len(), devices.len());
        assert!(
            devices
                .iter()
                .all(|(name, id)| !name.is_empty() && !id.is_empty())
        );
        player.seek(1500);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while player.time() < 1000 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(player.time() >= 1000);
        player.pause();
        player.stop();
        drop(player);
        std::fs::remove_file(path).unwrap();
    }
}
