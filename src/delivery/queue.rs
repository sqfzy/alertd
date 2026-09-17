use crate::model::Severity;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QueuedMessage {
    pub id: String,
    pub severity: Severity,
    pub text: String,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_name: Option<String>,
}

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("queue I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("queue serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("delivery queue is full ({0})")]
    Full(usize),
    #[error("corrupt queue message moved to quarantine: {0}")]
    Quarantined(PathBuf),
}

#[derive(Clone, Debug)]
pub struct DeliveryQueue {
    root: PathBuf,
    capacity: usize,
}

impl DeliveryQueue {
    pub fn open(state_dir: &Path, capacity: usize) -> Result<Self, QueueError> {
        let root = state_dir.join("spool");
        fs::create_dir_all(root.join("quarantine"))?;
        Ok(Self { root, capacity })
    }

    pub fn enqueue(&self, severity: Severity, text: String) -> Result<String, QueueError> {
        self.enqueue_with_limit(severity, text, None, self.capacity.saturating_sub(1))
    }

    pub fn enqueue_check(
        &self,
        check_name: &str,
        severity: Severity,
        text: String,
    ) -> Result<String, QueueError> {
        self.enqueue_with_limit(
            severity,
            text,
            Some(check_name.to_owned()),
            self.capacity.saturating_sub(1),
        )
    }

    pub fn enqueue_internal(&self, severity: Severity, text: String) -> Result<String, QueueError> {
        self.enqueue_with_limit(severity, text, None, self.capacity)
    }

    fn enqueue_with_limit(
        &self,
        severity: Severity,
        text: String,
        check_name: Option<String>,
        limit: usize,
    ) -> Result<String, QueueError> {
        if self.pending_paths()?.len() >= limit {
            return Err(QueueError::Full(self.capacity));
        }
        let id = format!(
            "{}-{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            std::process::id()
        );
        let message = QueuedMessage {
            id: id.clone(),
            severity,
            text,
            attempts: 0,
            check_name,
        };
        let temporary = self.root.join(format!(".{id}.tmp"));
        let final_path = self.root.join(format!("{id}.json"));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&message)?)?;
        file.sync_all()?;
        fs::rename(&temporary, &final_path)?;
        sync_dir(&self.root)?;
        Ok(id)
    }

    pub fn oldest(&self) -> Result<Option<(PathBuf, QueuedMessage)>, QueueError> {
        let Some(path) = self.pending_paths()?.into_iter().next() else {
            return Ok(None);
        };
        match serde_json::from_slice(&fs::read(&path)?) {
            Ok(message) => Ok(Some((path, message))),
            Err(error) => {
                let target = self.root.join("quarantine").join(path.file_name().unwrap());
                fs::rename(&path, &target)?;
                sync_dir(&self.root)?;
                let _ = error;
                Err(QueueError::Quarantined(target))
            }
        }
    }

    pub fn acknowledge(&self, path: &Path) -> Result<(), QueueError> {
        fs::remove_file(path)?;
        sync_dir(&self.root)?;
        Ok(())
    }
    pub fn pending_count(&self) -> Result<usize, QueueError> {
        Ok(self.pending_paths()?.len())
    }

    pub fn discard_inactive_checks(
        &self,
        active_checks: &HashSet<String>,
    ) -> Result<usize, QueueError> {
        let mut discarded = 0;
        for path in self.pending_paths()? {
            let message: QueuedMessage = serde_json::from_slice(&fs::read(&path)?)?;
            let is_inactive = message
                .check_name
                .as_ref()
                .is_some_and(|name| !active_checks.contains(name));
            if is_inactive {
                fs::remove_file(path)?;
                discarded += 1;
            }
        }
        if discarded != 0 {
            sync_dir(&self.root)?;
        }
        Ok(discarded)
    }

    fn pending_paths(&self) -> Result<Vec<PathBuf>, QueueError> {
        let mut paths: Vec<_> = fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        Ok(paths)
    }
}

fn sync_dir(path: &Path) -> Result<(), std::io::Error> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persists_orders_and_acknowledges() {
        let temp = tempfile::tempdir().unwrap();
        let queue = DeliveryQueue::open(temp.path(), 16).unwrap();
        queue.enqueue(Severity::Warn, "one".into()).unwrap();
        let (path, message) = queue.oldest().unwrap().unwrap();
        assert_eq!(message.text, "one");
        queue.acknowledge(&path).unwrap();
        assert_eq!(queue.pending_count().unwrap(), 0);
    }

    #[test]
    fn reserves_final_slot_for_internal_alerts() {
        let temp = tempfile::tempdir().unwrap();
        let queue = DeliveryQueue::open(temp.path(), 2).unwrap();
        queue.enqueue(Severity::Warn, "business".into()).unwrap();
        assert!(matches!(
            queue.enqueue(Severity::Warn, "rejected".into()),
            Err(QueueError::Full(2))
        ));
        queue
            .enqueue_internal(Severity::Warn, "internal".into())
            .unwrap();
        assert_eq!(queue.pending_count().unwrap(), 2);
    }

    #[test]
    fn quarantines_corrupt_messages() {
        let temp = tempfile::tempdir().unwrap();
        let queue = DeliveryQueue::open(temp.path(), 16).unwrap();
        fs::write(temp.path().join("spool/000.json"), b"not json").unwrap();
        let result = queue.oldest();
        assert!(matches!(result, Err(QueueError::Quarantined(_))));
        assert!(temp.path().join("spool/quarantine/000.json").exists());
    }

    #[test]
    fn discards_messages_for_removed_checks() {
        let temp = tempfile::tempdir().unwrap();
        let queue = DeliveryQueue::open(temp.path(), 16).unwrap();
        queue
            .enqueue_check("removed", Severity::Critical, "stale".into())
            .unwrap();
        queue
            .enqueue_check("active", Severity::Warn, "keep".into())
            .unwrap();
        queue
            .enqueue_internal(Severity::Ok, "internal".into())
            .unwrap();

        let active = HashSet::from(["active".to_owned()]);
        assert_eq!(queue.discard_inactive_checks(&active).unwrap(), 1);
        assert_eq!(queue.pending_count().unwrap(), 2);
    }

    #[test]
    fn reads_legacy_message_without_check_name() {
        let message: QueuedMessage =
            serde_json::from_str(r#"{"id":"old","severity":"warn","text":"legacy","attempts":0}"#)
                .unwrap();
        assert_eq!(message.check_name, None);
    }
}
