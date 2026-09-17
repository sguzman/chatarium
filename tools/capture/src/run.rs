//! Durable capture-run metadata and append-only event journal.

use crate::Experiment;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const RUN_SCHEMA: &str = "chatarium-capture-run";
const RUN_SCHEMA_VERSION: u64 = 1;
const EVENT_SCHEMA: &str = "chatarium-capture-event";
const EVENT_SCHEMA_VERSION: u64 = 1;
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Final or in-progress state of a capture experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureRunState {
    /// Run directory exists but browser mutation has not begun.
    Preparing,
    /// Experiment is actively being observed.
    Running,
    /// Experiment and capture completed without known warnings.
    Completed,
    /// Capture completed but one or more non-fatal observations were unavailable.
    CompletedWithWarnings,
    /// The remote outcome cannot be established from observed evidence.
    OutcomeAmbiguous,
    /// Capture failed before any remote mutation began.
    FailedBeforeMutation,
    /// Capture failed after a remote mutation began.
    FailedAfterMutation,
    /// Operator intentionally stopped the capture.
    AbortedByOperator,
}

impl CaptureRunState {
    /// Whether this state is terminal for the capture run.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::CompletedWithWarnings
                | Self::OutcomeAmbiguous
                | Self::FailedBeforeMutation
                | Self::FailedAfterMutation
                | Self::AbortedByOperator
        )
    }
}

/// Durable manifest for one capture run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRunManifest {
    /// Manifest schema name.
    pub schema: String,
    /// Manifest schema version.
    pub version: u64,
    /// Locally unique capture run identifier.
    pub run_id: String,
    /// Canonical experiment identifier.
    pub experiment_id: String,
    /// Whether the experiment definition may mutate remote state.
    pub experiment_mutation: bool,
    /// Exact text action, when the experiment sends text.
    pub exact_action_text: Option<String>,
    /// Exact expected marker, when the experiment defines one.
    pub expected_marker: Option<String>,
    /// Harness crate version that created the run.
    pub harness_version: String,
    /// Current durable run state.
    pub state: CaptureRunState,
    /// Unix epoch milliseconds when the run was created.
    pub started_unix_ms: u128,
    /// Unix epoch milliseconds when the run became terminal.
    pub finished_unix_ms: Option<u128>,
    /// Whether the harness positively crossed the remote-mutation boundary.
    pub remote_mutation_started: bool,
    /// Browser version when known.
    pub edge_version: Option<String>,
    /// CDP protocol version when known.
    pub cdp_protocol_version: Option<String>,
    /// Optional warnings accumulated during finalization.
    pub warnings: Vec<String>,
}

/// One append-only private capture journal record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureEvent {
    /// Event schema name.
    pub schema: String,
    /// Event schema version.
    pub version: u64,
    /// Monotonically increasing run-local sequence number.
    pub sequence: u64,
    /// Unix epoch milliseconds at observation time.
    pub at_unix_ms: u128,
    /// Stable local event kind or original CDP method name.
    pub kind: String,
    /// Structured event payload.
    pub payload: Value,
}

/// Paths owned by one private capture run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureRunPaths {
    /// Private run directory.
    pub root: PathBuf,
    /// Mutable manifest projection.
    pub manifest: PathBuf,
    /// Append-only event journal.
    pub events: PathBuf,
    /// Content-addressed body storage directory.
    pub bodies: PathBuf,
    /// Frontend-asset evidence directory.
    pub frontend: PathBuf,
    /// Finalization metadata path.
    pub finalize: PathBuf,
}

impl CaptureRunPaths {
    fn new(root: PathBuf) -> Self {
        Self {
            manifest: root.join("run.json"),
            events: root.join("events.jsonl"),
            bodies: root.join("bodies"),
            frontend: root.join("frontend"),
            finalize: root.join("finalize.json"),
            root,
        }
    }
}

/// Open durable private state for one capture run.
pub struct CaptureRun {
    paths: CaptureRunPaths,
    manifest: CaptureRunManifest,
    events: Vec<CaptureEvent>,
    event_file: File,
}

impl CaptureRun {
    /// Create a new run directory before any browser or remote mutation occurs.
    pub fn create(base_dir: &Path, experiment: &Experiment) -> Result<Self, String> {
        fs::create_dir_all(base_dir)
            .map_err(|error| format!("create capture base {}: {error}", base_dir.display()))?;

        let started_unix_ms = unix_ms()?;
        let run_id = make_run_id(started_unix_ms);
        let paths = CaptureRunPaths::new(base_dir.join(&run_id));
        fs::create_dir(&paths.root)
            .map_err(|error| format!("create run directory {}: {error}", paths.root.display()))?;
        fs::create_dir(&paths.bodies).map_err(|error| {
            format!("create body directory {}: {error}", paths.bodies.display())
        })?;
        fs::create_dir(&paths.frontend).map_err(|error| {
            format!(
                "create frontend directory {}: {error}",
                paths.frontend.display()
            )
        })?;

        let manifest = CaptureRunManifest {
            schema: RUN_SCHEMA.to_owned(),
            version: RUN_SCHEMA_VERSION,
            run_id,
            experiment_id: experiment.id.clone(),
            experiment_mutation: experiment.mutation,
            exact_action_text: experiment.action.text.clone(),
            expected_marker: experiment.success.text.clone(),
            harness_version: env!("CARGO_PKG_VERSION").to_owned(),
            state: CaptureRunState::Preparing,
            started_unix_ms,
            finished_unix_ms: None,
            remote_mutation_started: false,
            edge_version: None,
            cdp_protocol_version: None,
            warnings: Vec::new(),
        };
        persist_manifest(&paths.manifest, &manifest)?;

        let event_file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .read(true)
            .open(&paths.events)
            .map_err(|error| format!("create event journal {}: {error}", paths.events.display()))?;

        let mut run = Self {
            paths,
            manifest,
            events: Vec::new(),
            event_file,
        };
        run.append_event(
            "run_created",
            serde_json::json!({
                "experiment_id": experiment.id,
                "mutation": experiment.mutation,
            }),
        )?;
        Ok(run)
    }

    /// Reopen an unfinished or finalized run from disk.
    ///
    /// An unterminated malformed final JSON line is treated as a torn final write and truncated.
    /// A malformed complete line remains a hard integrity error.
    pub fn open(root: &Path) -> Result<Self, String> {
        let paths = CaptureRunPaths::new(root.to_path_buf());
        let manifest = read_manifest(&paths.manifest)?;
        let events = recover_and_read_events(&paths.events)?;
        let event_file = OpenOptions::new()
            .append(true)
            .read(true)
            .open(&paths.events)
            .map_err(|error| format!("open event journal {}: {error}", paths.events.display()))?;

        Ok(Self {
            paths,
            manifest,
            events,
            event_file,
        })
    }

    /// Current durable manifest projection.
    #[must_use]
    pub const fn manifest(&self) -> &CaptureRunManifest {
        &self.manifest
    }

    /// Run-owned paths.
    #[must_use]
    pub const fn paths(&self) -> &CaptureRunPaths {
        &self.paths
    }

    /// Events already recovered or appended for this run.
    #[must_use]
    pub fn events(&self) -> &[CaptureEvent] {
        &self.events
    }

    /// Append and fsync one event before returning success.
    pub fn append_event(&mut self, kind: impl Into<String>, payload: Value) -> Result<u64, String> {
        let sequence = self
            .events
            .last()
            .map_or(1, |event| event.sequence.saturating_add(1));
        let event = CaptureEvent {
            schema: EVENT_SCHEMA.to_owned(),
            version: EVENT_SCHEMA_VERSION,
            sequence,
            at_unix_ms: unix_ms()?,
            kind: kind.into(),
            payload,
        };
        let mut encoded = serde_json::to_vec(&event)
            .map_err(|error| format!("serialize capture event #{sequence}: {error}"))?;
        encoded.push(b'\n');
        self.event_file
            .write_all(&encoded)
            .map_err(|error| format!("append event #{sequence}: {error}"))?;
        self.event_file
            .flush()
            .map_err(|error| format!("flush event #{sequence}: {error}"))?;
        self.event_file
            .sync_data()
            .map_err(|error| format!("sync event #{sequence}: {error}"))?;
        self.events.push(event);
        Ok(sequence)
    }

    /// Transition from preparing to actively running.
    pub fn start(&mut self) -> Result<(), String> {
        if self.manifest.state != CaptureRunState::Preparing {
            return Err(format!(
                "cannot start run from state {:?}",
                self.manifest.state
            ));
        }
        self.append_event("run_started", Value::Null)?;
        self.manifest.state = CaptureRunState::Running;
        self.persist_manifest()
    }

    /// Record positive evidence that the experiment crossed the remote-mutation boundary.
    pub fn mark_remote_mutation_started(&mut self, evidence: Value) -> Result<(), String> {
        if !self.manifest.experiment_mutation {
            return Err("non-mutating experiment cannot begin remote mutation".to_owned());
        }
        if self.manifest.state != CaptureRunState::Running {
            return Err("remote mutation may only begin while a run is active".to_owned());
        }
        if self.manifest.remote_mutation_started {
            return Ok(());
        }
        self.append_event("remote_mutation_started", evidence)?;
        self.manifest.remote_mutation_started = true;
        self.persist_manifest()
    }

    /// Record browser/CDP version metadata when it becomes known.
    pub fn set_browser_versions(
        &mut self,
        edge_version: impl Into<String>,
        cdp_protocol_version: impl Into<String>,
    ) -> Result<(), String> {
        self.manifest.edge_version = Some(edge_version.into());
        self.manifest.cdp_protocol_version = Some(cdp_protocol_version.into());
        self.persist_manifest()
    }

    /// Add a non-fatal warning to the durable manifest.
    pub fn add_warning(&mut self, warning: impl Into<String>) -> Result<(), String> {
        self.manifest.warnings.push(warning.into());
        self.persist_manifest()
    }

    /// Finish the run in one explicit terminal state.
    pub fn finish(&mut self, state: CaptureRunState) -> Result<(), String> {
        if !state.is_terminal() {
            return Err(format!("finish requires a terminal state, got {state:?}"));
        }
        if self.manifest.state.is_terminal() {
            if self.manifest.state == state {
                return Ok(());
            }
            return Err(format!(
                "run is already terminal as {:?}",
                self.manifest.state
            ));
        }
        if state == CaptureRunState::FailedBeforeMutation && self.manifest.remote_mutation_started {
            return Err("cannot classify a post-mutation run as failed_before_mutation".to_owned());
        }
        if state == CaptureRunState::FailedAfterMutation && !self.manifest.remote_mutation_started {
            return Err("cannot classify a pre-mutation run as failed_after_mutation".to_owned());
        }
        if state == CaptureRunState::OutcomeAmbiguous && !self.manifest.remote_mutation_started {
            return Err("outcome_ambiguous requires positive mutation-start evidence".to_owned());
        }

        self.append_event("run_finished", serde_json::json!({ "state": state }))?;
        self.manifest.state = state;
        self.manifest.finished_unix_ms = Some(unix_ms()?);
        self.persist_manifest()
    }

    fn persist_manifest(&self) -> Result<(), String> {
        persist_manifest(&self.paths.manifest, &self.manifest)
    }
}

fn make_run_id(started_unix_ms: u128) -> String {
    let counter = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("run-{started_unix_ms}-{}-{counter}", std::process::id())
}

fn unix_ms() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))
}

fn persist_manifest(path: &Path, manifest: &CaptureRunManifest) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("serialize run manifest: {error}"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| format!("open manifest {}: {error}", path.display()))?;
    file.write_all(&bytes)
        .map_err(|error| format!("write manifest {}: {error}", path.display()))?;
    file.write_all(b"\n")
        .map_err(|error| format!("terminate manifest {}: {error}", path.display()))?;
    file.flush()
        .map_err(|error| format!("flush manifest {}: {error}", path.display()))?;
    file.sync_data()
        .map_err(|error| format!("sync manifest {}: {error}", path.display()))
}

fn read_manifest(path: &Path) -> Result<CaptureRunManifest, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("read manifest {}: {error}", path.display()))?;
    let manifest: CaptureRunManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse manifest {}: {error}", path.display()))?;
    if manifest.schema != RUN_SCHEMA || manifest.version != RUN_SCHEMA_VERSION {
        return Err(format!(
            "unsupported run manifest {} version {}",
            manifest.schema, manifest.version
        ));
    }
    Ok(manifest)
}

fn recover_and_read_events(path: &Path) -> Result<Vec<CaptureEvent>, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("open event journal {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("read event journal {}: {error}", path.display()))?;

    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        let tail_start = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        let tail = &bytes[tail_start..];
        if serde_json::from_slice::<CaptureEvent>(tail).is_err() {
            file.set_len(tail_start as u64)
                .map_err(|error| format!("truncate torn event tail {}: {error}", path.display()))?;
            file.seek(SeekFrom::Start(tail_start as u64))
                .map_err(|error| format!("seek event journal {}: {error}", path.display()))?;
            file.sync_data().map_err(|error| {
                format!("sync recovered event journal {}: {error}", path.display())
            })?;
        }
    }
    drop(file);

    let reader = BufReader::new(
        File::open(path)
            .map_err(|error| format!("reopen event journal {}: {error}", path.display()))?,
    );
    let mut events = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line.map_err(|error| format!("read event line {}: {error}", index + 1))?;
        if line.is_empty() {
            continue;
        }
        let event: CaptureEvent = serde_json::from_str(&line)
            .map_err(|error| format!("parse complete event line {}: {error}", index + 1))?;
        let expected = events.last().map_or(1, |previous: &CaptureEvent| {
            previous.sequence.saturating_add(1)
        });
        if event.sequence != expected {
            return Err(format!(
                "event sequence integrity error on line {}: expected {}, observed {}",
                index + 1,
                expected,
                event.sequence
            ));
        }
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical_experiment;

    fn temp_dir(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "chatarium-capture-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn create_writes_manifest_before_mutation_and_fsyncs_events() {
        let base = temp_dir("create");
        let experiment = canonical_experiment("C03-send-text").expect("C03");
        let run = CaptureRun::create(&base, &experiment).expect("create run");

        assert!(run.paths().manifest.is_file());
        assert!(run.paths().events.is_file());
        assert_eq!(run.manifest().state, CaptureRunState::Preparing);
        assert!(!run.manifest().remote_mutation_started);
        assert_eq!(run.events().len(), 1);
        assert_eq!(run.events()[0].kind, "run_created");

        let reopened = CaptureRun::open(&run.paths().root).expect("reopen run");
        assert_eq!(reopened.events(), run.events());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn ambiguous_state_requires_positive_mutation_evidence() {
        let base = temp_dir("ambiguous");
        let experiment = canonical_experiment("C03-send-text").expect("C03");
        let mut run = CaptureRun::create(&base, &experiment).expect("create run");
        run.start().expect("start");
        assert!(run.finish(CaptureRunState::OutcomeAmbiguous).is_err());
        run.mark_remote_mutation_started(serde_json::json!({ "source": "test" }))
            .expect("mutation evidence");
        run.finish(CaptureRunState::OutcomeAmbiguous)
            .expect("ambiguous finish");
        assert_eq!(run.manifest().state, CaptureRunState::OutcomeAmbiguous);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn non_mutating_experiment_rejects_mutation_boundary() {
        let base = temp_dir("nonmutating");
        let experiment = canonical_experiment("C00-idle-load").expect("C00");
        let mut run = CaptureRun::create(&base, &experiment).expect("create run");
        run.start().expect("start");
        assert!(run.mark_remote_mutation_started(Value::Null).is_err());
        run.finish(CaptureRunState::Completed).expect("finish");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn torn_final_event_is_truncated_but_complete_corruption_is_fatal() {
        let base = temp_dir("torn");
        let experiment = canonical_experiment("C00-idle-load").expect("C00");
        let run = CaptureRun::create(&base, &experiment).expect("create run");
        let root = run.paths().root.clone();
        let events_path = run.paths().events.clone();
        drop(run);

        let original_len = fs::metadata(&events_path).expect("metadata").len();
        OpenOptions::new()
            .append(true)
            .open(&events_path)
            .expect("append")
            .write_all(b"{\"schema\":\"chatarium")
            .expect("write torn tail");
        let reopened = CaptureRun::open(&root).expect("recover torn tail");
        assert_eq!(reopened.events().len(), 1);
        assert_eq!(
            fs::metadata(&events_path).expect("metadata").len(),
            original_len
        );
        drop(reopened);

        OpenOptions::new()
            .append(true)
            .open(&events_path)
            .expect("append")
            .write_all(b"not-json\n")
            .expect("write corrupt line");
        let error = CaptureRun::open(&root)
            .err()
            .expect("complete corruption fails");
        assert!(error.contains("parse complete event line"));
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn finish_is_idempotent_only_for_same_terminal_state() {
        let base = temp_dir("terminal");
        let experiment = canonical_experiment("C00-idle-load").expect("C00");
        let mut run = CaptureRun::create(&base, &experiment).expect("create run");
        run.start().expect("start");
        run.finish(CaptureRunState::Completed).expect("finish");
        run.finish(CaptureRunState::Completed)
            .expect("same state is idempotent");
        assert!(run.finish(CaptureRunState::CompletedWithWarnings).is_err());
        let _ = fs::remove_dir_all(base);
    }
}
