//! Narrow Win32 probes used only to recover Chatarium's own harness lock.
#![allow(unsafe_code)]

use std::ffi::OsStr;
use std::mem::zeroed;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, ReleaseMutex, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowExW, HWND_MESSAGE};

pub(super) struct RecoveryMutex(HANDLE);
impl RecoveryMutex {
    pub(super) fn acquire(profile: &Path) -> Result<Self, crate::transport::TransportError> {
        let name = wide(format!(
            "Local\\Chatarium-Capture-Recovery-{}",
            profile.display()
        ));
        // SAFETY: The name is NUL-terminated and the handle is owned by this guard.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(crate::transport::TransportError::StaleState(format!(
                "create lock recovery mutex: Windows error {}",
                unsafe { GetLastError() }
            )));
        }
        // SAFETY: bounded wait on the private mutex.
        let wait = unsafe { WaitForSingleObject(handle, 2_000) };
        if wait != WAIT_OBJECT_0 {
            unsafe { CloseHandle(handle) };
            return Err(crate::transport::TransportError::StaleState(
                if wait == WAIT_TIMEOUT {
                    "lock recovery mutex timed out"
                } else {
                    "lock recovery mutex wait failed"
                }
                .to_owned(),
            ));
        }
        Ok(Self(handle))
    }
}
impl Drop for RecoveryMutex {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum LockRecord {
    Legacy { pid: u32 },
    V2 { pid: u32, creation: u64 },
}
impl LockRecord {
    pub(super) fn pid(&self) -> u32 {
        match self {
            Self::Legacy { pid } | Self::V2 { pid, .. } => *pid,
        }
    }
}
pub(super) enum OwnerState {
    LiveSame,
    LiveUnknown,
    Dead,
    Reused,
}

pub(super) fn parse_lock(text: &str) -> Result<LockRecord, crate::transport::TransportError> {
    let mut version = None;
    let mut pid = None;
    let mut creation = None;
    for line in text.lines() {
        let mut parts = line.splitn(2, '=');
        let key = parts.next();
        let value = parts.next();
        match (key, value) {
            (Some("version"), Some(v)) => version = v.parse().ok(),
            (Some("harness_pid"), Some(v)) => pid = v.parse().ok(),
            (Some("harness_creation_filetime"), Some(v)) => creation = v.parse().ok(),
            (Some("pid"), Some(v)) => pid = v.parse().ok(),
            _ => {}
        }
    }
    match (version, pid, creation) {
        (Some(2), Some(pid), Some(creation)) => Ok(LockRecord::V2 { pid, creation }),
        (None, Some(pid), _)
            if text
                .lines()
                .any(|line| line.starts_with("created_unix_ms=")) =>
        {
            Ok(LockRecord::Legacy { pid })
        }
        _ => Err(crate::transport::TransportError::StaleState(
            "unrecognized harness lock format; retaining it".to_owned(),
        )),
    }
}

pub(super) fn current_process_identity() -> Result<u64, crate::transport::TransportError> {
    process_creation(unsafe { GetCurrentProcess() })
}

pub(super) fn owner_state(
    record: &LockRecord,
) -> Result<OwnerState, crate::transport::TransportError> {
    // SAFETY: narrow query/synchronize rights; the returned handle is closed below.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | 0x0010_0000,
            0,
            record.pid(),
        )
    };
    if handle.is_null() {
        return Ok(OwnerState::Dead);
    }
    let exit = unsafe { WaitForSingleObject(handle, 0) };
    if exit != WAIT_TIMEOUT {
        unsafe { CloseHandle(handle) };
        return Ok(OwnerState::Dead);
    }
    let result = match record {
        LockRecord::V2 { creation, .. } => {
            let current = match process_creation(handle) {
                Ok(value) => value,
                Err(error) => {
                    unsafe { CloseHandle(handle) };
                    return Err(error);
                }
            };
            if current == *creation {
                OwnerState::LiveSame
            } else {
                OwnerState::Reused
            }
        }
        LockRecord::Legacy { .. } => OwnerState::LiveUnknown,
    };
    unsafe { CloseHandle(handle) };
    Ok(result)
}

pub(super) fn profile_singleton_present(
    profile: &Path,
) -> Result<bool, crate::transport::TransportError> {
    let class = wide("Chrome_MessageWindow");
    let title = wide(profile.display().to_string());
    // SAFETY: exact class/title lookup under the message-only window desktop.
    let hwnd = unsafe {
        FindWindowExW(
            HWND_MESSAGE,
            std::ptr::null_mut(),
            class.as_ptr(),
            title.as_ptr(),
        )
    };
    Ok(!hwnd.is_null())
}

fn process_creation(handle: HANDLE) -> Result<u64, crate::transport::TransportError> {
    let mut created = unsafe { zeroed() };
    let mut exit = unsafe { zeroed() };
    let mut kernel = unsafe { zeroed() };
    let mut user = unsafe { zeroed() };
    if unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(crate::transport::TransportError::StaleState(format!(
            "process identity probe failed: Windows error {}",
            unsafe { GetLastError() }
        )));
    }
    Ok((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
}
fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
