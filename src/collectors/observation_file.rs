//! Reads a producer-owned atomic status snapshot without understanding business fields.

use super::CollectError;
use crate::{
    config::{CheckConfig, parse_duration},
    model::{ExternalReport, Observation, Severity},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{ErrorKind, Read},
    path::Path,
    time::{Duration, SystemTime},
};

const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024;
const MAX_SUMMARY_BYTES: usize = 512;
const MAX_DETAILS: usize = 64;
const MAX_DETAIL_KEY_BYTES: usize = 128;
const MAX_REPORT_ID_BYTES: usize = 128;
const MAX_REPORT_TITLE_BYTES: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    observed_at: String,
    status: SnapshotStatus,
    summary: String,
    #[serde(default)]
    details: BTreeMap<String, String>,
    report: Option<SnapshotReport>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum SnapshotStatus {
    Ok,
    Warn,
    Critical,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotReport {
    id: String,
    title: String,
    body: String,
}

#[tracing::instrument(skip(check), fields(check = %check.name, path = %path.display()), err)]
pub fn collect(
    check: &CheckConfig,
    path: &Path,
    stale_after: &str,
    forward_report: bool,
) -> Result<Observation, CollectError> {
    let Some(file) = open_snapshot(path)? else {
        return Ok(missing_observation(check, path));
    };
    let age = snapshot_age(&file)?;
    let stale_after =
        parse_duration(stale_after).map_err(|error| CollectError::Invalid(error.to_string()))?;
    if age >= stale_after {
        return Ok(stale_observation(check, path, age));
    }
    let snapshot = read_snapshot(file)?;
    snapshot_observation(check, path, age, snapshot, forward_report)
}

fn open_snapshot(path: &Path) -> Result<Option<File>, CollectError> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn snapshot_age(file: &File) -> Result<Duration, CollectError> {
    let modified = file.metadata()?.modified()?;
    Ok(SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default())
}

fn read_snapshot(mut file: File) -> Result<Snapshot, CollectError> {
    if file.metadata()?.len() > MAX_SNAPSHOT_BYTES {
        return Err(CollectError::Invalid(format!(
            "observation snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(CollectError::Invalid(format!(
            "observation snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes"
        )));
    }
    let snapshot: Snapshot = serde_json::from_slice(&bytes)
        .map_err(|error| CollectError::Invalid(format!("invalid observation JSON: {error}")))?;
    validate_snapshot(&snapshot)?;
    Ok(snapshot)
}

fn validate_snapshot(snapshot: &Snapshot) -> Result<(), CollectError> {
    if snapshot.schema_version != 1 {
        return Err(CollectError::Invalid(format!(
            "unsupported observation schema_version {}",
            snapshot.schema_version
        )));
    }
    if snapshot.summary.is_empty() || snapshot.summary.len() > MAX_SUMMARY_BYTES {
        return Err(CollectError::Invalid(
            "observation summary must contain 1..=512 bytes".into(),
        ));
    }
    DateTime::parse_from_rfc3339(&snapshot.observed_at)
        .map_err(|_| CollectError::Invalid("observation observed_at must be RFC3339".into()))?;
    if snapshot.details.len() > MAX_DETAILS
        || snapshot.details.iter().any(|(key, value)| {
            key.is_empty()
                || key.len() > MAX_DETAIL_KEY_BYTES
                || key.starts_with('_')
                || key.chars().any(char::is_control)
                || value.len() > MAX_SNAPSHOT_BYTES as usize
        })
    {
        return Err(CollectError::Invalid(
            "observation details are invalid".into(),
        ));
    }
    if let Some(report) = &snapshot.report {
        if report.id.is_empty()
            || report.id.len() > MAX_REPORT_ID_BYTES
            || report.id.chars().any(char::is_control)
            || report.title.is_empty()
            || report.title.len() > MAX_REPORT_TITLE_BYTES
            || report.title.chars().any(char::is_control)
            || report.body.is_empty()
        {
            return Err(CollectError::Invalid(
                "observation report is invalid".into(),
            ));
        }
    }
    Ok(())
}

fn missing_observation(check: &CheckConfig, path: &Path) -> Observation {
    Observation::unhealthy(&check.name, check.severity, "外部观察快照不存在")
        .detail("文件", path.display().to_string())
}

fn stale_observation(check: &CheckConfig, path: &Path, age: Duration) -> Observation {
    Observation::unhealthy(
        &check.name,
        check.severity,
        format!("外部观察快照已 {} 秒没有更新", age.as_secs()),
    )
    .detail("文件", path.display().to_string())
    .detail("快照年龄", format!("{} 秒", age.as_secs()))
}

fn snapshot_observation(
    check: &CheckConfig,
    path: &Path,
    age: Duration,
    snapshot: Snapshot,
    forward_report: bool,
) -> Result<Observation, CollectError> {
    let observed_at = DateTime::parse_from_rfc3339(&snapshot.observed_at)
        .map_err(|_| CollectError::Invalid("observation observed_at must be RFC3339".into()))?
        .with_timezone(&Utc);
    let mut observation = match snapshot.status {
        SnapshotStatus::Ok => Observation::healthy(&check.name, snapshot.summary),
        SnapshotStatus::Warn => {
            Observation::unhealthy(&check.name, Severity::Warn, snapshot.summary)
        }
        SnapshotStatus::Critical => {
            Observation::unhealthy(&check.name, Severity::Critical, snapshot.summary)
        }
    };
    observation.observed_at = observed_at;
    observation = observation
        .detail("文件", path.display().to_string())
        .detail("快照年龄", format!("{} 秒", age.as_secs()));
    for (key, value) in snapshot.details {
        observation = observation.detail(key, value);
    }
    if forward_report {
        if let Some(report) = snapshot.report {
            observation = observation.external_report(ExternalReport {
                id: report.id,
                title: report.title,
                body: report.body,
            });
        }
    }
    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;

    fn check() -> CheckConfig {
        let config: Config = toml::from_str(
            r#"
[delivery]
timeout = "3s"
queue_capacity = 16
queue_warn_pct = 80
failure_report_after = 3
retry_initial = "1s"
retry_max = "5s"

[[delivery.routes]]
name = "default"
token_env = "TOKEN"

[[checks]]
name = "external"
type = "observation_file"
path = "/tmp/external.json"
stale_after = "1h"
forward_report = true
"#,
        )
        .unwrap();
        config.checks.into_iter().next().unwrap()
    }

    #[test]
    fn maps_status_details_and_report() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.json");
        fs::write(
            &path,
            r#"{"schema_version":1,"observed_at":"2026-09-19T10:30:00+08:00","status":"warn","summary":"degraded","details":{"质量":"cached"},"report":{"id":"report-1","title":"统计","body":"body"}}"#,
        )
        .unwrap();
        let mut check = check();
        let crate::config::CheckKind::ObservationFile {
            path: configured, ..
        } = &mut check.kind
        else {
            panic!("expected observation_file");
        };
        *configured = path.clone();
        let observation = collect(&check, &path, "1h", true).unwrap();
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Unhealthy(Severity::Warn)
        ));
        assert_eq!(observation.details["质量"], "cached");
        assert_eq!(observation.external_report.unwrap().id, "report-1");
    }

    #[test]
    fn rejects_invalid_protocol() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("snapshot.json");
        fs::write(&path, r#"{"schema_version":2}"#).unwrap();
        assert!(read_snapshot(File::open(path).unwrap()).is_err());
    }
}
