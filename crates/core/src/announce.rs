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

/// Per-load hidden-window announcement dedupe (Audit 8 M10;
/// Python `gui/player.py` `_announce_when_hidden` +
/// `_hidden_announced`).
///
/// While the player window is hidden (Python's default), its
/// status line cannot be read, so load-state changes are spoken
/// instead — each distinct text at most once per load. A text is
/// recorded even when the window is shown: hiding the player
/// later must not replay states the user already saw. A load
/// the user did not ask to hear about (the play queue advancing
/// on its own) pre-suppresses its routine states; failures are
/// never pre-suppressed — only trouble is worth saying then.
#[derive(Debug, Clone, Default)]
pub struct HiddenAnnouncer {
    said: std::collections::HashSet<String>,
}

impl HiddenAnnouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new load: forget what the previous load said.
    /// `suppress` texts are marked as already said (queue
    /// auto-advance passes Python's "Loading media..." and
    /// "Playing").
    pub fn reset(&mut self, suppress: &[&str]) {
        self.said.clear();
        for text in suppress {
            self.said.insert((*text).to_string());
        }
    }

    /// Record `text` for this load and say whether the caller
    /// should speak it now: only the first time per load, and
    /// only while the window is hidden.
    pub fn should_speak(&mut self, text: &str, window_shown: bool) -> bool {
        let fresh = self.said.insert(text.to_string());
        fresh && !window_shown
    }

    /// Whether `text` was already recorded for this load.
    pub fn was_said(&self, text: &str) -> bool {
        self.said.contains(text)
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

/// Process names that mean JAWS/Fusion is the running screen
/// reader (Python's gate in _speak_jaws: a registered COM server on
/// disk is not enough — the process must be alive).
pub fn is_jaws_process_name(exe_name: &str) -> bool {
    let lower = exe_name.to_ascii_lowercase();
    matches!(lower.as_str(), "jfw.exe" | "jaws.exe" | "fusion.exe")
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
        if let Some(m) = load()
            && running(m)
        {
            let ok = unsafe {
                if let Some(cancel) = proc::<CancelSpeech>(m, "nvdaController_cancelSpeech") {
                    let _ = cancel();
                }
                proc::<SpeakText>(m, "nvdaController_speakText")
                    .map(|f| f(wide(text).as_ptr()) == 0)
                    .unwrap_or(false)
            };
            if ok {
                return true;
            }
        }
        // Python's order: NVDA first, JAWS second (speech only —
        // JAWS has no comparable simple Braille entry point).
        jaws::speak(text)
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

    /// JAWS speech through the FreedomSci.JawsApi COM object
    /// (Python's _speak_jaws): gated on a live jfw/jaws/fusion
    /// process, then IDispatch::Invoke("SayString", text, True).
    /// Hand-rolled COM via ole32 proc addresses — no COM crate.
    mod jaws {
        use std::ffi::c_void;
        use std::sync::OnceLock;

        unsafe extern "system" {
            fn LoadLibraryW(name: *const u16) -> usize;
            fn GetProcAddress(module: usize, name: *const u8) -> usize;
            fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
            fn Process32FirstW(snap: isize, entry: *mut ProcessEntry32) -> i32;
            fn Process32NextW(snap: isize, entry: *mut ProcessEntry32) -> i32;
            fn CloseHandle(h: isize) -> i32;
        }

        #[repr(C)]
        struct ProcessEntry32 {
            size: u32,
            cnt_usage: u32,
            process_id: u32,
            default_heap_id: usize,
            module_id: u32,
            cnt_threads: u32,
            parent_process_id: u32,
            pri_class_base: i32,
            flags: u32,
            exe_file: [u16; 260],
        }

        fn wide(s: &str) -> Vec<u16> {
            s.encode_utf16().chain(std::iter::once(0)).collect()
        }

        fn jaws_running() -> bool {
            unsafe {
                let snap = CreateToolhelp32Snapshot(0x0000_0002, 0);
                if snap == -1 {
                    return false;
                }
                let mut entry: ProcessEntry32 = std::mem::zeroed();
                entry.size = std::mem::size_of::<ProcessEntry32>() as u32;
                let mut found = false;
                if Process32FirstW(snap, &mut entry) != 0 {
                    loop {
                        let len = entry
                            .exe_file
                            .iter()
                            .position(|c| *c == 0)
                            .unwrap_or(entry.exe_file.len());
                        let name = String::from_utf16_lossy(&entry.exe_file[..len]);
                        if super::super::is_jaws_process_name(&name) {
                            found = true;
                            break;
                        }
                        if Process32NextW(snap, &mut entry) == 0 {
                            break;
                        }
                    }
                }
                CloseHandle(snap);
                found
            }
        }

        type CoInitializeEx = unsafe extern "system" fn(*const c_void, u32) -> i32;
        type CoUninitialize = unsafe extern "system" fn();
        type ClsidFromProgId = unsafe extern "system" fn(*const u16, *mut Guid) -> i32;
        type CoCreateInstance = unsafe extern "system" fn(
            *const Guid,
            *const c_void,
            u32,
            *const Guid,
            *mut *mut c_void,
        ) -> i32;
        type SysAllocString = unsafe extern "system" fn(*const u16) -> *mut u16;
        type SysFreeString = unsafe extern "system" fn(*mut u16);

        #[repr(C)]
        #[derive(Clone, Copy)]
        struct Guid {
            a: u32,
            b: u16,
            c: u16,
            d: [u8; 8],
        }

        const IID_NULL: Guid = Guid {
            a: 0,
            b: 0,
            c: 0,
            d: [0; 8],
        };
        const IID_IDISPATCH: Guid = Guid {
            a: 0x0002_0400,
            b: 0,
            c: 0,
            d: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
        };

        #[repr(C)]
        struct Variant {
            vt: u16,
            r1: u16,
            r2: u16,
            r3: u16,
            data: usize,
            extra: usize,
        }

        #[repr(C)]
        struct DispParams {
            args: *const Variant,
            named_args: *const i32,
            arg_count: u32,
            named_count: u32,
        }

        type GetIdsOfNames = unsafe extern "system" fn(
            *mut c_void,
            *const Guid,
            *const *const u16,
            u32,
            u32,
            *mut i32,
        ) -> i32;
        type Invoke = unsafe extern "system" fn(
            *mut c_void,
            i32,
            *const Guid,
            u32,
            u16,
            *const DispParams,
            *mut Variant,
            *const c_void,
            *const c_void,
        ) -> i32;
        type Release = unsafe extern "system" fn(*mut c_void) -> u32;

        fn ole_proc<T>(name: &str) -> Option<T> {
            static OLE32: OnceLock<usize> = OnceLock::new();
            let module = *OLE32.get_or_init(|| unsafe { LoadLibraryW(wide("ole32.dll").as_ptr()) });
            if module == 0 {
                return None;
            }
            let mut buf = name.as_bytes().to_vec();
            buf.push(0);
            let addr = unsafe { GetProcAddress(module, buf.as_ptr()) };
            if addr == 0 {
                None
            } else {
                Some(unsafe { std::mem::transmute_copy(&addr) })
            }
        }

        pub fn speak(text: &str) -> bool {
            if !jaws_running() {
                return false;
            }
            unsafe {
                let Some(co_init) = ole_proc::<CoInitializeEx>("CoInitializeEx") else {
                    return false;
                };
                // COINIT_APARTMENTTHREADED; RPC_E_CHANGED_MODE (-2)
                // means COM is already up in another mode — proceed.
                let hr = co_init(std::ptr::null(), 2);
                let initialized = hr >= 0;
                let ok = speak_inner(text);
                if initialized && let Some(co_uninit) = ole_proc::<CoUninitialize>("CoUninitialize")
                {
                    co_uninit();
                }
                ok
            }
        }

        unsafe fn speak_inner(text: &str) -> bool {
            unsafe {
                let Some(clsid_from) = ole_proc::<ClsidFromProgId>("CLSIDFromProgID") else {
                    return false;
                };
                let Some(create) = ole_proc::<CoCreateInstance>("CoCreateInstance") else {
                    return false;
                };
                let mut clsid = IID_NULL;
                if clsid_from(wide("FreedomSci.JawsApi").as_ptr(), &mut clsid) < 0 {
                    return false;
                }
                let mut obj: *mut c_void = std::ptr::null_mut();
                // CLSCTX_ALL = 0x17.
                if create(&clsid, std::ptr::null(), 0x17, &IID_IDISPATCH, &mut obj) < 0
                    || obj.is_null()
                {
                    return false;
                }
                let vtable = *(obj as *const *const usize);
                let get_ids: GetIdsOfNames = std::mem::transmute_copy(&*vtable.add(5));
                let invoke: Invoke = std::mem::transmute_copy(&*vtable.add(6));
                let release: Release = std::mem::transmute_copy(&*vtable.add(2));
                let mut dispid = 0i32;
                let name = wide("SayString");
                let names = [name.as_ptr()];
                let hr = get_ids(obj, &IID_NULL, names.as_ptr(), 1, 0x0400, &mut dispid);
                let mut result = false;
                if hr >= 0 {
                    result = invoke_say(obj, invoke, dispid, text);
                }
                release(obj);
                result
            }
        }

        unsafe fn invoke_say(obj: *mut c_void, invoke: Invoke, dispid: i32, text: &str) -> bool {
            unsafe {
                static AUT32: OnceLock<usize> = OnceLock::new();
                let module = *AUT32.get_or_init(|| LoadLibraryW(wide("oleaut32.dll").as_ptr()));
                if module == 0 {
                    return false;
                }
                let mut buf = b"SysAllocString\0".to_vec();
                let alloc_addr = GetProcAddress(module, buf.as_mut_ptr());
                buf = b"SysFreeString\0".to_vec();
                let free_addr = GetProcAddress(module, buf.as_mut_ptr());
                if alloc_addr == 0 || free_addr == 0 {
                    return false;
                }
                let alloc: SysAllocString = std::mem::transmute_copy(&alloc_addr);
                let free_str: SysFreeString = std::mem::transmute_copy(&free_addr);
                let bstr = alloc(wide(text).as_ptr());
                if bstr.is_null() {
                    return false;
                }
                // IDispatch arguments are passed in reverse order:
                // SayString(text, interrupt) -> [interrupt, text].
                let args = [
                    Variant {
                        vt: 11, // VT_BOOL
                        r1: 0,
                        r2: 0,
                        r3: 0,
                        data: 0xFFFF, // VARIANT_TRUE
                        extra: 0,
                    },
                    Variant {
                        vt: 8, // VT_BSTR
                        r1: 0,
                        r2: 0,
                        r3: 0,
                        data: bstr as usize,
                        extra: 0,
                    },
                ];
                let params = DispParams {
                    args: args.as_ptr(),
                    named_args: std::ptr::null(),
                    arg_count: 2,
                    named_count: 0,
                };
                let mut out = Variant {
                    vt: 0,
                    r1: 0,
                    r2: 0,
                    r3: 0,
                    data: 0,
                    extra: 0,
                };
                // DISPATCH_METHOD = 1, LOCALE_USER_DEFAULT = 0x0400.
                let hr = invoke(
                    obj,
                    dispid,
                    &IID_NULL,
                    0x0400,
                    1,
                    &params,
                    &mut out,
                    std::ptr::null(),
                    std::ptr::null(),
                );
                free_str(bstr);
                hr >= 0 && out.vt == 11 && (out.data as u16) != 0
            }
        }
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

#[cfg(test)]
mod hidden_announcer_tests {
    use super::HiddenAnnouncer;

    #[test]
    fn hidden_window_speaks_each_load_state_once() {
        // M10: with the player hidden, load/failure/readiness
        // transitions are spoken — but never twice per load.
        let mut a = HiddenAnnouncer::new();
        a.reset(&[]);
        assert!(a.should_speak("Loading media...", false));
        assert!(!a.should_speak("Loading media...", false));
        assert!(a.should_speak("Playing", false));
        assert!(!a.should_speak("Playing", false));
        assert!(a.should_speak("Failed to load media", false));
    }

    #[test]
    fn shown_window_records_without_speaking() {
        // Python: "even when shown: hiding the player later must
        // not replay it" — the text is recorded either way.
        let mut a = HiddenAnnouncer::new();
        a.reset(&[]);
        assert!(!a.should_speak("Playing", true));
        assert!(a.was_said("Playing"));
        assert!(!a.should_speak("Playing", false), "no replay after hiding");
    }

    #[test]
    fn reset_starts_a_fresh_load() {
        let mut a = HiddenAnnouncer::new();
        a.reset(&[]);
        assert!(a.should_speak("Playing", false));
        a.reset(&[]);
        assert!(a.should_speak("Playing", false), "a new load says it again");
    }

    #[test]
    fn queue_advance_suppresses_routine_states_but_not_failure() {
        // Python load_media(announce=False): the queue advancing
        // on its own stays quiet about loading/playing, while a
        // failure is still spoken.
        let mut a = HiddenAnnouncer::new();
        a.reset(&["Loading media...", "Playing"]);
        assert!(!a.should_speak("Loading media...", false));
        assert!(!a.should_speak("Playing", false));
        assert!(a.should_speak("Failed to load media", false));
    }
}
