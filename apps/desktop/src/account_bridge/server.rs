//! Local-only loopback server; original origin/body/time guards preserved.
use super::*;

pub(super) fn serve(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        if shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .shutdown
        {
            break;
        }

        let (stream, peer) = match listener.accept() {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        if shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .shutdown
        {
            break;
        }

        let connection_shared = Arc::clone(&shared);
        let _ = thread::Builder::new()
            .name("chatarium-account-bridge-connection".to_owned())
            .spawn(move || {
                let _ = handle_connection(stream, &connection_shared);
            });
    }
}

pub(super) fn handle_connection(mut stream: TcpStream, shared: &Shared) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    let request = match read_request(&mut stream) {
        Ok(request) => request,
        Err(HttpReadError::PayloadTooLarge) => {
            return write_response(&mut stream, 413, None);
        }
        Err(HttpReadError::Malformed) => {
            return write_response(&mut stream, 400, None);
        }
        Err(HttpReadError::Io(error)) => return Err(error),
    };

    if request.method == "OPTIONS" {
        return write_response(&mut stream, 403, None);
    }

    if request
        .headers
        .get("origin")
        .is_some_and(|origin| !is_extension_origin(origin))
    {
        return write_response(&mut stream, 403, None);
    }

    if request.headers.get(BRIDGE_HEADER).map(String::as_str) != Some(BRIDGE_HEADER_VALUE) {
        return write_response(&mut stream, 403, None);
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/v1/next") => handle_next(&mut stream, shared),
        ("POST", "/v1/result") => handle_result(&mut stream, shared, &request.body),
        _ => write_response(&mut stream, 404, None),
    }
}

pub(super) fn is_extension_origin(origin: &str) -> bool {
    let Some(extension_id) = origin
        .strip_prefix("chrome-extension://")
        .map(|value| value.trim_end_matches('/'))
    else {
        return false;
    };
    extension_id.len() == 32 && extension_id.bytes().all(|byte| matches!(byte, b'a'..=b'p'))
}

pub(super) fn handle_next(stream: &mut TcpStream, shared: &Shared) -> io::Result<()> {
    let deadline = Instant::now() + NEXT_WAIT;
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    loop {
        if state.shutdown {
            return write_response(stream, 204, None);
        }
        if let Some(command) = state.queued.pop_front() {
            diagnostics::debug(
                "bridge",
                format!("delivering command id={} kind={}", command.id, command.kind),
            );
            state.inflight.insert(command.id.clone(), command.kind);
            let body = serde_json::to_vec(&command.body)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            return write_response(stream, 200, Some(("application/json", &body)));
        }

        let now = Instant::now();
        if now >= deadline {
            return write_response(stream, 204, None);
        }
        let remaining = deadline.saturating_duration_since(now);
        let waited = shared
            .changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|error| error.into_inner());
        state = waited.0;
    }
}

pub(super) fn handle_result(
    stream: &mut TcpStream,
    shared: &Shared,
    body: &[u8],
) -> io::Result<()> {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return write_response(stream, 400, None),
    };
    let Some(object) = value.as_object() else {
        return write_response(stream, 400, None);
    };
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return write_response(stream, 400, None);
    }
    let Some(id) = object.get("id").and_then(Value::as_str) else {
        return write_response(stream, 400, None);
    };
    let Some(kind) = object.get("kind").and_then(Value::as_str) else {
        return write_response(stream, 400, None);
    };

    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let Some(expected_kind) = state.inflight.get(id).copied() else {
        return write_response(stream, 409, None);
    };
    if expected_kind != kind {
        return write_response(stream, 409, None);
    }
    diagnostics::debug(
        "bridge",
        format!(
            "accepted result id={id} kind={kind} {}",
            result_diagnostic_summary(&value)
        ),
    );
    state.inflight.remove(id);
    state.results.insert(id.to_owned(), value);
    shared.changed.notify_all();
    write_response(stream, 204, None)
}

pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: BTreeMap<String, String>,
    pub(super) body: Vec<u8>,
}

pub(super) enum HttpReadError {
    Io(io::Error),
    Malformed,
    PayloadTooLarge,
}

pub(super) fn read_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpReadError> {
    let mut bytes = Vec::new();
    let header_end;
    loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).map_err(HttpReadError::Io)?;
        if read == 0 {
            return Err(HttpReadError::Malformed);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HEADER_BYTES && find_header_end(&bytes).is_none() {
            return Err(HttpReadError::PayloadTooLarge);
        }
        if let Some(end) = find_header_end(&bytes) {
            header_end = end;
            break;
        }
    }

    let header_text =
        std::str::from_utf8(&bytes[..header_end]).map_err(|_| HttpReadError::Malformed)?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().ok_or(HttpReadError::Malformed)?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or(HttpReadError::Malformed)?
        .to_owned();
    let path = request_parts
        .next()
        .ok_or(HttpReadError::Malformed)?
        .to_owned();
    let version = request_parts.next().ok_or(HttpReadError::Malformed)?;
    if request_parts.next().is_some() || version != "HTTP/1.1" || !path.starts_with('/') {
        return Err(HttpReadError::Malformed);
    }

    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpReadError::Malformed);
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }

    if headers.contains_key("transfer-encoding") {
        return Err(HttpReadError::Malformed);
    }
    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| HttpReadError::Malformed)?,
        None => 0,
    };
    if content_length > MAX_RESULT_BODY_BYTES {
        return Err(HttpReadError::PayloadTooLarge);
    }

    let body_start = header_end + 4;
    while bytes.len() < body_start + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).map_err(HttpReadError::Io)?;
        if read == 0 {
            return Err(HttpReadError::Malformed);
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > body_start + MAX_RESULT_BODY_BYTES {
            return Err(HttpReadError::PayloadTooLarge);
        }
    }
    let body = bytes[body_start..body_start + content_length].to_vec();

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

pub(super) fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

pub(super) fn write_response(
    stream: &mut TcpStream,
    status: u16,
    body: Option<(&str, &[u8])>,
) -> io::Result<()> {
    write_response_with_extra_headers(stream, status, body, None)
}

pub(super) fn write_response_with_extra_headers(
    stream: &mut TcpStream,
    status: u16,
    body: Option<(&str, &[u8])>,
    extra_headers: Option<&[(&str, &str)]>,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Error",
    };
    let (content_type, bytes) = body.unwrap_or(("", &[]));
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n",
        bytes.len()
    )?;
    if !content_type.is_empty() {
        write!(stream, "Content-Type: {content_type}\r\n")?;
    }
    if let Some(extra_headers) = extra_headers {
        for (name, value) in extra_headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
    }
    write!(stream, "\r\n")?;
    stream.write_all(bytes)?;
    stream.flush()
}
