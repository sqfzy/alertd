//! daemon 跨重启状态的兼容反序列化和原子落盘。

use crate::model::CheckState;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PersistentState {
    #[serde(default)]
    pub checks: HashMap<String, CheckState>,
    #[serde(default)]
    pub journal_cursors: HashMap<String, String>,
    pub last_daily_date: Option<String>,
    #[serde(default)]
    pub clean_shutdown: Option<bool>,
    #[serde(default)]
    /// daemon 上次应用的全局监控状态；用于跨重启识别配置切换。
    pub monitoring_enabled: Option<bool>,
    #[serde(default)]
    /// 等待持久队列接受通知的目标状态；成功入队后清除。
    pub pending_monitoring_notice: Option<bool>,
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("state I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("state serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

fn path(root: &Path) -> PathBuf {
    root.join("state.json")
}

pub fn load(root: &Path) -> Result<PersistentState, StateError> {
    fs::create_dir_all(root)?;
    let path = path(root);
    if !path.exists() {
        return Ok(PersistentState::default());
    }
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub fn save(root: &Path, state: &PersistentState) -> Result<(), StateError> {
    fs::create_dir_all(root)?;
    let temporary = root.join(".state.json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(state)?)?;
    file.sync_all()?;
    fs::rename(&temporary, path(root))?;
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{PersistentState, load};

    #[test]
    fn old_state_without_lifecycle_fields_remains_compatible() {
        let state: PersistentState =
            serde_json::from_str(r#"{"checks":{},"journal_cursors":{},"last_daily_date":null}"#)
                .expect("old state remains compatible");

        assert_eq!(state.clean_shutdown, None);
        assert_eq!(state.monitoring_enabled, None);
        assert_eq!(state.pending_monitoring_notice, None);
    }

    #[test]
    fn state_with_retired_maintenance_fields_remains_compatible() {
        let state: PersistentState = serde_json::from_str(
            r#"{"checks":{},"journal_cursors":{},"last_daily_date":null,"maintenance_start_notice_id":"old","maintenance_end_notice_id":"old"}"#,
        )
        .expect("retired maintenance fields are ignored");

        assert_eq!(state.monitoring_enabled, None);
        assert_eq!(state.pending_monitoring_notice, None);
    }

    #[test]
    fn retired_maintenance_file_does_not_affect_state_loading() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(
            temporary.path().join("maintenance.json"),
            "invalid legacy data",
        )
        .unwrap();

        let state = load(temporary.path()).unwrap();

        assert_eq!(state.monitoring_enabled, None);
        assert!(temporary.path().join("maintenance.json").exists());
    }
}
