use super::{CollectContext, CollectError};
use crate::{
    config::{CheckConfig, JournalRule},
    model::{Observation, Severity},
};
#[cfg(target_os = "linux")]
use std::process::Command;

#[derive(Debug)]
struct JournalBatch {
    cursor: Option<String>,
    messages: Vec<String>,
}

fn read_batch(units: &[String], cursor: Option<&str>) -> Result<JournalBatch, CollectError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (units, cursor);
        Err(CollectError::Unsupported("journald requires Linux".into()))
    }
    #[cfg(target_os = "linux")]
    {
        let mut command = Command::new("journalctl");
        command.args(["--no-pager", "--quiet", "--output=json", "--show-cursor"]);
        for unit in units {
            command.arg("--unit").arg(unit);
        }
        if let Some(value) = cursor {
            command.arg("--after-cursor").arg(value);
        } else {
            command.arg("--since").arg("now");
        }
        let output = command.output()?;
        if !output.status.success() {
            if output.status.code() == Some(1)
                && output.stdout.is_empty()
                && output.stderr.is_empty()
            {
                return Ok(JournalBatch {
                    cursor: cursor.map(str::to_owned).or_else(current_cursor),
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
    )?;
    let mut severity = Severity::Ok;
    let mut hits = 0;
    let mut sample = String::new();
    for message in &batch.messages {
        for rule in rules {
            if message.contains(&rule.contains) {
                hits += 1;
                severity = severity.max(rule.severity);
                if sample.is_empty() {
                    sample = message.chars().take(240).collect();
                }
            }
        }
    }
    if let Some(cursor) = batch.cursor {
        context
            .pending_journal_cursors
            .insert(check.name.clone(), cursor);
    }
    let summary = format!("journal 新增 {} 行，命中 {hits} 条", batch.messages.len());
    let observation = if hits == 0 {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, severity, summary)
    };
    Ok(observation
        .detail("units", units.join(", "))
        .detail("样例", sample))
}

#[cfg(target_os = "linux")]
fn current_cursor() -> Option<String> {
    let output = Command::new("journalctl")
        .args(["--no-pager", "--quiet", "--show-cursor", "--lines=0"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("-- cursor: ").map(str::to_owned))
}
