//! Tier 1 of the HAL: a thin gateway to the Lenovo PM driver.
//!
//! `\\.\IBMPMDRV` is a WHQL-signed Lenovo driver that acts as a *generic ACPI method
//! evaluator*: each IOCTL code names a 4-character ACPI method, and the driver evaluates
//! it with a single DWORD argument, returning a single DWORD.
//!
//! Derived by static analysis of `ibmpmdrv.sys`; see `notes/04-ioctl-findings.md`.
//! The device grants `FILE_ANY_ACCESS`, so **no elevation is required**.

use std::ffi::c_void;
use std::io;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
use windows_sys::Win32::System::IO::DeviceIoControl;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// `\\.\IBMPMDRV`
const DEVICE_PATH: &str = r"\\.\IBMPMDRV";

/// Evaluate ACPI `MLCG` — get keyboard backlight state.
pub const IOCTL_MLCG: u32 = 0x0022_2680;
/// Evaluate ACPI `MLCS` — set keyboard backlight level.
pub const IOCTL_MLCS: u32 = 0x0022_2684;

/// A handle to the Lenovo PM driver.
///
/// Held open for the lifetime of the process: opening per call is wasteful, and the
/// handle is cheap. All evaluations are serialised, because each one ultimately traps
/// into SMM and two concurrent traps are not something to find out about the hard way.
pub struct AcpiGateway {
    handle: *mut c_void,
    /// Serialises `evaluate` so we never have two SMI traps in flight.
    lock: Mutex<()>,
}

// SAFETY: the handle is only ever used inside `evaluate`, which holds `lock` for the
// duration of the call. The kernel handle itself is not thread-affine.
unsafe impl Send for AcpiGateway {}
unsafe impl Sync for AcpiGateway {}

impl AcpiGateway {
    /// Open the driver. Fails with `NotFound`-ish errors if the Lenovo PM driver is not
    /// installed, which callers should treat as "feature unavailable", not as fatal.
    pub fn open() -> io::Result<Self> {
        let mut wide: Vec<u16> = DEVICE_PATH.encode_utf16().collect();
        wide.push(0);

        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            handle,
            lock: Mutex::new(()),
        })
    }

    /// Evaluate the ACPI method named by `ioctl`, passing `arg`, returning its DWORD.
    ///
    /// Both buffers are 4 bytes and the control codes are `METHOD_BUFFERED`, so the
    /// driver uses a single system buffer for input and output.
    pub fn evaluate(&self, ioctl: u32, arg: u32) -> io::Result<u32> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());

        let input: u32 = arg;
        let mut output: u32 = 0;
        let mut returned: u32 = 0;

        // SAFETY: both buffers are exactly the 4 bytes the driver requires, and the
        // handle is valid for the lifetime of `self`.
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                ioctl,
                &input as *const u32 as *const c_void,
                4,
                &mut output as *mut u32 as *mut c_void,
                4,
                &mut returned,
                std::ptr::null_mut(),
            )
        };

        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if returned != 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("driver returned {returned} bytes, expected 4"),
            ));
        }
        Ok(output)
    }
}

impl Drop for AcpiGateway {
    fn drop(&mut self) {
        // SAFETY: handle was produced by CreateFileW and is only closed once.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}
