//! Device-aware activity. Read only RAWINPUTHEADER, never key codes or mouse payloads.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io;
use std::time::Instant;

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Get_Parent, CM_Locate_DevNodeW, CM_LOCATE_DEVNODE_NORMAL, CR_SUCCESS,
};
use windows_sys::Win32::Foundation::{HANDLE, HWND};
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RegisterRawInputDevices,
    RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RIDEV_DEVNOTIFY, RIDEV_INPUTSINK,
    RIDEV_REMOVE, RIDI_DEVICENAME, RID_HEADER, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    Internal,
    External,
    Unknown,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Activity {
    pub last_internal: Option<Instant>,
    pub last_external: Option<Instant>,
    pub internal_events: u64,
    pub external_events: u64,
    pub unknown_events: u64,
}

thread_local! {
    static ACTIVITY: RefCell<Activity> = const { RefCell::new(Activity {
        last_internal: None, last_external: None,
        internal_events: 0, external_events: 0, unknown_events: 0,
    }) };
    static DEVICES: RefCell<HashMap<usize, InputSource>> = RefCell::new(HashMap::new());
}

pub fn activity() -> Activity {
    ACTIVITY.with(|a| *a.borrow())
}

pub fn clear_device_cache() {
    DEVICES.with(|d| d.borrow_mut().clear());
}

pub fn register(hwnd: HWND) -> io::Result<()> {
    let devices = [2, 6].map(|usage| RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: usage,
        dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
        hwndTarget: hwnd,
    });
    // SAFETY: correctly sized descriptors; a live window on this thread receives input.
    if unsafe {
        RegisterRawInputDevices(
            devices.as_ptr(),
            2,
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn unregister() {
    let devices = [2, 6].map(|usage| RAWINPUTDEVICE {
        usUsagePage: 1,
        usUsage: usage,
        dwFlags: RIDEV_REMOVE,
        hwndTarget: std::ptr::null_mut(),
    });
    // SAFETY: unregister the two collections this process registered.
    unsafe {
        RegisterRawInputDevices(
            devices.as_ptr(),
            2,
            std::mem::size_of::<RAWINPUTDEVICE>() as u32,
        )
    };
    clear_device_cache();
}

/// `handle` must be the live WM_INPUT lParam, used during its window callback.
pub unsafe fn observe(handle: isize) {
    let mut header = RAWINPUTHEADER::default();
    let mut bytes = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    // SAFETY: caller supplies a live raw-input handle; only the fixed header is requested.
    let read = unsafe {
        GetRawInputData(
            handle as _,
            RID_HEADER,
            (&mut header as *mut RAWINPUTHEADER).cast(),
            &mut bytes,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if read != std::mem::size_of::<RAWINPUTHEADER>() as u32
        || !matches!(header.dwType, RIM_TYPEMOUSE | RIM_TYPEKEYBOARD)
    {
        return;
    }
    let source = if header.hDevice.is_null() {
        // Windows documents a zero device handle for precision-touchpad mouse input.
        if header.dwType == RIM_TYPEMOUSE {
            InputSource::Internal
        } else {
            InputSource::Unknown
        }
    } else {
        DEVICES.with(|d| {
            *d.borrow_mut()
                .entry(header.hDevice as usize)
                .or_insert_with(|| {
                    device_name(header.hDevice)
                        .map(|n| classify(&n))
                        .unwrap_or(InputSource::Unknown)
                })
        })
    };
    ACTIVITY.with(|a| {
        let mut a = a.borrow_mut();
        match source {
            InputSource::Internal => {
                a.last_internal = Some(Instant::now());
                a.internal_events += 1;
            }
            InputSource::External => {
                a.last_external = Some(Instant::now());
                a.external_events += 1;
            }
            InputSource::Unknown => a.unknown_events += 1,
        }
    });
}

fn device_name(handle: HANDLE) -> Option<String> {
    let mut length = 0;
    // SAFETY: the first call asks for the required UTF-16 buffer length.
    if unsafe { GetRawInputDeviceInfoW(handle, RIDI_DEVICENAME, std::ptr::null_mut(), &mut length) }
        == u32::MAX
        || length == 0
        || length > 4096
    {
        return None;
    }
    let mut buffer = vec![0u16; length as usize];
    // SAFETY: the buffer is at least the requested length.
    if unsafe {
        GetRawInputDeviceInfoW(
            handle,
            RIDI_DEVICENAME,
            buffer.as_mut_ptr().cast(),
            &mut length,
        )
    } == u32::MAX
    {
        return None;
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

fn instance_from_interface(name: &str) -> Option<String> {
    let name = name
        .strip_prefix("\\\\?\\")
        .or_else(|| name.strip_prefix("\\??\\"))?;
    let parts: Vec<_> = name.split('#').collect();
    if parts.len() < 4 {
        return None;
    }
    Some(parts[..3].join("\\"))
}

fn node_source(id: &str) -> Option<InputSource> {
    let id = id.to_ascii_uppercase();
    if id.starts_with("USB\\") || id.starts_with("BTH") {
        Some(InputSource::External)
    } else if id.starts_with("ACPI\\") {
        Some(InputSource::Internal)
    } else {
        None
    }
}

fn classify(name: &str) -> InputSource {
    let Some(instance) = instance_from_interface(name) else {
        return InputSource::Unknown;
    };
    if let Some(source) = node_source(&instance) {
        return source;
    }
    let mut wide: Vec<u16> = instance.encode_utf16().chain(Some(0)).collect();
    let mut node = 0;
    // SAFETY: the instance string is writable and NUL-terminated as CM requires.
    if unsafe { CM_Locate_DevNodeW(&mut node, wide.as_mut_ptr(), CM_LOCATE_DEVNODE_NORMAL) }
        != CR_SUCCESS
    {
        return InputSource::Unknown;
    }
    for _ in 0..32 {
        let mut id = [0u16; 512];
        // SAFETY: a valid device node and correctly sized buffer.
        if unsafe { CM_Get_Device_IDW(node, id.as_mut_ptr(), id.len() as u32, 0) } != CR_SUCCESS {
            break;
        }
        let end = id.iter().position(|&c| c == 0).unwrap_or(id.len());
        if let Some(source) = node_source(&String::from_utf16_lossy(&id[..end])) {
            return source;
        }
        let mut parent = 0;
        // SAFETY: writes a device-node identifier to an owned variable.
        if unsafe { CM_Get_Parent(&mut parent, node, 0) } != CR_SUCCESS {
            break;
        }
        node = parent;
    }
    InputSource::Unknown
}

/// Diagnostics: device identity and classification only, with no input contents.
pub fn devices() -> io::Result<Vec<(String, InputSource)>> {
    let size = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
    let mut count = 0;
    // SAFETY: first call obtains the required entry count.
    if unsafe { GetRawInputDeviceList(std::ptr::null_mut(), &mut count, size) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
    // SAFETY: correctly sized writable list.
    let read = unsafe { GetRawInputDeviceList(list.as_mut_ptr(), &mut count, size) };
    if read == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    Ok(list
        .into_iter()
        .take(read as usize)
        .filter(|d| matches!(d.dwType, RIM_TYPEMOUSE | RIM_TYPEKEYBOARD))
        .filter_map(|d| {
            device_name(d.hDevice).map(|n| {
                let source = classify(&n);
                (n, source)
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_interface_names_become_device_instance_ids() {
        assert_eq!(
            instance_from_interface(r"\\?\HID#VID_1C4F&PID_0016&MI_00#8&11D8BFAA&0&0000#{guid}"),
            Some(r"HID\VID_1C4F&PID_0016&MI_00\8&11D8BFAA&0&0000".into())
        );
        assert_eq!(instance_from_interface("invalid"), None);
    }

    #[test]
    fn transport_classification_requires_known_ancestry() {
        assert_eq!(
            node_source(r"ACPI\LEN0071\4&375A6530&0"),
            Some(InputSource::Internal)
        );
        assert_eq!(
            node_source(r"USB\VID_1C4F&PID_0016\x"),
            Some(InputSource::External)
        );
        assert_eq!(node_source(r"BTHLEDEVICE\x"), Some(InputSource::External));
        assert_eq!(node_source(r"HID\VID_1C4F&PID_0016\x"), None);
    }
}
