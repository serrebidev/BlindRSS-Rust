//! Screen-reader announcements (Python parity: core/announcements.py
//! + core/screen_reader_announce.py, PARITY §7.4).
//!
//! Key actions (refresh start, read/unread toggle, favorite toggle,
//! and friends) confirm themselves through the screen reader's
//! speech and/or Braille channels instead of only the status bar.
//! Each event carries its own output mode, persisted under the
//! config's "announcements" map: none / speech / braille / both
//! (the default is both, as in Python).
//!
//! Output paths: Windows speaks and brailles through NVDA's
//! controller client DLL (the same one Python drives; JAWS's COM
//! API has no equivalent simple bridge and stays a noted gap);
//! macOS speaks through the `say` command; other platforms have no
//! path and announcements simply report false, failing closed so a
//! missing speech path never breaks the action itself.

use crate::config::Config;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    None,
    Speech,
    Braille,
    Both,
}

impl Mode {
    fn parse(s: &str) -> Option<Mode> {
        match s {
            "none" => Some(Mode::None),
            "speech" => Some(Mode::Speech),
            "braille" => Some(Mode::Braille),
            "both" => Some(Mode::Both),
            _ => None,
        }
    }
    fn as_str(&self) -> &'static str {
        match self {
            Mode::None => "none",
            Mode::Speech => "speech",
            Mode::Braille => "braille",
            Mode::Both => "both",
        }
    }
    fn wants_speech(&self) -> bool {
        matches!(self, Mode::Speech | Mode::Both)
    }
    fn wants_braille(&self) -> bool {
        matches!(self, Mode::Braille | Mode::Both)
    }
}

/// Mode strings and display labels in Settings order.
pub const MODE_ORDER: [&str; 4] = ["none", "speech", "braille", "both"];
pub const MODE_LABELS: [&str; 4] = ["None", "Only speech", "Only Braille", "Speech and Braille"];

#[derive(Debug, Clone, Copy)]
pub struct Event {
    pub id: &'static str,
    pub label: &'static str,
    pub help: &'static str,
}

/// The event table, ids stable because they persist in config
/// (Python core/announcements.py EVENTS, same order).
pub const EVENTS: [Event; 9] = [
    Event {
        id: "filter_change",
        label: "Filter change",
        help: "Announce the applied article filter (Ctrl+1 to Ctrl+6).",
    },
    Event {
        id: "status_toggle",
        label: "Read/unread status change",
        help: "Announce the new status when toggling read/unread (Backspace).",
    },
    Event {
        id: "start_update",
        label: "Start update",
        help: "Announce when a feed refresh is started (F5).",
    },
    Event {
        id: "stop_update",
        label: "Stop update",
        help: "Announce when a feed refresh is stopped (Shift+F5).",
    },
    Event {
        id: "favorite_toggle",
        label: "Favorites change",
        help: "Announce when an article is added to or removed from favorites (Ctrl+D).",
    },
    Event {
        id: "playback_speed",
        label: "Playback speed change",
        help: "Announce the current playback speed when it is changed by keyboard.",
    },
    Event {
        id: "media_navigation",
        label: "Media and chapter navigation",
        help: "Announce the item or chapter when moving to the next/previous one.",
    },
    Event {
        id: "sponsor_skip",
        label: "SponsorBlock skip",
        help: "Announce when a sponsor or other SponsorBlock segment is skipped.",
    },
    Event {
        id: "general",
        label: "Other notifications",
        help: "Announce other significant interface changes and notifications.",
    },
];

/// Per-event modes resolved from config (defaults: all "both").
#[derive(Debug, Clone, PartialEq)]
pub struct Announcements {
    modes: Vec<Mode>,
}

impl Default for Announcements {
    fn default() -> Self {
        Announcements {
            modes: vec![Mode::Both; EVENTS.len()],
        }
    }
}

impl Announcements {
    /// Merge the config's "announcements" map over the defaults,
    /// dropping unknown events and invalid values (Python's
    /// normalize_modes).
    pub fn from_config(cfg: &Config) -> Self {
        let raw = cfg.raw().get("announcements");
        Self::from_value(raw)
    }

    pub fn from_value(raw: Option<&Value>) -> Self {
        let mut out = Announcements::default();
        let Some(obj) = raw.and_then(|v| v.as_object()) else {
            return out;
        };
        for (i, ev) in EVENTS.iter().enumerate() {
            if let Some(mode) = obj
                .get(ev.id)
                .and_then(|v| v.as_str())
                .and_then(Mode::parse)
            {
                out.modes[i] = mode;
            }
        }
        out
    }

    pub fn mode(&self, event_id: &str) -> Mode {
        EVENTS
            .iter()
            .position(|e| e.id == event_id)
            .map(|i| self.modes[i])
            .unwrap_or(Mode::Both)
    }

    /// Settings index into MODE_ORDER for an event.
    pub fn mode_index(&self, event_id: &str) -> usize {
        MODE_ORDER
            .iter()
            .position(|m| *m == self.mode(event_id).as_str())
            .unwrap_or(3)
    }

    pub fn set_mode(&mut self, event_id: &str, mode: &str) {
        if let Some(i) = EVENTS.iter().position(|e| e.id == event_id)
            && let Some(m) = Mode::parse(mode)
        {
            self.modes[i] = m;
        }
    }

    /// The full map as a JSON object, for persisting to config.
    pub fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        for (i, ev) in EVENTS.iter().enumerate() {
            map.insert(
                ev.id.to_string(),
                Value::String(self.modes[i].as_str().into()),
            );
        }
        Value::Object(map)
    }

    /// Announce a message for an event under its configured mode.
    /// Returns true when an output path accepted it; every failure
    /// is swallowed so callers never break an action over speech.
    pub fn announce(&self, event_id: &str, message: &str) -> bool {
        let text = message.trim();
        if text.is_empty() {
            return false;
        }
        let mode = self.mode(event_id);
        if mode == Mode::None {
            return false;
        }
        emit(text, mode.wants_speech(), mode.wants_braille())
    }
}

/// Settings test path: speech AND Braille, ignoring configuration
/// (Python's announce_test — honoring "none" here would make a
/// working setup indistinguishable from a broken one).
pub fn announce_test(message: &str) -> bool {
    let text = message.trim();
    if text.is_empty() {
        return false;
    }
    emit(text, true, true)
}

fn emit(text: &str, speech: bool, braille: bool) -> bool {
    let mut handled = false;
    if speech && backend::speak(text) {
        handled = true;
    }
    if braille && backend::braille(text) {
        handled = true;
    }
    handled
}

#[cfg(target_os = "windows")]
mod backend {
    use std::sync::OnceLock;

    type TestIfRunning = unsafe extern "system" fn() -> u32;
    type SpeakText = unsafe extern "system" fn(*const u16) -> u32;
    type CancelSpeech = unsafe extern "system" fn() -> u32;
    type BrailleMessage = unsafe extern "system" fn(*const u16) -> u32;

    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> usize;
        fn GetProcAddress(module: usize, name: *const u8) -> usize;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Locate NVDA's controller client the way Python does: the
    /// BLINDRSS_NVDA_CONTROLLER_DLL override, then beside the
    /// executable, then the DLL search path.
    fn load() -> Option<usize> {
        static DLL: OnceLock<Option<usize>> = OnceLock::new();
        *DLL.get_or_init(|| unsafe {
            let mut candidates: Vec<String> = Vec::new();
            if let Ok(p) = std::env::var("BLINDRSS_NVDA_CONTROLLER_DLL") {
                candidates.push(p);
            }
            if let Ok(exe) = std::env::current_exe()
                && let Some(dir) = exe.parent()
            {
                for name in ["nvdaControllerClient64.dll", "nvdaControllerClient.dll"] {
                    candidates.push(dir.join(name).to_string_lossy().into_owned());
                }
            }
            candidates.push("nvdaControllerClient64.dll".to_string());
            candidates.push("nvdaControllerClient.dll".to_string());
            for c in &candidates {
                let h = LoadLibraryW(wide(c).as_ptr());
                if h != 0 {
                    return Some(h);
                }
            }
            None
        })
    }

    fn proc<T>(module: usize, name: &str) -> Option<T> {
        let mut buf = name.as_bytes().to_vec();
        buf.push(0);
        let addr = unsafe { GetProcAddress(module, buf.as_ptr()) };
        if addr == 0 {
            None
        } else {
            Some(unsafe { std::mem::transmute_copy(&addr) })
        }
    }

    fn running(module: usize) -> bool {
        proc::<TestIfRunning>(module, "nvdaController_testIfRunning")
            .map(|f| unsafe { f() } == 0)
            .unwrap_or(false)
    }

    pub fn speak(text: &str) -> bool {
        let Some(m) = load() else { return false };
        if !running(m) {
            return false;
        }
        unsafe {
            if let Some(cancel) = proc::<CancelSpeech>(m, "nvdaController_cancelSpeech") {
                let _ = cancel();
            }
            proc::<SpeakText>(m, "nvdaController_speakText")
                .map(|f| f(wide(text).as_ptr()) == 0)
                .unwrap_or(false)
        }
    }

    pub fn braille(text: &str) -> bool {
        let Some(m) = load() else { return false };
        if !running(m) {
            return false;
        }
        let Some(f) = proc::<BrailleMessage>(m, "nvdaController_brailleMessage") else {
            return false;
        };
        (unsafe { f(wide(text).as_ptr()) }) == 0
    }
}

#[cfg(target_os = "macos")]
mod backend {
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;

    static PREV: Mutex<Option<Child>> = Mutex::new(None);

    /// Python's macOS path: the `say` command, interrupting any
    /// announcement still speaking.
    pub fn speak(text: &str) -> bool {
        let Ok(mut prev) = PREV.lock() else {
            return false;
        };
        if let Some(mut child) = prev.take() {
            let _ = child.kill();
        }
        match Command::new("say")
            .arg("--")
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                *prev = Some(child);
                true
            }
            Err(_) => false,
        }
    }

    pub fn braille(_text: &str) -> bool {
        false
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod backend {
    pub fn speak(_text: &str) -> bool {
        false
    }
    pub fn braille(_text: &str) -> bool {
        false
    }
}
