use crate::model::{AlertEvent, Observation, ObservationStatus, Severity, Transition};
use chrono::Local;

#[derive(Clone, Copy, Debug)]
pub struct ReportContext<'a> {
    pub host: &'a str,
    pub ip: Option<&'a str>,
}

pub fn format_alert(context: ReportContext<'_>, event: &AlertEvent) -> String {
    let (icon, transition) = match event.transition {
        Transition::Firing => (severity_icon(event.severity), "告警"),
        Transition::Repeating => (severity_icon(event.severity), "持续"),
        Transition::Resolved => ("🟢", "恢复"),
    };
    let mut text = format!("{icon} **{} · {transition}**", event.severity.label());
    push_field(&mut text, "主机", context.host);
    if let Some(ip) = context.ip {
        push_field(&mut text, "IP", ip);
    }
    push_field(&mut text, "检查", &event.check_name);
    push_field(&mut text, "状态", &event.summary);
    push_field(
        &mut text,
        "异常开始",
        &event
            .started_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    );
    for (key, value) in &event.details {
        if !value.is_empty() {
            push_field(&mut text, key, value);
        }
    }
    if event.transition == Transition::Resolved {
        push_field(
            &mut text,
            "持续时间",
            &format_duration((event.observed_at - event.started_at).num_seconds()),
        );
        push_field(
            &mut text,
            "恢复时间",
            &event
                .observed_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        );
    }
    if let Some(runbook) = &event.runbook {
        push_field(&mut text, "处理", runbook);
    }
    text
}

pub fn format_daily(context: ReportContext<'_>, observations: &[Observation]) -> String {
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
    let mut text = "📋 **DAILY**".to_string();
    push_field(&mut text, "主机", context.host);
    if let Some(ip) = context.ip {
        push_field(&mut text, "IP", ip);
    }
    push_field(&mut text, "检查", &observations.len().to_string());
    push_field(&mut text, "正常", &healthy.to_string());
    push_field(&mut text, "异常", &unhealthy.len().to_string());
    for item in unhealthy {
        text.push_str(&format!("\n\n- {item}"));
    }
    text
}

fn push_field(text: &mut String, label: &str, value: &str) {
    text.push_str(&format!("\n\n**{label}：** {value}"));
}

fn format_duration(total_seconds: i64) -> String {
    let total_seconds = total_seconds.max(0);
    let days = total_seconds / 86_400;
    let hours = total_seconds % 86_400 / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days} 天"));
    }
    if hours > 0 {
        parts.push(format!("{hours} 小时"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes} 分钟"));
    }
    if parts.is_empty() || seconds > 0 {
        parts.push(format!("{seconds} 秒"));
    }
    parts.join(" ")
}

fn severity_icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "🔴",
        Severity::Warn => "🟡",
        Severity::Ok => "🟢",
    }
}
