pub mod disk;
pub mod journal;
pub mod latest_file;
pub mod memory;
pub mod network;
pub mod process;
pub mod shm;
pub mod system_tuning;
pub mod systemd;
pub mod time_sync;

use crate::{
    config::{CheckConfig, CheckKind},
    model::Observation,
};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    time::Duration,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CollectError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid data: {0}")]
    Invalid(String),
    #[error("unsupported on this platform: {0}")]
    Unsupported(String),
    #[error("command timeout: {0}")]
    Timeout(String),
}

#[derive(Default)]
pub struct CollectContext {
    pub proc_root: Option<PathBuf>,
    pub shm_root: Option<PathBuf>,
    pub sys_root: Option<PathBuf>,
    pub shm_progress: HashMap<String, shm::ProgressState>,
    pub journal_cursors: HashMap<String, String>,
    pub pending_journal_cursors: HashMap<String, String>,
    pub cpu_times: HashMap<String, BTreeMap<String, cpu::CpuTimes>>,
    pub network_samples: HashMap<String, network::NetworkSample>,
    pub command_timeout: Duration,
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
        CheckKind::Journal {
            units,
            ignore_contains,
            rules,
        } => journal::collect(check, units, ignore_contains, rules, context),
        CheckKind::Systemd { units } => systemd::collect(check, units, context.command_timeout),
        CheckKind::LatestFile {
            directory,
            prefix,
            suffix,
            stale_after,
            minimum_size_bytes,
        } => latest_file::collect(
            check,
            directory,
            prefix,
            suffix,
            stale_after,
            *minimum_size_bytes,
        ),
        CheckKind::Disk {
            mount,
            warn_used_pct,
            critical_used_pct,
            warn_inode_used_pct,
            critical_inode_used_pct,
        } => disk::collect(
            check,
            mount,
            *warn_used_pct,
            *critical_used_pct,
            *warn_inode_used_pct,
            *critical_inode_used_pct,
        ),
        CheckKind::Memory {
            warn_available_pct,
            critical_available_pct,
        } => memory::collect(check, *warn_available_pct, *critical_available_pct, context),
        CheckKind::Cpu {
            warn_usage_pct,
            critical_usage_pct,
        } => cpu::collect(check, *warn_usage_pct, *critical_usage_pct, context),
        CheckKind::TimeSync {
            warn_offset,
            critical_offset,
        } => time_sync::collect(check, warn_offset, critical_offset, context.command_timeout),
        CheckKind::Network {
            interfaces,
            warn_errors_per_second,
            critical_errors_per_second,
            warn_drops_per_second,
            critical_drops_per_second,
        } => network::collect(
            check,
            interfaces,
            *warn_errors_per_second,
            *critical_errors_per_second,
            *warn_drops_per_second,
            *critical_drops_per_second,
            context,
        ),
        CheckKind::SystemTuning => system_tuning::collect(check, context.command_timeout, context),
    }
}
pub mod command;
pub mod cpu;
