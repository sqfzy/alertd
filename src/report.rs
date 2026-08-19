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
    let (icon, transition) = match event.transition {
        Transition::Firing => (severity_icon(event.severity), "告警"),
        Transition::Repeating => (severity_icon(event.severity), "持续"),
        Transition::Resolved => ("🟢", "恢复"),
        Transition::Event => (severity_icon(event.severity), "事件"),
    };
    let mut text = format!("{icon} **{} · {transition}**", event.severity.label());
    push_field(&mut text, "主机", context.host);
    if let Some(ip) = context.ip {
        push_field(&mut text, "IP", ip);
    }
    push_identity(&mut text, context);
    push_field(&mut text, "检查", &event.check_name);
    push_field(&mut text, "状态", &event.summary);
    let time_label = if event.transition == Transition::Event {
        "发生时间"
    } else {
        "异常开始"
    };
    push_field(
        &mut text,
        time_label,
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
    let mut cpu_rows = None;
    let mut applications = (0, 0);
    let mut data_chain = (0, 0);
    let mut platform = Vec::new();
    let mut journal_warn = 0_u64;
    let mut journal_critical = 0_u64;
    for check in checks.iter().filter(|check| check.enabled) {
        let observation = observations
            .iter()
            .find(|item| item.check_name == check.name);
        match &check.kind {
            CheckKind::Cpu { .. } | CheckKind::Memory { .. } | CheckKind::Disk { .. } => {
                if let Some(item) = observation {
                    resources.push(format!("{}: {}", check.name, item.summary));
                    if matches!(check.kind, CheckKind::Cpu { .. }) {
                        cpu_rows = item.details.get("每核").cloned();
                    }
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
            CheckKind::Journal { .. } => {
                if let Some(state) = states.get(&check.name) {
                    journal_warn = journal_warn.saturating_add(state.daily_warn_count);
                    journal_critical = journal_critical.saturating_add(state.daily_critical_count);
                }
            }
            CheckKind::TimeSync { .. } | CheckKind::Network { .. } | CheckKind::SystemTuning => {
                if let Some(item) = observation {
                    platform.push(format!("{}: {}", check.name, item.summary));
                }
            }
        }
    }
    push_field(&mut text, "主机资源", &resources.join("\n"));
    if let Some(rows) = cpu_rows {
        push_field(&mut text, "每核 CPU", &rows);
    }
    push_field(
        &mut text,
        "进程/systemd",
        &format!("{}/{} 正常", applications.0, applications.1),
    );
    push_field(
        &mut text,
        "SHM/文件链路",
        &format!("{}/{} 正常", data_chain.0, data_chain.1),
    );
    push_field(
        &mut text,
        "日志 24h",
        &format!("WARN {journal_warn}，ERROR {journal_critical}"),
    );
    push_field(&mut text, "时钟/调优/网络", &platform.join("\n"));
    push_field(&mut text, "投递队列", &queue_pending.to_string());
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

fn push_field(text: &mut String, label: &str, value: &str) {
    text.push_str(&format!("\n\n**{label}：** {value}"));
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
