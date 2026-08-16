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
/// Uses `GetLastInputInfo`, which reports *that* input happened without exposing what was
/// typed. Deliberate: this process must never be able to read keystrokes. See
/// `notes/02-design-spec.md` §3.3.
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

        // SAFETY: hwnd is valid. Failures are non-fatal: we degrade to polling only.
        unsafe {
            RegisterPowerSettingNotification(hwnd as _, &GUID_ACDC_POWER_SOURCE, 0);
            RegisterPowerSettingNotification(hwnd as _, &GUID_CONSOLE_DISPLAY_STATE, 0);
            WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        }

        Ok(Self { hwnd })
    }

    /// Pump pending messages and return whatever the window procedure collected.
    pub fn drain(&self) -> Vec<PlatformEvent> {
        // SAFETY: standard non-blocking message pump on the owning thread.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()))
    }
}

impl Drop for EventPump {
    fn drop(&mut self) {
        // SAFETY: hwnd was created by us and is destroyed once.
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}
