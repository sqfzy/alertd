use super::CollectError;
use crate::{
    config::{CheckConfig, MetricRule, parse_duration},
    model::{Observation, Severity},
};
use serde_json::Value;
use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::Path,
    time::{Duration, SystemTime},
};

const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq)]
struct MetricValue {
    key: String,
    rendered: String,
    severity: Severity,
    threshold: Option<f64>,
}

#[tracing::instrument(
    name = "collect_metrics_file",
    skip(check, metrics),
    fields(check = %check.name, path = %path.display()),
    err
)]
pub fn collect(
    check: &CheckConfig,
    path: &Path,
    stale_after: &str,
    metrics: &[MetricRule],
) -> Result<Observation, CollectError> {
    let Some(file) = open_snapshot(path)? else {
        return Ok(missing_observation(check, path));
    };
    let age = snapshot_age(&file)?;
    let stale =
        parse_duration(stale_after).map_err(|error| CollectError::Invalid(error.to_string()))?;
    if age >= stale {
        return Ok(stale_observation(check, path, age));
    }
    let values = read_metrics(file, metrics)?;
    Ok(metrics_observation(check, path, age, &values))
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

fn read_metrics(mut file: File, metrics: &[MetricRule]) -> Result<Vec<MetricValue>, CollectError> {
    if file.metadata()?.len() > MAX_SNAPSHOT_BYTES {
        return Err(CollectError::Invalid(format!(
            "metrics snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes"
        )));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_SNAPSHOT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(CollectError::Invalid(format!(
            "metrics snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes"
        )));
    }
    let snapshot: Value = serde_json::from_slice(&bytes)
        .map_err(|error| CollectError::Invalid(format!("invalid metrics JSON: {error}")))?;
    let object = snapshot
        .as_object()
        .ok_or_else(|| CollectError::Invalid("metrics JSON root must be an object".into()))?;
    metrics
        .iter()
        .map(|metric| read_metric(object, metric))
        .collect()
}

fn read_metric(
    object: &serde_json::Map<String, Value>,
    metric: &MetricRule,
) -> Result<MetricValue, CollectError> {
    let value = object.get(&metric.key).ok_or_else(|| {
        CollectError::Invalid(format!("metrics JSON key {:?} is missing", metric.key))
    })?;
    let number = value
        .as_f64()
        .filter(|number| number.is_finite())
        .ok_or_else(|| {
            CollectError::Invalid(format!(
                "metrics JSON key {:?} must be a finite number",
                metric.key
            ))
        })?;
    let rendered = value
        .as_number()
        .expect("as_f64 accepted a JSON number")
        .to_string();
    let (severity, threshold) = metric_severity(number, metric);
    Ok(MetricValue {
        key: metric.key.clone(),
        rendered,
        severity,
        threshold,
    })
}

fn metric_severity(value: f64, metric: &MetricRule) -> (Severity, Option<f64>) {
    if metric
        .critical_above
        .is_some_and(|threshold| value >= threshold)
    {
        return (Severity::Critical, metric.critical_above);
    }
    if metric
        .warn_above
        .is_some_and(|threshold| value >= threshold)
    {
        return (Severity::Warn, metric.warn_above);
    }
    (Severity::Ok, None)
}

fn missing_observation(check: &CheckConfig, path: &Path) -> Observation {
    Observation::unhealthy(&check.name, check.severity, "指标快照不存在")
        .detail("文件", path.display().to_string())
}

fn stale_observation(check: &CheckConfig, path: &Path, age: Duration) -> Observation {
    Observation::unhealthy(
        &check.name,
        check.severity,
        format!("指标快照已 {} 秒没有更新", age.as_secs()),
    )
    .detail("文件", path.display().to_string())
    .detail("快照年龄", format!("{} 秒", age.as_secs()))
}

fn metrics_observation(
    check: &CheckConfig,
    path: &Path,
    age: Duration,
    values: &[MetricValue],
) -> Observation {
    let severity = values
        .iter()
        .map(|value| value.severity)
        .max()
        .unwrap_or_default();
    let summary = metrics_summary(severity, values);
    let observation = if severity == Severity::Ok {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, severity, summary)
    };
    observation
        .detail("文件", path.display().to_string())
        .detail("快照年龄", format!("{} 秒", age.as_secs()))
        .detail("指标", render_metrics(values))
}

fn metrics_summary(severity: Severity, values: &[MetricValue]) -> String {
    if severity == Severity::Ok {
        return "指标快照正常".into();
    }
    let exceeded = values
        .iter()
        .filter(|value| value.severity != Severity::Ok)
        .map(|value| {
            format!(
                "{}={}（阈值 {}）",
                value.key,
                value.rendered,
                value.threshold.expect("non-OK metric has threshold")
            )
        })
        .collect::<Vec<_>>()
        .join("，");
    format!("指标越线：{exceeded}")
}

fn render_metrics(values: &[MetricValue]) -> String {
    values
        .iter()
        .map(|value| format!("{}={}", value.key, value.rendered))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, model::ObservationStatus};
    use std::fs;

    fn check() -> CheckConfig {
        let config: Config = toml::from_str(
            r#"
[[checks]]
name = "latency"
type = "metrics_file"
path = "/tmp/metrics.json"
stale_after = "1h"
metrics = [
  { key = "latest" },
  { key = "p99", warn_above = 80, critical_above = 120 },
  { key = "max", critical_above = 500 },
]
"#,
        )
        .unwrap();
        config.checks.into_iter().next().unwrap()
    }

    fn rules(check: &CheckConfig) -> &[MetricRule] {
        let crate::config::CheckKind::MetricsFile { metrics, .. } = &check.kind else {
            panic!("expected metrics_file check");
        };
        metrics
    }

    #[test]
    fn reports_values_and_highest_threshold_severity() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("metrics.json");
        fs::write(
            &path,
            br#"{"latest":17,"p99":120,"max":700,"extra":"ignored"}"#,
        )
        .unwrap();
        let check = check();

        let observation = collect(&check, &path, "1h", rules(&check)).unwrap();

        assert_eq!(
            observation.status,
            ObservationStatus::Unhealthy(Severity::Critical)
        );
        assert!(observation.summary.contains("p99=120"));
        assert!(observation.summary.contains("max=700"));
        assert_eq!(observation.details["指标"], "latest=17\np99=120\nmax=700");
    }

    #[test]
    fn report_only_metrics_do_not_alert() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("metrics.json");
        fs::write(&path, br#"{"latest":999,"p99":79,"max":499}"#).unwrap();
        let check = check();

        let observation = collect(&check, &path, "1h", rules(&check)).unwrap();

        assert_eq!(observation.status, ObservationStatus::Healthy);
    }

    #[test]
    fn missing_and_stale_files_are_direct_observations() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("metrics.json");
        let check = check();
        let missing = collect(&check, &path, "1h", rules(&check)).unwrap();
        assert_eq!(
            missing.status,
            ObservationStatus::Unhealthy(Severity::Critical)
        );

        fs::write(&path, br#"{"latest":1,"p99":1,"max":1}"#).unwrap();
        let stale = collect(&check, &path, "0s", rules(&check)).unwrap();
        assert!(stale.summary.contains("没有更新"));
    }

    #[test]
    fn rejects_invalid_json_shapes_and_oversized_files() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("metrics.json");
        let check = check();
        for contents in [
            b"[]".as_slice(),
            br#"{"latest":1,"p99":"bad","max":1}"#,
            br#"{"latest":1,"max":1}"#,
            b"{".as_slice(),
        ] {
            fs::write(&path, contents).unwrap();
            assert!(collect(&check, &path, "1h", rules(&check)).is_err());
        }
        fs::write(&path, vec![b' '; MAX_SNAPSHOT_BYTES as usize + 1]).unwrap();
        assert!(collect(&check, &path, "1h", rules(&check)).is_err());
    }

    #[test]
    fn reads_from_open_file_after_atomic_path_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("metrics.json");
        let replacement = temporary.path().join("replacement.json");
        fs::write(&path, br#"{"latest":1,"p99":2,"max":3}"#).unwrap();
        let opened = File::open(&path).unwrap();
        fs::write(&replacement, br#"{"latest":4,"p99":5,"max":6}"#).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let check = check();

        let values = read_metrics(opened, rules(&check)).unwrap();

        assert_eq!(values[0].rendered, "1");
        assert_eq!(values[1].rendered, "2");
    }

    #[test]
    fn thresholds_trigger_at_exact_boundaries_and_remain_optional() {
        let warn = MetricRule {
            key: "warn".into(),
            warn_above: Some(80.0),
            critical_above: Some(120.0),
        };
        let critical = MetricRule {
            key: "critical".into(),
            warn_above: None,
            critical_above: Some(500.0),
        };
        let report_only = MetricRule {
            key: "report".into(),
            warn_above: None,
            critical_above: None,
        };

        assert_eq!(metric_severity(80.0, &warn).0, Severity::Warn);
        assert_eq!(metric_severity(120.0, &warn).0, Severity::Critical);
        assert_eq!(metric_severity(500.0, &critical).0, Severity::Critical);
        assert_eq!(metric_severity(f64::MAX, &report_only).0, Severity::Ok);
    }
}
