use alertd::{
    model::{AlertEvent, Severity, Transition},
    report::{self, ReportContext},
};
use chrono::{TimeZone, Utc};
use std::collections::BTreeMap;

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
