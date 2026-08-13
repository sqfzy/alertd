#[cfg(target_os = "linux")]
use super::command;
use super::{CollectContext, CollectError};
use crate::{
    config::{CheckConfig, JournalRule},
    model::{Observation, Severity},
};
use std::time::Duration;

#[derive(Debug)]
struct JournalBatch {
    cursor: Option<String>,
    messages: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct JournalMatches {
    severity: Severity,
    hits: u64,
    warn_hits: u64,
    critical_hits: u64,
    sample: String,
}

fn match_messages(messages: &[String], rules: &[JournalRule]) -> JournalMatches {
    let mut matches = JournalMatches::default();
    for message in messages {
        for rule in rules {
            if message.contains(&rule.contains) {
                matches.hits += 1;
                matches.severity = matches.severity.max(rule.severity);
                match rule.severity {
                    Severity::Warn => matches.warn_hits += 1,
                    Severity::Critical => matches.critical_hits += 1,
                    Severity::Ok => {}
                }
                if matches.sample.is_empty() {
                    matches.sample = message.chars().take(240).collect();
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
                    messages: Vec::new(),
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
            messages: Vec::new(),
        };
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("-- cursor: ") {
                batch.cursor = Some(value.into());
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(message) = value.get("MESSAGE").and_then(|v| v.as_str()) {
                    batch.messages.push(message.into());
                }
            }
        }
        Ok(batch)
    }
}

pub fn collect(
    check: &CheckConfig,
    units: &[String],
    rules: &[JournalRule],
    context: &mut CollectContext,
) -> Result<Observation, CollectError> {
    let batch = read_batch(
        units,
        context.journal_cursors.get(&check.name).map(String::as_str),
        context.command_timeout,
    )?;
    let matches = match_messages(&batch.messages, rules);
    if let Some(cursor) = batch.cursor {
        context
            .pending_journal_cursors
            .insert(check.name.clone(), cursor);
    }
    let summary = format!(
        "journal 新增 {} 行，命中 {} 条",
        batch.messages.len(),
        matches.hits
    );
    let observation = if matches.hits == 0 {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, matches.severity, summary)
    };
    Ok(observation
        .event_counts(matches.warn_hits, matches.critical_hits)
        .detail("units", units.join(", "))
        .detail("本次命中", matches.hits.to_string())
        .detail("样例", matches.sample))
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
            &["WARN first".into(), "normal".into(), "ERROR failed".into()],
            &rules,
        );
        assert_eq!(matches.hits, 2);
        assert_eq!(matches.warn_hits, 1);
        assert_eq!(matches.critical_hits, 1);
        assert_eq!(matches.severity, Severity::Critical);
    }
}
