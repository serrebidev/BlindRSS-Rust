//! Native player surface with libVLC 3 when installed and wxMediaCtrl as fallback.
//! Completion is polled on the UI thread; no VLC callback ever enters wxWidgets.

mod vlc;

use blindrss_core::{config::Config, equalizer::EqConfig};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use wxdragon::prelude::*;
use wxdragon::widgets::media_ctrl::SeekMode;

pub fn soundcards(cfg: &Config) -> Vec<(String, String)> {
    vlc::VlcPlayer::new(cfg)
        .map(|engine| engine.soundcards())
        .unwrap_or_default()
}

struct Entry {
    engine: Rc<vlc::VlcPlayer>,
    finished: Option<Rc<dyn Fn(())>>,
    ended: bool,
    desired_volume: Option<f64>,
}

thread_local! {
    static ENGINES: RefCell<HashMap<u64, Entry>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

#[derive(Clone, Copy)]
pub struct MediaBackend {
    id: u64,
    surface: Panel,
    fallback: Option<MediaCtrl>,
}

impl MediaBackend {
    pub fn new(parent: &Frame, cfg: &Config) -> Self {
        let surface = Panel::builder(parent).build();
        surface.set_name("Video");
        let id = NEXT_ID.with(|next| {
            let id = next.get();
            next.set(id + 1);
            id
        });
        let fallback = match vlc::VlcPlayer::new(cfg) {
            Ok(engine) => {
                engine.attach_video(surface.get_handle());
                ENGINES.with(|registry| {
                    registry.borrow_mut().insert(
                        id,
                        Entry {
                            engine: Rc::new(engine),
                            finished: None,
                            ended: false,
                            desired_volume: None,
                        },
                    )
                });
                None
            }
            Err(_) => {
                let native = MediaCtrl::builder(&surface).build();
                let sizer = BoxSizer::builder(Orientation::Vertical).build();
                sizer.add(&native, 1, SizerFlag::Expand, 0);
                surface.set_sizer(sizer, true);
                Some(native)
            }
        };
        let backend = Self {
            id,
            surface,
            fallback,
        };
        // Parent destruction can bypass the explicit close command (app shutdown).
        surface.bind_internal(EventType::DESTROY, move |_| {
            backend.close();
        });
        backend
    }

    pub fn window(&self) -> Panel {
        self.surface
    }

    fn engine(&self) -> Option<Rc<vlc::VlcPlayer>> {
        ENGINES.with(|registry| {
            registry
                .borrow()
                .get(&self.id)
                .map(|entry| entry.engine.clone())
        })
    }

    pub fn uses_vlc(&self) -> bool {
        self.engine().is_some()
    }

    pub fn load_uri(&self, url: &str) -> bool {
        self.load_uri_inner(url, &referer_for_load(url, &[]))
    }

    fn load_uri_inner(&self, url: &str, referer: &str) -> bool {
        if let Some(engine) = self.engine() {
            // Re-attach on every load: at construction the surface may
            // not be realized yet (on wxGTK its X11 window id does not
            // exist until then), and set_xwindow must land before play.
            engine.attach_video(self.surface.get_handle());
            let cfg = Config::load(&crate::config_path()).unwrap_or_default();
            let jar = blindrss_core::cookies::load_jar_beside(&crate::config_path());
            let cookie = blindrss_core::cookies::cookie_header_for(&jar, url);
            let options = vlc::media_options(
                &cfg,
                url,
                &blindrss_core::http::user_agent(),
                referer,
                &cookie,
            );
            ENGINES.with(|registry| {
                if let Some(entry) = registry.borrow_mut().get_mut(&self.id) {
                    entry.ended = false;
                }
            });
            engine.set_output(cfg.get_str("preferred_soundcard", ""));
            engine.load(url, &options)
        } else {
            self.fallback.is_some_and(|native| native.load_uri(url))
        }
    }

    pub fn load_uri_with_headers(&self, url: &str, headers: &[(String, String)]) -> bool {
        // VLC only supports a subset of channel headers; the existing HTTP relay supports all.
        // The referer additionally goes into the VLC media options,
        // as Python's load_vlc_url does (`:http-referrer`) — the
        // relay applies it on its own fetch too, so both paths
        // carry it (Audit 8 Task 19, deferred from Task 18).
        let referer = referer_for_load(url, headers);
        let target = blindrss_core::http_headers::protected_media_url(url, headers);
        self.load_uri_inner(&target, &referer)
    }

    pub fn play(&self) -> bool {
        if let Some(engine) = self.engine() {
            engine.play()
        } else {
            self.fallback.is_some_and(|native| native.play())
        }
    }

    pub fn pause(&self) -> bool {
        if let Some(engine) = self.engine() {
            engine.pause();
            true
        } else {
            self.fallback.is_some_and(|native| native.pause())
        }
    }

    pub fn stop(&self) -> bool {
        if let Some(engine) = self.engine() {
            engine.stop();
            true
        } else {
            self.fallback.is_some_and(|native| native.stop())
        }
    }

    pub fn tell(&self) -> i64 {
        self.engine()
            .map_or_else(|| self.fallback.map_or(0, |n| n.tell()), |e| e.time())
    }
    pub fn length(&self) -> i64 {
        self.engine()
            .map_or_else(|| self.fallback.map_or(0, |n| n.length()), |e| e.length())
    }

    pub fn seek(&self, position: i64, mode: SeekMode) -> i64 {
        if let Some(engine) = self.engine() {
            let target = match mode {
                SeekMode::FromStart => position,
                SeekMode::FromCurrent => engine.time().saturating_add(position),
                SeekMode::FromEnd => engine.length().saturating_add(position),
            }
            .max(0);
            engine.seek(target);
            target
        } else {
            self.fallback.map_or(0, |n| n.seek(position, mode))
        }
    }

    pub fn get_volume(&self) -> f64 {
        self.engine().map_or_else(
            || self.fallback.map_or(1.0, |n| n.get_volume()),
            |e| e.volume(),
        )
    }

    pub fn set_volume(&self, volume: f64) -> bool {
        if !volume.is_finite() {
            return false;
        }
        let volume = volume.clamp(0.0, 1.0);
        if let Some(engine) = self.engine() {
            ENGINES.with(|registry| {
                if let Some(entry) = registry.borrow_mut().get_mut(&self.id) {
                    entry.desired_volume = Some(volume);
                }
            });
            engine.set_volume(volume)
        } else {
            self.fallback.is_some_and(|n| n.set_volume(volume))
        }
    }

    pub fn set_playback_rate(&self, rate: f64) -> bool {
        if let Some(engine) = self.engine() {
            engine.set_rate(rate)
        } else {
            self.fallback.is_some_and(|n| n.set_playback_rate(rate))
        }
    }

    pub fn get_state(&self) -> MediaState {
        if let Some(engine) = self.engine() {
            match engine.state() {
                3 => MediaState::Playing,
                4 => MediaState::Paused,
                _ => MediaState::Stopped,
            }
        } else {
            self.fallback.map_or(MediaState::Stopped, |n| n.get_state())
        }
    }

    pub fn apply_equalizer(&self, config: &EqConfig) -> bool {
        self.engine()
            .is_some_and(|engine| engine.apply_equalizer(config))
    }

    /// Apply an output-device (soundcard) choice to the LIVE
    /// session (Audit 8 M21, Python's `apply_preferred_soundcard`
    /// via `gui/mainframe.py`): the engine's audio output moves
    /// without reloading the media, so playback position and the
    /// loaded stream are untouched. The wx MediaCtrl fallback has
    /// no device selection and reports false.
    pub fn apply_soundcard(&self, device: &str) -> bool {
        match self.engine() {
            Some(engine) => {
                engine.set_output(device);
                true
            }
            None => false,
        }
    }

    /// Stream phase for the seek verifier (Audit 8 M05): VLC's
    /// Opening (1) / Buffering (2) states mean a seek landing
    /// cannot be judged yet. `get_state` collapses those to
    /// Stopped, so the verifier reads this instead. The fallback
    /// has no opening signal; a stopped backend with no length
    /// yet counts as still opening.
    pub fn stream_phase(&self) -> blindrss_core::playback::StreamPhase {
        use blindrss_core::playback::StreamPhase;
        if let Some(engine) = self.engine() {
            match engine.state() {
                1 | 2 => StreamPhase::Opening,
                _ => StreamPhase::Ready,
            }
        } else if self.get_state() == MediaState::Stopped && self.length() <= 0 {
            StreamPhase::Opening
        } else {
            StreamPhase::Ready
        }
    }

    /// Whether the backend itself reports the current input
    /// seekable, when it can say (libVLC can; the fallback
    /// cannot, hence None rather than a guess).
    pub fn is_seekable(&self) -> Option<bool> {
        self.engine().map(|engine| engine.is_seekable())
    }

    pub fn on_finished(&self, callback: impl Fn(()) + 'static) {
        let callback: Rc<dyn Fn(())> = Rc::new(callback);
        if let Some(native) = self.fallback {
            native.on_finished(move |_| callback(()));
        } else {
            ENGINES.with(|registry| {
                if let Some(entry) = registry.borrow_mut().get_mut(&self.id) {
                    entry.finished = Some(callback);
                }
            });
        }
    }

    pub fn tick(&self) {
        let Some(engine) = self.engine() else {
            return;
        };
        let state = engine.state();
        let (callback, volume) = ENGINES.with(|registry| {
            let mut registry = registry.borrow_mut();
            let Some(entry) = registry.get_mut(&self.id) else {
                return (None, None);
            };
            let callback = if state == 6 && !entry.ended {
                entry.finished.clone()
            } else {
                None
            };
            entry.ended = state == 6;
            // VLC can discard volume set before its audio output exists.
            let volume = if state == 3 {
                entry.desired_volume.take()
            } else {
                None
            };
            (callback, volume)
        });
        if let Some(volume) = volume {
            engine.set_volume(volume);
        }
        if let Some(callback) = callback {
            callback(());
        }
    }

    pub fn close(&self) {
        // Drop outside the registry borrow: libVLC shutdown may wait for worker threads.
        let entry = ENGINES.with(|registry| registry.borrow_mut().remove(&self.id));
        drop(entry);
    }
}

/// The referer a VLC load carries (Audit 8 Task 19): the
/// playback headers' explicit `referer` when the load has them
/// (channel value or the same-origin/origin-root default
/// `playback_headers` computed), else Python's `referer_for_url`
/// default — the media origin root — for remote http(s) URLs.
/// Loopback (the local proxies) and non-http inputs get none:
/// their own fetches already carry the article's headers.
fn referer_for_load(url: &str, headers: &[(String, String)]) -> String {
    if let Some((_, value)) = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("referer"))
    {
        return value.clone();
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return String::new();
    };
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    if authority.is_empty() || matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]") {
        return String::new();
    }
    format!("{scheme}://{authority}/")
}

#[cfg(test)]
mod tests {
    use super::referer_for_load;

    #[test]
    fn referer_header_wins_over_default() {
        let headers = vec![(
            "referer".to_string(),
            "https://blog.example/post".to_string(),
        )];
        assert_eq!(
            referer_for_load("https://cdn.example/ep.mp3", &headers),
            "https://blog.example/post"
        );
        // Header names match case-insensitively.
        let headers = vec![("Referer".to_string(), "https://blog.example/".to_string())];
        assert_eq!(
            referer_for_load("https://cdn.example/ep.mp3", &headers),
            "https://blog.example/"
        );
    }

    #[test]
    fn referer_defaults_to_media_origin_root() {
        assert_eq!(
            referer_for_load("https://cdn.example:8443/a/ep.mp3?x=1", &[]),
            "https://cdn.example:8443/"
        );
        assert_eq!(
            referer_for_load("http://media.example/ep.mp3", &[]),
            "http://media.example/"
        );
    }

    #[test]
    fn referer_absent_for_loopback_and_local_inputs() {
        assert_eq!(
            referer_for_load("http://127.0.0.1:8765/media?id=1", &[]),
            ""
        );
        assert_eq!(referer_for_load("http://localhost:8765/play?u=x", &[]), "");
        assert_eq!(referer_for_load("/home/user/ep.mp3", &[]), "");
        assert_eq!(referer_for_load("file:///home/user/ep.mp3", &[]), "");
    }
}
