//! Native Windows ownership for the human-facing plain Edge phase.
//!
//! The process is created suspended and assigned to a Job Object before its first
//! instruction runs. Job membership therefore remains authoritative if the root
//! process exits or descendants are reparented. This module is the narrow unsafe
//! boundary for that ownership and visible-window observation.

#![allow(unsafe_code)]

use super::ManagedEdgeChild;
use super::job_membership::{grow_capacity, max_process_ids, parse_process_id_list};
use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, QueryInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, GetExitCodeProcess, OpenProcess,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, ResumeThread, STARTUPINFOW,
    WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowThreadProcessId, IsWindowVisible, WNDENUMPROC,
};

const TERMINATION_WAIT_TIMEOUT_MS: u32 = 5_000;

pub(super) struct PlainEdgeProcess {
    pid: u32,
    process: HANDLE,
    job: HANDLE,
}

// SAFETY: Process and job handles are owned kernel handles and the Win32 operations used here
// are thread-safe; the value never exposes either raw handle outside this module.
unsafe impl Send for PlainEdgeProcess {}

impl Drop for PlainEdgeProcess {
    fn drop(&mut self) {
        // SAFETY: Both handles are owned by this value and closed exactly once.
        unsafe {
            CloseHandle(self.process);
            CloseHandle(self.job);
        }
    }
}

impl ManagedEdgeChild for PlainEdgeProcess {
    fn id(&self) -> u32 {
        self.pid
    }

    fn try_wait(&mut self) -> Result<Option<i32>, String> {
        // SAFETY: `process` is a valid process handle retained for this object.
        let wait = unsafe { WaitForSingleObject(self.process, 0) };
        if wait != WAIT_OBJECT_0 {
            return Ok(None);
        }
        let mut code = 0;
        // SAFETY: The output pointer is valid and process is an owned process handle.
        if unsafe { GetExitCodeProcess(self.process, &mut code) } == 0 {
            return Err(last_error("read plain Edge exit code"));
        }
        Ok(Some(code as i32))
    }

    fn kill(&mut self) -> Result<(), String> {
        // SAFETY: TerminateJobObject targets only processes assigned to this private job.
        if unsafe { TerminateJobObject(self.job, 1) } == 0 {
            Err(last_error("terminate harness-owned Edge job"))
        } else {
            Ok(())
        }
    }

    fn wait(&mut self) -> Result<i32, String> {
        // SAFETY: Waiting on this owned process handle is valid.
        match unsafe { WaitForSingleObject(self.process, TERMINATION_WAIT_TIMEOUT_MS) } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => {
                return Err(format!(
                    "plain Edge did not exit within {TERMINATION_WAIT_TIMEOUT_MS} ms after job termination"
                ));
            }
            _ => return Err(last_error("wait for plain Edge")),
        }
        self.try_wait()?
            .ok_or_else(|| "plain Edge process remained unsignaled after wait".to_owned())
    }

    fn owned_process_ids(&self) -> Result<Vec<u32>, String> {
        job_process_ids(self.job)
    }

    fn owns_live_process_id(&self, pid: u32) -> Result<bool, String> {
        // Opening a fresh handle binds the check to a process object, not a reusable PID.
        // SAFETY: OpenProcess receives a documented access mask and a scalar PID.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(last_error("open visible window owner for job verification"));
        }
        // SAFETY: `process` is a valid handle and `job` is retained by this object.
        let result = unsafe {
            let mut in_job = 0;
            if IsProcessInJob(process, self.job, &mut in_job) == 0 {
                Err(last_error("verify visible window owner job membership"))
            } else {
                Ok(in_job != 0)
            }
        };
        // SAFETY: `process` was returned by OpenProcess and is closed exactly once.
        unsafe { CloseHandle(process) };
        result
    }

    fn owned_processes_alive(&self) -> Result<bool, String> {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: `info` is a correctly sized output structure for this query class.
        if unsafe {
            QueryInformationJobObject(
                self.job,
                JobObjectBasicAccountingInformation,
                (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error("query harness-owned Edge job"));
        }
        Ok(info.ActiveProcesses != 0)
    }
}

pub(super) fn spawn(
    executable: &Path,
    arguments: &[String],
) -> Result<Box<dyn ManagedEdgeChild>, String> {
    let application = wide_null(executable.as_os_str());
    let mut argv = vec![executable.to_string_lossy().into_owned()];
    argv.extend(arguments.iter().cloned());
    let mut command_line = argv
        .iter()
        .map(|argument| quote_windows_arg(argument))
        .collect::<Vec<_>>()
        .join(" ")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    // SAFETY: Win32 calls receive NUL-terminated buffers and valid out structures. The
    // suspended child is assigned before ResumeThread, so no descendant can escape capture.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(last_error("create harness-owned Edge job"));
        }
        let mut startup: STARTUPINFOW = zeroed();
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        let mut process_info: PROCESS_INFORMATION = zeroed();
        let created = CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_SUSPENDED | CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process_info,
        );
        if created == 0 {
            CloseHandle(job);
            return Err(last_error("create suspended plain Edge process"));
        }
        if AssignProcessToJobObject(job, process_info.hProcess) == 0 {
            let error = last_error("assign plain Edge to harness-owned job");
            windows_sys::Win32::System::Threading::TerminateProcess(process_info.hProcess, 1);
            CloseHandle(process_info.hThread);
            CloseHandle(process_info.hProcess);
            CloseHandle(job);
            return Err(error);
        }
        if ResumeThread(process_info.hThread) == u32::MAX {
            let error = last_error("resume plain Edge process");
            let _ = TerminateJobObject(job, 1);
            CloseHandle(process_info.hThread);
            CloseHandle(process_info.hProcess);
            CloseHandle(job);
            return Err(error);
        }
        CloseHandle(process_info.hThread);
        Ok(Box::new(PlainEdgeProcess {
            pid: process_info.dwProcessId,
            process: process_info.hProcess,
            job,
        }))
    }
}

pub(super) fn visible_window_owners(owned_pids: &[u32]) -> Result<Vec<u32>, String> {
    struct Search<'a> {
        pids: &'a [u32],
        owners: Vec<u32>,
    }
    unsafe extern "system" fn visit(
        hwnd: windows_sys::Win32::Foundation::HWND,
        data: isize,
    ) -> i32 {
        // SAFETY: `data` points to the live Search value passed to EnumWindows below.
        let search = unsafe { &mut *(data as *mut Search<'_>) };
        // SAFETY: hwnd is supplied by EnumWindows and remains valid for these queries.
        if unsafe { IsWindowVisible(hwnd) } == 0 {
            return 1;
        }
        let mut pid = 0;
        // SAFETY: the output pointer is valid and hwnd comes from EnumWindows.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if search.pids.contains(&pid) {
            search.owners.push(pid);
            1
        } else {
            1
        }
    }
    let mut search = Search {
        pids: owned_pids,
        owners: Vec::new(),
    };
    let callback: WNDENUMPROC = Some(visit);
    // SAFETY: The callback uses the live stack value for the duration of EnumWindows only.
    let completed = unsafe { EnumWindows(callback, (&mut search as *mut Search<'_>) as isize) };
    if completed == 0 {
        return Err(last_error("enumerate top-level windows"));
    }
    search.owners.sort_unstable();
    search.owners.dedup();
    Ok(search.owners)
}

fn job_process_ids(job: HANDLE) -> Result<Vec<u32>, String> {
    let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: `accounting` is a correctly sized output structure.
    if unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(last_error("query Edge job process count"));
    }
    let mut capacity = (accounting.ActiveProcesses as usize).max(8);
    const MAX_RETRIES: usize = 8;
    for attempt in 0..MAX_RETRIES {
        if capacity > max_process_ids() {
            return Err(format!(
                "Edge job process membership exceeded limit {}",
                max_process_ids()
            ));
        }
        let size = 8usize
            .checked_add(
                capacity
                    .checked_mul(size_of::<usize>())
                    .ok_or_else(|| "Edge job process query size overflow".to_owned())?,
            )
            .ok_or_else(|| "Edge job process query size overflow".to_owned())?;
        let word_size = size_of::<usize>();
        let words = size
            .checked_add(word_size - 1)
            .ok_or_else(|| "Edge job process query size overflow".to_owned())?
            / word_size;
        // Back the C structure with pointer-aligned storage. Parsing remains byte-oriented
        // because the header contains two DWORDs followed by ULONG_PTR entries at offset 8.
        let mut storage = vec![0usize; words];
        let storage_len = storage.len() * word_size;
        let mut returned = 0;
        // SAFETY: storage is aligned for ULONG_PTR and large enough for the DWORD header plus
        // the requested trailing process-ID capacity.
        let ok = unsafe {
            QueryInformationJobObject(
                job,
                JobObjectBasicProcessIdList,
                storage.as_mut_ptr().cast(),
                storage_len as u32,
                &mut returned,
            )
        };
        // SAFETY: storage remains alive and unchanged while this read-only byte view is used.
        let bytes = unsafe {
            std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), storage_len)
        };
        let declared = u32::from_ne_bytes(bytes[4..8].try_into().unwrap()) as usize;
        if ok != 0 {
            return parse_process_id_list(bytes, word_size);
        }
        let error = unsafe { GetLastError() };
        if error != 122 && error != 234 {
            return Err(format!(
                "query Edge job process membership: Windows error {error}"
            ));
        }
        capacity = grow_capacity(capacity, declared, attempt)?;
    }
    Err("Edge job process membership did not stabilize within bounded retries".to_owned())
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn quote_windows_arg(argument: &str) -> String {
    if !argument.is_empty() && !argument.chars().any(|c| c.is_whitespace() || c == '"') {
        return argument.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut slashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            slashes += 1;
        } else if character == '"' {
            quoted.push_str(&"\\".repeat(slashes * 2 + 1));
            quoted.push('"');
            slashes = 0;
        } else {
            quoted.push_str(&"\\".repeat(slashes));
            quoted.push(character);
            slashes = 0;
        }
    }
    quoted.push_str(&"\\".repeat(slashes * 2));
    quoted.push('"');
    quoted
}

fn last_error(context: &str) -> String {
    // SAFETY: GetLastError has no preconditions.
    format!("{context}: Windows error {}", unsafe { GetLastError() })
}
