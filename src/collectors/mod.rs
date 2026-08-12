pub mod disk;
pub mod journal;
pub mod memory;
pub mod process;
pub mod shm;

use crate::{
    config::{CheckConfig, CheckKind},
    model::Observation,
};
use std::{collections::HashMap, path::PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CollectError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error("unsupported on this platform: {0}")]
    Unsupported(String),
}

#[derive(Default)]
pub struct CollectContext {
    pub proc_root: Option<PathBuf>,
    pub shm_root: Option<PathBuf>,
    pub shm_progress: HashMap<String, shm::ProgressState>,
    pub journal_cursors: HashMap<String, String>,
    pub pending_journal_cursors: HashMap<String, String>,
}

pub fn collect(
    check: &CheckConfig,
    context: &mut CollectContext,
) -> Result<Observation, CollectError> {
    match &check.kind {
        CheckKind::Process {
            cmdline_contains,
            min_count,
        } => process::collect(check, cmdline_contains, *min_count, context),
        CheckKind::Shm {
            path,
            probe,
            require_progress,
            stale_after,
            offset,
            endian,
            magic,
            layout_version,
        } => shm::collect(
            check,
            path,
            *probe,
            *require_progress,
            stale_after.as_deref(),
            *offset,
            *endian,
            *magic,
            *layout_version,
            context,
        ),
        CheckKind::Journal { units, rules } => journal::collect(check, units, rules, context),
        CheckKind::Disk {
            mount,
            warn_used_pct,
            critical_used_pct,
        } => disk::collect(check, mount, *warn_used_pct, *critical_used_pct),
        CheckKind::Memory {
            warn_available_pct,
            critical_available_pct,
        } => memory::collect(check, *warn_available_pct, *critical_available_pct, context),
    }
}
