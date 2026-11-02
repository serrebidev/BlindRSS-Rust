//! HWND name annotations preserve native MSAA roles, states, values and children.
use wxdragon::prelude::*;

pub fn set_name(control: &(impl WxWidget + WxEvtHandler), name: &str) {
    control.set_name(name);
    #[cfg(windows)]
    {
        let hwnd = control.get_handle() as usize;
        if imp::set(hwnd, Some(name)) {
            control.bind_internal(EventType::DESTROY, move |event| {
                imp::set(hwnd, None);
                event.skip(true);
            });
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::ptr;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid(u32, u16, u16, [u8; 8]);

    const CLASS: Guid = Guid(
        0xb5f8350b,
        0x0548,
        0x48b1,
        [0xa6, 0xee, 0x88, 0xbd, 0, 0xb4, 0xa5, 0xe7],
    );
    const INTERFACE: Guid = Guid(
        0x6e26e776,
        0x04f0,
        0x495d,
        [0x80, 0xe4, 0x33, 0x30, 0x35, 0x2e, 0x31, 0x69],
    );
    const NAME: Guid = Guid(
        0x608d3df8,
        0x8128,
        0x4aa7,
        [0xa4, 0x28, 0xf5, 0x5e, 0x49, 0x26, 0x72, 0x91],
    );
    const CLIENT: u32 = (-4_i32) as u32;

    #[repr(C)]
    struct Services {
        vtable: *const Vtable,
    }

    // IUnknown then IAccPropServices, in the order declared in oleacc.h.
    #[repr(C)]
    struct Vtable {
        unknown: [usize; 2],
        release: unsafe extern "system" fn(*mut Services) -> u32,
        before_set: [usize; 4],
        set: unsafe extern "system" fn(*mut Services, usize, u32, u32, Guid, *const u16) -> i32,
        server: usize,
        clear: unsafe extern "system" fn(*mut Services, usize, u32, u32, *const Guid, i32) -> i32,
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, flags: u32) -> i32;
        fn CoUninitialize();
        fn CoCreateInstance(
            class: *const Guid,
            outer: *mut c_void,
            context: u32,
            iid: *const Guid,
            result: *mut *mut Services,
        ) -> i32;
    }

    pub(super) fn set(hwnd: usize, name: Option<&str>) -> bool {
        if hwnd == 0 {
            return false;
        }
        let wide = name.map(|s| s.encode_utf16().chain(Some(0)).collect::<Vec<_>>());
        // Calls remain on the GUI thread; no MSAA callback enters wxWidgets.
        unsafe {
            let initialized = CoInitializeEx(ptr::null_mut(), 2);
            // RPC_E_CHANGED_MODE means wx already initialized another apartment.
            if initialized < 0 && initialized != -2147417850 {
                return false;
            }
            let mut services = ptr::null_mut();
            let created = CoCreateInstance(&CLASS, ptr::null_mut(), 1, &INTERFACE, &mut services);
            let success = if created >= 0 && !services.is_null() {
                let methods = &*(*services).vtable;
                let result = match wide {
                    Some(ref text) => (methods.set)(services, hwnd, CLIENT, 0, NAME, text.as_ptr()),
                    None => (methods.clear)(services, hwnd, CLIENT, 0, &NAME, 1),
                };
                (methods.release)(services);
                result >= 0
            } else {
                false
            };
            if initialized >= 0 {
                CoUninitialize();
            }
            success
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[link(name = "user32")]
        unsafe extern "system" {
            fn CreateWindowExW(
                ex: u32,
                class: *const u16,
                title: *const u16,
                style: u32,
                x: i32,
                y: i32,
                width: i32,
                height: i32,
                parent: usize,
                menu: usize,
                instance: usize,
                param: *mut c_void,
            ) -> usize;
            fn DestroyWindow(hwnd: usize) -> i32;
        }
        #[link(name = "oleacc")]
        unsafe extern "system" {
            fn AccessibleObjectFromWindow(
                hwnd: usize,
                object: u32,
                iid: *const Guid,
                result: *mut *mut *const usize,
            ) -> i32;
        }
        #[link(name = "oleaut32")]
        unsafe extern "system" {
            fn SysStringLen(text: *const u16) -> u32;
            fn SysFreeString(text: *mut u16);
        }

        // VARIANT's payload includes a two-pointer record on 32- and 64-bit Windows.
        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        struct Variant {
            kind: u16,
            reserved: [u16; 3],
            data: [usize; 2],
        }

        unsafe fn snapshot(hwnd: usize) -> (Option<String>, u32, u32, Option<String>) {
            let iid = Guid(
                0x618736e0,
                0x3c3d,
                0x11cf,
                [0x81, 0x0c, 0, 0xaa, 0, 0x38, 0x9b, 0x71],
            );
            let mut object = ptr::null_mut();
            unsafe {
                assert_eq!(
                    AccessibleObjectFromWindow(hwnd, CLIENT, &iid, &mut object),
                    0
                );
                let table = *object;
                let child = Variant {
                    kind: 3,
                    ..Variant::default()
                };
                let text = |slot| {
                    let get: unsafe extern "system" fn(
                        *mut *const usize,
                        Variant,
                        *mut *mut u16,
                    ) -> i32 = std::mem::transmute(*table.add(slot));
                    let mut value = ptr::null_mut();
                    get(object, child, &mut value);
                    if value.is_null() {
                        return None;
                    }
                    let text = String::from_utf16_lossy(std::slice::from_raw_parts(
                        value,
                        SysStringLen(value) as usize,
                    ));
                    SysFreeString(value);
                    Some(text)
                };
                let number = |slot| {
                    let get: unsafe extern "system" fn(
                        *mut *const usize,
                        Variant,
                        *mut Variant,
                    ) -> i32 = std::mem::transmute(*table.add(slot));
                    let mut value = Variant::default();
                    assert_eq!(get(object, child, &mut value), 0);
                    assert_eq!(value.kind, 3);
                    value.data[0] as u32
                };
                let result = (text(10), number(13), number(14), text(11));
                let release: unsafe extern "system" fn(*mut *const usize) -> u32 =
                    std::mem::transmute(*table.add(2));
                release(object);
                result
            }
        }

        #[test]
        fn hidden_native_edit_keeps_role_state_and_value_when_named() {
            unsafe {
                assert!(CoInitializeEx(ptr::null_mut(), 2) >= 0);
                let class: Vec<u16> = "EDIT\0".encode_utf16().collect();
                let body: Vec<u16> = "Body\0".encode_utf16().collect();
                let hwnd = CreateWindowExW(
                    0,
                    class.as_ptr(),
                    body.as_ptr(),
                    0x0800,
                    0,
                    0,
                    100,
                    30,
                    0,
                    0,
                    0,
                    ptr::null_mut(),
                );
                assert_ne!(hwnd, 0);
                let before = snapshot(hwnd);
                assert!(set(hwnd, Some("Article text – 読む")));
                let after = snapshot(hwnd);
                assert_eq!(after.0.as_deref(), Some("Article text – 読む"));
                assert_eq!(
                    (after.1, after.2, after.3),
                    (before.1, before.2, before.3.clone())
                );
                assert!(set(hwnd, None));
                assert_eq!(snapshot(hwnd), before);
                DestroyWindow(hwnd);
                CoUninitialize();
            }
        }

        #[test]
        fn annotation_abi_matches_oleacc_and_null_window_is_ignored() {
            assert_eq!(size_of::<Guid>(), 16);
            assert_eq!(
                std::mem::offset_of!(Vtable, release),
                2 * size_of::<usize>()
            );
            assert_eq!(std::mem::offset_of!(Vtable, set), 7 * size_of::<usize>());
            assert_eq!(std::mem::offset_of!(Vtable, clear), 9 * size_of::<usize>());
            assert!(!set(0, Some("Article text")));
        }
    }
}
