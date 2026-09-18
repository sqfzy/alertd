//! Route-partitioned atomic spool with one shared capacity limit.

use crate::model::Severity;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use thiserror::Error;

pub const DEFAULT_ROUTE: &str = "default";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QueuedMessage {
    pub id: String,
    pub severity: Severity,
    pub text: String,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_name: Option<String>,
    #[serde(default = "default_route")]
    pub delivery_route: String,
}

fn default_route() -> String {
    DEFAULT_ROUTE.into()
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
/// `state_dir/spool` 下的共享容量、route 隔离持久消息队列。
pub struct DeliveryQueue {
    root: PathBuf,
    capacity: usize,
}

impl DeliveryQueue {
    pub fn open(state_dir: &Path, capacity: usize) -> Result<Self, QueueError> {
        let root = state_dir.join("spool");
        fs::create_dir_all(root.join("quarantine"))?;
        fs::create_dir_all(root.join(DEFAULT_ROUTE))?;
        migrate_legacy_messages(&root)?;
        Ok(Self { root, capacity })
    }

    pub fn enqueue(&self, severity: Severity, text: String) -> Result<String, QueueError> {
        self.enqueue_route(
            DEFAULT_ROUTE,
            severity,
            text,
            None,
            self.capacity.saturating_sub(1),
        )
    }

    pub fn enqueue_check(
        &self,
        check_name: &str,
        delivery_route: &str,
        severity: Severity,
        text: String,
    ) -> Result<String, QueueError> {
        self.enqueue_route(
            delivery_route,
            severity,
            text,
            Some(check_name.to_owned()),
            self.capacity.saturating_sub(1),
        )
    }

    pub fn enqueue_internal(&self, severity: Severity, text: String) -> Result<String, QueueError> {
        self.enqueue_route(DEFAULT_ROUTE, severity, text, None, self.capacity)
    }

    fn enqueue_route(
        &self,
        route: &str,
        severity: Severity,
        text: String,
        check_name: Option<String>,
        limit: usize,
    ) -> Result<String, QueueError> {
        if self.pending_count()? >= limit {
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
            delivery_route: route.to_owned(),
        };
        let directory = self.route_dir(route);
        fs::create_dir_all(&directory)?;
        let temporary = directory.join(format!(".{id}.tmp"));
        let final_path = directory.join(format!("{id}.json"));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&message)?)?;
        file.sync_all()?;
        fs::rename(&temporary, &final_path)?;
        sync_dir(&directory)?;
        Ok(id)
    }

    pub fn oldest_for_route(
        &self,
        route: &str,
    ) -> Result<Option<(PathBuf, QueuedMessage)>, QueueError> {
        let Some(path) = self.pending_paths_for_route(route)?.into_iter().next() else {
            return Ok(None);
        };
        match serde_json::from_slice(&fs::read(&path)?) {
            Ok(message) => Ok(Some((path, message))),
            Err(_) => Err(self.quarantine(&path)?),
        }
    }

    /// 兼容默认 route 的读取入口；内部事件和日报固定使用该 route。
    pub fn oldest(&self) -> Result<Option<(PathBuf, QueuedMessage)>, QueueError> {
        self.oldest_for_route(DEFAULT_ROUTE)
    }

    pub fn acknowledge(&self, path: &Path) -> Result<(), QueueError> {
        fs::remove_file(path)?;
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "queue path has no parent")
        })?;
        sync_dir(parent)?;
        Ok(())
    }

    pub fn pending_count(&self) -> Result<usize, QueueError> {
        Ok(self.all_pending_paths()?.len())
    }

    pub fn pending_counts(&self) -> Result<BTreeMap<String, usize>, QueueError> {
        let mut counts = BTreeMap::new();
        for route in self.routes()? {
            let count = self.pending_paths_for_route(&route)?.len();
            if count != 0 {
                counts.insert(route, count);
            }
        }
        Ok(counts)
    }

    pub fn routes(&self) -> Result<Vec<String>, QueueError> {
        let mut routes: Vec<_> = fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name != "quarantine")
            .collect();
        routes.sort();
        Ok(routes)
    }

    pub fn discard_inactive_checks(
        &self,
        active_checks: &HashSet<String>,
    ) -> Result<usize, QueueError> {
        let mut discarded = 0;
        for path in self.all_pending_paths()? {
            let message: QueuedMessage = serde_json::from_slice(&fs::read(&path)?)?;
            let is_inactive = message
                .check_name
                .as_ref()
                .is_some_and(|name| !active_checks.contains(name));
            if is_inactive {
                fs::remove_file(&path)?;
                discarded += 1;
            }
        }
        if discarded != 0 {
            for route in self.routes()? {
                sync_dir(&self.route_dir(&route))?;
            }
        }
        Ok(discarded)
    }

    fn all_pending_paths(&self) -> Result<Vec<PathBuf>, QueueError> {
        let mut paths = Vec::new();
        for route in self.routes()? {
            paths.extend(self.pending_paths_for_route(&route)?);
        }
        Ok(paths)
    }

    fn pending_paths_for_route(&self, route: &str) -> Result<Vec<PathBuf>, QueueError> {
        let directory = self.route_dir(route);
        if !directory.exists() {
            return Ok(Vec::new());
        }
        let mut paths: Vec<_> = fs::read_dir(directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        paths.sort();
        Ok(paths)
    }

    fn route_dir(&self, route: &str) -> PathBuf {
        self.root.join(route)
    }

    fn quarantine(&self, path: &Path) -> Result<QueueError, QueueError> {
        let route = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("message.json");
        let target = self
            .root
            .join("quarantine")
            .join(format!("{route}-{file_name}"));
        fs::rename(path, &target)?;
        sync_dir(&self.root.join("quarantine"))?;
        Ok(QueueError::Quarantined(target))
    }
}

fn migrate_legacy_messages(root: &Path) -> Result<(), QueueError> {
    let default = root.join(DEFAULT_ROUTE);
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file()
            || path.extension().is_none_or(|extension| extension != "json")
        {
            continue;
        }
        fs::rename(&path, default.join(entry.file_name()))?;
    }
    sync_dir(root)?;
    sync_dir(&default)?;
    Ok(())
}

fn sync_dir(path: &Path) -> Result<(), std::io::Error> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_preserve_independent_fifo_and_global_capacity() {
        let temporary = tempfile::tempdir().unwrap();
        let queue = DeliveryQueue::open(temporary.path(), 3).unwrap();
        queue.enqueue(Severity::Warn, "default".into()).unwrap();
        queue
            .enqueue_check("live", "live-mm", Severity::Critical, "business".into())
            .unwrap();
        let (_, live) = queue.oldest_for_route("live-mm").unwrap().unwrap();
        assert_eq!(live.delivery_route, "live-mm");
        assert_eq!(live.check_name.as_deref(), Some("live"));
        assert_eq!(queue.pending_count().unwrap(), 2);
        assert!(matches!(
            queue.enqueue(Severity::Warn, "full".into()),
            Err(QueueError::Full(3))
        ));
        queue
            .enqueue_internal(Severity::Warn, "internal".into())
            .unwrap();
    }

    #[test]
    fn legacy_root_message_migrates_to_default() {
        let temporary = tempfile::tempdir().unwrap();
        let spool = temporary.path().join("spool");
        fs::create_dir_all(&spool).unwrap();
        fs::write(
            spool.join("legacy.json"),
            r#"{"id":"old","severity":"warn","text":"legacy","attempts":0}"#,
        )
        .unwrap();
        let queue = DeliveryQueue::open(temporary.path(), 16).unwrap();
        let (_, message) = queue.oldest_for_route(DEFAULT_ROUTE).unwrap().unwrap();
        assert_eq!(message.delivery_route, DEFAULT_ROUTE);
        assert!(spool.join("default/legacy.json").exists());
    }

    #[test]
    fn corrupt_route_message_is_quarantined() {
        let temporary = tempfile::tempdir().unwrap();
        let queue = DeliveryQueue::open(temporary.path(), 16).unwrap();
        let directory = temporary.path().join("spool/live-mm");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("bad.json"), "bad").unwrap();
        assert!(matches!(
            queue.oldest_for_route("live-mm"),
            Err(QueueError::Quarantined(_))
        ));
    }
}
