//! Archive operations execute on dedicated background threads, never in egui.
use super::super::*;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

pub(crate) enum ArchiveOperation {
    Check { data_dir: PathBuf },
    Backup { data_dir: PathBuf, output: PathBuf },
    Verify { backup: PathBuf },
}

fn perform_archive_operation(operation: ArchiveOperation) -> String {
    match operation {
        ArchiveOperation::Check { data_dir } => {
            match chatarium_store::archive_maintenance::check_archive(&data_dir) {
                Ok(report) => format!(
                    "{} · {} events · {} catalog items",
                    report.status, report.journal_event_count, report.catalog_count
                ),
                Err(error) => format!("archive check failed · {error}"),
            }
        }
        ArchiveOperation::Backup { data_dir, output } => {
            match chatarium_store::archive_maintenance::create_backup(&data_dir, &output) {
                Ok(_) => "backup created and verified".to_owned(),
                Err(error) => format!("backup failed · {error}"),
            }
        }
        ArchiveOperation::Verify { backup } => {
            match chatarium_store::archive_maintenance::verify_backup(&backup) {
                Ok(_) => "backup verified".to_owned(),
                Err(error) => format!("backup verification failed · {error}"),
            }
        }
    }
}

impl ChatariumApp {
    pub(crate) fn archive_maintenance_busy(&self) -> bool {
        self.archive_maintenance_result.is_some()
    }

    pub(crate) fn launch_archive_operation(&mut self, operation: ArchiveOperation) {
        if self.archive_maintenance_busy() {
            self.archive_maintenance_status = "archive operation already running".to_owned();
            return;
        }
        let (tx, rx) = mpsc::channel();
        match thread::Builder::new()
            .name("chatarium-archive-maintenance".to_owned())
            .spawn(move || {
                let _ = tx.send(perform_archive_operation(operation));
            }) {
            Ok(_) => {
                self.archive_maintenance_result = Some(rx);
                self.archive_maintenance_status = "working on archive in background…".to_owned();
            }
            Err(error) => {
                self.archive_maintenance_status =
                    format!("failed to start archive maintenance worker · {error}");
            }
        }
    }

    /// Nonblocking on the render thread: consume only an already completed
    /// message, never wait for a scan, verification, filesystem copy or lock.
    pub(crate) fn poll_archive_operation(&mut self, ctx: &egui::Context) {
        let result = self.archive_maintenance_result.as_ref().map(Receiver::try_recv);
        match result {
            Some(Ok(message)) => {
                self.archive_maintenance_status = message;
                self.archive_maintenance_result = None;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.archive_maintenance_status =
                    "archive maintenance worker stopped without a result".to_owned();
                self.archive_maintenance_result = None;
            }
            Some(Err(TryRecvError::Empty)) => {
                ctx.request_repaint_after(Duration::from_millis(120));
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatarium_core::EventKind;
    use chatarium_store::{EventStore, JsonlEventStore};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn archive_tasks_use_explicit_paths_and_preserve_live_journal() {
        let root = std::env::temp_dir().join(format!(
            "chatarium-archive-async-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let source = root.join("source");
        let backup = root.join("backup");
        fs::create_dir_all(&source).unwrap();
        let mut store = JsonlEventStore::open(source.join("journal.jsonl")).unwrap();
        store.append(EventKind::DraftChanged, "native preserved".to_owned()).unwrap();
        let original = fs::read(source.join("journal.jsonl")).unwrap();

        assert!(perform_archive_operation(ArchiveOperation::Check {
            data_dir: source.clone(),
        }).contains("1 events"));
        assert_eq!(
            perform_archive_operation(ArchiveOperation::Backup {
                data_dir: source.clone(),
                output: backup.clone(),
            }),
            "backup created and verified"
        );
        assert_eq!(
            perform_archive_operation(ArchiveOperation::Verify {
                backup: backup.clone(),
            }),
            "backup verified"
        );
        assert_eq!(fs::read(source.join("journal.jsonl")).unwrap(), original);
        assert_eq!(fs::read(backup.join("journal.jsonl")).unwrap(), original);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
