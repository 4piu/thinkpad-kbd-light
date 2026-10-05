//! The settings window: two columns, AC and Battery side by side.
//!
//! Controls are created in code rather than from a dialog resource, keeping the build to
//! plain `cargo build`. Laying the two profiles out side by side is deliberate — the
//! asymmetry between them is the whole point, and a dropdown would hide it.
//!
//! There is no "dim to" setting: with three levels, the only target not already
//! expressible through the two timers is the level below Active, so dimming always drops
//! one step. Controls that cannot apply are greyed out rather than silently ignored.

use std::cell::RefCell;
use std::io;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, FW_NORMAL, FW_SEMIBOLD, HFONT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, GetSystemMetrics,
    GetWindowTextW, MessageBoxW, PostMessageW, RegisterClassExW, SendMessageW, SetForegroundWindow,
    SetWindowTextW, ShowWindow, CW_USEDEFAULT, MB_ICONWARNING, MB_OK, SM_CXSCREEN, SM_CYSCREEN,
    SW_RESTORE, SW_SHOW, WM_CLOSE, WM_COMMAND, WM_DESTROY, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

use crate::backlight::Level;
use crate::settings::{Location, Profile, Settings};

// Stable Win32 constants, defined here rather than chased across windows-sys features.
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const BS_GROUPBOX: u32 = 0x0007;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const ES_NUMBER: u32 = 0x2000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const WM_SETFONT: u32 = 0x0030;

const ID_SAVE: u32 = 1;
const ID_CANCEL: u32 = 2;

/// Fixed-size dialog: no resize or maximise, since the layout is hand-placed.
const STYLE: u32 = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;

/// What the user did with the window.
#[derive(Debug, Clone)]
pub enum Outcome {
    Saved(Box<Settings>),
    Cancelled,
}

#[derive(Clone, Copy)]
struct ProfileControls {
    enabled: HWND,
    active: HWND,
    dim_on: HWND,
    dim_secs: HWND,
    off_on: HWND,
    off_secs: HWND,
}

struct Controls {
    ac: ProfileControls,
    battery: ProfileControls,
    turn_off_when_locked: HWND,
    restore_on_exit: HWND,
    suppress_osd: HWND,
    sunset_only: HWND,
    internal_input_only: HWND,
    sunset_location: Option<Location>,
    font: HFONT,
}

thread_local! {
    static CONTROLS: RefCell<Option<Controls>> = const { RefCell::new(None) };
    static OUTCOME: RefCell<Option<Outcome>> = const { RefCell::new(None) };
    static OPEN: RefCell<bool> = const { RefCell::new(false) };
    /// Handle of the live window, so a second request can raise it instead of ignoring.
    static WINDOW: RefCell<isize> = const { RefCell::new(0) };
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Scale a logical coordinate for the system DPI.
fn scale(v: i32) -> i32 {
    // SAFETY: no preconditions.
    let dpi = unsafe { GetDpiForSystem() } as i32;
    v * dpi / 96
}

fn checked(hwnd: HWND) -> bool {
    // SAFETY: hwnd is a checkbox we created.
    unsafe { SendMessageW(hwnd, BM_GETCHECK, 0, 0) == 1 }
}

fn set_checked(hwnd: HWND, on: bool) {
    // SAFETY: hwnd is a checkbox we created.
    unsafe { SendMessageW(hwnd, BM_SETCHECK, if on { 1 } else { 0 }, 0) };
}

fn combo_index(hwnd: HWND) -> i32 {
    // SAFETY: hwnd is a combo box we created.
    unsafe { SendMessageW(hwnd, CB_GETCURSEL, 0, 0) as i32 }
}

fn read_u32(hwnd: HWND) -> Option<u32> {
    let mut buf = [0u16; 32];
    // SAFETY: buffer length is passed correctly.
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return None;
    }
    String::from_utf16_lossy(&buf[..n as usize])
        .trim()
        .parse()
        .ok()
}

fn level_from_index(i: i32) -> Level {
    match i {
        1 => Level::Low,
        2 => Level::High,
        _ => Level::Off,
    }
}

/// Control geometry in logical (96 dpi) units.
#[derive(Clone, Copy)]
struct R {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

struct Builder {
    parent: HWND,
    font: HFONT,
    hinstance: windows_sys::Win32::Foundation::HMODULE,
}

impl Builder {
    fn make(&self, class: &str, text: &str, style: u32, r: R, id: u32) -> HWND {
        // SAFETY: strings are NUL-terminated; parent is valid.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                scale(r.x),
                scale(r.y),
                scale(r.w),
                scale(r.h),
                self.parent,
                id as isize as _,
                self.hinstance,
                std::ptr::null(),
            )
        };
        // SAFETY: applying our font to a control we just made.
        unsafe { SendMessageW(hwnd, WM_SETFONT, self.font as usize, 1) };
        hwnd
    }

    fn label(&self, text: &str, x: i32, y: i32, w: i32) -> HWND {
        self.make("STATIC", text, 0, R { x, y, w, h: 18 }, 0)
    }

    fn checkbox(&self, text: &str, x: i32, y: i32, w: i32) -> HWND {
        self.make(
            "BUTTON",
            text,
            BS_AUTOCHECKBOX | WS_TABSTOP,
            R { x, y, w, h: 20 },
            0,
        )
    }

    fn combo(&self, x: i32, y: i32, w: i32, items: &[&str]) -> HWND {
        let h = self.make(
            "COMBOBOX",
            "",
            CBS_DROPDOWNLIST | WS_TABSTOP | WS_VSCROLL,
            // Height covers the dropped list, not just the closed control.
            R { x, y, w, h: 120 },
            0,
        );
        for item in items {
            // SAFETY: item text is NUL-terminated and outlives the call.
            unsafe { SendMessageW(h, CB_ADDSTRING, 0, wide(item).as_ptr() as isize) };
        }
        h
    }

    fn edit(&self, x: i32, y: i32, w: i32) -> HWND {
        self.make(
            "EDIT",
            "",
            ES_NUMBER | WS_BORDER | WS_TABSTOP,
            R { x, y, w, h: 22 },
            0,
        )
    }
}

fn build_profile_column(b: &Builder, x: i32, p: &Profile) -> ProfileControls {
    let levels = ["Off", "Low", "High"];
    let c = ProfileControls {
        enabled: b.checkbox("", x, 52, 24),
        active: b.combo(x, 80, 90, &levels),
        dim_on: b.checkbox("", x, 112, 24),
        dim_secs: b.edit(x + 26, 110, 62),
        off_on: b.checkbox("", x, 142, 24),
        off_secs: b.edit(x + 26, 140, 62),
    };

    set_checked(c.enabled, p.enabled);
    // SAFETY: combo handle is ours.
    unsafe {
        SendMessageW(c.active, CB_SETCURSEL, p.active_level as usize, 0);
    }
    set_checked(c.dim_on, p.dim_after_secs.is_some());
    set_checked(c.off_on, p.off_after_secs.is_some());
    // SAFETY: edit handles are ours; strings are NUL-terminated.
    unsafe {
        SetWindowTextW(
            c.dim_secs,
            wide(&p.dim_after_secs.unwrap_or(30).to_string()).as_ptr(),
        );
        SetWindowTextW(
            c.off_secs,
            wide(&p.off_after_secs.unwrap_or(300).to_string()).as_ptr(),
        );
    }
    c
}

/// Grey out anything that cannot apply, so an inert setting is visible rather than
/// silently ignored by the engine.
fn refresh_enabled() {
    CONTROLS.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            refresh_column(&c.ac);
            refresh_column(&c.battery);
        }
    });
}

fn refresh_column(c: &ProfileControls) {
    let on = checked(c.enabled);
    let active = level_from_index(combo_index(c.active));
    // Dimming needs a level below the active one to drop to, which only High has.
    let can_dim = on && active == Level::High;
    // Turning off only means anything if the light is on to begin with.
    let can_off = on && active != Level::Off;

    enable(c.active, on);
    enable(c.dim_on, can_dim);
    enable(c.dim_secs, can_dim && checked(c.dim_on));
    enable(c.off_on, can_off);
    enable(c.off_secs, can_off && checked(c.off_on));
}

fn enable(hwnd: HWND, on: bool) {
    // SAFETY: hwnd is a control we created.
    unsafe { EnableWindow(hwnd, i32::from(on)) };
}

fn read_profile(c: &ProfileControls) -> Profile {
    Profile {
        enabled: checked(c.enabled),
        active_level: level_from_index(combo_index(c.active)),
        dim_after_secs: if checked(c.dim_on) {
            Some(read_u32(c.dim_secs).unwrap_or(30))
        } else {
            None
        },
        off_after_secs: if checked(c.off_on) {
            Some(read_u32(c.off_secs).unwrap_or(300))
        } else {
            None
        },
    }
}

fn warn(hwnd: HWND, text: &str) {
    // SAFETY: strings are NUL-terminated.
    unsafe {
        MessageBoxW(
            hwnd,
            wide(text).as_ptr(),
            wide("Check the settings").as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}

unsafe extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            // BN_CLICKED (0) and CBN_SELCHANGE (1) can both change what is applicable.
            if ((wp >> 16) & 0xffff) <= 1 {
                refresh_enabled();
            }
            match (wp & 0xffff) as u32 {
                ID_SAVE => {
                    let built = CONTROLS.with(|c| {
                        c.borrow().as_ref().map(|c| Settings {
                            version: crate::settings::CURRENT_VERSION,
                            ac: read_profile(&c.ac),
                            battery: read_profile(&c.battery),
                            turn_off_when_locked: checked(c.turn_off_when_locked),
                            restore_on_exit: checked(c.restore_on_exit),
                            suppress_osd: checked(c.suppress_osd),
                            sunset_only: checked(c.sunset_only),
                            sunset_location: c.sunset_location,
                            internal_input_only: checked(c.internal_input_only),
                            // Owned by the tray menu, not this window; the caller merges
                            // the live value back in.
                            start_with_windows: false,
                        })
                    });
                    if let Some(settings) = built {
                        let mut settings = settings;
                        for (name, p) in [("AC", &settings.ac), ("Battery", &settings.battery)] {
                            if let Err(e) = p.validate() {
                                warn(hwnd, &format!("{name}: {e}"));
                                return 0;
                            }
                        }
                        if settings.sunset_only {
                            match crate::location::from_windows_settings() {
                                Ok(location) => settings.sunset_location = Some(location),
                                Err(e) => {
                                    warn(hwnd, &e);
                                    return 0;
                                }
                            }
                        } else {
                            settings.sunset_location = None;
                        }
                        OUTCOME
                            .with(|o| *o.borrow_mut() = Some(Outcome::Saved(Box::new(settings))));
                        // SAFETY: destroying the window we own.
                        unsafe { DestroyWindow(hwnd) };
                    }
                }
                ID_CANCEL => {
                    // SAFETY: posting to our own window.
                    unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
                }
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            OUTCOME.with(|o| {
                let mut o = o.borrow_mut();
                if o.is_none() {
                    *o = Some(Outcome::Cancelled);
                }
            });
            // SAFETY: destroying the window we own.
            unsafe { DestroyWindow(hwnd) };
            0
        }
        WM_DESTROY => {
            CONTROLS.with(|c| {
                if let Some(c) = c.borrow_mut().take() {
                    // SAFETY: the font is ours and no control needs it now.
                    unsafe { DeleteObject(c.font as _) };
                }
            });
            OPEN.with(|o| *o.borrow_mut() = false);
            WINDOW.with(|w| *w.borrow_mut() = 0);
            0
        }
        // SAFETY: standard default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// True if the window is already on screen.
pub fn is_open() -> bool {
    OPEN.with(|o| *o.borrow())
}

/// Take the user's decision, if they have made one.
pub fn take_outcome() -> Option<Outcome> {
    OUTCOME.with(|o| o.borrow_mut().take())
}

/// Show the settings window, raising the existing one if it is already up.
pub fn open(settings: &Settings) -> io::Result<()> {
    if is_open() {
        // Clicking the tray icon again should bring the window forward, not do nothing.
        WINDOW.with(|w| {
            let hwnd = *w.borrow() as HWND;
            if !hwnd.is_null() {
                // SAFETY: the handle is live while OPEN is set; cleared on WM_DESTROY.
                unsafe {
                    ShowWindow(hwnd, SW_RESTORE);
                    SetForegroundWindow(hwnd);
                }
            }
        });
        return Ok(());
    }
    OUTCOME.with(|o| *o.borrow_mut() = None);

    let class_name = wide("TkblSettingsWindow");
    let (w, h) = (470, 406);

    // SAFETY: standard window creation; all strings outlive the calls.
    let hwnd = unsafe {
        let hinstance = GetModuleHandleW(std::ptr::null());
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(proc_),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: std::ptr::null_mut(),
            // COLOR_BTNFACE + 1, so the window matches dialog chrome.
            hbrBackground: 16 as _,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        RegisterClassExW(&wc);

        // Controls are positioned in client coordinates, so grow the window by the title
        // bar and borders. Without this the bottom row is clipped off the window entirely.
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: scale(w),
            bottom: scale(h),
        };
        AdjustWindowRect(&mut rect, STYLE, 0);
        let (ww, wh) = (rect.right - rect.left, rect.bottom - rect.top);

        // Centre on the primary display.
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let x = if sw > ww {
            (sw - ww) / 2
        } else {
            CW_USEDEFAULT
        };
        let y = if sh > wh {
            (sh - wh) / 2
        } else {
            CW_USEDEFAULT
        };

        CreateWindowExW(
            0,
            class_name.as_ptr(),
            wide("ThinkPad Keyboard Backlight").as_ptr(),
            STYLE,
            x,
            y,
            ww,
            wh,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: standard font creation with a NUL-terminated face name.
    let font = unsafe {
        CreateFontW(
            -scale(12),
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            wide("Segoe UI").as_ptr(),
        )
    };
    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let b = Builder {
        parent: hwnd,
        font,
        hinstance,
    };

    // Column headers.
    let (col_ac, col_bat) = (190, 320);
    b.label("Profile", 20, 22, 90);
    let head_ac = b.label("AC power", col_ac, 22, 110);
    let head_bat = b.label("Battery", col_bat, 22, 110);
    // SAFETY: applying a heavier font to the two headers.
    unsafe {
        let bold = CreateFontW(
            -scale(12),
            0,
            0,
            0,
            FW_SEMIBOLD as i32,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            wide("Segoe UI").as_ptr(),
        );
        SendMessageW(head_ac, WM_SETFONT, bold as usize, 1);
        SendMessageW(head_bat, WM_SETFONT, bold as usize, 1);
    }

    b.label("Enabled", 20, 54, 150);
    b.label("Active level", 20, 84, 150);
    b.label("Dim after (s)", 20, 114, 150);
    b.label("Off after (s)", 20, 144, 150);

    let ac = build_profile_column(&b, col_ac, &settings.ac);
    let battery = build_profile_column(&b, col_bat, &settings.battery);

    b.make(
        "BUTTON",
        "",
        BS_GROUPBOX,
        R {
            x: 16,
            y: 176,
            w: 434,
            h: 152,
        },
        0,
    );
    let turn_off_when_locked = b.checkbox("Turn the backlight off while locked", 30, 192, 300);
    let restore_on_exit = b.checkbox("Restore the light when exiting", 30, 218, 300);
    let suppress_osd = b.checkbox("Hide Lenovo's popup for automatic changes", 30, 244, 340);
    let sunset_only = b.checkbox(
        "Only light after sunset (Windows Location on Save)",
        30,
        270,
        400,
    );
    set_checked(turn_off_when_locked, settings.turn_off_when_locked);
    set_checked(restore_on_exit, settings.restore_on_exit);
    set_checked(suppress_osd, settings.suppress_osd);
    set_checked(sunset_only, settings.sunset_only);
    let internal_input_only = b.checkbox(
        "Only built-in keyboard / touchpad / TrackPoint wake light",
        30,
        296,
        410,
    );
    set_checked(internal_input_only, settings.internal_input_only);

    b.label(
        "Three hardware levels; dimming drops to the one below Active.",
        20,
        341,
        420,
    );

    let button = |x| R {
        x,
        y: 366,
        w: 85,
        h: 26,
    };
    b.make(
        "BUTTON",
        "Save",
        BS_DEFPUSHBUTTON | WS_TABSTOP,
        button(268),
        ID_SAVE,
    );
    b.make("BUTTON", "Cancel", WS_TABSTOP, button(363), ID_CANCEL);

    CONTROLS.with(|c| {
        *c.borrow_mut() = Some(Controls {
            ac,
            battery,
            turn_off_when_locked,
            restore_on_exit,
            suppress_osd,
            sunset_only,
            internal_input_only,
            sunset_location: settings.sunset_location,
            font,
        })
    });
    OPEN.with(|o| *o.borrow_mut() = true);
    WINDOW.with(|w| *w.borrow_mut() = hwnd as isize);
    refresh_enabled();

    // SAFETY: showing the window we just created.
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
    Ok(())
}
