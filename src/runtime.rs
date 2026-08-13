use crate::{
    alarm::{self, AlarmPolicy},
    collectors::{self, CollectContext},
    config::{self, CheckConfig, Config},
    delivery::{dingtalk::DingTalkClient, queue::DeliveryQueue},
    model::{CheckState, Observation, Severity},
    report,
    state::{self, PersistentState},
    systemd_notify,
};
use chrono::Local;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use thiserror::Error;
use tracing::{debug, error, info, warn};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    State(#[from] state::StateError),
    #[error(transparent)]
    Queue(#[from] crate::delivery::queue::QueueError),
    #[error(transparent)]
    DingTalk(#[from] crate::delivery::dingtalk::DingTalkError),
    #[error("signal registration failed: {0}")]
    Signal(#[from] std::io::Error),
}

pub struct RuntimeOptions {
    pub config_path: PathBuf,
    pub dry_run: bool,
}

#[derive(Default)]
struct RuntimeHealth {
    queue_warned: bool,
    state_save_failed: bool,
}

#[derive(Clone)]
struct RuntimeIdentity {
    host: String,
    ip: Option<String>,
}

pub fn run(options: RuntimeOptions) -> Result<(), RuntimeError> {
    let mut config = config::load_config(&options.config_path)?;
    let initial_host = resolve_host(&config);
    let mut persistent = state::load(&config.runtime.state_dir)?;
    let queue = DeliveryQueue::open(&config.runtime.state_dir, config.delivery.queue_capacity)?;
    let initial_context = report::ReportContext {
        host: &initial_host,
        ip: config.runtime.ip.as_deref(),
    };
    let identity = Arc::new(RwLock::new(RuntimeIdentity {
        host: initial_host.clone(),
        ip: config.runtime.ip.clone(),
    }));
    if persistent.clean_shutdown == Some(false) {
        enqueue_internal(
            &queue,
            Severity::Warn,
            report::format_internal(
                initial_context,
                Severity::Warn,
                "检测到上次非正常退出",
                "alertd 未记录完整的正常关闭流程",
            ),
            options.dry_run,
        );
    }
    persistent.clean_shutdown = Some(false);
    state::save(&config.runtime.state_dir, &persistent)?;
    let dingtalk = if options.dry_run {
        None
    } else {
        Some(build_client(&config)?)
    };
    let stop = Arc::new(AtomicBool::new(false));
    let reload = Arc::new(AtomicBool::new(false));
    flag::register(SIGINT, stop.clone())?;
    flag::register(SIGTERM, stop.clone())?;
    flag::register(SIGHUP, reload.clone())?;
    let delivery_worker = dingtalk.map(|client| {
        start_delivery_worker(
            queue.clone(),
            client,
            &config,
            identity.clone(),
            stop.clone(),
        )
    });
    let mut context = CollectContext {
        journal_cursors: persistent.journal_cursors.clone(),
        command_timeout: config::parse_duration(&config.runtime.command_timeout)?,
        ..Default::default()
    };
    let mut observations = Vec::new();
    let mut health = RuntimeHealth::default();
    info!(host = initial_host, "alertd started");
    notify_systemd("READY=1");
    while !stop.load(Ordering::Relaxed) {
        if reload.swap(false, Ordering::Relaxed) {
            reload_config(
                &options.config_path,
                &mut config,
                &mut persistent,
                &queue,
                options.dry_run,
            );
        }
        let host = resolve_host(&config);
        if let Ok(mut current) = identity.write() {
            current.host.clone_from(&host);
            current.ip.clone_from(&config.runtime.ip);
        }
        let report_context = report::ReportContext {
            host: &host,
            ip: config.runtime.ip.as_deref(),
        };
        observations.clear();
        run_checks(
            &config,
            report_context,
            &mut persistent,
            &mut context,
            &queue,
            options.dry_run,
            &mut observations,
        );
        persistent
            .journal_cursors
            .clone_from(&context.journal_cursors);
        maybe_daily(
            &config,
            report_context,
            &mut persistent,
            &queue,
            options.dry_run,
            &observations,
        );
        check_queue_health(
            &config,
            report_context,
            &queue,
            options.dry_run,
            &mut health,
        );
        save_runtime_state(
            &config,
            report_context,
            &persistent,
            &queue,
            options.dry_run,
            &mut health,
        );
        notify_systemd("WATCHDOG=1");
        sleep_interruptibly(
            config::parse_duration(&config.runtime.interval)?,
            &stop,
            &reload,
        );
    }
    info!("alertd stopping");
    notify_systemd("STOPPING=1");
    persistent.clean_shutdown = Some(true);
    if let Err(error) = state::save(&config.runtime.state_dir, &persistent) {
        error!(%error, "cannot persist clean shutdown state");
    }
    if let Some(worker) = delivery_worker {
        let _ = worker.join();
    }
    Ok(())
}

pub fn send_test(config: &Config, dry_run: bool) -> Result<(), RuntimeError> {
    let host = resolve_host(config);
    let mut text = format!("🟢 **OK · alertd 测试**\n\n**主机：** {host}");
    if let Some(ip) = &config.runtime.ip {
        text.push_str(&format!("\n\n**IP：** {ip}"));
    }
    text.push_str("\n\n**状态：** 配置与钉钉投递正常");
    if dry_run {
        println!("{text}");
    } else {
        build_client(config)?.send(&text, false)?;
    }
    Ok(())
}

fn run_checks(
    config: &Config,
    report_context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    context: &mut CollectContext,
    queue: &DeliveryQueue,
    dry_run: bool,
    observations: &mut Vec<Observation>,
) {
    let global_policy = AlarmPolicy::from_strings(
        &config.alarm.pending_for,
        &config.alarm.recover_for,
        &config.alarm.warn_repeat,
        &config.alarm.critical_repeat,
    )
    .expect("validated config");
    for check in config.checks.iter().filter(|check| check.enabled) {
        match collectors::collect(check, context) {
            Ok(observation) => {
                resolve_collector_alarm(
                    check,
                    persistent,
                    &global_policy,
                    report_context,
                    queue,
                    dry_run,
                );
                let state = persistent.checks.entry(check.name.clone()).or_default();
                state.collection_failures = 0;
                let accepted = process_observation(
                    check,
                    observation.clone(),
                    state,
                    &global_policy,
                    report_context,
                    queue,
                    dry_run,
                );
                if accepted {
                    if let Some(cursor) = context.pending_journal_cursors.remove(&check.name) {
                        context.journal_cursors.insert(check.name.clone(), cursor);
                    }
                }
                observations.push(observation);
            }
            Err(error_value) => {
                let state = persistent.checks.entry(check.name.clone()).or_default();
                state.collection_failures = state.collection_failures.saturating_add(1);
                warn!(check = %check.name, error = %error_value, failures = state.collection_failures, "collector failed");
                let observation = alarm::collection_failure_observation(
                    &check.name,
                    state.collection_failures,
                    config.alarm.collect_fail_after_n,
                    &error_value.to_string(),
                );
                let collector_name = observation.check_name.clone();
                let mut collector_state = persistent
                    .checks
                    .remove(&collector_name)
                    .unwrap_or_default();
                process_observation(
                    check,
                    observation.clone(),
                    &mut collector_state,
                    &global_policy,
                    report_context,
                    queue,
                    dry_run,
                );
                persistent.checks.insert(collector_name, collector_state);
                observations.push(observation);
            }
        }
    }
}

fn resolve_collector_alarm(
    check: &CheckConfig,
    persistent: &mut PersistentState,
    policy: &AlarmPolicy,
    report_context: report::ReportContext<'_>,
    queue: &DeliveryQueue,
    dry_run: bool,
) {
    let name = format!("{}/collector", check.name);
    let Some(mut state) = persistent.checks.remove(&name) else {
        return;
    };
    let observation = Observation::healthy(&name, "监控采集恢复");
    process_observation(
        check,
        observation,
        &mut state,
        policy,
        report_context,
        queue,
        dry_run,
    );
    persistent.checks.insert(name, state);
}

fn process_observation(
    check: &CheckConfig,
    observation: Observation,
    state: &mut CheckState,
    global: &AlarmPolicy,
    report_context: report::ReportContext<'_>,
    queue: &DeliveryQueue,
    dry_run: bool,
) -> bool {
    debug!(check = %observation.check_name, summary = %observation.summary, "observation");
    let mut policy = *global;
    if let Some(pending) = &check.pending_for {
        policy.pending_for = config::parse_duration(pending).expect("validated config");
    }
    if let Some(recover) = &check.recover_for {
        policy.recover_for = config::parse_duration(recover).expect("validated config");
    }
    if matches!(check.kind, config::CheckKind::Journal { .. }) {
        policy.pending_for = Duration::ZERO;
    }
    let previous = state.clone();
    let Some(event) = alarm::evaluate(state, &observation, &policy, check.runbook.clone()) else {
        return true;
    };
    let text = report::format_alert(report_context, &event);
    let accepted = enqueue(queue, event.severity, text, dry_run);
    if !accepted {
        *state = previous;
    }
    accepted
}

fn enqueue(queue: &DeliveryQueue, severity: Severity, text: String, dry_run: bool) -> bool {
    if dry_run {
        println!("--- alertd dry-run ---\n{text}\n");
        return true;
    }
    match queue.enqueue(severity, text) {
        Ok(id) => {
            info!(%id, "alert queued");
            true
        }
        Err(error) => {
            error!(%error, "ALERT LOST: durable queue rejected message");
            false
        }
    }
}

fn enqueue_internal(
    queue: &DeliveryQueue,
    severity: Severity,
    text: String,
    dry_run: bool,
) -> bool {
    if dry_run {
        println!("--- alertd internal dry-run ---\n{text}\n");
        return true;
    }
    match queue.enqueue_internal(severity, text) {
        Ok(id) => {
            info!(%id, "internal alert queued");
            true
        }
        Err(error) => {
            error!(%error, "INTERNAL ALERT LOST: durable queue rejected message");
            false
        }
    }
}

enum DrainOutcome {
    Idle,
    Delivered,
    Failed,
}

fn drain_once(
    queue: &DeliveryQueue,
    client: &DingTalkClient,
    context: report::ReportContext<'_>,
) -> DrainOutcome {
    match queue.oldest() {
        Ok(Some((path, message))) => {
            match client.send(&message.text, message.severity == Severity::Critical) {
                Ok(()) => {
                    if let Err(error) = queue.acknowledge(&path) {
                        error!(%error, "delivered alert could not be acknowledged; duplicate is possible");
                        return DrainOutcome::Failed;
                    }
                    DrainOutcome::Delivered
                }
                Err(error) => {
                    warn!(%error, id = %message.id, "delivery failed; message remains queued");
                    DrainOutcome::Failed
                }
            }
        }
        Ok(None) => DrainOutcome::Idle,
        Err(crate::delivery::queue::QueueError::Quarantined(path)) => {
            error!(path = %path.display(), "corrupt spool message quarantined");
            enqueue_internal(
                queue,
                Severity::Warn,
                report::format_internal(
                    context,
                    Severity::Warn,
                    "投递队列消息损坏",
                    &format!("已隔离 {}", path.display()),
                ),
                false,
            );
            DrainOutcome::Failed
        }
        Err(error) => {
            error!(%error, "cannot read delivery queue");
            DrainOutcome::Failed
        }
    }
}

fn start_delivery_worker(
    queue: DeliveryQueue,
    client: DingTalkClient,
    config: &Config,
    identity: Arc<RwLock<RuntimeIdentity>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let initial = config::parse_duration(&config.delivery.retry_initial).expect("validated config");
    let maximum = config::parse_duration(&config.delivery.retry_max).expect("validated config");
    let failure_report_after = config.delivery.failure_report_after;
    thread::spawn(move || {
        let mut retry = initial;
        let mut consecutive_failures = 0_u32;
        let mut degraded = false;
        while !stop.load(Ordering::Relaxed) {
            let current = identity
                .read()
                .map(|value| value.clone())
                .unwrap_or_else(|value| value.into_inner().clone());
            let context = report::ReportContext {
                host: &current.host,
                ip: current.ip.as_deref(),
            };
            match drain_once(&queue, &client, context) {
                DrainOutcome::Failed => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    degraded |= consecutive_failures >= failure_report_after;
                    sleep_worker(retry, &stop);
                    retry = retry.saturating_mul(2).min(maximum);
                }
                DrainOutcome::Delivered | DrainOutcome::Idle => {
                    retry = initial;
                    if degraded && queue.pending_count().is_ok_and(|count| count == 0) {
                        enqueue_internal(
                            &queue,
                            Severity::Ok,
                            report::format_internal(
                                context,
                                Severity::Ok,
                                "钉钉投递已恢复",
                                &format!("连续失败 {consecutive_failures} 次，积压已清空"),
                            ),
                            false,
                        );
                        degraded = false;
                    }
                    consecutive_failures = 0;
                    sleep_worker(Duration::from_millis(500), &stop);
                }
            }
        }
    })
}

fn sleep_worker(duration: Duration, stop: &AtomicBool) {
    let deadline = std::time::Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
}

fn maybe_daily(
    config: &Config,
    report_context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    queue: &DeliveryQueue,
    dry_run: bool,
    observations: &[Observation],
) {
    let Some(target) = &config.alarm.daily_report_at else {
        return;
    };
    let now = Local::now();
    let current = now.format("%H:%M").to_string();
    let date = now.format("%Y-%m-%d").to_string();
    if target == "off" || current < *target || persistent.last_daily_date.as_deref() == Some(&date)
    {
        return;
    }
    if enqueue(
        queue,
        Severity::Ok,
        report::format_daily(
            report_context,
            &config.checks,
            observations,
            &persistent.checks,
            queue.pending_count().unwrap_or_default(),
        ),
        dry_run,
    ) {
        persistent.last_daily_date = Some(date);
        for check in config
            .checks
            .iter()
            .filter(|check| matches!(check.kind, config::CheckKind::Journal { .. }))
        {
            if let Some(state) = persistent.checks.get_mut(&check.name) {
                state.daily_warn_count = 0;
                state.daily_critical_count = 0;
            }
        }
    }
}

fn reload_config(
    path: &Path,
    config: &mut Config,
    persistent: &mut PersistentState,
    queue: &DeliveryQueue,
    dry_run: bool,
) {
    match config::load_config(path) {
        Ok(next) if startup_config_equal(config, &next) => {
            persistent.checks.retain(|name, _| {
                next.checks.iter().any(|check| {
                    &check.name == name || format!("{}/collector", check.name) == *name
                })
            });
            *config = next;
            info!("configuration reloaded");
        }
        Ok(_) => {
            reject_reload(
                config,
                queue,
                dry_run,
                "启动级字段发生变化：state_dir、log_level、command_timeout 或 delivery",
            );
        }
        Err(error) => {
            error!(%error, "configuration reload rejected; old configuration remains active");
            reject_reload(config, queue, dry_run, &error.to_string());
        }
    }
}

fn startup_config_equal(current: &Config, next: &Config) -> bool {
    current.runtime.state_dir == next.runtime.state_dir
        && current.runtime.log_level == next.runtime.log_level
        && current.runtime.command_timeout == next.runtime.command_timeout
        && current.delivery == next.delivery
}

fn reject_reload(config: &Config, queue: &DeliveryQueue, dry_run: bool, detail: &str) {
    let host = resolve_host(config);
    enqueue_internal(
        queue,
        Severity::Warn,
        report::format_internal(
            report::ReportContext {
                host: &host,
                ip: config.runtime.ip.as_deref(),
            },
            Severity::Warn,
            "配置热加载被拒绝",
            detail,
        ),
        dry_run,
    );
}

fn resolve_host(config: &Config) -> String {
    config.runtime.host.clone().unwrap_or_else(|| {
        hostname::get()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    })
}

fn build_client(config: &Config) -> Result<DingTalkClient, RuntimeError> {
    let (token, secret) = config::resolve_dingtalk_credentials(&config.delivery)?;
    Ok(DingTalkClient::new(
        token,
        secret,
        config::parse_duration(&config.delivery.timeout)?,
        config.delivery.at_all_on_critical,
    )?)
}

fn check_queue_health(
    config: &Config,
    context: report::ReportContext<'_>,
    queue: &DeliveryQueue,
    dry_run: bool,
    health: &mut RuntimeHealth,
) {
    let Ok(pending) = queue.pending_count() else {
        return;
    };
    let threshold = config
        .delivery
        .queue_capacity
        .saturating_mul(config.delivery.queue_warn_pct.into())
        .div_ceil(100);
    if pending >= threshold && !health.queue_warned {
        health.queue_warned = enqueue_internal(
            queue,
            Severity::Warn,
            report::format_internal(
                context,
                Severity::Warn,
                "投递队列接近容量上限",
                &format!(
                    "当前 {pending}/{}，阈值 {}%",
                    config.delivery.queue_capacity, config.delivery.queue_warn_pct
                ),
            ),
            dry_run,
        );
    } else if pending < threshold {
        health.queue_warned = false;
    }
}

fn save_runtime_state(
    config: &Config,
    context: report::ReportContext<'_>,
    persistent: &PersistentState,
    queue: &DeliveryQueue,
    dry_run: bool,
    health: &mut RuntimeHealth,
) {
    match state::save(&config.runtime.state_dir, persistent) {
        Ok(()) => health.state_save_failed = false,
        Err(error_value) => {
            error!(error = %error_value, "cannot persist runtime state");
            if !health.state_save_failed {
                health.state_save_failed = enqueue_internal(
                    queue,
                    Severity::Warn,
                    report::format_internal(
                        context,
                        Severity::Warn,
                        "状态持久化失败",
                        &error_value.to_string(),
                    ),
                    dry_run,
                );
            }
        }
    }
}

fn notify_systemd(message: &str) {
    if let Err(error_value) = systemd_notify::send(message) {
        warn!(error = %error_value, message, "systemd notification failed");
    }
}

fn sleep_interruptibly(duration: Duration, stop: &AtomicBool, reload: &AtomicBool) {
    let deadline = std::time::Instant::now() + duration;
    let mut next_watchdog = std::time::Instant::now() + Duration::from_secs(30);
    while !stop.load(Ordering::Relaxed)
        && !reload.load(Ordering::Relaxed)
        && std::time::Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(200));
        if std::time::Instant::now() >= next_watchdog {
            notify_systemd("WATCHDOG=1");
            next_watchdog = std::time::Instant::now() + Duration::from_secs(30);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        toml::from_str(
            r#"
[runtime]
state_dir = "/tmp/alertd"
[[checks]]
name = "memory"
type = "memory"
warn_available_pct = 20
critical_available_pct = 10
"#,
        )
        .unwrap()
    }

    #[test]
    fn hot_reload_rejects_startup_fields_only() {
        let current = config();
        let mut mutable = current.clone();
        mutable.runtime.interval = "10s".into();
        mutable.alarm.recover_for = "10s".into();
        assert!(startup_config_equal(&current, &mutable));

        let mut state_dir = current.clone();
        state_dir.runtime.state_dir = "/var/lib/other".into();
        assert!(!startup_config_equal(&current, &state_dir));
        let mut delivery = current.clone();
        delivery.delivery.queue_capacity = 2048;
        assert!(!startup_config_equal(&current, &delivery));
    }
}
