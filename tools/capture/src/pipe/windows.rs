#![allow(unsafe_code)]
//! Native Windows anonymous pipes and the flattened browser-wide CDP transport.

use super::{AsciizDecoder, encode_asciiz_message};
use crate::edge::ManagedEdgeChild;
use crate::run::CaptureRun;
use crate::transport::{
    BrowserTransport, BrowserVersion, CdpMessageChannel, CdpPageSession, PageSession, TargetInfo,
    TransportError,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;
#[cfg(test)]
use windows_sys::Win32::Foundation::GetHandleInformation;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, GetLastError, HANDLE, HANDLE_FLAG_INHERIT,
    SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetExitCodeProcess, InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROCESS_INFORMATION, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

const MAX_READ_CHUNK: usize = 8192;

/// Windows anonymous pipe pair with parent/child ownership made explicit.
pub struct WindowsPipePair {
    parent_write: Option<File>,
    parent_read: Option<File>,
    child_read: Option<OwnedHandle>,
    child_write: Option<OwnedHandle>,
}

impl WindowsPipePair {
    /// Create parent-write/child-read and child-write/parent-read pipes.
    pub fn create() -> Result<Self, TransportError> {
        // SAFETY: All outputs are initialized null handles. The returned handles are immediately
        // wrapped in OwnedHandle, and inheritance is restricted again by the process attribute list.
        unsafe {
            let mut child_read: HANDLE = std::ptr::null_mut();
            let mut parent_write: HANDLE = std::ptr::null_mut();
            let mut parent_read: HANDLE = std::ptr::null_mut();
            let mut child_write: HANDLE = std::ptr::null_mut();
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: std::ptr::null_mut(),
                // Start with every endpoint non-inheritable so no concurrent child process can
                // accidentally receive a pipe handle before the restricted launch.
                bInheritHandle: 0,
            };
            if CreatePipe(&mut child_read, &mut parent_write, &mut attributes, 0) == 0 {
                return Err(last_error("CreatePipe parent-to-child").into());
            }
            if CreatePipe(&mut parent_read, &mut child_write, &mut attributes, 0) == 0 {
                CloseHandle(child_read);
                CloseHandle(parent_write);
                return Err(last_error("CreatePipe child-to-parent").into());
            }
            let pair = Self {
                parent_write: Some(File::from(OwnedHandle::from_raw_handle(
                    parent_write.cast(),
                ))),
                parent_read: Some(File::from(OwnedHandle::from_raw_handle(parent_read.cast()))),
                child_read: Some(OwnedHandle::from_raw_handle(child_read.cast())),
                child_write: Some(OwnedHandle::from_raw_handle(child_write.cast())),
            };
            for child in [
                pair.child_read.as_ref().unwrap(),
                pair.child_write.as_ref().unwrap(),
            ] {
                if SetHandleInformation(
                    child.as_raw_handle().cast(),
                    HANDLE_FLAG_INHERIT,
                    HANDLE_FLAG_INHERIT,
                ) == 0
                {
                    return Err(last_error("make intended Edge pipe end inheritable").into());
                }
            }
            Ok(pair)
        }
    }

    fn child_handle_values(&self) -> Result<(usize, usize), TransportError> {
        let read = self
            .child_read
            .as_ref()
            .ok_or_else(|| TransportError::Process("child read handle is closed".to_owned()))?
            .as_raw_handle() as usize;
        let write = self
            .child_write
            .as_ref()
            .ok_or_else(|| TransportError::Process("child write handle is closed".to_owned()))?
            .as_raw_handle() as usize;
        Ok((read, write))
    }

    fn into_parent_channel(mut self) -> Result<PipeMessageChannel, TransportError> {
        // Child ends are closed in the parent immediately after a successful process creation.
        self.close_child_ends();
        let reader = self
            .parent_read
            .take()
            .ok_or_else(|| TransportError::Process("parent read handle is closed".to_owned()))?;
        let writer = self
            .parent_write
            .take()
            .ok_or_else(|| TransportError::Process("parent write handle is closed".to_owned()))?;
        PipeMessageChannel::new(reader, writer)
    }

    fn close_child_ends(&mut self) {
        drop(self.child_read.take());
        drop(self.child_write.take());
    }
}

struct PipeMessageChannel {
    writer: Option<File>,
    receiver: Receiver<Result<String, TransportError>>,
    reader: Option<JoinHandle<()>>,
    reader_closed: Arc<AtomicBool>,
    stop_reader: Arc<AtomicBool>,
    closed: bool,
}

impl PipeMessageChannel {
    fn new(reader: File, writer: File) -> Result<Self, TransportError> {
        let (sender, receiver) = mpsc::channel();
        let stop_reader = Arc::new(AtomicBool::new(false));
        let reader_stop = stop_reader.clone();
        let reader_closed = Arc::new(AtomicBool::new(false));
        let reader_closed_thread = reader_closed.clone();
        let reader_thread = thread::Builder::new()
            .name("chatarium-devtools-pipe-reader".to_owned())
            .spawn(move || {
                read_pipe_messages(reader, sender, reader_stop);
                reader_closed_thread.store(true, Ordering::Release);
            })
            .map_err(|error| TransportError::Process(format!("start pipe reader: {error}")))?;
        Ok(Self {
            writer: Some(writer),
            receiver,
            reader: Some(reader_thread),
            reader_closed,
            stop_reader,
            closed: false,
        })
    }
}

impl CdpMessageChannel for PipeMessageChannel {
    fn send_message(&mut self, text: &str) -> Result<(), TransportError> {
        if self.closed {
            return Err(TransportError::Disconnected);
        }
        let bytes = encode_asciiz_message(text);
        let writer = self.writer.as_mut().ok_or(TransportError::Disconnected)?;
        writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .map_err(|error| TransportError::Io(format!("write ASCIIZ CDP message: {error}")))
    }

    fn receive_message(&mut self, timeout: Duration) -> Result<Option<String>, TransportError> {
        if self.closed {
            return Err(TransportError::Disconnected);
        }
        match self.receiver.recv_timeout(timeout) {
            Ok(Ok(message)) => Ok(Some(message)),
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(TransportError::Disconnected),
        }
    }

    fn close(&mut self) -> Result<(), TransportError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        drop(self.writer.take());
        self.stop_reader.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            reader
                .join()
                .map_err(|_| TransportError::Process("pipe reader thread panicked".to_owned()))?;
        }
        if !self.reader_closed.load(Ordering::Acquire) {
            return Err(TransportError::Process(
                "pipe reader stopped without closing its owned endpoint".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Drop for PipeMessageChannel {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn read_pipe_messages(
    reader: File,
    sender: Sender<Result<String, TransportError>>,
    stop: Arc<AtomicBool>,
) {
    let mut decoder = AsciizDecoder::default();
    let mut chunk = [0_u8; MAX_READ_CHUNK];
    while !stop.load(Ordering::Acquire) {
        let mut available = 0_u32;
        // SAFETY: `reader` owns a valid anonymous pipe; only the output byte count is written.
        let peeked = unsafe {
            PeekNamedPipe(
                reader.as_raw_handle().cast(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if peeked == 0 {
            // SAFETY: GetLastError has no preconditions.
            let code = unsafe { GetLastError() };
            if code == ERROR_BROKEN_PIPE {
                let error = decoder.finish().err().unwrap_or(TransportError::Eof {
                    unterminated_message: false,
                });
                let _ = sender.send(Err(error));
            } else if !stop.load(Ordering::Acquire) {
                let _ = sender.send(Err(TransportError::Io(format!(
                    "inspect ASCIIZ CDP pipe: Windows error {code}"
                ))));
            }
            break;
        }
        if available == 0 {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        let requested = (available as usize).min(chunk.len()) as u32;
        let mut count = 0_u32;
        // SAFETY: `chunk` is writable for `requested` bytes and `reader` is a valid pipe handle.
        let read = unsafe {
            ReadFile(
                reader.as_raw_handle().cast(),
                chunk.as_mut_ptr(),
                requested,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if read == 0 {
            // SAFETY: GetLastError has no preconditions.
            let code = unsafe { GetLastError() };
            let _ = sender.send(Err(TransportError::Io(format!(
                "read ASCIIZ CDP pipe: Windows error {code}"
            ))));
            break;
        }
        match decoder.push(&chunk[..count as usize]) {
            Ok(messages) => {
                for message in messages {
                    if sender.send(Ok(message)).is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error));
                break;
            }
        }
    }
}

/// Browser-wide CDP transport using one NUL-framed anonymous pipe pair.
pub struct PipeCdpBrowserTransport {
    root: CdpPageSession,
    version: Option<BrowserVersion>,
    targets: HashMap<String, TargetInfo>,
    command_timeout: Duration,
}

impl PipeCdpBrowserTransport {
    /// Create the browser-wide transport over the parent pipe endpoints.
    pub fn new(channel: Box<dyn CdpMessageChannel>) -> Self {
        Self {
            root: CdpPageSession::with_channel(channel),
            version: None,
            targets: HashMap::new(),
            command_timeout: Duration::from_secs(3),
        }
    }

    /// Bound startup command I/O to the remaining overall browser deadline.
    pub fn set_command_timeout(&mut self, timeout: Duration) {
        self.command_timeout = timeout;
    }

    /// Close pipe endpoints and cancel the blocking reader.
    pub fn close(&mut self) -> Result<(), TransportError> {
        PageSession::close(&mut self.root)
    }
}

impl BrowserTransport for PipeCdpBrowserTransport {
    fn browser_version(&mut self, run: &mut CaptureRun) -> Result<BrowserVersion, TransportError> {
        if let Some(version) = &self.version {
            return Ok(version.clone());
        }
        let value = self
            .root
            .command("Browser.getVersion", json!({}), self.command_timeout)?;
        let version = BrowserVersion {
            browser: value
                .get("product")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed_field("Browser.getVersion", "product"))?
                .to_owned(),
            protocol_version: value
                .get("protocolVersion")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed_field("Browser.getVersion", "protocolVersion"))?
                .to_owned(),
        };
        run.append_event(
            "devtools_pipe_browser_version",
            json!({
                "transport_mode": "pipe",
                "browser": version.browser,
                "protocol_version": version.protocol_version,
                "metadata": value,
            }),
        )
        .map_err(TransportError::Journal)?;
        run.set_browser_versions(version.browser.clone(), version.protocol_version.clone())
            .map_err(TransportError::Journal)?;
        self.version = Some(version.clone());
        Ok(version)
    }

    fn list_targets(&mut self, run: &mut CaptureRun) -> Result<Vec<TargetInfo>, TransportError> {
        if !self.targets.is_empty() {
            let mut cached = self.targets.values().cloned().collect::<Vec<_>>();
            cached.sort_by(|left, right| left.id.cmp(&right.id));
            return Ok(cached);
        }
        let value = self
            .root
            .command("Target.getTargets", json!({}), self.command_timeout)?;
        let entries = value
            .get("targetInfos")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                TransportError::MalformedMessage(
                    "Target.getTargets result is missing targetInfos array".to_owned(),
                )
            })?;
        let mut targets = HashMap::new();
        for entry in entries {
            if entry.get("type").and_then(Value::as_str) != Some("page") {
                continue;
            }
            let id = entry
                .get("targetId")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed_field("Target.getTargets", "targetId"))?
                .to_owned();
            let target = TargetInfo {
                id: id.clone(),
                target_type: "page".to_owned(),
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                url: entry
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed_field("Target.getTargets", "url"))?
                    .to_owned(),
                websocket_url: None,
            };
            if targets.insert(id.clone(), target).is_some() {
                return Err(TransportError::MalformedMessage(format!(
                    "Target.getTargets returned duplicate target ID '{id}'"
                )));
            }
        }
        let mut observed = targets.values().map(|target| json!({"target_id": target.id, "type": target.target_type, "title": target.title, "url": target.url})).collect::<Vec<_>>();
        observed
            .sort_by(|left, right| left["target_id"].as_str().cmp(&right["target_id"].as_str()));
        run.append_event(
            "cdp_page_targets_discovered",
            json!({"transport_mode":"pipe", "targets": observed, "target_count": targets.len()}),
        )
        .map_err(TransportError::Journal)?;
        let mut result = targets.values().cloned().collect::<Vec<_>>();
        result.sort_by(|left, right| left.id.cmp(&right.id));
        self.targets = targets;
        Ok(result)
    }

    fn attach(
        &mut self,
        target_id: &str,
        run: &mut CaptureRun,
    ) -> Result<Box<dyn PageSession>, TransportError> {
        let target = self
            .targets
            .get(target_id)
            .ok_or_else(|| TransportError::UnknownTarget(target_id.to_owned()))?;
        if target.url != "about:blank" {
            return Err(TransportError::InvalidEndpoint(
                "pipe smoke refuses to attach outside about:blank".to_owned(),
            ));
        }
        let result = self.root.command(
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
            Duration::from_secs(3),
        )?;
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| malformed_field("Target.attachToTarget", "sessionId"))?
            .to_owned();
        run.append_event("cdp_target_attached", json!({"target_id": target_id, "target_type": target.target_type, "transport_mode":"pipe", "flatten":true, "session_id":session_id})).map_err(TransportError::Journal)?;
        Ok(Box::new(
            self.root.attached_to(target_id.to_owned(), session_id),
        ))
    }
}

/// Spawn Edge with only its two pipe child handles in the inherited-handle allowlist.
pub fn spawn_edge_with_pipe(
    executable: &Path,
    arguments: &[String],
    run: &mut CaptureRun,
) -> Result<(Box<dyn ManagedEdgeChild>, PipeCdpBrowserTransport), TransportError> {
    run.append_event(
        "devtools_pipe_setup_started",
        json!({"transport_mode":"pipe"}),
    )
    .map_err(TransportError::Journal)?;
    let pair = match WindowsPipePair::create() {
        Ok(pair) => pair,
        Err(error) => return Err(record_setup_failure(run, error)),
    };
    let inherited_handles = inherited_handle_allowlist(&pair)?;
    let all_arguments = append_pipe_switches(arguments, inherited_handles);
    run.append_event(
        "devtools_pipe_setup_succeeded",
        json!({"transport_mode":"pipe", "restricted_handle_inheritance":true, "child_handle_count":2}),
    ).map_err(TransportError::Journal)?;
    let mut child = match spawn_restricted(executable, &all_arguments, &pair) {
        Ok(child) => child,
        Err(error) => {
            return Err(record_setup_failure(run, error));
        }
    };
    let channel = match pair.into_parent_channel() {
        Ok(channel) => channel,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(record_setup_failure(run, error));
        }
    };
    Ok((child, PipeCdpBrowserTransport::new(Box::new(channel))))
}

fn inherited_handle_allowlist(pair: &WindowsPipePair) -> Result<[HANDLE; 2], TransportError> {
    let (read, write) = pair.child_handle_values()?;
    Ok([read as HANDLE, write as HANDLE])
}

fn handle_to_uint32(handle: HANDLE) -> u32 {
    // Chromium serializes inherited Windows handles as unsigned 32-bit values. Windows keeps
    // kernel handle values 32-bit for 32/64-bit interoperability even though HANDLE is pointer-sized.
    handle as usize as u32
}

fn append_pipe_switches(arguments: &[String], handles: [HANDLE; 2]) -> Vec<String> {
    let mut result = arguments.to_vec();
    result.push("--remote-debugging-pipe=asciiz".to_owned());
    result.push(format!(
        "--remote-debugging-io-pipes={},{}",
        handle_to_uint32(handles[0]),
        handle_to_uint32(handles[1])
    ));
    result
}

fn record_setup_failure(run: &mut CaptureRun, primary: TransportError) -> TransportError {
    match run.append_event(
        "devtools_pipe_setup_failed",
        json!({"transport_mode":"pipe", "error":primary.to_string()}),
    ) {
        Ok(_) => primary,
        Err(journal_failure) => TransportError::DiagnosticJournalFailure {
            primary_failure: Some(primary.to_string()),
            journal_failure,
        },
    }
}

fn malformed_field(method: &str, field: &str) -> TransportError {
    TransportError::MalformedMessage(format!("{method} result is missing string field '{field}'"))
}

struct Win32EdgeChild {
    process: Option<OwnedHandle>,
    pid: u32,
}

impl ManagedEdgeChild for Win32EdgeChild {
    fn id(&self) -> u32 {
        self.pid
    }
    fn try_wait(&mut self) -> Result<Option<i32>, String> {
        let process = self
            .process
            .as_ref()
            .ok_or_else(|| "Edge process handle is closed".to_owned())?;
        // SAFETY: `process` is a valid owned process handle.
        let result = unsafe { WaitForSingleObject(process.as_raw_handle().cast(), 0) };
        if result == WAIT_TIMEOUT {
            return Ok(None);
        }
        if result != WAIT_OBJECT_0 {
            return Err(last_error("wait for Edge process"));
        }
        let mut code = 0;
        // SAFETY: `process` is signaled and `code` is a valid output pointer.
        if unsafe { GetExitCodeProcess(process.as_raw_handle().cast(), &mut code) } == 0 {
            return Err(last_error("read Edge exit code"));
        }
        Ok(Some(code as i32))
    }
    fn kill(&mut self) -> Result<(), String> {
        let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
            "SystemRoot is unavailable; cannot terminate the owned Edge process tree".to_owned()
        })?;
        let taskkill = Path::new(&system_root)
            .join("System32")
            .join("taskkill.exe");
        let status = Command::new(taskkill)
            .args(["/PID", &self.pid.to_string(), "/T", "/F"])
            .status()
            .map_err(|error| error.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "taskkill failed with {}",
                status.code().unwrap_or(-1)
            ))
        }
    }
    fn wait(&mut self) -> Result<i32, String> {
        let process = self
            .process
            .as_ref()
            .ok_or_else(|| "Edge process handle is closed".to_owned())?;
        // SAFETY: `process` is a valid owned process handle.
        if unsafe { WaitForSingleObject(process.as_raw_handle().cast(), u32::MAX) } != WAIT_OBJECT_0
        {
            return Err(last_error("wait for Edge process"));
        }
        self.try_wait()?
            .ok_or_else(|| "Edge remained unsignaled after wait".to_owned())
    }
}

fn spawn_restricted(
    executable: &Path,
    arguments: &[String],
    pair: &WindowsPipePair,
) -> Result<Box<dyn ManagedEdgeChild>, TransportError> {
    let mut command_args = vec![executable.to_string_lossy().into_owned()];
    command_args.extend(arguments.iter().cloned());
    let mut command_line = command_args
        .iter()
        .map(|arg| quote_windows_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let application = wide_null(executable.as_os_str());
    let handles = inherited_handle_allowlist(pair)?;
    // SAFETY: The attribute list is allocated with the size reported by Windows. Its only
    // attribute is HANDLE_LIST containing exactly the two explicitly inheritable child pipe ends.
    unsafe {
        let mut bytes = 0usize;
        let _ = InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut bytes);
        let mut storage = vec![0usize; bytes.div_ceil(std::mem::size_of::<usize>())];
        let list = storage.as_mut_ptr().cast();
        if InitializeProcThreadAttributeList(list, 1, 0, &mut bytes) == 0 {
            return Err(last_error("initialize restricted process handle list").into());
        }
        let update = UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr().cast(),
            std::mem::size_of_val(&handles),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if update == 0 {
            DeleteProcThreadAttributeList(list);
            return Err(last_error("set restricted process handle list").into());
        }
        let mut startup: STARTUPINFOEXW = std::mem::zeroed();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = list;
        let mut process_info: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup.StartupInfo,
            &mut process_info,
        );
        DeleteProcThreadAttributeList(list);
        if created == 0 {
            return Err(last_error("CreateProcessW Edge with restricted pipe handles").into());
        }
        CloseHandle(process_info.hThread);
        Ok(Box::new(Win32EdgeChild {
            process: Some(OwnedHandle::from_raw_handle(process_info.hProcess.cast())),
            pid: process_info.dwProcessId,
        }))
    }
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

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn last_error(context: &str) -> String {
    // SAFETY: GetLastError has no preconditions.
    format!("{context}: Windows error {}", unsafe { GetLastError() })
}

impl From<String> for TransportError {
    fn from(value: String) -> Self {
        TransportError::Process(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct ProtocolScript {
        incoming: VecDeque<String>,
        outgoing: Arc<Mutex<Vec<Value>>>,
        closed: bool,
    }

    impl CdpMessageChannel for ProtocolScript {
        fn send_message(&mut self, text: &str) -> Result<(), TransportError> {
            let command: Value = serde_json::from_str(text).unwrap();
            let id = command["id"].as_u64().unwrap();
            let method = command["method"].as_str().unwrap();
            self.outgoing.lock().unwrap().push(command.clone());
            let response = match method {
                "Browser.getVersion" => {
                    json!({"id":id,"result":{"product":"Microsoft Edge/test","protocolVersion":"1.3","userAgent":"Edge test agent","jsVersion":"test-js"}})
                }
                "Target.getTargets" => {
                    json!({"id":id,"result":{"targetInfos":[{"targetId":"blank-target","type":"page","title":"","url":"about:blank"},{"targetId":"browser-target","type":"browser","title":"","url":""}]}})
                }
                "Target.attachToTarget" => {
                    self.incoming.push_back(json!({"method":"Page.domContentEventFired","params":{"timestamp":1},"sessionId":"session-blank"}).to_string());
                    json!({"id":id,"result":{"sessionId":"session-blank"}})
                }
                "Page.getFrameTree" => {
                    assert_eq!(command["sessionId"], "session-blank");
                    json!({"id":id,"sessionId":"session-blank","result":{"frameTree":{"frame":{"id":"root","url":"about:blank"}}}})
                }
                "Target.detachFromTarget" => json!({"id":id,"result":{}}),
                other => panic!("unexpected CDP method: {other}"),
            };
            self.incoming.push_back(response.to_string());
            Ok(())
        }

        fn receive_message(
            &mut self,
            _timeout: Duration,
        ) -> Result<Option<String>, TransportError> {
            Ok(self.incoming.pop_front())
        }

        fn close(&mut self) -> Result<(), TransportError> {
            self.closed = true;
            Ok(())
        }
    }

    fn test_run() -> (CaptureRun, std::path::PathBuf) {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "chatarium-pipe-native-{}-{stamp}",
            std::process::id()
        ));
        let mut run = CaptureRun::create_diagnostic(&base, "pipe-test").unwrap();
        run.start().unwrap();
        (run, base)
    }

    fn flags(handle: HANDLE) -> Option<u32> {
        let mut flags = 0;
        // SAFETY: GetHandleInformation writes flags for the supplied live handle.
        (unsafe { GetHandleInformation(handle, &mut flags) } != 0).then_some(flags)
    }

    #[test]
    fn only_child_pipe_ends_are_inheritable_and_allowlisted() {
        let pair = WindowsPipePair::create().unwrap();
        let allowlist = inherited_handle_allowlist(&pair).unwrap();
        assert_eq!(allowlist.len(), 2);
        assert_eq!(
            allowlist[0],
            pair.child_read.as_ref().unwrap().as_raw_handle().cast()
        );
        assert_eq!(
            allowlist[1],
            pair.child_write.as_ref().unwrap().as_raw_handle().cast()
        );
        assert_ne!(allowlist[0], allowlist[1]);
        assert_ne!(flags(allowlist[0]).unwrap() & HANDLE_FLAG_INHERIT, 0);
        assert_ne!(flags(allowlist[1]).unwrap() & HANDLE_FLAG_INHERIT, 0);
        assert_eq!(
            flags(pair.parent_read.as_ref().unwrap().as_raw_handle().cast()).unwrap()
                & HANDLE_FLAG_INHERIT,
            0
        );
        assert_eq!(
            flags(pair.parent_write.as_ref().unwrap().as_raw_handle().cast()).unwrap()
                & HANDLE_FLAG_INHERIT,
            0
        );
    }

    #[test]
    fn child_ends_close_in_parent_and_parent_ends_close_on_shutdown() {
        let mut pair = WindowsPipePair::create().unwrap();
        let (child_read, child_write) = pair.child_handle_values().unwrap();
        pair.close_child_ends();
        assert!(flags(child_read as HANDLE).is_none());
        assert!(flags(child_write as HANDLE).is_none());
        let mut channel = pair.into_parent_channel().unwrap();
        channel.close().unwrap();
        // Windows may reuse a closed handle value immediately for another thread or runtime
        // resource, so assert ownership closure instead of querying recycled numeric handles.
        assert!(channel.reader_closed.load(Ordering::Acquire));
        assert!(channel.writer.is_none());
    }

    #[test]
    fn restricted_spawn_failure_drops_pipe_handles() {
        let pair = WindowsPipePair::create().unwrap();
        let (child_read, child_write) = pair.child_handle_values().unwrap();
        let missing = std::env::temp_dir().join("chatarium-definitely-missing-edge.exe");
        assert!(spawn_restricted(&missing, &[], &pair).is_err());
        drop(pair);
        assert!(flags(child_read as HANDLE).is_none());
        assert!(flags(child_write as HANDLE).is_none());
    }

    #[test]
    fn pipe_browser_uses_browser_commands_flattened_sessions_and_read_only_page_command() {
        let (mut run, base) = test_run();
        let script = ProtocolScript::default();
        let sent = script.outgoing.clone();
        let mut browser = PipeCdpBrowserTransport::new(Box::new(script));
        let version = browser.browser_version(&mut run).unwrap();
        assert_eq!(version.browser, "Microsoft Edge/test");
        assert_eq!(version.protocol_version, "1.3");
        let targets = browser.list_targets(&mut run).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].url, "about:blank");
        let mut page = browser.attach("blank-target", &mut run).unwrap();
        let event = page.next_event(Duration::from_millis(1)).unwrap().unwrap();
        assert_eq!(event.method, "Page.domContentEventFired");
        assert_eq!(event.session_id.as_deref(), Some("session-blank"));
        let frame = page
            .command("Page.getFrameTree", json!({}), Duration::from_secs(1))
            .unwrap();
        assert_eq!(frame["frameTree"]["frame"]["url"], "about:blank");
        page.close().unwrap();
        browser.close().unwrap();

        let commands = sent.lock().unwrap();
        assert_eq!(commands[0]["method"], "Browser.getVersion");
        assert_eq!(commands[1]["method"], "Target.getTargets");
        assert_eq!(commands[2]["method"], "Target.attachToTarget");
        assert_eq!(commands[2]["params"]["flatten"], true);
        assert_eq!(commands[3]["method"], "Page.getFrameTree");
        assert_eq!(commands[3]["sessionId"], "session-blank");
        assert_eq!(commands[4]["method"], "Target.detachFromTarget");
        assert!(
            run.events()
                .iter()
                .any(|event| event.kind == "devtools_pipe_browser_version"
                    && event.payload["metadata"]["jsVersion"] == "test-js")
        );
        assert!(
            run.events()
                .iter()
                .any(|event| event.kind == "cdp_target_attached"
                    && event.payload["session_id"] == "session-blank")
        );
        drop(run);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn pipe_switches_have_asciiz_handles_and_never_enable_tcp() {
        let args = append_pipe_switches(
            &["about:blank".to_owned()],
            [11usize as HANDLE, 22usize as HANDLE],
        );
        assert!(
            args.iter()
                .any(|arg| arg == "--remote-debugging-pipe=asciiz")
        );
        assert!(
            args.iter()
                .any(|arg| arg == "--remote-debugging-io-pipes=11,22")
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-port"))
        );
    }

    #[test]
    fn windows_command_line_quoting_preserves_spaces_quotes_and_trailing_slashes() {
        assert_eq!(
            quote_windows_arg("C:\\profile directory\\"),
            "\"C:\\profile directory\\\\\""
        );
        assert_eq!(quote_windows_arg("a\"b"), "\"a\\\"b\"");
    }
}
