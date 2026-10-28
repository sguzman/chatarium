//! selection logic mechanically isolated from the fail-closed flight sanitization pipeline.
use super::*;

pub(super) fn validate_experiment(experiment: &ExperimentDefinition) -> Result<(), String> {
    if experiment.schema != "chatarium-experiment" || experiment.version != 1 {
        return Err(format!(
            "unsupported experiment definition {} v{}",
            experiment.schema, experiment.version
        ));
    }
    if experiment.id.trim().is_empty() {
        return Err("experiment id is empty".to_owned());
    }
    if experiment.action.kind == "send_text"
        && experiment.action.text.as_deref().unwrap_or("").is_empty()
    {
        return Err("send_text experiment is missing action.text".to_owned());
    }
    if experiment.success.kind == "assistant_text_contains"
        && experiment.success.text.as_deref().unwrap_or("").is_empty()
    {
        return Err("assistant_text_contains experiment is missing success.text".to_owned());
    }
    Ok(())
}

pub(super) fn validate_export(export: &Value) -> Result<(), String> {
    let format = export.get("format").and_then(Value::as_str);
    let version = export.get("version").and_then(Value::as_u64);
    if format != Some("chatarium-flight-recorder-export") {
        return Err("input is not a chatarium-flight-recorder-export".to_owned());
    }
    if version != Some(3) {
        return Err(format!(
            "unsupported Flight Recorder export version {}",
            version.map_or_else(|| "?".to_owned(), |value| value.to_string())
        ));
    }
    if export.get("events").and_then(Value::as_array).is_none() {
        return Err("Flight Recorder export is missing events".to_owned());
    }
    Ok(())
}

pub(super) fn validate_run_matches_experiment(
    export: &Value,
    selected: &SelectedRun,
    experiment: &ExperimentDefinition,
) -> Result<(), String> {
    if experiment.action.kind != "send_text" {
        return Ok(());
    }

    let expected = experiment
        .action
        .text
        .as_deref()
        .ok_or_else(|| "send_text experiment is missing action.text".to_owned())?;
    let matched = export
        .get("sendIntents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|intent| {
            intent
                .get("at")
                .and_then(Value::as_str)
                .is_some_and(|at| at >= selected.started_at.as_str())
                && intent.get("text").and_then(Value::as_str) == Some(expected)
        });

    if !matched {
        return Err(format!(
            "latest recorder run does not contain the exact action text for experiment {}",
            experiment.id
        ));
    }
    Ok(())
}

pub(super) fn is_read_experiment(experiment: &ExperimentDefinition) -> bool {
    matches!(
        experiment.action.kind.as_str(),
        "observe_conversation_list" | "open_existing_conversation"
    )
}

pub(super) fn select_run_for_experiment(
    export: &Value,
    experiment: &ExperimentDefinition,
) -> Result<SelectedRun, String> {
    if is_read_experiment(experiment) {
        select_latest_read_capture_run(export)
    } else {
        select_latest_run(export)
    }
}

pub(super) fn select_latest_read_capture_run(export: &Value) -> Result<SelectedRun, String> {
    let events = export
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| "Flight Recorder export is missing events".to_owned())?;

    let recorder_start = events
        .iter()
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some("recorder-started"))
        .max_by_key(|event| event.get("seq").and_then(Value::as_u64).unwrap_or_default())
        .ok_or_else(|| "Flight Recorder export contains no recorder-started event".to_owned())?;
    let recorder_started_seq = recorder_start
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| "latest recorder-started event has no numeric seq".to_owned())?;

    let arm = events
        .iter()
        .filter(|event| {
            event.get("kind").and_then(Value::as_str) == Some("protocol-read-capture-armed")
                && event
                    .get("seq")
                    .and_then(Value::as_u64)
                    .is_some_and(|seq| seq >= recorder_started_seq)
        })
        .max_by_key(|event| event.get("seq").and_then(Value::as_u64).unwrap_or_default())
        .ok_or_else(|| {
            "latest recorder run contains no protocol-read-capture-armed event".to_owned()
        })?;

    let started_seq = arm
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| "latest protocol-read-capture-armed event has no numeric seq".to_owned())?;
    let started_at = arm
        .get("at")
        .and_then(Value::as_str)
        .ok_or_else(|| "latest protocol-read-capture-armed event has no timestamp".to_owned())?
        .to_owned();
    let run_id = arm.pointer("/payload/runId").and_then(Value::as_str);
    let recorder_version = recorder_start
        .pointer("/payload/version")
        .and_then(Value::as_str)
        .or_else(|| export.get("recorderVersion").and_then(Value::as_str))
        .unwrap_or("?")
        .to_owned();

    let legacy_disarm_seq = run_id
        .is_none()
        .then(|| {
            events
                .iter()
                .filter(|event| {
                    event.get("kind").and_then(Value::as_str)
                        == Some("protocol-read-capture-disarmed")
                        && event
                            .get("seq")
                            .and_then(Value::as_u64)
                            .is_some_and(|seq| seq >= started_seq)
                })
                .filter_map(|event| event.get("seq").and_then(Value::as_u64))
                .min()
        })
        .flatten();

    let matching_read_event = |event: &&Value| {
        let Some(seq) = event.get("seq").and_then(Value::as_u64) else {
            return false;
        };
        if seq < started_seq {
            return false;
        }
        let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
        if !kind.starts_with("protocol-read-") {
            return false;
        }
        match run_id {
            Some(expected) => {
                event.pointer("/payload/runId").and_then(Value::as_str) == Some(expected)
            }
            None => legacy_disarm_seq.is_none_or(|disarm_seq| seq <= disarm_seq),
        }
    };

    let selected_events = events
        .iter()
        .filter(matching_read_event)
        .cloned()
        .collect::<Vec<_>>();

    let has_disarm = selected_events.iter().any(|event| {
        event.get("kind").and_then(Value::as_str) == Some("protocol-read-capture-disarmed")
    });
    if !has_disarm {
        return Err("latest protocol-read capture run is not durably disarmed".to_owned());
    }

    let has_capture = selected_events.iter().any(|event| {
        event.get("kind").and_then(Value::as_str) == Some("protocol-read-response-captured")
    });
    if !has_capture {
        return Err(
            "latest protocol-read capture run contains no captured JSON response".to_owned(),
        );
    }

    Ok(SelectedRun {
        started_seq,
        started_at,
        recorder_version,
        events: selected_events,
    })
}

pub(super) fn select_latest_run(export: &Value) -> Result<SelectedRun, String> {
    let events = export
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| "Flight Recorder export is missing events".to_owned())?;

    let start = events
        .iter()
        .filter(|event| event.get("kind").and_then(Value::as_str) == Some("recorder-started"))
        .max_by_key(|event| event.get("seq").and_then(Value::as_u64).unwrap_or_default())
        .ok_or_else(|| "Flight Recorder export contains no recorder-started event".to_owned())?;

    let started_seq = start
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| "latest recorder-started event has no numeric seq".to_owned())?;
    let started_at = start
        .get("at")
        .and_then(Value::as_str)
        .ok_or_else(|| "latest recorder-started event has no timestamp".to_owned())?
        .to_owned();
    let recorder_version = start
        .pointer("/payload/version")
        .and_then(Value::as_str)
        .or_else(|| export.get("recorderVersion").and_then(Value::as_str))
        .unwrap_or("?")
        .to_owned();

    let selected_events = events
        .iter()
        .filter(|event| {
            event
                .get("seq")
                .and_then(Value::as_u64)
                .is_some_and(|seq| seq >= started_seq)
        })
        .cloned()
        .collect::<Vec<_>>();

    Ok(SelectedRun {
        started_seq,
        started_at,
        recorder_version,
        events: selected_events,
    })
}
