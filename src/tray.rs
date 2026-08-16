//! Notification-area icon and context menu.
//!
//! Icons are drawn at runtime from pixel buffers rather than embedded as resources: it
//! keeps the build to plain `cargo build` with no resource compiler, and the icon is a
//! brightness indicator that is easier to express in code than in three `.ico` files.

use std::cell::RefCell;
use std::io;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, GetCursorPos, PostMessageW, RegisterClassExW, SetForegroundWindow,
    TrackPopupMenu, HWND_MESSAGE, ICONINFO, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    TPM_BOTTOMALIGN, TPM_RIGHTALIGN, WM_APP, WM_COMMAND, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::backlight::Level;

/// Private message the shell sends us for icon activity.
const WM_TRAY: u32 = WM_APP + 1;

pub const ID_ON: u32 = 1001;
pub const ID_DIM: u32 = 1002;
pub const ID_OFF: u32 = 1003;
pub const ID_AUTO: u32 = 1010;
pub const ID_SETTINGS: u32 = 1020;
pub const ID_STARTUP: u32 = 1021;
pub const ID_EXIT: u32 = 1030;

/// Something the user did to the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// Left click: toggle between off and the profile's active level.
    Toggle,
    MenuCommand(u32),
}

thread_local! {
    static TRAY_EVENTS: RefCell<Vec<TrayEvent>> = const { RefCell::new(Vec::new()) };
    /// Menu state, read by the window procedure when it builds the popup.
    static MENU_STATE: RefCell<MenuState> = const {
        RefCell::new(MenuState {
            level: None,
            paused: false,
            on_ac: true,
            start_with_windows: false,
        })
    };
}

#[derive(Debug, Clone, Copy)]
struct MenuState {
    level: Option<Level>,
    paused: bool,
    on_ac: bool,
    start_with_windows: bool,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Build a 32x32 icon whose brightness reflects `level`.
///
/// A mid-grey keyboard outline keeps the icon legible on both light and dark taskbars,
/// while the interior glow carries the actual state.
fn make_icon(level: Option<Level>) -> Option<windows_sys::Win32::UI::WindowsAndMessaging::HICON> {
    const N: usize = 32;
    // BGRA, premultiplied not required for icons with an all-zero mask.
    let mut px = vec![0u8; N * N * 4];

    let (glow, alpha): ((u8, u8, u8), u8) = match level {
        Some(Level::High) => ((120, 200, 255), 255),
        Some(Level::Low) => ((70, 120, 160), 220),
        Some(Level::Off) | None => ((0, 0, 0), 0),
    };
    let outline = (170u8, 170u8, 170u8);

    // Keyboard body: rounded rectangle from (4,9) to (27,23).
    let (x0, y0, x1, y1) = (4usize, 9usize, 27usize, 23usize);
    for y in y0..=y1 {
        for x in x0..=x1 {
            // Knock out the corners to fake a radius.
            let corner = (x == x0 || x == x1) && (y == y0 || y == y1);
            if corner {
                continue;
            }
            let edge = x == x0 || x == x1 || y == y0 || y == y1;
            let i = (y * N + x) * 4;
            if edge {
                px[i] = outline.2;
                px[i + 1] = outline.1;
                px[i + 2] = outline.0;
                px[i + 3] = 255;
            } else if alpha > 0 {
                px[i] = glow.2;
                px[i + 1] = glow.1;
                px[i + 2] = glow.0;
                px[i + 3] = alpha;
            }
        }
    }

    // Three light rays above the keyboard, lit according to level.
    let lit = match level {
        Some(Level::High) => 3,
        Some(Level::Low) => 1,
        _ => 0,
    };
    for (n, x) in [9usize, 15, 21].iter().enumerate() {
        if n >= lit {
            continue;
        }
        for y in 3usize..8 {
            for dx in 0..2usize {
                let i = (y * N + x + dx) * 4;
                px[i] = glow.2;
                px[i + 1] = glow.1;
                px[i + 2] = glow.0;
                px[i + 3] = 255;
            }
        }
    }

    // SAFETY: buffers are correctly sized for the bitmap dimensions declared.
    unsafe {
        let color = CreateBitmap(N as i32, N as i32, 1, 32, px.as_ptr().cast());
        if color.is_null() {
            return None;
        }
        let mask_bits = [0u8; N * N / 8];
        let mask = CreateBitmap(N as i32, N as i32, 1, 1, mask_bits.as_ptr().cast());

        let info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info);
        DeleteObject(color as _);
        DeleteObject(mask as _);
        if icon.is_null() {
            None
        } else {
            Some(icon)
        }
    }
}

unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => TRAY_EVENTS.with(|e| e.borrow_mut().push(TrayEvent::Toggle)),
                WM_RBUTTONUP => unsafe { show_menu(hwnd) },
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            let id = (wparam & 0xffff) as u32;
            TRAY_EVENTS.with(|e| e.borrow_mut().push(TrayEvent::MenuCommand(id)));
            0
        }
        // SAFETY: standard default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let state = MENU_STATE.with(|s| *s.borrow());

    // SAFETY: menu handles are created and destroyed within this function.
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }

        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };

        AppendMenuW(
            menu,
            MF_STRING | check(state.level == Some(Level::High)),
            ID_ON as usize,
            wide("&On (high)").as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | check(state.level == Some(Level::Low)),
            ID_DIM as usize,
            wide("&Dim (low)").as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | check(state.level == Some(Level::Off)),
            ID_OFF as usize,
            wide("O&ff").as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

        let profile = if state.on_ac {
            "Profile: AC"
        } else {
            "Profile: Battery"
        };
        // Informational only: id 0 is not reported as a command.
        AppendMenuW(menu, MF_STRING, 0, wide(profile).as_ptr());
        AppendMenuW(
            menu,
            MF_STRING | check(state.paused),
            ID_AUTO as usize,
            wide("&Pause automatic control").as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(
            menu,
            MF_STRING,
            ID_SETTINGS as usize,
            wide("&Settings\u{2026}").as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING | check(state.start_with_windows),
            ID_STARTUP as usize,
            wide("Start with &Windows").as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, ID_EXIT as usize, wide("E&xit").as_ptr());

        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        // Required so the menu dismisses when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        TrackPopupMenu(
            menu,
            TPM_RIGHTALIGN | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
    }
}

/// The notification-area icon.
pub struct Tray {
    hwnd: HWND,
    icon: Option<windows_sys::Win32::UI::WindowsAndMessaging::HICON>,
    shown_level: Option<Option<Level>>,
}

impl Tray {
    pub fn new() -> io::Result<Self> {
        let class_name = wide("TkblTrayWindow");

        // SAFETY: standard window registration.
        let hwnd = unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(tray_proc),
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
            RegisterClassExW(&wc);
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                wide("tkbl tray").as_ptr(),
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

        let mut tray = Self {
            hwnd,
            icon: None,
            shown_level: None,
        };
        tray.notify(NIM_ADD, None, "ThinkPad keyboard backlight")?;
        Ok(tray)
    }

    fn notify(&mut self, action: u32, level: Option<Level>, tip: &str) -> io::Result<()> {
        let icon = make_icon(level);
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = 1;
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = icon.unwrap_or(std::ptr::null_mut());

        let tip_wide = wide(tip);
        let n = tip_wide.len().min(data.szTip.len());
        data.szTip[..n].copy_from_slice(&tip_wide[..n]);
        if let Some(last) = data.szTip.get_mut(n.saturating_sub(1)) {
            *last = 0;
        }

        // SAFETY: `data` is fully initialised with its size set.
        let ok = unsafe { Shell_NotifyIconW(action, &data) };

        // Replace the retained icon only after the shell has taken the new one.
        if let Some(old) = self.icon.take() {
            unsafe { DestroyIcon(old) };
        }
        self.icon = icon;

        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Refresh icon and tooltip. Cheap to call every tick: it only touches the shell when
    /// the displayed level actually changed.
    pub fn update(&mut self, level: Option<Level>, tip: &str, force: bool) {
        if !force && self.shown_level == Some(level) {
            return;
        }
        self.shown_level = Some(level);
        let _ = self.notify(NIM_MODIFY, level, tip);
    }

    /// Publish the state the context menu should show next time it opens.
    pub fn set_menu_state(
        &self,
        level: Option<Level>,
        paused: bool,
        on_ac: bool,
        start_with_windows: bool,
    ) {
        MENU_STATE.with(|s| {
            *s.borrow_mut() = MenuState {
                level,
                paused,
                on_ac,
                start_with_windows,
            }
        });
    }

    /// Take queued user actions. Call [`crate::platform::pump_messages`] first.
    pub fn take_events(&self) -> Vec<TrayEvent> {
        TRAY_EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()))
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = 1;
        // SAFETY: removes the icon we added; failures are not actionable here.
        unsafe {
            Shell_NotifyIconW(NIM_DELETE, &data);
            if let Some(icon) = self.icon.take() {
                DestroyIcon(icon);
            }
            DestroyWindow(self.hwnd);
        }
    }
}
