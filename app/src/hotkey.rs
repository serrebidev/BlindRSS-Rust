//! Windows global show/hide hotkey: raw Win32 RegisterHotKey on
//! the main frame (Python's approach — wx re-maps key codes and
//! drops MOD_NOREPEAT), received through a wndproc subclass that
//! forwards WM_HOTKEY to the app's message drain as
//! RefreshMsg::ToggleWindow. No-op off Windows.

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};

    pub const HOTKEY_ID: i32 = 0xB001;
    const WM_HOTKEY: u32 = 0x0312;
    const GWLP_WNDPROC: i32 = -4;
    const MOD_NOREPEAT: u32 = 0x4000;

    type WndProc = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;

    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> usize;
        fn GetProcAddress(module: usize, name: *const u8) -> usize;
    }

    type RegisterHotKeyFn = unsafe extern "system" fn(isize, i32, u32, u32) -> i32;
    type UnregisterHotKeyFn = unsafe extern "system" fn(isize, i32) -> i32;
    type SetWindowLongPtrWFn = unsafe extern "system" fn(isize, i32, WndProc) -> isize;
    type CallWindowProcWFn = unsafe extern "system" fn(isize, isize, u32, usize, isize) -> isize;
    type GetForegroundWindowFn = unsafe extern "system" fn() -> isize;
    type SetForegroundWindowFn = unsafe extern "system" fn(isize) -> i32;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn user32<T>(name: &str) -> Option<T> {
        static USER32: OnceLock<usize> = OnceLock::new();
        let module = *USER32.get_or_init(|| unsafe { LoadLibraryW(wide("user32.dll").as_ptr()) });
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

    static OLD_PROC: AtomicUsize = AtomicUsize::new(0);
    static SENDER: OnceLock<Mutex<Option<std::sync::mpsc::Sender<crate::RefreshMsg>>>> =
        OnceLock::new();
    static APPLIED: Mutex<Option<(u32, u32)>> = Mutex::new(None);

    unsafe extern "system" fn subclass_proc(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        if msg == WM_HOTKEY
            && wparam as i32 == HOTKEY_ID
            && let Some(sender) = SENDER.get()
            && let Ok(guard) = sender.lock()
            && let Some(tx) = &*guard
        {
            let _ = tx.send(crate::RefreshMsg::ToggleWindow);
        }
        let old = OLD_PROC.load(Ordering::SeqCst);
        if old == 0 {
            return 0;
        }
        match user32::<CallWindowProcWFn>("CallWindowProcW") {
            Some(call) => unsafe { call(old as isize, hwnd, msg, wparam, lparam) },
            None => 0,
        }
    }

    /// Install the wndproc subclass once; later calls just retarget
    /// the sender.
    pub fn install(hwnd: isize, tx: std::sync::mpsc::Sender<crate::RefreshMsg>) {
        let slot = SENDER.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(tx);
        }
        if OLD_PROC.load(Ordering::SeqCst) != 0 {
            return;
        }
        if let Some(set_long) = user32::<SetWindowLongPtrWFn>("SetWindowLongPtrW") {
            let old = unsafe { set_long(hwnd, GWLP_WNDPROC, subclass_proc) };
            if old != 0 {
                OLD_PROC.store(old as usize, Ordering::SeqCst);
            }
        }
    }

    pub enum ApplyError {
        /// The combination is not usable as a global hotkey.
        Invalid,
        /// Another program owns the combination.
        Taken,
    }

    /// (Re)register from an accelerator string. Empty string turns
    /// the hotkey off. On error the previously registered hotkey
    /// (if any) stays registered.
    pub fn apply(hwnd: isize, accel: &str) -> Result<Option<(u32, u32)>, ApplyError> {
        let Some(register) = user32::<RegisterHotKeyFn>("RegisterHotKey") else {
            return Err(ApplyError::Invalid);
        };
        let Some(unregister) = user32::<UnregisterHotKeyFn>("UnregisterHotKey") else {
            return Err(ApplyError::Invalid);
        };
        let trimmed = accel.trim();
        if trimmed.is_empty() {
            unsafe {
                unregister(hwnd, HOTKEY_ID);
            }
            if let Ok(mut guard) = APPLIED.lock() {
                *guard = None;
            }
            return Ok(None);
        }
        let Some(spec) = blindrss_core::shortcuts::global_hotkey_spec(trimmed) else {
            return Err(ApplyError::Invalid);
        };
        // Register the new one first; only drop the old on success
        // is impossible (same id), so unregister then register and
        // roll back on failure.
        let previous = APPLIED.lock().ok().and_then(|g| *g);
        unsafe {
            unregister(hwnd, HOTKEY_ID);
        }
        if unsafe { register(hwnd, HOTKEY_ID, spec.0 | MOD_NOREPEAT, spec.1) } != 0 {
            if let Ok(mut guard) = APPLIED.lock() {
                *guard = Some(spec);
            }
            return Ok(Some(spec));
        }
        if let Some(prev) = previous {
            unsafe {
                register(hwnd, HOTKEY_ID, prev.0 | MOD_NOREPEAT, prev.1);
            }
        }
        Err(ApplyError::Taken)
    }

    pub fn foreground_is(hwnd: isize) -> bool {
        user32::<GetForegroundWindowFn>("GetForegroundWindow")
            .map(|f| unsafe { f() } == hwnd)
            .unwrap_or(false)
    }

    pub fn set_foreground(hwnd: isize) {
        if let Some(f) = user32::<SetForegroundWindowFn>("SetForegroundWindow") {
            unsafe {
                f(hwnd);
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use imp::{ApplyError, apply, foreground_is, install, set_foreground};

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub enum ApplyError {
    Invalid,
    Taken,
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn install(_hwnd: isize, _tx: std::sync::mpsc::Sender<crate::RefreshMsg>) {}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn apply(_hwnd: isize, _accel: &str) -> Result<Option<(u32, u32)>, ApplyError> {
    Ok(None)
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn foreground_is(_hwnd: isize) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn set_foreground(_hwnd: isize) {}
