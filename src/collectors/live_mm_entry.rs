use super::CollectError;
#[cfg(target_os = "linux")]
use super::command;
use crate::{
    config::{self, CheckConfig, LiveMmInstance},
    model::{Observation, Severity},
};
use chrono::{DateTime, Utc};
use std::{collections::HashMap, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq)]
struct EntryState {
    observed_at: DateTime<Utc>,
    operator_enabled: u8,
    entries_enabled: u8,
    runtime_risk_mask: u32,
    reason: u16,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct JournalState {
    unit: String,
    message: String,
    observed_at: DateTime<Utc>,
}

pub fn collect(
    check: &CheckConfig,
    instances: &[LiveMmInstance],
    stale_after: &str,
    timeout: Duration,
) -> Result<Observation, CollectError> {
    let stale_after = config::parse_duration(stale_after)
        .map_err(|error| CollectError::Invalid(error.to_string()))?;
    let rows = read_latest(instances, stale_after, timeout)?;
    Ok(evaluate(check, instances, &rows, Utc::now(), stale_after))
}

fn evaluate(
    check: &CheckConfig,
    instances: &[LiveMmInstance],
    rows: &[JournalState],
    now: DateTime<Utc>,
    stale_after: Duration,
) -> Observation {
    let latest = latest_by_unit(rows);
    let mut failures = Vec::new();
    let mut details = Vec::new();

    for instance in instances {
        let result = latest
            .get(instance.unit.as_str())
            .ok_or_else(|| "状态缺失".to_owned())
            .and_then(|row| {
                let age = now
                    .signed_duration_since(row.observed_at)
                    .to_std()
                    .unwrap_or(Duration::ZERO);
                if age > stale_after {
                    return Err(format!("状态过期 age={}s", age.as_secs()));
                }
                parse_state(row).map(|state| (state, age))
            });

        match result {
            Ok((state, age)) => {
                let healthy = state.operator_enabled == 1
                    && state.entries_enabled == 1
                    && state.runtime_risk_mask == 0;
                details.push(format_state(instance, &state, age, healthy));
                if !healthy {
                    failures.push(instance.name.clone());
                }
            }
            Err(error) => {
                details.push(format!(
                    "- 🔴 **{}**｜状态 `{}`｜Unit `{}`",
                    instance.name, error, instance.unit
                ));
                failures.push(instance.name.clone());
            }
        }
    }

    let summary = if failures.is_empty() {
        format!("{} 个 live_mm 交易入口与风险位正常", instances.len())
    } else {
        format!("live_mm 状态异常：{}", failures.join(", "))
    };
    let observation = if failures.is_empty() {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, Severity::Critical, summary)
    };
    observation.detail("实例状态", details.join("\n\n"))
}

fn format_state(
    instance: &LiveMmInstance,
    state: &EntryState,
    age: Duration,
    healthy: bool,
) -> String {
    let marker = if healthy { "🟢" } else { "🔴" };
    format!(
        "- {marker} **{}**｜操作开关 `{}`｜实际开仓 `{}`｜风险位 `0x{:08x}`  \n  原因 `{}`｜代次 `{}`｜年龄 `{}s`｜Unit `{}`",
        instance.name,
        enabled_label(state.operator_enabled),
        enabled_label(state.entries_enabled),
        state.runtime_risk_mask,
        state.reason,
        state.generation,
        age.as_secs(),
        instance.unit
    )
}

fn enabled_label(value: u8) -> &'static str {
    match value {
        0 => "关",
        1 => "开",
        _ => "异常",
    }
}

fn latest_by_unit(rows: &[JournalState]) -> HashMap<&str, &JournalState> {
    let mut latest: HashMap<&str, &JournalState> = HashMap::new();
    for row in rows {
        if !row.message.contains("[live_mm_host]") || !row.message.contains("entries_enabled=") {
            continue;
        }
        match latest.get(row.unit.as_str()) {
            Some(previous) if previous.observed_at >= row.observed_at => {}
            _ => {
                latest.insert(row.unit.as_str(), row);
            }
        }
    }
    latest
}

fn parse_state(row: &JournalState) -> Result<EntryState, String> {
    Ok(EntryState {
        observed_at: row.observed_at,
        operator_enabled: field(&row.message, "operator_enabled")?,
        entries_enabled: field(&row.message, "entries_enabled")?,
        runtime_risk_mask: hex_field(&row.message, "runtime_risk_mask")?,
        reason: field(&row.message, "reason")?,
        generation: field(&row.message, "generation")?,
    })
}

fn field<T: std::str::FromStr>(message: &str, name: &str) -> Result<T, String> {
    let prefix = format!("{name}=");
    let value = message
        .split_ascii_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .ok_or_else(|| format!("字段缺失 {name}"))?;
    value
        .parse()
        .map_err(|_| format!("字段非法 {name}={value}"))
}

fn hex_field(message: &str, name: &str) -> Result<u32, String> {
    let prefix = format!("{name}=");
    let value = message
        .split_ascii_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .ok_or_else(|| format!("字段缺失 {name}"))?;
    u32::from_str_radix(value.trim_start_matches("0x"), 16)
        .map_err(|_| format!("字段非法 {name}={value}"))
}

#[cfg(not(target_os = "linux"))]
fn read_latest(
    _instances: &[LiveMmInstance],
    _stale_after: Duration,
    _timeout: Duration,
) -> Result<Vec<JournalState>, CollectError> {
    Err(CollectError::Unsupported("journald requires Linux".into()))
}

#[cfg(target_os = "linux")]
fn read_latest(
    instances: &[LiveMmInstance],
    stale_after: Duration,
    timeout: Duration,
) -> Result<Vec<JournalState>, CollectError> {
    let since = format!("-{}s", stale_after.as_secs().saturating_add(1));
    let mut arguments = vec!["--no-pager", "--quiet", "--output=json", "--since", &since];
    for instance in instances {
        arguments.push("--unit");
        arguments.push(&instance.unit);
    }
    let output = command::run("journalctl", &arguments, timeout)?;
    if !output.status.success() {
        return Err(CollectError::Invalid(format!(
            "journalctl exited {}",
            output.status
        )));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(parse_journal_json)
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn parse_journal_json(line: &str) -> Result<JournalState, CollectError> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| CollectError::Invalid(format!("invalid journal JSON: {error}")))?;
    let unit = value
        .get("_SYSTEMD_UNIT")
        .and_then(|value| value.as_str())
        .ok_or_else(|| CollectError::Invalid("journal row missing _SYSTEMD_UNIT".into()))?;
    let message = value
        .get("MESSAGE")
        .and_then(|value| value.as_str())
        .ok_or_else(|| CollectError::Invalid("journal row missing MESSAGE".into()))?;
    let micros = value
        .get("__REALTIME_TIMESTAMP")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| CollectError::Invalid("journal row missing timestamp".into()))?;
    let observed_at = DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| CollectError::Invalid("journal row has invalid timestamp".into()))?;
    Ok(JournalState {
        unit: unit.into(),
        message: message.into(),
        observed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CheckKind;
    use chrono::TimeZone;

    fn check() -> CheckConfig {
        CheckConfig {
            name: "live-mm-entry".into(),
            enabled: true,
            severity: Severity::Critical,
            pending_for: None,
            recover_for: None,
            runbook: None,
            kind: CheckKind::LiveMmEntry {
                instances: instances(),
                stale_after: "20s".into(),
            },
        }
    }

    fn instances() -> Vec<LiveMmInstance> {
        ["live_mm1", "live_mm2", "live_mm3", "live_mm4"]
            .into_iter()
            .map(|name| LiveMmInstance {
                name: name.into(),
                unit: format!("{name}.service"),
            })
            .collect()
    }

    fn row(name: &str, seconds: i64, operator: u8, entries: u8, risk: &str) -> JournalState {
        JournalState {
            unit: format!("{name}.service"),
            message: format!(
                "[live_mm_host] progress entries_enabled={entries} operator_enabled={operator} runtime_risk_mask={risk} reason=2 generation=7"
            ),
            observed_at: Utc.timestamp_opt(seconds, 0).unwrap(),
        }
    }

    #[test]
    fn reports_all_instances_healthy() {
        let rows = instances()
            .iter()
            .map(|instance| row(&instance.name, 100, 1, 1, "00000000"))
            .collect::<Vec<_>>();
        let observation = evaluate(
            &check(),
            &instances(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            Duration::from_secs(20),
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Healthy
        ));
    }

    #[test]
    fn reports_risk_operator_and_effective_entry_failures() {
        let rows = vec![
            row("live_mm1", 100, 1, 1, "00000000"),
            row("live_mm2", 100, 1, 0, "00000002"),
            row("live_mm3", 100, 0, 0, "00000000"),
            row("live_mm4", 100, 1, 1, "00000000"),
        ];
        let observation = evaluate(
            &check(),
            &instances(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            Duration::from_secs(20),
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Unhealthy(Severity::Critical)
        ));
        assert!(observation.summary.contains("live_mm2"));
        assert!(observation.summary.contains("live_mm3"));
        let details = &observation.details["实例状态"];
        assert!(details.contains("- 🟢 **live_mm1**｜操作开关 `开`｜实际开仓 `开`"));
        assert!(
            details
                .contains("- 🔴 **live_mm2**｜操作开关 `开`｜实际开仓 `关`｜风险位 `0x00000002`")
        );
        assert!(details.contains("\n\n- 🔴 **live_mm3**"));
    }

    #[test]
    fn reports_missing_stale_and_malformed_states() {
        let rows = vec![
            row("live_mm1", 70, 1, 1, "00000000"),
            JournalState {
                unit: "live_mm2.service".into(),
                message: "[live_mm_host] entries_enabled=unknown".into(),
                observed_at: Utc.timestamp_opt(100, 0).unwrap(),
            },
        ];
        let observation = evaluate(
            &check(),
            &instances(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            Duration::from_secs(20),
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Unhealthy(Severity::Critical)
        ));
        let details = &observation.details["实例状态"];
        assert!(details.contains("状态过期"));
        assert!(details.contains("字段缺失 operator_enabled"));
        assert!(details.contains("- 🔴 **live_mm3**｜状态 `状态缺失`｜Unit `live_mm3.service`"));
    }

    #[test]
    fn parses_journal_json_identity_message_and_timestamp() {
        let row = parse_journal_json(
            r#"{"_SYSTEMD_UNIT":"live-mm-v0.service","MESSAGE":"[live_mm_host] progress entries_enabled=1 operator_enabled=1 runtime_risk_mask=00000000 reason=2 generation=7","__REALTIME_TIMESTAMP":"1000000"}"#,
        )
        .unwrap();
        assert_eq!(row.unit, "live-mm-v0.service");
        assert_eq!(row.observed_at, Utc.timestamp_opt(1, 0).unwrap());
        assert!(row.message.contains("runtime_risk_mask=00000000"));
    }
}
