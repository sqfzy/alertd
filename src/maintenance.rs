//! 由 CLI 和 daemon 共享的单窗口状态、原子文件操作与进程间锁。

use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};
use thiserror::Error;
use tracing::{debug, instrument};

const FILE_NAME: &str = "maintenance.json";
const LOCK_NAME: &str = ".maintenance.lock";
const MAX_REASON_BYTES: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
/// 持久化维护请求；绝对结束时间保留调用者提供的时区。
pub struct MaintenanceWindow {
    pub id: String,
    pub requested_at: DateTime<FixedOffset>,
    pub until: DateTime<FixedOffset>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<FixedOffset>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// daemon 在当前时刻对窗口生命周期的判断。
pub enum MaintenancePhase {
    Active,
    Ending,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// CLI 结合窗口文件和持久通知 ID 展示的维护状态。
pub enum MaintenanceStatus {
    PendingStartNotice,
    Active,
    PendingEndNotice,
    None,
}

#[derive(Debug, Error)]
pub enum MaintenanceError {
    #[error("maintenance I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("maintenance state is invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("maintenance window is invalid: {0}")]
    Invalid(String),
    #[error("a maintenance window already exists; cancel it or wait for alertd to finish it")]
    AlreadyExists,
}

pub fn parse_until(
    value: &str,
    now: DateTime<Utc>,
) -> Result<DateTime<FixedOffset>, MaintenanceError> {
    let until = DateTime::parse_from_rfc3339(value).map_err(|error| {
        MaintenanceError::Invalid(format!("--until must be RFC3339 with a timezone: {error}"))
    })?;
    if until.with_timezone(&Utc) < now + chrono::Duration::minutes(1) {
        return Err(MaintenanceError::Invalid(
            "--until must be at least one minute in the future".into(),
        ));
    }
    Ok(until)
}

pub fn validate_reason(reason: &str) -> Result<(), MaintenanceError> {
    if reason.is_empty() || reason.len() > MAX_REASON_BYTES {
        return Err(MaintenanceError::Invalid(format!(
            "--reason must contain 1..={MAX_REASON_BYTES} bytes"
        )));
    }
    if reason.chars().any(char::is_control) {
        return Err(MaintenanceError::Invalid(
            "--reason must not contain control characters".into(),
        ));
    }
    Ok(())
}

#[instrument(err, skip(reason))]
pub fn start(
    state_dir: &Path,
    until: DateTime<FixedOffset>,
    reason: String,
    now: DateTime<Utc>,
) -> Result<MaintenanceWindow, MaintenanceError> {
    validate_reason(&reason)?;
    if until.with_timezone(&Utc) < now + chrono::Duration::minutes(1) {
        return Err(MaintenanceError::Invalid(
            "--until must be at least one minute in the future".into(),
        ));
    }
    let requested_at = now.fixed_offset();
    let window = MaintenanceWindow {
        id: format!(
            "{}-{}",
            requested_at.timestamp_nanos_opt().unwrap_or_default(),
            std::process::id()
        ),
        requested_at,
        until,
        reason,
        cancelled_at: None,
    };
    with_lock(state_dir, || {
        if window_path(state_dir).exists() {
            return Err(MaintenanceError::AlreadyExists);
        }
        write_atomic(state_dir, &window)
    })?;
    debug!(id = %window.id, until = %window.until, "maintenance window created");
    Ok(window)
}

#[instrument(err)]
pub fn cancel(state_dir: &Path) -> Result<Option<MaintenanceWindow>, MaintenanceError> {
    with_lock(state_dir, || {
        let Some(mut window) = load(state_dir)? else {
            return Ok(None);
        };
        if window.cancelled_at.is_none() {
            window.cancelled_at = Some(Utc::now().fixed_offset());
            write_atomic(state_dir, &window)?;
        }
        Ok(Some(window))
    })
}

#[instrument(err)]
pub fn load(state_dir: &Path) -> Result<Option<MaintenanceWindow>, MaintenanceError> {
    let path = window_path(state_dir);
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let window: MaintenanceWindow = serde_json::from_slice(&bytes)?;
    validate_window(&window)?;
    Ok(Some(window))
}

pub fn phase(window: &MaintenanceWindow, now: DateTime<Utc>) -> MaintenancePhase {
    if window.cancelled_at.is_none() && window.until.with_timezone(&Utc) > now {
        MaintenancePhase::Active
    } else {
        MaintenancePhase::Ending
    }
}

pub fn status(
    window: Option<&MaintenanceWindow>,
    start_notice_id: Option<&str>,
    end_notice_id: Option<&str>,
    now: DateTime<Utc>,
) -> MaintenanceStatus {
    let Some(window) = window else {
        return MaintenanceStatus::None;
    };
    match phase(window, now) {
        MaintenancePhase::Active if start_notice_id == Some(window.id.as_str()) => {
            MaintenanceStatus::Active
        }
        MaintenancePhase::Active => MaintenanceStatus::PendingStartNotice,
        MaintenancePhase::Ending if end_notice_id == Some(window.id.as_str()) => {
            MaintenanceStatus::None
        }
        MaintenancePhase::Ending => MaintenanceStatus::PendingEndNotice,
    }
}

#[instrument(err)]
pub fn remove_if_id(state_dir: &Path, expected_id: &str) -> Result<bool, MaintenanceError> {
    with_lock(state_dir, || {
        let Some(window) = load(state_dir)? else {
            return Ok(false);
        };
        if window.id != expected_id {
            return Ok(false);
        }
        fs::remove_file(window_path(state_dir))?;
        File::open(state_dir)?.sync_all()?;
        Ok(true)
    })
}

pub fn format_time(value: DateTime<FixedOffset>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn validate_window(window: &MaintenanceWindow) -> Result<(), MaintenanceError> {
    if window.id.is_empty() || window.id.len() > 128 || window.id.chars().any(char::is_control) {
        return Err(MaintenanceError::Invalid("invalid window id".into()));
    }
    validate_reason(&window.reason)?;
    if window.until <= window.requested_at {
        return Err(MaintenanceError::Invalid(
            "window end must be later than its request time".into(),
        ));
    }
    Ok(())
}

fn window_path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

fn with_lock<T>(
    state_dir: &Path,
    operation: impl FnOnce() -> Result<T, MaintenanceError>,
) -> Result<T, MaintenanceError> {
    fs::create_dir_all(state_dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(state_dir.join(LOCK_NAME))?;
    // SAFETY: fd 在调用时由 `lock` 持有且有效；锁随该 File 保持到 operation 返回后释放。
    let result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    operation()
}

fn write_atomic(state_dir: &Path, window: &MaintenanceWindow) -> Result<(), MaintenanceError> {
    fs::create_dir_all(state_dir)?;
    let temporary = state_dir.join(format!(
        ".maintenance.json.{}-{}.tmp",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(window)?)?;
    file.sync_all()?;
    fs::rename(&temporary, window_path(state_dir))?;
    File::open(state_dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn future(minutes: i64) -> DateTime<FixedOffset> {
        (Utc::now() + chrono::Duration::minutes(minutes)).fixed_offset()
    }

    #[test]
    fn validates_cli_values() {
        let now = Utc::now();
        assert!(parse_until("2026-08-21T16:00:00", now).is_err());
        assert!(parse_until(&(now + chrono::Duration::seconds(59)).to_rfc3339(), now).is_err());
        assert!(parse_until(&(now + chrono::Duration::days(3650)).to_rfc3339(), now).is_ok());
        assert!(validate_reason("").is_err());
        assert!(validate_reason(&"x".repeat(257)).is_err());
        assert!(validate_reason("line\nbreak").is_err());
        assert!(validate_reason("业务重新部署").is_ok());
    }

    #[test]
    fn starts_reports_and_cancels_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let window = start(root.path(), future(60), "deploy".into(), Utc::now()).unwrap();
        assert!(matches!(
            start(root.path(), future(120), "again".into(), Utc::now()),
            Err(MaintenanceError::AlreadyExists)
        ));
        assert_eq!(
            fs::metadata(window_path(root.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(load(root.path()).unwrap().unwrap().id, window.id);
        let cancelled = cancel(root.path()).unwrap().unwrap();
        let cancelled_again = cancel(root.path()).unwrap().unwrap();
        assert_eq!(cancelled.cancelled_at, cancelled_again.cancelled_at);
        assert_eq!(phase(&cancelled, Utc::now()), MaintenancePhase::Ending);
    }

    #[test]
    fn removes_only_matching_window() {
        let root = tempfile::tempdir().unwrap();
        let window = start(root.path(), future(60), "deploy".into(), Utc::now()).unwrap();
        assert!(!remove_if_id(root.path(), "different").unwrap());
        assert!(remove_if_id(root.path(), &window.id).unwrap());
        assert!(load(root.path()).unwrap().is_none());
        assert!(cancel(root.path()).unwrap().is_none());
    }

    #[test]
    fn rejects_corrupt_or_invalid_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(window_path(root.path()), b"not-json").unwrap();
        assert!(load(root.path()).is_err());
        fs::write(
            window_path(root.path()),
            br#"{"id":"x","requested_at":"2026-08-21T10:00:00+08:00","until":"2026-08-21T09:00:00+08:00","reason":"bad"}"#,
        )
        .unwrap();
        assert!(load(root.path()).is_err());
    }

    #[test]
    fn status_tracks_daemon_notice_ids() {
        let window = MaintenanceWindow {
            id: "window-1".into(),
            requested_at: Utc::now().fixed_offset(),
            until: future(60),
            reason: "deploy".into(),
            cancelled_at: None,
        };
        assert_eq!(
            status(Some(&window), None, None, Utc::now()),
            MaintenanceStatus::PendingStartNotice
        );
        assert_eq!(
            status(Some(&window), Some("window-1"), None, Utc::now()),
            MaintenanceStatus::Active
        );
    }
}
