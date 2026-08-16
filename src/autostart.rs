//! "Start with Windows", via the per-user Run key.
//!
//! `HKCU` deliberately: it needs no elevation, which the whole design depends on.

use std::io;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "ThinkpadKbdLight";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open(access: u32) -> io::Result<HKEY> {
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: `key` receives the handle; the path is NUL-terminated.
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            access,
            &mut key,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    Ok(key)
}

pub fn is_enabled() -> bool {
    let Ok(key) = open(KEY_READ) else {
        return false;
    };
    let mut size: u32 = 0;
    // SAFETY: querying size only, with null buffer.
    let rc = unsafe {
        RegQueryValueExW(
            key,
            wide(VALUE_NAME).as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    unsafe { RegCloseKey(key) };
    rc == ERROR_SUCCESS
}

pub fn set(enabled: bool) -> io::Result<()> {
    let key = open(KEY_WRITE)?;
    let name = wide(VALUE_NAME);

    let rc = if enabled {
        let exe = std::env::current_exe()?;
        // Quote the path: Run entries are parsed as command lines.
        let command = wide(&format!("\"{}\"", exe.display()));
        let bytes = command.len() * std::mem::size_of::<u16>();
        // SAFETY: `command` outlives the call and `bytes` matches its length.
        unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                bytes as u32,
            )
        }
    } else {
        // SAFETY: deleting a value that may not exist is handled below.
        unsafe { RegDeleteValueW(key, name.as_ptr()) }
    };

    unsafe { RegCloseKey(key) };

    // Deleting something already absent is success as far as the caller cares.
    const ERROR_FILE_NOT_FOUND: u32 = 2;
    if rc == ERROR_SUCCESS || (!enabled && rc == ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc as i32))
    }
}
