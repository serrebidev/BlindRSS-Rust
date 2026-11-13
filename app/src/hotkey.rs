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
    const WM_ACTIVATE: u32 = 0x0006;
    const WM_SYSCOMMAND: u32 = 0x0112;
    const SC_MINIMIZE: usize = 0xf020;
    const WM_HOTKEY: u32 = 0x0312;
    const GWLP_WNDPROC: i32 = -4;
    const MOD_NOREPEAT: u32 = 0x4000;

    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> usize;
        fn GetProcAddress(module: usize, name: *const u8) -> usize;
    }

    type RegisterHotKeyFn = unsafe extern "system" fn(isize, i32, u32, u32) -> i32;
    type UnregisterHotKeyFn = unsafe extern "system" fn(isize, i32) -> i32;
    type SetWindowLongPtrWFn = unsafe extern "system" fn(isize, i32, isize) -> isize;
    type CallWindowProcWFn = unsafe extern "system" fn(isize, isize, u32, usize, isize) -> isize;
    type DefWindowProcWFn = unsafe extern "system" fn(isize, u32, usize, isize) -> isize;
    type GetForegroundWindowFn = unsafe extern "system" fn() -> isize;
    type SetForegroundWindowFn = unsafe extern "system" fn(isize) -> i32;
    type GetFocusFn = unsafe extern "system" fn() -> isize;
    type SetFocusFn = unsafe extern "system" fn(isize) -> isize;
    type IsChildFn = unsafe extern "system" fn(isize, isize) -> i32;
    type WindowBoolFn = unsafe extern "system" fn(isize) -> i32;

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
            unsafe { Some(std::mem::transmute_copy(&addr)) }
        }
    }

    static OLD_PROC: AtomicUsize = AtomicUsize::new(0);
    static LAST_FOCUS: AtomicUsize = AtomicUsize::new(0);
    // Frame hwnd awaiting the deferred re-assertion of LAST_FOCUS.
    // wx can settle its default activation focus after this
    // subclass returns (async activation queues), clobbering the
    // immediate restore; the app drain re-asserts once, after
    // activation has settled.
    static PENDING_FRAME: AtomicUsize = AtomicUsize::new(0);
    static SENDER: OnceLock<Mutex<Option<std::sync::mpsc::Sender<crate::RefreshMsg>>>> =
        OnceLock::new();
    static APPLIED: Mutex<Option<(u32, u32)>> = Mutex::new(None);

    // WA_ACTIVE (low word 1) is "activated by anything other than a
    // mouse click" -- keyboard (Alt+Tab), programmatic raise. The
    // high word carries minimized state, not input source, so it
    // must not gate this: a window restored from minimized arrives
    // with the high word set and still needs its focus back. The
    // restore guard checks IsIconic itself.
    fn active_not_mouse(wparam: usize) -> bool {
        wparam & 0xffff == 1
    }

    fn remember_focus(hwnd: isize) {
        if let (Some(get), Some(child)) = (
            user32::<GetFocusFn>("GetFocus"),
            user32::<IsChildFn>("IsChild"),
        ) {
            let focused = unsafe { get() };
            if focused != 0 && unsafe { child(hwnd, focused) } != 0 {
                LAST_FOCUS.store(focused as usize, Ordering::SeqCst);
            }
        }
    }

    fn restore_activation_focus(hwnd: isize, focused: isize) {
        if focused == 0 || !foreground_is(hwnd) {
            return;
        }
        if let (Some(get), Some(set), Some(child), Some(visible), Some(enabled), Some(iconic)) = (
            user32::<GetFocusFn>("GetFocus"),
            user32::<SetFocusFn>("SetFocus"),
            user32::<IsChildFn>("IsChild"),
            user32::<WindowBoolFn>("IsWindowVisible"),
            user32::<WindowBoolFn>("IsWindowEnabled"),
            user32::<WindowBoolFn>("IsIconic"),
        ) {
            // Validate again after wx activation: a modal, layout change or
            // destroyed/hidden reader must not receive stale keyboard focus.
            unsafe {
                if enabled(hwnd) != 0
                    && iconic(hwnd) == 0
                    && child(hwnd, focused) != 0
                    && visible(focused) != 0
                    && enabled(focused) != 0
                    && get() != focused
                {
                    set(focused);
                }
            }
        }
    }

    unsafe extern "system" fn subclass_proc(
        hwnd: isize,
        msg: u32,
        wparam: usize,
        lparam: isize,
    ) -> isize {
        // Save the exact native descendant before wx deactivation can lose it.
        // Restore only non-mouse activation; mouse clicks choose their own target.
        if (msg == WM_ACTIVATE && wparam & 0xffff == 0)
            || (msg == WM_SYSCOMMAND && wparam & 0xfff0 == SC_MINIMIZE)
        {
            remember_focus(hwnd);
        }
        let restore_to = if msg == WM_ACTIVATE && active_not_mouse(wparam) {
            PENDING_FRAME.store(hwnd as usize, Ordering::SeqCst);
            LAST_FOCUS.load(Ordering::SeqCst) as isize
        } else {
            0
        };
        if msg == WM_HOTKEY
            && wparam as i32 == HOTKEY_ID
            && let Some(sender) = SENDER.get()
            && let Ok(guard) = sender.lock()
            && let Some(tx) = &*guard
        {
            let _ = tx.send(crate::RefreshMsg::ToggleWindow);
        }
        let old = OLD_PROC.load(Ordering::SeqCst);
        if old != 0
            && let Some(call) = user32::<CallWindowProcWFn>("CallWindowProcW")
        {
            let result = unsafe { call(old as isize, hwnd, msg, wparam, lparam) };
            if restore_to != 0 {
                restore_activation_focus(hwnd, restore_to);
            }
            return result;
        }
        // The original procedure was never captured (SetWindowLongPtrW
        // reported failure after installing us, or the export is
        // missing). Returning 0 here dropped every window message —
        // painting, sizing, keyboard, timers — leaving the main window
        // dead. Fall through to the default procedure instead.
        match user32::<DefWindowProcWFn>("DefWindowProcW") {
            Some(def) => unsafe { def(hwnd, msg, wparam, lparam) },
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
            let old = unsafe { set_long(hwnd, GWLP_WNDPROC, subclass_proc as *const () as isize) };
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

    /// Deferred half of the activation restore (see PENDING_FRAME):
    /// the app drain calls this within one tick of a keyboard-
    /// activation. Returns true when a frame was awaiting the
    /// re-assertion. The restore's own guards (foreground,
    /// visible, enabled, not minimized) keep this from stealing
    /// focus after a fast Alt+Tab away or a mouse click.
    pub fn consume_activation_restore() -> bool {
        let frame = PENDING_FRAME.swap(0, Ordering::SeqCst) as isize;
        if frame == 0 {
            return false;
        }
        restore_activation_focus(frame, LAST_FOCUS.load(Ordering::SeqCst) as isize);
        true
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

    #[cfg(test)]
    mod tests {
        use super::active_not_mouse;

        #[test]
        fn activation_preserves_mouse_targets_and_skips_minimized_windows() {
            // WA_ACTIVE (1) regardless of the minimized-state high
            // word restores; WA_CLICKACTIVE (2) and WA_INACTIVE (0)
            // never poke focus.
            assert!(active_not_mouse(1));
            assert!(active_not_mouse(1 | (1 << 16)));
            for message in [0, 2, 2 | (1 << 16), 0xffff & !(1 | (1 << 16))] {
                assert!(!active_not_mouse(message));
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use imp::{
    ApplyError, apply, consume_activation_restore, foreground_is, install, set_foreground,
};

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
pub fn consume_activation_restore() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn foreground_is(_hwnd: isize) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn set_foreground(_hwnd: isize) {}
