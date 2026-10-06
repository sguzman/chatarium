use crate::siwc_bridge::BridgeError;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fs;
use std::path::Path;

const TINY_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
const TINY_TEXT: &str = "data:text/plain;base64,Q2hhdGFyaXVtIGNhcGFiaWxpdHkgcHJvYmUuCg==";

#[derive(Debug, Clone)]
pub struct ProbeSpec {
    pub name: &'static str,
    pub input: Value,
    pub request_patch: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub name: String,
    pub status: String,
    pub code: Option<String>,
    pub status_code: Option<u16>,
    pub param: Option<String>,
    pub text_received: Option<bool>,
    pub reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct ProbeRun {
    pub model: Option<String>,
    pub profile_id: Option<String>,
    pub generated_unix_ms: Option<u64>,
    pub queue: VecDeque<ProbeSpec>,
    pub active: Option<String>,
    pub results: Vec<ProbeResult>,
    pub status: String,
}

impl ProbeRun {
    pub fn start(&mut self, model: String, profile_id: Option<String>) {
        self.model = Some(model);
        self.profile_id = profile_id;
        self.generated_unix_ms = None;
        self.queue = definitions();
        self.active = None;
        self.results.clear();
        self.status = format!("queued {} capability probes", self.queue.len());
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.active.is_some() || !self.queue.is_empty()
    }

    pub fn next(&mut self) -> Option<ProbeSpec> {
        self.queue.pop_front()
    }

    pub fn mark_active(&mut self, name: &str) {
        self.active = Some(name.to_owned());
        self.status = format!("probing {name}…");
    }

    pub fn complete_supported(&mut self, name: &str, text_received: bool) {
        self.active = None;
        self.results.push(ProbeResult {
            name: name.to_owned(),
            status: "supported".to_owned(),
            code: None,
            status_code: None,
            param: None,
            text_received: Some(text_received),
            reason: None,
        });
    }

    pub fn complete_failed(&mut self, name: &str, error: &BridgeError) {
        self.active = None;
        self.results.push(ProbeResult {
            name: name.to_owned(),
            status: classify_error(error).to_owned(),
            code: Some(error.code.clone()),
            status_code: error.status,
            param: error.param.clone(),
            text_received: None,
            reason: Some(error.message.clone()),
        });
        if name == "baseline" {
            self.skip_remaining("baseline_failed");
        }
    }

    pub fn complete_dispatch_error(&mut self, name: &str, error: String) {
        self.active = None;
        self.results.push(ProbeResult {
            name: name.to_owned(),
            status: "error".to_owned(),
            code: Some("bridge_dispatch_failed".to_owned()),
            status_code: None,
            param: None,
            text_received: None,
            reason: Some(error),
        });
        if name == "baseline" {
            self.skip_remaining("baseline_failed");
        }
    }

    pub fn abort(&mut self, reason: &str) {
        if let Some(name) = self.active.take() {
            self.results.push(ProbeResult {
                name,
                status: "error".to_owned(),
                code: Some("runtime_unavailable".to_owned()),
                status_code: None,
                param: None,
                text_received: None,
                reason: Some(reason.to_owned()),
            });
        }
        self.skip_remaining("runtime_unavailable");
        self.status = format!("capability probes aborted · {reason}");
    }

    fn skip_remaining(&mut self, reason: &str) {
        while let Some(spec) = self.queue.pop_front() {
            self.results.push(ProbeResult {
                name: spec.name.to_owned(),
                status: "not_run".to_owned(),
                code: None,
                status_code: None,
                param: None,
                text_received: None,
                reason: Some(reason.to_owned()),
            });
        }
    }
}

#[must_use]
pub fn request_id(name: &str) -> String {
    format!("probe:{name}")
}

#[must_use]
pub fn probe_name_from_request_id(request_id: &str) -> Option<&str> {
    request_id.strip_prefix("probe:")
}

#[must_use]
pub fn classify_error(error: &BridgeError) -> &'static str {
    if error.code == "subscription_sharing_unsupported_capability" {
        return "unsupported_route";
    }
    if error.code == "model_not_found" {
        return "model_unavailable";
    }
    if matches!(
        error.code.as_str(),
        "invalid_request" | "invalid_request_error"
    ) || matches!(error.status, Some(400 | 422))
    {
        return "rejected";
    }
    "error"
}

pub fn load_report(path: &Path) -> Result<Option<ProbeRun>, String> {
    let encoded = match fs::read(path) {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not read capability probe report {}: {error}",
                path.display()
            ));
        }
    };
    let value: Value = serde_json::from_slice(&encoded).map_err(|error| {
        format!(
            "invalid capability probe report {}: {error}",
            path.display()
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some("chatarium-siwc-capability-probe")
        || value.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(format!(
            "unsupported capability probe report format at {}",
            path.display()
        ));
    }

    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let profile_id = value
        .get("profile_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let generated_unix_ms = value.get("generated_unix_ms").and_then(Value::as_u64);
    let probes = value
        .get("probes")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "capability probe report {} has no probes array",
                path.display()
            )
        })?;

    let mut results = Vec::with_capacity(probes.len());
    for probe in probes {
        let name = probe.get("name").and_then(Value::as_str).ok_or_else(|| {
            format!(
                "capability probe report {} has a probe without a name",
                path.display()
            )
        })?;
        let status = probe.get("status").and_then(Value::as_str).ok_or_else(|| {
            format!(
                "capability probe report {} has a probe without a status",
                path.display()
            )
        })?;
        results.push(ProbeResult {
            name: name.to_owned(),
            status: status.to_owned(),
            code: probe
                .get("code")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            status_code: probe
                .get("status_code")
                .and_then(Value::as_u64)
                .and_then(|status| u16::try_from(status).ok()),
            param: probe
                .get("param")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            text_received: probe.get("text_received").and_then(Value::as_bool),
            reason: probe
                .get("reason")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        });
    }

    Ok(Some(ProbeRun {
        model,
        profile_id,
        generated_unix_ms,
        queue: VecDeque::new(),
        active: None,
        results,
        status: format!("loaded {} saved capability probe results", probes.len()),
    }))
}

pub fn save_report(path: &Path, run: &ProbeRun, generated_unix_ms: u64) -> Result<(), String> {
    let payload = json!({
        "schema": "chatarium-siwc-capability-probe",
        "version": 1,
        "generated_unix_ms": generated_unix_ms,
        "model": run.model,
        "profile_id": run.profile_id,
        "probes": run.results.iter().map(|result| json!({
            "name": result.name,
            "status": result.status,
            "code": result.code,
            "status_code": result.status_code,
            "param": result.param,
            "text_received": result.text_received,
            "reason": result.reason,
        })).collect::<Vec<_>>(),
    });
    let encoded = serde_json::to_vec_pretty(&payload)
        .map_err(|error| format!("could not encode capability probe report: {error}"))?;
    fs::write(path, encoded).map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn definitions() -> VecDeque<ProbeSpec> {
    let function_tool = json!({
        "type": "function",
        "name": "echo_probe",
        "description": "Return a supplied probe value unchanged.",
        "parameters": {
            "type": "object",
            "properties": {"value": {"type": "string"}},
            "required": ["value"],
            "additionalProperties": false,
        },
    });

    VecDeque::from(vec![
        ProbeSpec {
            name: "baseline",
            input: json!("Reply with exactly PROBE_OK."),
            request_patch: json!({}),
        },
        ProbeSpec {
            name: "image_input",
            input: json!("Reply with exactly PROBE_OK."),
            request_patch: json!({
                "input": [{
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "This is a capability probe. Reply with exactly IMAGE_OK.",
                        },
                        {"type": "input_image", "image_url": TINY_PNG},
                    ],
                }],
            }),
        },
        ProbeSpec {
            name: "file_input",
            input: json!("Reply with exactly PROBE_OK."),
            request_patch: json!({
                "input": [{
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": "Read the attached capability probe file and reply with exactly FILE_OK.",
                        },
                        {
                            "type": "input_file",
                            "filename": "chatarium-probe.txt",
                            "file_data": TINY_TEXT,
                        },
                    ],
                }],
            }),
        },
        ProbeSpec {
            name: "function_tools",
            input: json!("Reply with exactly FUNCTION_TOOL_OK. Do not call any tool."),
            request_patch: json!({
                "tools": [{
                    "type": "namespace",
                    "name": "chatarium_probe",
                    "description": "Tiny functions used only to probe tool admission.",
                    "tools": [function_tool.clone()],
                }],
            }),
        },
        ProbeSpec {
            name: "additional_tools",
            input: json!("Reply with exactly PROBE_OK."),
            request_patch: json!({
                "input": [
                    {
                        "type": "additional_tools",
                        "role": "developer",
                        "tools": [function_tool],
                    },
                    {
                        "role": "user",
                        "content": "Reply with exactly ADDITIONAL_TOOLS_OK. Do not call any tool.",
                    },
                ],
            }),
        },
        ProbeSpec {
            name: "web_search",
            input: json!("Reply with exactly WEB_SEARCH_OK. Do not search the web."),
            request_patch: json!({"tools": [{"type": "web_search"}]}),
        },
        ProbeSpec {
            name: "reasoning",
            input: json!("Reply with exactly REASONING_OK."),
            request_patch: json!({"reasoning": {"effort": "low"}}),
        },
        ProbeSpec {
            name: "verbosity",
            input: json!("Reply with exactly VERBOSITY_OK."),
            request_patch: json!({"text": {"verbosity": "low"}}),
        },
        ProbeSpec {
            name: "structured_output",
            input: json!("Return JSON with one boolean field named \"ok\" set to true."),
            request_patch: json!({
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": "chatarium_probe",
                        "schema": {
                            "type": "object",
                            "properties": {"ok": {"type": "boolean"}},
                            "required": ["ok"],
                            "additionalProperties": false,
                        },
                        "strict": true,
                    },
                },
            }),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_cover_expected_probe_matrix() {
        let names = definitions()
            .into_iter()
            .map(|probe| probe.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "baseline",
                "image_input",
                "file_input",
                "function_tools",
                "additional_tools",
                "web_search",
                "reasoning",
                "verbosity",
                "structured_output",
            ]
        );
    }

    #[test]
    fn baseline_failure_marks_later_probes_not_run() {
        let mut run = ProbeRun::default();
        run.start("gpt-example".to_owned(), Some("profile-1".to_owned()));
        let baseline = run.next().unwrap();
        run.mark_active(baseline.name);
        run.complete_failed(
            baseline.name,
            &BridgeError {
                code: "invalid_request".to_owned(),
                message: "rejected".to_owned(),
                retryable: false,
                status: Some(400),
                param: Some("tools".to_owned()),
            },
        );
        assert!(!run.running());
        assert_eq!(run.results[0].status, "rejected");
        assert_eq!(run.results[0].param.as_deref(), Some("tools"));
        assert!(
            run.results[1..]
                .iter()
                .all(|result| result.status == "not_run")
        );
    }

    #[test]
    fn saved_probe_report_round_trips_without_credentials() {
        let root =
            std::env::temp_dir().join(format!("chatarium-capability-probe-{}", std::process::id()));
        let _ = fs::remove_file(&root);
        let mut run = ProbeRun::default();
        run.model = Some("gpt-example".to_owned());
        run.profile_id = Some("profile-1".to_owned());
        run.results.push(ProbeResult {
            name: "reasoning".to_owned(),
            status: "supported".to_owned(),
            code: None,
            status_code: None,
            param: None,
            text_received: Some(true),
            reason: None,
        });
        save_report(&root, &run, 1234).unwrap();

        let loaded = load_report(&root).unwrap().unwrap();
        assert_eq!(loaded.model.as_deref(), Some("gpt-example"));
        assert_eq!(loaded.profile_id.as_deref(), Some("profile-1"));
        assert_eq!(loaded.generated_unix_ms, Some(1234));
        assert_eq!(loaded.results, run.results);
        let encoded = fs::read_to_string(&root).unwrap();
        assert!(!encoded.contains("access_token"));
        assert!(!encoded.contains("refresh_token"));
        let _ = fs::remove_file(root);
    }

    #[test]
    fn malformed_probe_report_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "chatarium-capability-probe-malformed-{}",
            std::process::id()
        ));
        fs::write(&root, br#"{"schema":"wrong","version":1,"probes":[]}"#).unwrap();
        assert!(load_report(&root).is_err());
        let _ = fs::remove_file(root);
    }

    #[test]
    fn probe_request_ids_round_trip() {
        let request_id = request_id("reasoning");
        assert_eq!(probe_name_from_request_id(&request_id), Some("reasoning"));
        assert_eq!(probe_name_from_request_id("turn-1"), None);
    }
}
