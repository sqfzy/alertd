#[cfg(target_os = "linux")]
use super::command;
use super::{CollectContext, CollectError};
use crate::{
    config::{CheckConfig, JournalRule},
    model::{Observation, Severity},
};
use chrono::{DateTime, SecondsFormat, Utc};
#[cfg(any(target_os = "linux", test))]
use serde_json::Value;
use std::time::Duration;
use tracing::info;

#[derive(Debug)]
struct JournalBatch {
    cursor: Option<String>,
    entries: Vec<JournalEntry>,
}

#[derive(Debug, PartialEq, Eq)]
struct JournalEntry {
    message: String,
    unit: Option<String>,
    occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq)]
struct JournalSample {
    severity: Severity,
    unit: Option<String>,
    rule: String,
    occurred_at: Option<DateTime<Utc>>,
    message: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct JournalMatches {
    severity: Severity,
    hits: u64,
    ignored: u64,
    warn_hits: u64,
    critical_hits: u64,
    sample: Option<JournalSample>,
}

fn match_messages(
    entries: &[JournalEntry],
    ignore_contains: &[String],
    rules: &[JournalRule],
) -> JournalMatches {
    let mut matches = JournalMatches::default();
    for entry in entries {
        if ignore_contains
            .iter()
            .any(|ignored| entry.message.contains(ignored))
        {
            matches.ignored += 1;
            continue;
        }
        for rule in rules {
            if entry.message.contains(&rule.contains) {
                matches.hits += 1;
                matches.severity = matches.severity.max(rule.severity);
                match rule.severity {
                    Severity::Warn => matches.warn_hits += 1,
                    Severity::Critical => matches.critical_hits += 1,
                    Severity::Ok => {}
                }
                if matches
                    .sample
                    .as_ref()
                    .is_none_or(|sample| rule.severity > sample.severity)
                {
                    matches.sample = Some(JournalSample {
                        severity: rule.severity,
                        unit: entry.unit.clone(),
                        rule: rule.contains.clone(),
                        occurred_at: entry.occurred_at,
                        message: truncate_sample(&entry.message),
                    });
                }
            }
        }
    }
    matches
}

fn read_batch(
    units: &[String],
    cursor: Option<&str>,
    timeout: Duration,
) -> Result<JournalBatch, CollectError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (units, cursor, timeout);
        Err(CollectError::Unsupported("journald requires Linux".into()))
    }
    #[cfg(target_os = "linux")]
    {
        let mut arguments = vec!["--no-pager", "--quiet", "--output=json", "--show-cursor"];
        for unit in units {
            arguments.push("--unit");
            arguments.push(unit);
        }
        if let Some(value) = cursor {
            arguments.push("--after-cursor");
            arguments.push(value);
        } else {
            arguments.push("--since");
            arguments.push("now");
        }
        let output = command::run("journalctl", &arguments, timeout)?;
        if !output.status.success() {
            if output.status.code() == Some(1)
                && output.stdout.is_empty()
                && output.stderr.is_empty()
            {
                return Ok(JournalBatch {
                    cursor: cursor
                        .map(str::to_owned)
                        .or_else(|| current_cursor(timeout)),
                    entries: Vec::new(),
                });
            }
            return Err(CollectError::Invalid(format!(
                "journalctl exited {}",
                output.status
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut batch = JournalBatch {
            cursor: None,
            entries: Vec::new(),
        };
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("-- cursor: ") {
                batch.cursor = Some(value.into());
                continue;
            }
            if let Some(entry) = parse_entry(line) {
                batch.entries.push(entry);
            }
        }
        Ok(batch)
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_entry(line: &str) -> Option<JournalEntry> {
    let value: Value = serde_json::from_str(line).ok()?;
    Some(JournalEntry {
        message: value.get("MESSAGE")?.as_str()?.to_owned(),
        unit: value
            .get("_SYSTEMD_UNIT")
            .and_then(Value::as_str)
            .map(str::to_owned),
        occurred_at: value
            .get("__REALTIME_TIMESTAMP")
            .and_then(parse_realtime_timestamp),
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_realtime_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    let micros = value
        .as_str()
        .and_then(|raw| raw.parse::<i64>().ok())
        .or_else(|| value.as_i64())?;
    DateTime::from_timestamp_micros(micros)
}

fn truncate_sample(message: &str) -> String {
    let mut characters = message.chars();
    let prefix: String = characters.by_ref().take(240).collect();
    if characters.next().is_none() {
        return prefix;
    }
    let mut truncated: String = prefix.chars().take(239).collect();
    truncated.push('…');
    truncated
}

pub fn collect(
    check: &CheckConfig,
    units: &[String],
    ignore_contains: &[String],
    rules: &[JournalRule],
    context: &mut CollectContext,
) -> Result<Observation, CollectError> {
    let batch = read_batch(
        units,
        context.journal_cursors.get(&check.name).map(String::as_str),
        context.command_timeout,
    )?;
    let matches = match_messages(&batch.entries, ignore_contains, rules);
    if matches.ignored > 0 {
        info!(
            check = %check.name,
            read = batch.entries.len(),
            ignored = matches.ignored,
            "journal messages filtered"
        );
    }
    if let Some(cursor) = batch.cursor {
        context
            .pending_journal_cursors
            .insert(check.name.clone(), cursor);
    }
    let summary = format!(
        "journal 新增 {} 行，规则命中 {} 次",
        batch.entries.len(),
        matches.hits
    );
    let observation = if matches.hits == 0 {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, matches.severity, summary)
    };
    let mut observation = observation
        .event_counts(matches.warn_hits, matches.critical_hits)
        .detail("本批读取", batch.entries.len().to_string())
        .detail("本次命中", matches.hits.to_string());
    if let Some(sample) = matches.sample {
        observation = observation
            .detail("服务", sample.unit.unwrap_or_else(|| "未知".into()))
            .detail("命中规则", sample.rule)
            .detail("日志", sample.message);
        if let Some(occurred_at) = sample.occurred_at {
            observation = observation.detail(
                "日志时间",
                occurred_at.to_rfc3339_opts(SecondsFormat::Micros, true),
            );
        }
    }
    Ok(observation)
}

#[cfg(target_os = "linux")]
fn current_cursor(timeout: Duration) -> Option<String> {
    let output = command::run(
        "journalctl",
        &["--no-pager", "--quiet", "--show-cursor", "--lines=0"],
        timeout,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("-- cursor: ").map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(message: &str) -> JournalEntry {
        JournalEntry {
            message: message.into(),
            unit: None,
            occurred_at: None,
        }
    }

    #[test]
    fn counts_warn_and_critical_substrings() {
        let rules = vec![
            JournalRule {
                contains: "WARN".into(),
                severity: Severity::Warn,
            },
            JournalRule {
                contains: "ERROR".into(),
                severity: Severity::Critical,
            },
        ];
        let matches = match_messages(
            &[entry("WARN first"), entry("normal"), entry("ERROR failed")],
            &[],
            &rules,
        );
        assert_eq!(matches.hits, 2);
        assert_eq!(matches.warn_hits, 1);
        assert_eq!(matches.critical_hits, 1);
        assert_eq!(matches.severity, Severity::Critical);
        assert_eq!(matches.sample.unwrap().message, "ERROR failed");
    }

    #[test]
    fn ignores_messages_before_matching_alert_rules() {
        let rules = vec![
            JournalRule {
                contains: "WARN".into(),
                severity: Severity::Warn,
            },
            JournalRule {
                contains: "ERROR".into(),
                severity: Severity::Critical,
            },
        ];
        let matches = match_messages(
            &[
                entry("ERROR expected during shutdown"),
                entry("error expected during shutdown"),
                entry("WARN retrying"),
                entry("ERROR failed"),
            ],
            &["expected during shutdown".into(), "not present".into()],
            &rules,
        );

        assert_eq!(matches.ignored, 2);
        assert_eq!(matches.hits, 2);
        assert_eq!(matches.warn_hits, 1);
        assert_eq!(matches.critical_hits, 1);
        assert_eq!(matches.severity, Severity::Critical);
        assert_eq!(matches.sample.unwrap().message, "ERROR failed");
    }

    #[test]
    fn ignore_matching_is_case_sensitive() {
        let rules = vec![JournalRule {
            contains: "ERROR".into(),
            severity: Severity::Critical,
        }];
        let matches = match_messages(&[entry("ERROR failed")], &["error failed".into()], &rules);

        assert_eq!(matches.ignored, 0);
        assert_eq!(matches.hits, 1);
    }

    #[test]
    fn fully_filtered_batch_has_no_alert_occurrences() {
        let rules = vec![JournalRule {
            contains: "ERROR".into(),
            severity: Severity::Critical,
        }];
        let matches = match_messages(
            &[entry("ERROR expected during shutdown")],
            &["expected during shutdown".into()],
            &rules,
        );

        assert_eq!(matches.ignored, 1);
        assert_eq!(matches.hits, 0);
        assert_eq!(matches.warn_hits, 0);
        assert_eq!(matches.critical_hits, 0);
        assert_eq!(matches.severity, Severity::Ok);
        assert!(matches.sample.is_none());
    }

    #[test]
    fn parses_journal_metadata_and_tolerates_missing_optional_fields() {
        let parsed = parse_entry(
            r#"{"MESSAGE":"fatal\ncontinued","_SYSTEMD_UNIT":"market.service","__REALTIME_TIMESTAMP":"1787605818149168"}"#,
        )
        .unwrap();
        assert_eq!(parsed.message, "fatal\ncontinued");
        assert_eq!(parsed.unit.as_deref(), Some("market.service"));
        assert_eq!(
            parsed.occurred_at.unwrap().timestamp_micros(),
            1_787_605_818_149_168
        );

        let missing = parse_entry(r#"{"MESSAGE":"WARN only"}"#).unwrap();
        assert_eq!(missing.unit, None);
        assert_eq!(missing.occurred_at, None);
        let invalid_time =
            parse_entry(r#"{"MESSAGE":"WARN only","__REALTIME_TIMESTAMP":"invalid"}"#).unwrap();
        assert_eq!(invalid_time.occurred_at, None);
        assert!(parse_entry("not json").is_none());
        assert!(parse_entry(r#"{"_SYSTEMD_UNIT":"market.service"}"#).is_none());
    }

    #[test]
    fn highest_severity_sample_keeps_its_unit_rule_and_time() {
        let occurred_at = DateTime::from_timestamp_micros(1_787_605_818_149_168).unwrap();
        let entries = [
            JournalEntry {
                message: "WARN retrying".into(),
                unit: Some("first.service".into()),
                occurred_at: None,
            },
            JournalEntry {
                message: "ERROR fatal".into(),
                unit: Some("critical.service".into()),
                occurred_at: Some(occurred_at),
            },
            JournalEntry {
                message: "ERROR later".into(),
                unit: Some("later.service".into()),
                occurred_at: None,
            },
        ];
        let rules = [
            JournalRule {
                contains: "WARN".into(),
                severity: Severity::Warn,
            },
            JournalRule {
                contains: "ERROR".into(),
                severity: Severity::Critical,
            },
        ];

        let sample = match_messages(&entries, &[], &rules).sample.unwrap();
        assert_eq!(sample.unit.as_deref(), Some("critical.service"));
        assert_eq!(sample.rule, "ERROR");
        assert_eq!(sample.occurred_at, Some(occurred_at));
        assert_eq!(sample.message, "ERROR fatal");
    }

    #[test]
    fn sample_truncation_is_unicode_safe_and_marks_truncation() {
        let exact = "延".repeat(240);
        assert_eq!(truncate_sample(&exact), exact);

        let truncated = truncate_sample(&"延".repeat(241));
        assert_eq!(truncated.chars().count(), 240);
        assert!(truncated.ends_with('…'));
    }
}
