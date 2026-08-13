use alertd::{
    config::Config,
    model::{AlertEvent, Severity, Transition},
    report::{self, ReportContext},
};
use chrono::{TimeZone, Utc};
use std::collections::{BTreeMap, HashMap};

#[test]
fn alert_fields_are_separate_markdown_paragraphs() {
    let event = AlertEvent {
        check_name: "data-disk".into(),
        severity: Severity::Warn,
        transition: Transition::Repeating,
        started_at: Utc.with_ymd_and_hms(2026, 8, 12, 7, 47, 58).unwrap(),
        observed_at: Utc.with_ymd_and_hms(2026, 8, 12, 8, 0, 0).unwrap(),
        summary: "磁盘已用 83.6%".into(),
        details: BTreeMap::from([("挂载点".into(), "/mnt/jt".into())]),
        runbook: None,
    };

    let context = ReportContext {
        host: "sg-alertd-test",
        ip: Some("52.221.32.231"),
    };
    let text = report::format_alert(context, &event);

    assert!(text.contains("\n\n**主机：** sg-alertd-test"));
    assert!(text.contains("\n\n**IP：** 52.221.32.231"));
    assert!(text.contains("\n\n**检查：** data-disk"));
    assert!(text.contains("\n\n**异常开始：**"));
    assert!(!text.contains("\n\n**开始：**"));
    assert!(text.contains("\n\n**挂载点：** /mnt/jt"));
}

#[test]
fn recovery_shows_readable_duration_and_recovery_time() {
    let event = AlertEvent {
        check_name: "data-disk".into(),
        severity: Severity::Ok,
        transition: Transition::Resolved,
        started_at: Utc.with_ymd_and_hms(2026, 8, 12, 7, 47, 58).unwrap(),
        observed_at: Utc.with_ymd_and_hms(2026, 8, 12, 10, 3, 3).unwrap(),
        summary: "磁盘使用率恢复正常".into(),
        details: BTreeMap::new(),
        runbook: None,
    };
    let context = ReportContext {
        host: "sg-alertd-test",
        ip: Some("52.221.32.231"),
    };

    let text = report::format_alert(context, &event);

    assert!(text.contains("\n\n**持续时间：** 2 小时 15 分钟 5 秒"));
    assert!(text.contains("\n\n**恢复时间：**"));
}

#[test]
fn alert_omits_unconfigured_ip() {
    let event = AlertEvent {
        check_name: "process".into(),
        severity: Severity::Critical,
        transition: Transition::Firing,
        started_at: Utc::now(),
        observed_at: Utc::now(),
        summary: "进程不存在".into(),
        details: BTreeMap::new(),
        runbook: None,
    };

    let context = ReportContext {
        host: "host",
        ip: None,
    };
    assert!(!report::format_alert(context, &event).contains("**IP：**"));
}

#[test]
fn journal_event_uses_occurrence_time_without_recovery_fields() {
    let now = Utc.with_ymd_and_hms(2026, 8, 13, 1, 2, 3).unwrap();
    let event = AlertEvent {
        check_name: "journal".into(),
        severity: Severity::Critical,
        transition: Transition::Event,
        started_at: now,
        observed_at: now,
        summary: "journal 命中".into(),
        details: BTreeMap::from([("本次命中".into(), "2".into())]),
        runbook: None,
    };
    let text = report::format_alert(
        ReportContext {
            host: "host",
            ip: None,
        },
        &event,
    );
    assert!(text.contains("**发生时间：**"));
    assert!(!text.contains("异常开始"));
    assert!(!text.contains("恢复时间"));
}

#[test]
fn daily_report_groups_checks_and_keeps_all_cpu_rows() {
    let config: Config = toml::from_str(
        r#"
[[checks]]
name = "cpu"
type = "cpu"
warn_usage_pct = 80
critical_usage_pct = 95

[[checks]]
name = "journal"
type = "journal"
units = ["app.service"]
rules = [{ contains = "WARN", severity = "warn" }]
"#,
    )
    .unwrap();
    let cpu = alertd::model::Observation::healthy("cpu", "CPU 平均 20%，最高 cpu1 30%")
        .detail("每核", "cpu0 10% · cpu1 30%");
    let mut states = HashMap::new();
    states.insert(
        "journal".into(),
        alertd::model::CheckState {
            daily_warn_count: 3,
            daily_critical_count: 1,
            ..Default::default()
        },
    );
    let text = report::format_daily(
        ReportContext {
            host: "host",
            ip: None,
        },
        &config.checks,
        &[cpu],
        &states,
        2,
    );
    assert!(text.contains("**每核 CPU：** cpu0 10% · cpu1 30%"));
    assert!(text.contains("**日志 24h：** WARN 3，ERROR 1"));
    assert!(text.contains("**投递队列：** 2"));
}
