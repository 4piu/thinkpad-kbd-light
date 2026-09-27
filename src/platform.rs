//! Windows glue: idle time, power source, and the system events that mean "the user is
//! back". See `notes/02-design-spec.md` §3.3 and §3.4.

use std::cell::RefCell;
use std::io;
use std::time::Duration;

use windows_sys::core::PCWSTR;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, POWERBROADCAST_SETTING,
    SYSTEM_POWER_STATUS,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
    RegisterClassExW, TranslateMessage, HWND_MESSAGE, MSG, PBT_APMRESUMEAUTOMATIC,
    PBT_APMRESUMESUSPEND, PBT_POWERSETTINGCHANGE, PM_REMOVE, WM_POWERBROADCAST,
    WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::settings::PowerSource;

// Not exposed by windows-sys under a stable path across versions; these are fixed.
const GUID_ACDC_POWER_SOURCE: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x5d3e9a59_e9d5_4b00_a6bd_ff34ff516548);
const GUID_CONSOLE_DISPLAY_STATE: windows_sys::core::GUID =
    windows_sys::core::GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);
const WTS_SESSION_LOCK: WPARAM = 0x7;
const WTS_SESSION_UNLOCK: WPARAM = 0x8;

/// `GUID` does not implement `PartialEq` in windows-sys, so compare the fields.
fn guid_eq(a: &windows_sys::core::GUID, b: &windows_sys::core::GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

/// Something happened that the state machine needs to know about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformEvent {
    /// Woke from sleep, or the display came back on.
    ///
    /// On this hardware (S0ix / Modern Standby) there is often no clean S3-style resume,
    /// so display-on is the more reliable "user is back" signal. Both are reported, and
    /// the handler is expected to be idempotent.
    Resume,
    /// The display was switched off. Treated as "user is gone".
    DisplayOff,
    PowerSourceChanged(PowerSource),
    SessionLock,
    SessionUnlock,
}

/// Idle time as reported by the OS.
///
/// Used in the all-device mode. Built-in-only mode instead consumes the device identity
/// and type from Raw Input headers; neither mode reads key codes or typed contents.
pub fn idle_duration() -> Duration {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info.cbSize` is set as the API requires.
    if unsafe { GetLastInputInfo(&mut info) } == 0 {
        return Duration::ZERO;
    }
    // Both are 32-bit tick counts; wrapping_sub keeps this correct across the ~49.7 day
    // rollover rather than producing an absurd idle time.
    let elapsed_ms = unsafe { GetTickCount() }.wrapping_sub(info.dwTime);
    Duration::from_millis(elapsed_ms as u64)
}

/// Whether we are on mains or battery right now.
pub fn power_source() -> PowerSource {
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: writes into a correctly sized, owned struct.
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return PowerSource::Ac; // conservative: assume plugged in
    }
    match status.ACLineStatus {
        0 => PowerSource::Battery,
        _ => PowerSource::Ac, // 1 = online, 255 = unknown
    }
}

thread_local! {
    /// Events collected by the window procedure, drained by [`EventPump::drain`].
    static PENDING: RefCell<Vec<PlatformEvent>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        0x00FF => {
            // WM_INPUT
            // SAFETY: lparam is the live raw-input handle for this callback.
            unsafe { crate::input::observe(lparam) };
            // Windows requires default handling for foreground raw-input cleanup.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        0x00FE => {
            // WM_INPUT_DEVICE_CHANGE
            crate::input::clear_device_cache();
            0
        }
        WM_POWERBROADCAST => {
            match wparam as u32 {
                PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => push(PlatformEvent::Resume),
                PBT_POWERSETTINGCHANGE => {
                    // SAFETY: for PBT_POWERSETTINGCHANGE, lparam is a POWERBROADCAST_SETTING.
                    let setting = unsafe { &*(lparam as *const POWERBROADCAST_SETTING) };
                    let data = setting.Data[0];
                    if guid_eq(&setting.PowerSetting, &GUID_ACDC_POWER_SOURCE) {
                        // 0 = AC, 1 = battery, 2 = short-term (UPS); treat UPS as battery.
                        push(PlatformEvent::PowerSourceChanged(if data == 0 {
                            PowerSource::Ac
                        } else {
                            PowerSource::Battery
                        }));
                    } else if guid_eq(&setting.PowerSetting, &GUID_CONSOLE_DISPLAY_STATE) {
                        // 0 = off, 1 = on, 2 = dimmed
                        push(if data == 0 {
                            PlatformEvent::DisplayOff
                        } else {
                            PlatformEvent::Resume
                        });
                    }
                }
                _ => {}
            }
            1
        }
        WM_WTSSESSION_CHANGE => {
            match wparam {
                WTS_SESSION_LOCK => push(PlatformEvent::SessionLock),
                WTS_SESSION_UNLOCK => push(PlatformEvent::SessionUnlock),
                _ => {}
            }
            0
        }
        // SAFETY: standard default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn push(ev: PlatformEvent) {
    PENDING.with(|p| p.borrow_mut().push(ev));
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A message-only window that receives power and session notifications.
///
/// Must be created and pumped on the same thread.
pub struct EventPump {
    hwnd: HWND,
}

impl EventPump {
    pub fn new() -> io::Result<Self> {
        let class_name = wide("TkblEventWindow");

        // SAFETY: standard window registration; all pointers outlive the calls.
        let hwnd = unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());

            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            // Ignore failure: a duplicate class registration is fine.
            RegisterClassExW(&wc);

            CreateWindowExW(
                0,
                class_name.as_ptr(),
                wide("tkbl").as_ptr() as PCWSTR,
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            )
        };

        if hwnd.is_null() {
            return Err(io::Error::last_os_error());
        }

        if let Err(e) = crate::input::register(hwnd) {
            // SAFETY: the newly created window is ours.
            unsafe { DestroyWindow(hwnd) };
            return Err(e);
        }

        // SAFETY: hwnd is valid. Failures are non-fatal: we degrade to polling only.
        unsafe {
            RegisterPowerSettingNotification(hwnd as _, &GUID_ACDC_POWER_SOURCE, 0);
            RegisterPowerSettingNotification(hwnd as _, &GUID_CONSOLE_DISPLAY_STATE, 0);
            WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        }

        Ok(Self { hwnd })
    }

    /// Take whatever the window procedure collected.
    ///
    /// Call [`pump_messages`] first — this only drains the queue, it does not pump.
    pub fn take_events(&self) -> Vec<PlatformEvent> {
        PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()))
    }
}

/// Opt in to system-DPI scaling before any window exists.
///
/// Without this, Windows bitmap-stretches our windows on a HiDPI panel and the settings
/// text comes out blurry. System-aware (rather than per-monitor) keeps the layout code
/// to a single scale factor, which is the right trade for one small fixed dialog.
///
/// Must be called before the first window is created; failure is not actionable.
pub fn set_dpi_aware() {
    // SAFETY: no preconditions; returns false on older Windows, which we ignore.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
    }
}

/// Dispatch all pending messages for this thread.
///
/// One call services every window owned by the thread, so the event window and the tray
/// window are both driven by this. Call it once per iteration, before draining any
/// window's event queue.
pub fn pump_messages() {
    // SAFETY: standard non-blocking message pump on the owning thread.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

impl Drop for EventPump {
    fn drop(&mut self) {
        crate::input::unregister();
        // SAFETY: hwnd was created by us and is destroyed once.
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}
