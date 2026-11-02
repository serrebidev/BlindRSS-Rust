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
    ($($name:ident: $ty:ty),+ $(,)?) => {
        struct Api {
            $($name: $ty,)+
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
    media_player_get_rate: unsafe extern "C" fn(Handle) -> f32,
    media_player_set_rate: unsafe extern "C" fn(Handle, f32) -> i32,
    audio_get_volume: unsafe extern "C" fn(Handle) -> i32,
    audio_set_volume: unsafe extern "C" fn(Handle, i32) -> i32,
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
    paths.push(PathBuf::from(library_name()));
    paths
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

fn load_api() -> Result<Api, String> {
    let mut last_error = "libVLC was not found".to_owned();
    for path in library_candidates() {
        // Windows does not search a loaded DLL's directory for its dependencies.
        #[cfg(windows)]
        let core = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .and_then(|p| unsafe { Library::new(p.join("libvlccore.dll")) }.ok());
        #[cfg(not(windows))]
        let core = None;
        match unsafe { Library::new(&path) }
            .map_err(|e| e.to_string())
            .and_then(|lib| unsafe { Api::from_library(lib, core) })
        {
            Ok(api) => return Ok(api),
            Err(e) => last_error = e,
        }
    }
    Err(last_error)
}

pub fn media_options(
    cfg: &Config,
    url: &str,
    user_agent: &str,
    referer: &str,
    cookie: &str,
) -> Vec<String> {
    let local_proxy = url.starts_with("http://127.0.0.1:");
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
        Self::with_args(
            cfg,
            &["--quiet", "--no-video-title-show", "--no-snapshot-preview"],
        )
    }

    fn with_args(cfg: &Config, args: &[&str]) -> Result<Self, String> {
        let api = load_api()?;
        let args: Vec<CString> = args.iter().map(|s| CString::new(*s).unwrap()).collect();
        let argv: Vec<_> = args.iter().map(|s| s.as_ptr()).collect();
        let instance = unsafe { (api.new)(argv.len() as i32, argv.as_ptr()) };
        if instance.is_null() {
            return Err("Could not initialize libVLC".to_owned());
        }
        let player = unsafe { (api.media_player_new)(instance) };
        if player.is_null() {
            unsafe { (api.release)(instance) };
            return Err("Could not create the VLC player".to_owned());
        }
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
    pub fn rate(&self) -> f64 {
        unsafe { (self.api.media_player_get_rate)(self.player) as f64 }
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

    pub fn soundcards(&self) -> Vec<(String, String)> {
        let module = if cfg!(windows) {
            c"mmdevice"
        } else if cfg!(target_os = "macos") {
            c"auhal"
        } else {
            c"pulse"
        };
        let head =
            unsafe { (self.api.audio_output_device_list_get)(self.instance, module.as_ptr()) };
        let mut next = head;
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
            unsafe { (self.api.audio_output_device_list_release)(head) };
        }
        devices
    }

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
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = drawable;
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
        let local = media_options(&cfg, "http://127.0.0.1:1234/stream?url=x", "", "", "");
        assert!(local.contains(&":network-caching=75".to_owned()));
        assert!(local.contains(&":file-caching=125".to_owned()));
        let remote = media_options(&cfg, "https://example.com/file", "", "", "");
        assert!(remote.contains(&":network-caching=0".to_owned()));
        assert!(remote.contains(&":file-caching=500".to_owned()));
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
        let devices = player.soundcards();
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
