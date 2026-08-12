use crate::model::{AlertEvent, Observation, ObservationStatus, Severity, Transition};
use chrono::Local;

pub fn format_alert(host: &str, event: &AlertEvent) -> String {
    let (icon, transition) = match event.transition {
        Transition::Firing => (severity_icon(event.severity), "告警"),
        Transition::Repeating => (severity_icon(event.severity), "持续"),
        Transition::Resolved => ("🟢", "恢复"),
    };
    let mut text = format!(
        "{icon} {} · {transition}\n\n主机：{host}\n检查：{}\n状态：{}\n开始：{}",
        event.severity.label(),
        event.check_name,
        event.summary,
        event
            .started_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
    );
    for (key, value) in &event.details {
        if !value.is_empty() {
            text.push_str(&format!("\n{key}：{value}"));
        }
    }
    if event.transition == Transition::Resolved {
        text.push_str(&format!(
            "\n持续：{} 秒",
            (event.observed_at - event.started_at).num_seconds().max(0)
        ));
    }
    if let Some(runbook) = &event.runbook {
        text.push_str(&format!("\n处理：{runbook}"));
    }
    text
}

pub fn format_daily(host: &str, observations: &[Observation]) -> String {
    let mut healthy = 0;
    let mut unhealthy = Vec::new();
    for observation in observations {
        match observation.status {
            ObservationStatus::Healthy => healthy += 1,
            ObservationStatus::Unhealthy(_) => unhealthy.push(format!(
                "{}: {}",
                observation.check_name, observation.summary
            )),
        }
    }
    let mut text = format!(
        "📋 DAILY · {host}\n\n检查：{}，正常：{healthy}，异常：{}",
        observations.len(),
        unhealthy.len()
    );
    for item in unhealthy {
        text.push_str(&format!("\n- {item}"));
    }
    text
}

fn severity_icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "🔴",
        Severity::Warn => "🟡",
        Severity::Ok => "🟢",
    }
}
