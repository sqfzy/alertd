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
    pub maintenance_start_notice_id: Option<String>,
    #[serde(default)]
    pub maintenance_end_notice_id: Option<String>,
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
    use super::PersistentState;

    #[test]
    fn old_state_without_clean_shutdown_remains_unknown() {
        let state: PersistentState =
            serde_json::from_str(r#"{"checks":{},"journal_cursors":{},"last_daily_date":null}"#)
                .expect("old state remains compatible");

        assert_eq!(state.clean_shutdown, None);
        assert_eq!(state.maintenance_start_notice_id, None);
        assert_eq!(state.maintenance_end_notice_id, None);
    }
}
