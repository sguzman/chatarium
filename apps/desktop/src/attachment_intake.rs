//! Bounded, side-effect-free preparation for local SIWC file input.
//!
//! A prepared object lives only in memory. UI staging, durable attachment
//! authorship, backup and dispatch must be wired separately; preparation
//! alone is deliberately not a remote-send authorization.

use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub const MAX_ATTACHMENT_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    File,
    Image,
}

#[derive(Debug, Clone)]
pub struct PreparedAttachment {
    pub filename: String,
    pub kind: AttachmentKind,
    pub mime: &'static str,
    pub byte_len: usize,
    /// Source observation time, not an assertion of server delivery.
    pub prepared_at_unix_ms: u64,
    pub read_elapsed_ms: f64,
    pub encoding_elapsed_ms: f64,
    /// Contains the complete local content: never include in diagnostics.
    pub data_url: String,
}

impl PreparedAttachment {
    #[must_use]
    pub fn response_content(&self) -> Value {
        match self.kind {
            AttachmentKind::File => json!({
                "type": "input_file",
                "filename": self.filename,
                "file_data": self.data_url,
            }),
            AttachmentKind::Image => json!({
                "type": "input_image",
                "image_url": self.data_url,
            }),
        }
    }

    /// Atomically persisted with authored text; this intentionally includes
    /// encoded bytes and must never be printed in logs.
    #[must_use]
    pub fn durable_record(&self) -> Value {
        let mut record = self.timing_metadata();
        record["content"] = self.response_content();
        record
    }

    /// Metadata-only envelope for future journal provenance. No source
    /// path or attachment bytes are included.
    #[must_use]
    pub fn timing_metadata(&self) -> Value {
        json!({
            "filename": self.filename,
            "mime": self.mime,
            "byte_len": self.byte_len,
            "prepared_at_unix_ms": self.prepared_at_unix_ms,
            "read_elapsed_ms": self.read_elapsed_ms,
            "encoding_elapsed_ms": self.encoding_elapsed_ms,
        })
    }
}

/// Attach a staged source only to the final user turn in the composed input.
/// A previous message with identical wording is never a valid fallback.
pub fn attach_to_authored_input(
    mut input: Value,
    authored_text: &str,
    content: Value,
) -> Result<Value, String> {
    let rows = input
        .as_array_mut()
        .ok_or_else(|| "composed Responses input is not an array".to_owned())?;
    let row = rows
        .iter_mut()
        .rev()
        .find(|row| row.get("role").and_then(Value::as_str) == Some("user"))
        .ok_or_else(|| "composed Responses input has no user turn".to_owned())?;
    if row.get("content").and_then(Value::as_str) != Some(authored_text) {
        return Err("last user turn does not match exact authored attachment text".to_owned());
    }
    row["content"] = json!([
        {"type": "input_text", "text": authored_text},
        content
    ]);
    Ok(input)
}

fn file_type(path: &Path) -> Result<(String, AttachmentKind, &'static str), String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "attachment name is not valid UTF-8".to_owned())?;
    if filename.is_empty() || filename.len() > 128 || filename.chars().any(char::is_control) {
        return Err("attachment filename is invalid".to_owned());
    }
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (kind, mime) = match extension.as_str() {
        "txt" | "md" => (AttachmentKind::File, "text/plain"),
        "csv" => (AttachmentKind::File, "text/csv"),
        "json" => (AttachmentKind::File, "application/json"),
        "pdf" => (AttachmentKind::File, "application/pdf"),
        "png" => (AttachmentKind::Image, "image/png"),
        "jpg" | "jpeg" => (AttachmentKind::Image, "image/jpeg"),
        "webp" => (AttachmentKind::Image, "image/webp"),
        _ => return Err("unsupported attachment file type".to_owned()),
    };
    Ok((filename.to_owned(), kind, mime))
}

/// Read a single, explicitly chosen regular file with a hard byte ceiling.
/// Symlinks and directories are not valid attachment sources.
pub fn prepare(path: &Path) -> Result<PreparedAttachment, String> {
    let (filename, kind, mime) = file_type(path)?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect attachment: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("attachment must be a regular, non-symlink file".to_owned());
    }
    if metadata.len() > MAX_ATTACHMENT_BYTES as u64 {
        return Err("attachment exceeds the 128 KiB local intake limit".to_owned());
    }
    let read_start = Instant::now();
    let file = File::open(path).map_err(|error| format!("cannot open attachment: {error}"))?;
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(MAX_ATTACHMENT_BYTES));
    file.take((MAX_ATTACHMENT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read attachment: {error}"))?;
    let read_elapsed_ms = read_start.elapsed().as_secs_f64() * 1_000.0;
    if bytes.is_empty() {
        return Err("empty attachments are not supported".to_owned());
    }
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err("attachment grew beyond the 128 KiB intake limit".to_owned());
    }
    let encode_start = Instant::now();
    let data_url = format!("data:{mime};base64,{}", encode_base64(&bytes));
    let encoding_elapsed_ms = encode_start.elapsed().as_secs_f64() * 1_000.0;
    let prepared_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    Ok(PreparedAttachment {
        filename,
        kind,
        mime,
        byte_len: bytes.len(),
        prepared_at_unix_ms,
        read_elapsed_ms,
        encoding_elapsed_ms,
        data_url,
    })
}

fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = *chunk.get(1).unwrap_or(&0);
        let c = *chunk.get(2).unwrap_or(&0);
        encoded.push(TABLE[(a >> 2) as usize] as char);
        encoded.push(TABLE[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        encoded.push(if chunk.len() >= 2 {
            TABLE[(((b & 15) << 2) | (c >> 6)) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() == 3 {
            TABLE[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(extension: &str, data: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "chatarium-attachment-{}-{}.{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            extension,
        ));
        let mut file = File::create(&path).unwrap();
        file.write_all(data).unwrap();
        path
    }

    #[test]
    fn encodes_base64_padding_without_external_dependencies() {
        for (bytes, expected) in [
            (&b""[..], ""),
            (&b"f"[..], "Zg=="),
            (&b"fo"[..], "Zm8="),
            (&b"foo"[..], "Zm9v"),
            (&b"foob"[..], "Zm9vYg=="),
        ] {
            assert_eq!(encode_base64(bytes), expected);
        }
    }

    #[test]
    fn file_input_has_exact_bytes_and_metadata_only_timing() {
        let path = fixture("txt", b"Hello world");
        let prepared = prepare(&path).unwrap();
        assert_eq!(prepared.byte_len, 11);
        assert_eq!(prepared.response_content()["type"], "input_file");
        assert_eq!(
            prepared.response_content()["file_data"],
            "data:text/plain;base64,SGVsbG8gd29ybGQ="
        );
        assert!(prepared.read_elapsed_ms.is_finite());
        assert!(prepared.encoding_elapsed_ms.is_finite());
        let meta = prepared.timing_metadata();
        assert!(meta.get("file_data").is_none());
        assert!(meta.get("source_path").is_none());
        assert!(meta.get("prepared_at_unix_ms").is_some());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn attaches_only_to_latest_exact_authored_user_message() {
        let input = json!([
            {"role":"user", "content":"same"},
            {"role":"assistant", "content":"the source"},
            {"role":"user", "content":"same"},
            {"role":"system", "content":"irrelevant"}
        ]);
        let file = json!({"type":"input_file","filename":"a.txt","file_data":"data:text/plain;base64,YQ=="});
        let output = attach_to_authored_input(input, "same", file.clone()).unwrap();
        assert_eq!(output[0]["content"], "same");
        assert_eq!(output[1]["content"], "the source");
        assert_eq!(output[2]["content"][0]["type"], "input_text");
        assert_eq!(output[2]["content"][0]["text"], "same");
        assert_eq!(output[2]["content"][1], file);
        assert_eq!(output[3]["content"], "irrelevant");
    }

    #[test]
    fn stale_identical_text_is_not_a_safe_attachment_target() {
        let input = json!([
            {"role": "user", "content": "repeat"},
            {"role": "assistant", "content": "ok"},
            {"role": "user", "content": "newer"}
        ]);
        assert!(attach_to_authored_input(
            input,
            "repeat",
            json!({"type": "input_file", "filename": "file.txt", "file_data": "x"})
        )
        .unwrap_err()
        .contains("last user turn"));
    }

    #[test]
    fn missing_authored_turn_fails_closed_without_fallback() {
        assert!(
            attach_to_authored_input(
                json!([{"role": "user", "content": "other"}]),
                "wanted",
                json!({"type": "input_image", "image_url": "x"})
            )
            .is_err()
        );
    }

    #[test]
    fn image_input_uses_correct_content_shape() {
        let path = fixture("png", b"PNG-data");
        let prepared = prepare(&path).unwrap();
        assert_eq!(prepared.response_content()["type"], "input_image");
        assert_eq!(
            prepared.response_content()["image_url"],
            "data:image/png;base64,UE5HLWRhdGE="
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn oversized_and_unsupported_files_fail_closed() {
        let large = fixture("pdf", &vec![b'a'; MAX_ATTACHMENT_BYTES + 1]);
        assert!(prepare(&large).unwrap_err().contains("limit"));
        fs::remove_file(large).unwrap();
        let unsupported = fixture("sh", b"echo hello");
        assert!(prepare(&unsupported).unwrap_err().contains("unsupported"));
        fs::remove_file(unsupported).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_refused_even_when_target_is_safe() {
        let target = fixture("txt", b"safe");
        let link = target.with_extension("md");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(prepare(&link).unwrap_err().contains("non-symlink"));
        fs::remove_file(link).unwrap();
        fs::remove_file(target).unwrap();
    }
}
