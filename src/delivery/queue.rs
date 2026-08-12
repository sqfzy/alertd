use crate::model::Severity;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
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
}

#[derive(Debug, Error)]
pub enum QueueError {
    #[error("queue I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("queue serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("delivery queue is full ({0})")]
    Full(usize),
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
        if self.pending_paths()?.len() >= self.capacity {
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
                fs::rename(&path, target)?;
                Err(QueueError::Json(error))
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
}
