use alertd::{
    config::Config,
    model::{AlertEvent, Severity, Transition},
    report::{self, ReportContext},
};
use chrono::{TimeZone, Utc};
use std::collections::{BTreeMap, HashMap};

fn context(ip: Option<&'static str>) -> ReportContext<'static> {
    ReportContext {
        host: "sg-alertd-test",
        ip,
        system_hostname: "ip-10-0-0-1.internal",
        machine_sha256: "111111111111aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        boot_sha256: "222222222222bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        pid: 4242,
        config_sha256: "333333333333cccccccccccccccccccccccccccccccccccccccccccccccccccc",
    }
}

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

    let text = report::format_alert(context(Some("52.221.32.231")), &event);

    assert!(text.contains("\n\n**主机：** sg-alertd-test"));
    assert!(text.contains("\n\n**IP：** 52.221.32.231"));
    assert!(text.contains("\n\n**系统主机：** ip-10-0-0-1.internal"));
    assert!(text.contains(
        "\n\n**实例：** machine=111111111111 boot=222222222222 pid=4242 config=333333333333"
    ));
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
    let text = report::format_alert(context(Some("52.221.32.231")), &event);

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

    assert!(!report::format_alert(context(None), &event).contains("**IP：**"));
}

#[test]
fn journal_event_prioritizes_source_and_log_before_identity() {
    let now = Utc.with_ymd_and_hms(2026, 8, 13, 1, 2, 3).unwrap();
    let event = AlertEvent {
        check_name: "market-journal".into(),
        severity: Severity::Critical,
        transition: Transition::Event,
        started_at: now,
        observed_at: now,
        summary: "journal 命中".into(),
        details: BTreeMap::from([
            ("服务".into(), "market.service".into()),
            ("命中规则".into(), "fatal".into()),
            ("日志时间".into(), "2026-08-25T07:50:18.149168Z".into()),
            ("日志".into(), "first line\nsecond_line".into()),
            ("本批读取".into(), "146".into()),
            ("本次命中".into(), "1".into()),
            ("窗口累计".into(), "3".into()),
            ("units".into(), "market.service, archive.service".into()),
        ]),
        runbook: Some("https://runbook.example/journal".into()),
    };
    let text = report::format_alert(context(Some("13.230.124.110")), &event);

    assert!(text.starts_with("🔴 **CRITICAL · 日志告警**"));
    assert!(text.contains("**服务：** market.service"));
    assert!(text.contains("**命中规则：** fatal"));
    assert!(text.contains("**日志时间：**"));
    assert!(text.contains(":18.149"));
    assert!(text.contains("**日志：**\n> first line\n> second_line"));
    assert!(text.contains("**统计：** 本批读取 146 行，规则命中 1 次；窗口累计 3 次"));
    assert!(!text.contains("**状态：**"));
    assert!(!text.contains("**units：**"));
    assert!(!text.contains("异常开始"));
    assert!(!text.contains("恢复时间"));
    assert!(text.find("**检查：**").unwrap() < text.find("**系统主机：**").unwrap());
    assert!(text.find("**处理：**").unwrap() < text.find("**系统主机：**").unwrap());
}

#[test]
fn journal_event_falls_back_to_discovery_time_and_unknown_source() {
    let now = Utc.with_ymd_and_hms(2026, 8, 13, 1, 2, 3).unwrap();
    let event = AlertEvent {
        check_name: "journal".into(),
        severity: Severity::Warn,
        transition: Transition::Event,
        started_at: now,
        observed_at: now,
        summary: "journal 命中".into(),
        details: BTreeMap::from([("日志时间".into(), "invalid".into())]),
        runbook: None,
    };

    let text = report::format_alert(context(None), &event);
    assert!(text.contains("**服务：** 未知"));
    assert!(text.contains("**命中规则：** 未知"));
    assert!(text.contains("**发现时间：**"));
    assert!(!text.contains("**日志时间：**"));
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

[[checks]]
name = "latency"
type = "metrics_file"
path = "/run/market/latency.metrics.json"
stale_after = "90s"
metrics = [
  { key = "latency_p99_us", warn_above = 80 },
  { key = "samples" },
]

[[checks]]
name = "unavailable-metrics"
type = "metrics_shm"
path = "/unavailable-metrics"
metrics = [{ key = "value", offset = 0, value_type = "u64" }]
"#,
    )
    .unwrap();
    let cpu = alertd::model::Observation::healthy("cpu", "CPU 平均 20%，最高 cpu1 30%")
        .detail("每核", "cpu0 10% · cpu1 30%");
    let metrics = alertd::model::Observation::healthy("latency", "指标快照正常")
        .detail("指标", "latency_p99_us=72\nsamples=180000");
    let mut states = HashMap::new();
    states.insert(
        "journal".into(),
        alertd::model::CheckState {
            daily_warn_count: 3,
            daily_critical_count: 1,
            ..Default::default()
        },
    );
    let text = report::format_daily(context(None), &config.checks, &[cpu, metrics], &states, 2);
    assert!(text.contains("**每核 CPU：** cpu0 10% · cpu1 30%"));
    assert!(text.contains(
        "**业务指标：** latency: latency_p99_us=72 · samples=180000\nunavailable-metrics: 不可用"
    ));
    assert!(text.contains("**日志 24h：** WARN 3，ERROR 1"));
    assert!(text.contains("**投递队列：** 待发送 2 条"));
    assert!(!text.contains("**进程/systemd：**"));
    assert!(!text.contains("**SHM/文件链路：**"));
    assert!(!text.contains("**时钟/调优/网络：**"));
}

#[test]
fn daily_report_omits_all_unconfigured_groups() {
    let text = report::format_daily(context(None), &[], &[], &HashMap::new(), 0);
    assert!(!text.contains("**主机资源：**"));
    assert!(!text.contains("**进程/systemd：**"));
    assert!(!text.contains("**SHM/文件链路：**"));
    assert!(!text.contains("**业务指标：**"));
    assert!(!text.contains("**日志 24h：**"));
    assert!(!text.contains("**时钟/调优/网络：**"));
    assert!(text.contains("**投递队列：** 待发送 0 条"));
}

#[test]
fn daily_report_explains_configured_platform_collection_failure() {
    let config: Config = toml::from_str(
        r#"
[[checks]]
name = "clock"
type = "time_sync"
warn_offset = "1ms"
critical_offset = "5ms"
"#,
    )
    .unwrap();
    let failure =
        alertd::model::Observation::unhealthy("clock/collector", Severity::Warn, "监控采集盲区")
            .detail("错误", "chronyc timeout");

    let text = report::format_daily(
        context(None),
        &config.checks,
        &[failure],
        &HashMap::new(),
        0,
    );
    assert!(text.contains("**时钟/调优/网络：** clock: 采集不可用（chronyc timeout）"));
}

#[test]
fn every_message_kind_includes_the_same_identity() {
    let make_event = |transition| AlertEvent {
        check_name: "check".into(),
        severity: Severity::Warn,
        transition,
        started_at: Utc::now(),
        observed_at: Utc::now(),
        summary: "summary".into(),
        details: BTreeMap::new(),
        runbook: None,
    };
    let expected = "**实例：** machine=111111111111 boot=222222222222 pid=4242 config=333333333333";
    let messages = [
        report::format_alert(context(None), &make_event(Transition::Firing)),
        report::format_alert(context(None), &make_event(Transition::Repeating)),
        report::format_alert(context(None), &make_event(Transition::Resolved)),
        report::format_daily(context(None), &[], &[], &HashMap::new(), 0),
        report::format_internal(context(None), Severity::Warn, "title", "detail"),
        report::format_test(context(None)),
    ];
    assert!(messages.iter().all(|message| message.contains(expected)));
}
