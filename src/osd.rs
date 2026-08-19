//! Suppressing Lenovo's on-screen display for changes *we* make.
//!
//! `tposd.exe` shows a popup whenever the keyboard backlight level changes. That is useful
//! feedback when the user presses Fn+Space, and noise when this program dims the keyboard
//! on a timer.
//!
//! There is no supported way to turn it off: the `MLCS` ACPI method raises a notification
//! on both of its branches (the `0x00010000` argument bit picks between `MHKQ` and `MHKK`,
//! not whether to notify), and the only registry values under `Software\Lenovo\
//! OnScreenDisplay` are state rather than preferences.
//!
//! So instead we hide the popup, and only the one our own write caused: take a snapshot of
//! `tposd`'s visible windows immediately before writing, then hide anything that appears
//! afterwards. Fn+Space still shows its OSD, because for that we never write.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, ShowWindow, SW_HIDE,
};

/// How long after a write to keep watching for the popup.
const WINDOW: Duration = Duration::from_millis(600);
/// How often to re-check which process is `tposd.exe`; it can be restarted.
const PID_TTL: Duration = Duration::from_secs(30);

struct Collect {
    pid: u32,
    out: Vec<HWND>,
}

unsafe extern "system" fn collect_visible(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: lparam is the &mut Collect handed to EnumWindows below.
    let ctx = unsafe { &mut *(lparam as *mut Collect) };
    let mut pid = 0u32;
    // SAFETY: hwnd comes from the enumerator; pid is a valid out-param.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == ctx.pid && IsWindowVisible(hwnd) != 0 {
            ctx.out.push(hwnd);
        }
    }
    1
}

fn visible_windows_of(pid: u32) -> Vec<HWND> {
    let mut ctx = Collect {
        pid,
        out: Vec::new(),
    };
    // SAFETY: the callback only touches `ctx`, which outlives the call.
    unsafe {
        EnumWindows(Some(collect_visible), &mut ctx as *mut Collect as LPARAM);
    }
    ctx.out
}

fn find_tposd() -> Option<u32> {
    // SAFETY: standard ToolHelp walk; the snapshot is closed on every path.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let end = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
                if name.eq_ignore_ascii_case("tposd.exe") {
                    found = Some(entry.th32ProcessID);
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

/// Hides the OSD popup caused by our own backlight writes.
#[derive(Default)]
pub struct OsdSuppressor {
    pid: Option<u32>,
    pid_checked: Option<Instant>,
    /// Windows already on screen when we armed; these are not ours to hide.
    before: Vec<HWND>,
    until: Option<Instant>,
}

impl OsdSuppressor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call immediately *before* writing a level.
    pub fn arm(&mut self, now: Instant) {
        let stale = self
            .pid_checked
            .is_none_or(|t| now.duration_since(t) >= PID_TTL);
        if stale || self.pid.is_none() {
            self.pid = find_tposd();
            self.pid_checked = Some(now);
        }
        let Some(pid) = self.pid else { return };
        self.before = visible_windows_of(pid);
        self.until = Some(now + WINDOW);
    }

    /// Hide any popup that has appeared since [`arm`]. Cheap when not armed.
    ///
    /// Returns true once it has hidden something, so the caller can stop early.
    pub fn poll(&mut self, now: Instant) -> bool {
        let Some(until) = self.until else {
            return false;
        };
        if now >= until {
            self.until = None;
            return false;
        }
        let Some(pid) = self.pid else {
            self.until = None;
            return false;
        };

        let mut hid = false;
        for hwnd in visible_windows_of(pid) {
            if !self.before.contains(&hwnd) {
                // SAFETY: hiding a top-level window owned by the same user session.
                unsafe { ShowWindow(hwnd, SW_HIDE) };
                hid = true;
            }
        }
        if hid {
            self.until = None;
        }
        hid
    }

    pub fn disarm(&mut self) {
        self.until = None;
    }
}
