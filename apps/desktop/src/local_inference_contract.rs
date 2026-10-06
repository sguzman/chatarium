use crate::capability_probes::ProbeRun;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedContract {
    pub state: String,
    pub profile_id: Option<String>,
    pub model: String,
    pub probe_generated_unix_ms: u64,
    pub capabilities: BTreeMap<String, String>,
}

impl LoadedContract {
    #[must_use]
    pub fn ready_for(&self, profile_id: Option<&str>, model: &str) -> bool {
        self.state == "ready" && self.profile_id.as_deref() == profile_id && self.model == model
    }
}

const EXPECTED_PROBES: [&str; 9] = [
    "baseline",
    "image_input",
    "file_input",
    "function_tools",
    "additional_tools",
    "web_search",
    "reasoning",
    "verbosity",
    "structured_output",
];

#[must_use]
pub fn contract_state(run: &ProbeRun) -> &'static str {
    if run.model.is_none() || run.profile_id.is_none() || run.generated_unix_ms.is_none() {
        return "incomplete";
    }

    let results = run
        .results
        .iter()
        .map(|result| (result.name.as_str(), result.status.as_str()))
        .collect::<BTreeMap<_, _>>();

    let all_terminal = EXPECTED_PROBES.iter().all(|name| {
        matches!(
            results.get(name).copied(),
            Some("supported" | "rejected" | "unsupported_route")
        )
    });
    if !all_terminal {
        return "incomplete";
    }

    if EXPECTED_PROBES
        .iter()
        .any(|name| results.get(name).copied() == Some("rejected"))
    {
        "needs_review"
    } else {
        "ready"
    }
}

pub fn load_contract(path: &Path) -> Result<Option<LoadedContract>, String> {
    let encoded = match fs::read(path) {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not read local inference contract {}: {error}",
                path.display()
            ));
        }
    };
    let value: Value = serde_json::from_slice(&encoded).map_err(|error| {
        format!(
            "invalid local inference contract {}: {error}",
            path.display()
        )
    })?;
    if value.get("schema").and_then(Value::as_str) != Some("chatarium-local-inference-contract")
        || value.get("version").and_then(Value::as_u64) != Some(1)
    {
        return Err(format!(
            "unsupported local inference contract format at {}",
            path.display()
        ));
    }

    let state = value
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("local inference contract {} has no state", path.display()))?;
    if !matches!(state, "ready" | "needs_review" | "incomplete") {
        return Err(format!(
            "local inference contract {} has unknown state {state}",
            path.display()
        ));
    }

    let evidence = value
        .get("evidence")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            format!(
                "local inference contract {} has no evidence",
                path.display()
            )
        })?;
    let model = evidence
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("local inference contract {} has no model", path.display()))?;
    let profile_id = evidence
        .get("profile_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let probe_generated_unix_ms = evidence
        .get("probe_generated_unix_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!(
                "local inference contract {} has no probe timestamp",
                path.display()
            )
        })?;

    let empirical = value
        .get("empirical_capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            format!(
                "local inference contract {} has no empirical capabilities",
                path.display()
            )
        })?;
    let mut capabilities = BTreeMap::new();
    for item in empirical {
        let name = item.get("name").and_then(Value::as_str).ok_or_else(|| {
            format!(
                "local inference contract {} has a capability without a name",
                path.display()
            )
        })?;
        let status = item.get("status").and_then(Value::as_str).ok_or_else(|| {
            format!(
                "local inference contract {} has a capability without a status",
                path.display()
            )
        })?;
        if capabilities
            .insert(name.to_owned(), status.to_owned())
            .is_some()
        {
            return Err(format!(
                "local inference contract {} has duplicate capability {name}",
                path.display()
            ));
        }
    }

    Ok(Some(LoadedContract {
        state: state.to_owned(),
        profile_id,
        model: model.to_owned(),
        probe_generated_unix_ms,
        capabilities,
    }))
}

pub fn save_contract(path: &Path, run: &ProbeRun, generated_unix_ms: u64) -> Result<(), String> {
    let model = run
        .model
        .as_deref()
        .ok_or_else(|| "cannot derive local inference contract without a model".to_owned())?;
    let probe_generated_unix_ms = run.generated_unix_ms.ok_or_else(|| {
        "cannot derive local inference contract without probe evidence timestamp".to_owned()
    })?;

    let payload = json!({
        "schema": "chatarium-local-inference-contract",
        "version": 1,
        "generated_unix_ms": generated_unix_ms,
        "state": contract_state(run),
        "evidence": {
            "profile_id": run.profile_id,
            "model": model,
            "probe_generated_unix_ms": probe_generated_unix_ms,
        },
        "local_guarantees": {
            "context_ownership": "local",
            "conversation_isolation": true,
            "server_response_storage": false,
            "streaming": "required",
            "history_continuation": "resend_required_context",
            "roles": ["user", "assistant", "developer"],
            "top_level_instructions": true,
            "active_response_cancellation": true,
        },
        "fixed_constraints": {
            "persistent_responses_conversation": false,
            "previous_response_id": false,
            "explicit_system_role_message": false,
            "nonstreaming_http_inference": false,
        },
        "empirical_capabilities": run.results.iter().map(|result| json!({
            "name": result.name,
            "status": result.status,
            "code": result.code,
            "status_code": result.status_code,
            "text_received": result.text_received,
            "reason": result.reason,
        })).collect::<Vec<Value>>(),
    });

    let encoded = serde_json::to_vec_pretty(&payload)
        .map_err(|error| format!("could not encode local inference contract: {error}"))?;
    fs::write(path, encoded).map_err(|error| format!("could not write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability_probes::ProbeResult;

    fn result(name: &str, status: &str) -> ProbeResult {
        ProbeResult {
            name: name.to_owned(),
            status: status.to_owned(),
            code: None,
            status_code: None,
            text_received: None,
            reason: None,
        }
    }

    #[test]
    fn contract_state_distinguishes_ready_review_and_incomplete() {
        let mut run = ProbeRun {
            model: Some("gpt-example".to_owned()),
            profile_id: Some("profile-1".to_owned()),
            generated_unix_ms: Some(1234),
            ..ProbeRun::default()
        };
        run.results = EXPECTED_PROBES
            .iter()
            .map(|name| result(name, "supported"))
            .collect();
        assert_eq!(contract_state(&run), "ready");

        run.results[3].status = "unsupported_route".to_owned();
        assert_eq!(contract_state(&run), "ready");

        run.results[4].status = "rejected".to_owned();
        assert_eq!(contract_state(&run), "needs_review");

        run.results[5].status = "error".to_owned();
        assert_eq!(contract_state(&run), "incomplete");

        run.results[5].status = "supported".to_owned();
        run.profile_id = None;
        assert_eq!(contract_state(&run), "incomplete");
    }

    #[test]
    fn derived_contract_keeps_fixed_and_empirical_layers_separate() {
        let path = std::env::temp_dir().join(format!(
            "chatarium-local-inference-contract-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);

        let mut run = ProbeRun {
            model: Some("gpt-example".to_owned()),
            profile_id: Some("profile-1".to_owned()),
            generated_unix_ms: Some(1234),
            ..ProbeRun::default()
        };
        run.results = EXPECTED_PROBES
            .iter()
            .map(|name| result(name, "supported"))
            .collect();

        save_contract(&path, &run, 5678).unwrap();
        let loaded = load_contract(&path).unwrap().unwrap();
        assert!(loaded.ready_for(Some("profile-1"), "gpt-example"));
        assert!(!loaded.ready_for(Some("profile-2"), "gpt-example"));
        assert!(!loaded.ready_for(Some("profile-1"), "gpt-other"));
        assert_eq!(
            loaded.capabilities.get("reasoning").map(String::as_str),
            Some("supported")
        );

        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["state"], "ready");
        assert_eq!(value["evidence"]["model"], "gpt-example");
        assert_eq!(value["evidence"]["profile_id"], "profile-1");
        assert_eq!(value["local_guarantees"]["context_ownership"], "local");
        assert_eq!(value["fixed_constraints"]["previous_response_id"], false);
        assert_eq!(
            value["empirical_capabilities"].as_array().unwrap().len(),
            EXPECTED_PROBES.len()
        );
        let _ = fs::remove_file(path);
    }
}
