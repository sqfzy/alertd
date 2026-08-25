use crate::{
    config::{CheckConfig, CheckKind},
    model::{AlertEvent, CheckState, Observation, ObservationStatus, Severity, Transition},
};
use chrono::Local;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct ReportContext<'a> {
    pub host: &'a str,
    pub ip: Option<&'a str>,
    pub system_hostname: &'a str,
    pub machine_sha256: &'a str,
    pub boot_sha256: &'a str,
    pub pid: u32,
    pub config_sha256: &'a str,
}

pub fn format_alert(context: ReportContext<'_>, event: &AlertEvent) -> String {
    if event.transition == Transition::Event {
        return format_journal_event(context, event);
    }
    let (icon, transition) = match event.transition {
        Transition::Firing => (severity_icon(event.severity), "告警"),
        Transition::Repeating => (severity_icon(event.severity), "持续"),
        Transition::Resolved => ("🟢", "恢复"),
        Transition::Event => unreachable!("journal events use their dedicated formatter"),
    };
    let mut text = format!("{icon} **{} · {transition}**", event.severity.label());
    push_field(&mut text, "主机", context.host);
    if let Some(ip) = context.ip {
        push_field(&mut text, "IP", ip);
    }
    push_identity(&mut text, context);
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

fn format_journal_event(context: ReportContext<'_>, event: &AlertEvent) -> String {
    let mut text = format!(
        "{} **{} · 日志告警**",
        severity_icon(event.severity),
        event.severity.label()
    );
    push_field(&mut text, "主机", context.host);
    if let Some(ip) = context.ip {
        push_field(&mut text, "IP", ip);
    }
    push_field(
        &mut text,
        "服务",
        event
            .details
            .get("服务")
            .map(String::as_str)
            .unwrap_or("未知"),
    );
    push_field(
        &mut text,
        "命中规则",
        event
            .details
            .get("命中规则")
            .map(String::as_str)
            .unwrap_or("未知"),
    );
    let (time_label, occurred_at) = journal_event_time(event);
    push_field(&mut text, time_label, &occurred_at);
    if let Some(sample) = event.details.get("日志").filter(|value| !value.is_empty()) {
        push_quote_field(&mut text, "日志", sample);
    }
    push_field(&mut text, "统计", &journal_event_statistics(event));
    push_field(&mut text, "检查", &event.check_name);
    if let Some(runbook) = &event.runbook {
        push_field(&mut text, "处理", runbook);
    }
    push_identity(&mut text, context);
    text
}

fn journal_event_time(event: &AlertEvent) -> (&'static str, String) {
    if let Some(occurred_at) = event
        .details
        .get("日志时间")
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
    {
        return (
            "日志时间",
            occurred_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S%.3f")
                .to_string(),
        );
    }
    (
        "发现时间",
        event
            .started_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S%.3f")
            .to_string(),
    )
}

fn journal_event_statistics(event: &AlertEvent) -> String {
    let read = event
        .details
        .get("本批读取")
        .map(String::as_str)
        .unwrap_or("0");
    let hits = event
        .details
        .get("本次命中")
        .map(String::as_str)
        .unwrap_or("0");
    let window = event
        .details
        .get("窗口累计")
        .map(String::as_str)
        .unwrap_or(hits);
    format!("本批读取 {read} 行，规则命中 {hits} 次；窗口累计 {window} 次")
}

pub fn format_daily(
    context: ReportContext<'_>,
    checks: &[CheckConfig],
    observations: &[Observation],
    states: &HashMap<String, CheckState>,
    queue_pending: usize,
) -> String {
    let mut text = "📋 **DAILY**".to_string();
    push_field(&mut text, "主机", context.host);
    if let Some(ip) = context.ip {
        push_field(&mut text, "IP", ip);
    }
    push_identity(&mut text, context);
    let mut current: Vec<_> = states
        .iter()
        .filter(|(_, state)| state.firing_since.is_some())
        .map(|(name, _)| name.as_str())
        .collect();
    current.sort_unstable();
    push_field(
        &mut text,
        "当前异常",
        &if current.is_empty() {
            "无".into()
        } else {
            current.join(", ")
        },
    );

    let mut resources = Vec::new();
    let mut resource_checks = 0_usize;
    let mut cpu_rows = None;
    let mut applications = (0, 0);
    let mut data_chain = (0, 0);
    let mut business_metrics = Vec::new();
    let mut platform = Vec::new();
    let mut journal_warn = 0_u64;
    let mut journal_critical = 0_u64;
    let mut journal_checks = 0_usize;
    for check in checks.iter().filter(|check| check.enabled) {
        let observation = observations
            .iter()
            .find(|item| item.check_name == check.name);
        match &check.kind {
            CheckKind::Cpu { .. } | CheckKind::Memory { .. } | CheckKind::Disk { .. } => {
                resource_checks += 1;
                if let Some(item) = observation {
                    resources.push(format!("{}: {}", check.name, item.summary));
                    if matches!(check.kind, CheckKind::Cpu { .. }) {
                        cpu_rows = item.details.get("每核").cloned();
                    }
                } else {
                    resources.push(format_unavailable_check(check, observations));
                }
            }
            CheckKind::Process { .. } | CheckKind::Systemd { .. } => {
                applications.1 += 1;
                applications.0 += usize::from(observation.is_some_and(is_healthy));
            }
            CheckKind::Shm { .. } | CheckKind::LatestFile { .. } => {
                data_chain.1 += 1;
                data_chain.0 += usize::from(observation.is_some_and(is_healthy));
            }
            CheckKind::MetricsFile { .. } | CheckKind::MetricsShm { .. } => {
                business_metrics.push(format_metrics_report(check, observation));
            }
            CheckKind::Journal { .. } => {
                journal_checks += 1;
                if let Some(state) = states.get(&check.name) {
                    journal_warn = journal_warn.saturating_add(state.daily_warn_count);
                    journal_critical = journal_critical.saturating_add(state.daily_critical_count);
                }
            }
            CheckKind::TimeSync { .. } | CheckKind::Network { .. } | CheckKind::SystemTuning => {
                platform.push(match observation {
                    Some(item) => format!("{}: {}", check.name, item.summary),
                    None => format_unavailable_check(check, observations),
                });
            }
        }
    }
    if resource_checks > 0 {
        push_field(&mut text, "主机资源", &resources.join("\n"));
    }
    if let Some(rows) = cpu_rows {
        push_field(&mut text, "每核 CPU", &rows);
    }
    if applications.1 > 0 {
        push_field(
            &mut text,
            "进程/systemd",
            &format!("{}/{} 正常", applications.0, applications.1),
        );
    }
    if data_chain.1 > 0 {
        push_field(
            &mut text,
            "SHM/文件链路",
            &format!("{}/{} 正常", data_chain.0, data_chain.1),
        );
    }
    if !business_metrics.is_empty() {
        push_field(&mut text, "业务指标", &business_metrics.join("\n"));
    }
    if journal_checks > 0 {
        push_field(
            &mut text,
            "日志 24h",
            &format!("WARN {journal_warn}，ERROR {journal_critical}"),
        );
    }
    if !platform.is_empty() {
        push_field(&mut text, "时钟/调优/网络", &platform.join("\n"));
    }
    push_field(&mut text, "投递队列", &format!("待发送 {queue_pending} 条"));
    text
}

pub fn format_internal(
    context: ReportContext<'_>,
    severity: Severity,
    title: &str,
    detail: &str,
) -> String {
    let mut text = format!(
        "{} **{} · alertd 自监控**",
        severity_icon(severity),
        severity.label()
    );
    push_field(&mut text, "主机", context.host);
    if let Some(ip) = context.ip {
        push_field(&mut text, "IP", ip);
    }
    push_identity(&mut text, context);
    push_field(&mut text, "状态", title);
    push_field(&mut text, "详情", detail);
    text
}

pub fn format_test(context: ReportContext<'_>) -> String {
    let mut text = "🟢 **OK · alertd 测试**".to_string();
    push_field(&mut text, "主机", context.host);
    if let Some(ip) = context.ip {
        push_field(&mut text, "IP", ip);
    }
    push_identity(&mut text, context);
    push_field(&mut text, "状态", "配置与钉钉投递正常");
    text
}

fn is_healthy(observation: &Observation) -> bool {
    matches!(observation.status, ObservationStatus::Healthy)
}

fn format_metrics_report(check: &CheckConfig, observation: Option<&Observation>) -> String {
    let Some(observation) = observation else {
        return format!("{}: 不可用", check.name);
    };
    let value = observation
        .details
        .get("指标")
        .map(|metrics| metrics.replace('\n', " · "))
        .unwrap_or_else(|| observation.summary.clone());
    format!("{}: {value}", check.name)
}

fn format_unavailable_check(check: &CheckConfig, observations: &[Observation]) -> String {
    let collector_name = format!("{}/collector", check.name);
    let reason = observations
        .iter()
        .find(|item| item.check_name == collector_name)
        .map(unavailable_reason)
        .unwrap_or_else(|| "无本轮数据".into());
    format!("{}: 采集不可用（{reason}）", check.name)
}

fn unavailable_reason(observation: &Observation) -> String {
    observation
        .details
        .get("错误")
        .cloned()
        .unwrap_or_else(|| observation.summary.clone())
}

fn push_field(text: &mut String, label: &str, value: &str) {
    text.push_str(&format!("\n\n**{label}：** {value}"));
}

fn push_quote_field(text: &mut String, label: &str, value: &str) {
    text.push_str(&format!("\n\n**{label}：**\n"));
    for line in value.lines() {
        text.push_str("> ");
        text.push_str(line);
        text.push('\n');
    }
    text.pop();
}

fn push_identity(text: &mut String, context: ReportContext<'_>) {
    push_field(text, "系统主机", context.system_hostname);
    push_field(
        text,
        "实例",
        &format!(
            "machine={} boot={} pid={} config={}",
            short_fingerprint(context.machine_sha256),
            short_fingerprint(context.boot_sha256),
            context.pid,
            short_fingerprint(context.config_sha256),
        ),
    );
}

fn short_fingerprint(value: &str) -> String {
    value.chars().take(12).collect()
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
