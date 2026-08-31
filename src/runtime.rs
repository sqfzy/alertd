//! 守护进程编排：全局监控开关、采集、热加载、日报、自监控和有界关闭。

use crate::{
    alarm::{self, AlarmPolicy},
    collectors::{self, CollectContext},
    config::{self, CheckConfig, Config, LoadedConfig},
    delivery::{dingtalk::DingTalkClient, queue::DeliveryQueue},
    identity::{self, RuntimeIdentity},
    model::{CheckState, Observation, Severity},
    report,
    state::{self, PersistentState},
    systemd_notify,
};
use chrono::Local;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;
use std::{
    collections::HashSet,
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
    #[error(transparent)]
    Identity(#[from] identity::IdentityError),
    #[error("signal registration failed: {0}")]
    Signal(#[from] std::io::Error),
}

pub struct RuntimeOptions {
    pub config_path: PathBuf,
    pub loaded_config: LoadedConfig,
    pub dry_run: bool,
}

#[derive(Default)]
struct RuntimeHealth {
    queue_warned: bool,
    state_save_failed: bool,
}

pub fn run(options: RuntimeOptions) -> Result<(), RuntimeError> {
    let LoadedConfig {
        mut config,
        source_sha256,
    } = options.loaded_config;
    let initial_identity = identity::load_runtime_identity(&config, source_sha256)?;
    let mut persistent = state::load(&config.runtime.state_dir)?;
    initialize_monitoring_state(config.runtime.enabled, &mut persistent);
    retain_active_check_state(&mut persistent, &config.checks);
    let queue = DeliveryQueue::open(&config.runtime.state_dir, config.delivery.queue_capacity)?;
    let initial_context = report_context(&initial_identity);
    let identity = Arc::new(RwLock::new(initial_identity.clone()));
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
    let mut health = RuntimeHealth::default();
    info!(
        host = initial_identity.host,
        system_hostname = initial_identity.system_hostname,
        machine_sha256 = initial_identity.machine_sha256,
        boot_sha256 = initial_identity.boot_sha256,
        pid = initial_identity.pid,
        config_sha256 = initial_identity.config_sha256,
        monitoring_enabled = config.runtime.enabled,
        "alertd started"
    );
    notify_monitoring_status(config.runtime.enabled, true);
    while !stop.load(Ordering::Relaxed) {
        if reload.swap(false, Ordering::Relaxed) {
            reload_config(
                &options.config_path,
                &mut config,
                &mut persistent,
                &mut context,
                &queue,
                &identity,
                options.dry_run,
            );
        }
        let current_identity = read_identity(&identity);
        let report_context = report_context(&current_identity);
        retry_monitoring_notice(
            &config,
            report_context,
            &mut persistent,
            &queue,
            options.dry_run,
        );
        run_monitoring_cycle(
            &config,
            report_context,
            &mut persistent,
            &mut context,
            &queue,
            options.dry_run,
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
        notify_monitoring_status(config.runtime.enabled, false);
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

pub fn send_test(
    config: &Config,
    config_sha256: String,
    dry_run: bool,
) -> Result<(), RuntimeError> {
    let identity = identity::load_runtime_identity(config, config_sha256)?;
    let text = report::format_test(report_context(&identity));
    if dry_run {
        println!("{text}");
    } else {
        build_client(config)?.send(&text, false)?;
    }
    Ok(())
}

fn initialize_monitoring_state(enabled: bool, persistent: &mut PersistentState) {
    match persistent.monitoring_enabled {
        None if enabled => {
            persistent.monitoring_enabled = Some(true);
            persistent.pending_monitoring_notice = None;
        }
        Some(previous) if previous == enabled => {}
        _ => apply_persistent_monitoring_transition(enabled, persistent),
    }
    if !enabled {
        reset_persistent_monitoring_state(persistent);
    }
}

fn apply_monitoring_transition(
    enabled: bool,
    persistent: &mut PersistentState,
    context: &mut CollectContext,
) {
    // 开关切换是监控时间线的断点；清空 cursor 才能让 journald 从重新开启时继续。
    apply_persistent_monitoring_transition(enabled, persistent);
    reset_collect_context(context);
}

fn apply_persistent_monitoring_transition(enabled: bool, persistent: &mut PersistentState) {
    reset_persistent_monitoring_state(persistent);
    persistent.monitoring_enabled = Some(enabled);
    persistent.pending_monitoring_notice = Some(enabled);
}

fn reset_persistent_monitoring_state(persistent: &mut PersistentState) {
    persistent.checks.clear();
    persistent.journal_cursors.clear();
    // 日报日期和进程生命周期不属于监控采样状态，切换时必须保留。
}

fn reset_collect_context(context: &mut CollectContext) {
    context.shm_progress.clear();
    context.journal_cursors.clear();
    context.pending_journal_cursors.clear();
    context.cpu_times.clear();
    context.network_samples.clear();
}

fn retry_monitoring_notice(
    config: &Config,
    context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    queue: &DeliveryQueue,
    dry_run: bool,
) {
    let Some(enabled) = persistent.pending_monitoring_notice else {
        return;
    };
    let (severity, title, detail) = if enabled {
        (
            Severity::Ok,
            "alertd 监控已开启",
            "已清空旧告警状态和采样基线；journald 从当前时间开始读取",
        )
    } else {
        (
            Severity::Warn,
            "alertd 监控已关闭",
            "所有 check、告警判断和日报已停止；daemon 与已有队列投递继续运行",
        )
    };
    if enqueue_internal(
        queue,
        severity,
        report::format_internal(context, severity, title, detail),
        dry_run,
    ) {
        persistent.pending_monitoring_notice = None;
        persist_monitoring_notice_state(&config.runtime.state_dir, persistent);
        info!(enabled, "monitoring switch notification queued");
    }
}

fn persist_monitoring_notice_state(state_dir: &Path, persistent: &PersistentState) {
    if let Err(error_value) = state::save(state_dir, persistent) {
        error!(
            error = %error_value,
            "cannot immediately persist monitoring switch notification state"
        );
    }
}

fn run_monitoring_cycle(
    config: &Config,
    report_context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    context: &mut CollectContext,
    queue: &DeliveryQueue,
    dry_run: bool,
) -> Vec<Observation> {
    if !config.runtime.enabled {
        debug!("monitoring cycle skipped because runtime.enabled=false");
        return Vec::new();
    }
    let observations = run_checks(config, report_context, persistent, context, queue, dry_run);
    persistent
        .journal_cursors
        .clone_from(&context.journal_cursors);
    maybe_daily(
        config,
        report_context,
        persistent,
        queue,
        dry_run,
        &observations,
    );
    observations
}

fn run_checks(
    config: &Config,
    report_context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    context: &mut CollectContext,
    queue: &DeliveryQueue,
    dry_run: bool,
) -> Vec<Observation> {
    let mut observations = Vec::new();
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
                    // 需要通知时，journal cursor 必须晚于消息入队，避免队列拒绝时越过日志。
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
    observations
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
    let accepted = enqueue_check(queue, &check.name, event.severity, text, dry_run);
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

fn enqueue_check(
    queue: &DeliveryQueue,
    check_name: &str,
    severity: Severity,
    text: String,
    dry_run: bool,
) -> bool {
    if dry_run {
        println!("--- alertd dry-run ---\n{text}\n");
        return true;
    }
    match queue.enqueue_check(check_name, severity, text) {
        Ok(id) => {
            info!(%id, %check_name, "alert queued");
            true
        }
        Err(error) => {
            error!(%error, %check_name, "ALERT LOST: durable queue rejected message");
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
            let current = read_identity(&identity);
            let context = report_context(&current);
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
    context: &mut CollectContext,
    queue: &DeliveryQueue,
    identity: &Arc<RwLock<RuntimeIdentity>>,
    dry_run: bool,
) {
    match config::load_config_with_sha256(path) {
        Ok(next) if startup_config_equal(config, &next.config) => {
            let monitoring_changed = config.runtime.enabled != next.config.runtime.enabled;
            let active_checks = active_check_names(&next.config.checks);
            match queue.discard_inactive_checks(&active_checks) {
                Ok(discarded) if discarded != 0 => {
                    info!(discarded, "discarded queued alerts for inactive checks");
                }
                Ok(_) => {}
                Err(error) => {
                    error!(%error, "configuration reload rejected; queued alerts could not be reconciled");
                    reject_reload(identity, queue, dry_run, &error.to_string());
                    return;
                }
            }
            retain_active_check_state(persistent, &next.config.checks);
            context
                .journal_cursors
                .retain(|name, _| active_checks.contains(name));
            context
                .pending_journal_cursors
                .retain(|name, _| active_checks.contains(name));
            *config = next.config;
            if monitoring_changed {
                apply_monitoring_transition(config.runtime.enabled, persistent, context);
                persist_monitoring_notice_state(&config.runtime.state_dir, persistent);
                info!(
                    enabled = config.runtime.enabled,
                    "monitoring switch applied from reloaded configuration"
                );
                notify_monitoring_status(config.runtime.enabled, false);
            }
            update_identity(identity, config, next.source_sha256.clone());
            info!(config_sha256 = next.source_sha256, "configuration reloaded");
        }
        Ok(_) => {
            reject_reload(
                identity,
                queue,
                dry_run,
                "启动级字段发生变化：state_dir、log_level、command_timeout 或 delivery",
            );
        }
        Err(error) => {
            error!(%error, "configuration reload rejected; old configuration remains active");
            reject_reload(identity, queue, dry_run, &error.to_string());
        }
    }
}

fn active_check_names(checks: &[CheckConfig]) -> HashSet<String> {
    checks
        .iter()
        .filter(|check| check.enabled)
        .map(|check| check.name.clone())
        .collect()
}

fn retain_active_check_state(persistent: &mut PersistentState, checks: &[CheckConfig]) {
    let active_checks = active_check_names(checks);
    persistent.checks.retain(|name, _| {
        active_checks.contains(name)
            || name
                .strip_suffix("/collector")
                .is_some_and(|parent| active_checks.contains(parent))
    });
    persistent
        .journal_cursors
        .retain(|name, _| active_checks.contains(name));
}

fn startup_config_equal(current: &Config, next: &Config) -> bool {
    current.runtime.state_dir == next.runtime.state_dir
        && current.runtime.log_level == next.runtime.log_level
        && current.runtime.command_timeout == next.runtime.command_timeout
        && current.delivery == next.delivery
}

fn reject_reload(
    identity: &Arc<RwLock<RuntimeIdentity>>,
    queue: &DeliveryQueue,
    dry_run: bool,
    detail: &str,
) {
    let current = read_identity(identity);
    enqueue_internal(
        queue,
        Severity::Warn,
        report::format_internal(
            report_context(&current),
            Severity::Warn,
            "配置热加载被拒绝",
            detail,
        ),
        dry_run,
    );
}

fn update_identity(shared: &Arc<RwLock<RuntimeIdentity>>, config: &Config, config_sha256: String) {
    let mut current = shared.write().unwrap_or_else(|value| value.into_inner());
    identity::update_config_identity(&mut current, config, config_sha256);
}

fn read_identity(shared: &Arc<RwLock<RuntimeIdentity>>) -> RuntimeIdentity {
    shared
        .read()
        .map(|value| value.clone())
        .unwrap_or_else(|value| value.into_inner().clone())
}

fn report_context(identity: &RuntimeIdentity) -> report::ReportContext<'_> {
    report::ReportContext {
        host: &identity.host,
        ip: identity.ip.as_deref(),
        system_hostname: &identity.system_hostname,
        machine_sha256: &identity.machine_sha256,
        boot_sha256: &identity.boot_sha256,
        pid: identity.pid,
        config_sha256: &identity.config_sha256,
    }
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

fn notify_monitoring_status(enabled: bool, ready: bool) {
    let status = if enabled {
        "Monitoring enabled"
    } else {
        "Monitoring disabled"
    };
    let message = if ready {
        format!("READY=1\nSTATUS={status}")
    } else {
        format!("WATCHDOG=1\nSTATUS={status}")
    };
    notify_systemd(&message);
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

    fn test_report_context() -> report::ReportContext<'static> {
        report::ReportContext {
            host: "test-host",
            ip: Some("192.0.2.1"),
            system_hostname: "system-host",
            machine_sha256: "machine",
            boot_sha256: "boot",
            pid: 7,
            config_sha256: "config",
        }
    }

    #[test]
    fn hot_reload_rejects_startup_fields_only() {
        let current = config();
        let mut mutable = current.clone();
        mutable.runtime.enabled = false;
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

    #[test]
    fn removes_state_for_inactive_checks() {
        let mut persistent = PersistentState::default();
        persistent
            .checks
            .insert("memory".into(), CheckState::default());
        persistent
            .checks
            .insert("memory/collector".into(), CheckState::default());
        persistent
            .checks
            .insert("removed".into(), CheckState::default());
        persistent
            .journal_cursors
            .insert("removed".into(), "old".into());

        let config = config();
        retain_active_check_state(&mut persistent, &config.checks);

        assert!(persistent.checks.contains_key("memory"));
        assert!(persistent.checks.contains_key("memory/collector"));
        assert!(!persistent.checks.contains_key("removed"));
        assert!(persistent.journal_cursors.is_empty());
    }

    #[test]
    fn hot_reload_updates_identity_only_after_success() {
        let temporary = tempfile::tempdir().unwrap();
        let config_path = temporary.path().join("alertd.toml");
        let initial_text = r#"
[runtime]
host = "old-role"
state_dir = "/tmp/alertd"
[[checks]]
name = "memory"
type = "memory"
warn_available_pct = 20
critical_available_pct = 10
"#;
        std::fs::write(&config_path, initial_text).unwrap();
        let mut current = config::load_config(&config_path).unwrap();
        let identity = Arc::new(RwLock::new(RuntimeIdentity {
            host: "old-role".into(),
            ip: None,
            system_hostname: "system-host".into(),
            machine_sha256: "machine".into(),
            boot_sha256: "boot".into(),
            pid: 7,
            config_sha256: "old-hash".into(),
        }));
        let queue = DeliveryQueue::open(temporary.path(), 16).unwrap();
        let mut persistent = PersistentState::default();
        let mut context = CollectContext::default();
        let next_text = initial_text.replace("old-role", "new-role");
        std::fs::write(&config_path, &next_text).unwrap();

        reload_config(
            &config_path,
            &mut current,
            &mut persistent,
            &mut context,
            &queue,
            &identity,
            true,
        );
        let accepted = read_identity(&identity);
        assert_eq!(accepted.host, "new-role");
        assert_ne!(accepted.config_sha256, "old-hash");

        std::fs::write(&config_path, "invalid = [").unwrap();
        reload_config(
            &config_path,
            &mut current,
            &mut persistent,
            &mut context,
            &queue,
            &identity,
            true,
        );
        let rejected = read_identity(&identity);
        assert_eq!(rejected.host, accepted.host);
        assert_eq!(rejected.config_sha256, accepted.config_sha256);
    }

    #[test]
    fn hot_reload_applies_monitoring_switch_and_preserves_queued_alerts() {
        let temporary = tempfile::tempdir().unwrap();
        let config_path = temporary.path().join("alertd.toml");
        let initial_text = format!(
            r#"
[runtime]
enabled = true
state_dir = "{}"
[[checks]]
name = "memory"
type = "memory"
warn_available_pct = 20
critical_available_pct = 10
"#,
            temporary.path().display()
        );
        std::fs::write(&config_path, &initial_text).unwrap();
        let mut current = config::load_config(&config_path).unwrap();
        let identity = Arc::new(RwLock::new(RuntimeIdentity {
            host: "role".into(),
            ip: None,
            system_hostname: "system-host".into(),
            machine_sha256: "machine".into(),
            boot_sha256: "boot".into(),
            pid: 7,
            config_sha256: "old-hash".into(),
        }));
        let queue = DeliveryQueue::open(temporary.path(), 16).unwrap();
        queue
            .enqueue_check("memory", Severity::Critical, "already queued".into())
            .unwrap();
        let mut persistent = PersistentState {
            monitoring_enabled: Some(true),
            ..Default::default()
        };
        persistent
            .checks
            .insert("memory".into(), CheckState::default());
        persistent
            .journal_cursors
            .insert("journal".into(), "old".into());
        let mut context = CollectContext::default();
        context
            .journal_cursors
            .insert("journal".into(), "old".into());
        context.cpu_times.insert(
            "cpu".into(),
            collectors::cpu::parse_cpu_times("cpu0 1 2 3 4 5 6 7 8").unwrap(),
        );
        std::fs::write(
            &config_path,
            initial_text.replace("enabled = true", "enabled = false"),
        )
        .unwrap();

        reload_config(
            &config_path,
            &mut current,
            &mut persistent,
            &mut context,
            &queue,
            &identity,
            false,
        );

        assert!(!current.runtime.enabled);
        assert!(persistent.checks.is_empty());
        assert!(persistent.journal_cursors.is_empty());
        assert!(context.journal_cursors.is_empty());
        assert!(context.cpu_times.is_empty());
        assert_eq!(persistent.monitoring_enabled, Some(false));
        assert_eq!(persistent.pending_monitoring_notice, Some(false));
        assert_eq!(queue.pending_count().unwrap(), 1);
        let saved = state::load(temporary.path()).unwrap();
        assert_eq!(saved.monitoring_enabled, Some(false));
        assert_eq!(saved.pending_monitoring_notice, Some(false));

        std::fs::write(
            &config_path,
            initial_text.replace("enabled = true", "enabled = \"false\""),
        )
        .unwrap();
        reload_config(
            &config_path,
            &mut current,
            &mut persistent,
            &mut context,
            &queue,
            &identity,
            true,
        );
        assert!(!current.runtime.enabled);
        assert_eq!(persistent.monitoring_enabled, Some(false));
    }

    #[test]
    fn disabled_initialization_clears_monitoring_state_and_requests_notice() {
        let mut persistent = PersistentState::default();
        persistent
            .checks
            .insert("memory".into(), CheckState::default());
        persistent
            .journal_cursors
            .insert("journal".into(), "old-cursor".into());
        persistent.last_daily_date = Some("2026-08-27".into());

        initialize_monitoring_state(false, &mut persistent);

        assert!(persistent.checks.is_empty());
        assert!(persistent.journal_cursors.is_empty());
        assert_eq!(persistent.last_daily_date.as_deref(), Some("2026-08-27"));
        assert_eq!(persistent.monitoring_enabled, Some(false));
        assert_eq!(persistent.pending_monitoring_notice, Some(false));
    }

    #[test]
    fn first_enabled_start_preserves_existing_monitoring_state() {
        let mut persistent = PersistentState::default();
        persistent
            .checks
            .insert("memory".into(), CheckState::default());

        initialize_monitoring_state(true, &mut persistent);

        assert!(persistent.checks.contains_key("memory"));
        assert_eq!(persistent.monitoring_enabled, Some(true));
        assert_eq!(persistent.pending_monitoring_notice, None);
    }

    #[test]
    fn monitoring_transition_resets_alarm_cursors_and_sampling_baselines() {
        let mut persistent = PersistentState::default();
        persistent
            .checks
            .insert("memory".into(), CheckState::default());
        persistent
            .journal_cursors
            .insert("journal".into(), "old-cursor".into());
        persistent.last_daily_date = Some("2026-08-27".into());
        let mut context = CollectContext::default();
        context
            .journal_cursors
            .insert("journal".into(), "old-cursor".into());
        context
            .pending_journal_cursors
            .insert("journal".into(), "pending-cursor".into());
        context.cpu_times.insert(
            "cpu".into(),
            collectors::cpu::parse_cpu_times("cpu0 1 2 3 4 5 6 7 8").unwrap(),
        );

        apply_monitoring_transition(false, &mut persistent, &mut context);

        assert!(persistent.checks.is_empty());
        assert!(persistent.journal_cursors.is_empty());
        assert_eq!(persistent.last_daily_date.as_deref(), Some("2026-08-27"));
        assert!(context.journal_cursors.is_empty());
        assert!(context.pending_journal_cursors.is_empty());
        assert!(context.cpu_times.is_empty());
        assert_eq!(persistent.monitoring_enabled, Some(false));
        assert_eq!(persistent.pending_monitoring_notice, Some(false));
    }

    #[test]
    fn monitoring_notice_is_deduplicated_after_queue_acceptance() {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = config();
        config.runtime.state_dir = temporary.path().into();
        let queue = DeliveryQueue::open(temporary.path(), 16).unwrap();
        let mut persistent = PersistentState {
            monitoring_enabled: Some(false),
            pending_monitoring_notice: Some(false),
            ..Default::default()
        };

        retry_monitoring_notice(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
        );
        assert_eq!(persistent.pending_monitoring_notice, None);
        assert_eq!(queue.pending_count().unwrap(), 1);
        let (_, message) = queue.oldest().unwrap().unwrap();
        assert_eq!(message.severity, Severity::Warn);
        assert!(message.text.contains("alertd 监控已关闭"));

        let mut persistent = state::load(temporary.path()).unwrap();
        initialize_monitoring_state(false, &mut persistent);
        retry_monitoring_notice(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
        );
        assert_eq!(queue.pending_count().unwrap(), 1);
    }

    #[test]
    fn reenable_transition_starts_with_fresh_state_and_ok_notice() {
        let mut persistent = PersistentState {
            monitoring_enabled: Some(false),
            ..Default::default()
        };
        persistent
            .checks
            .insert("old-alert".into(), CheckState::default());
        persistent
            .journal_cursors
            .insert("journal".into(), "disabled-period".into());
        let mut context = CollectContext::default();
        context
            .journal_cursors
            .insert("journal".into(), "disabled-period".into());

        apply_monitoring_transition(true, &mut persistent, &mut context);

        assert!(persistent.checks.is_empty());
        assert!(persistent.journal_cursors.is_empty());
        assert!(context.journal_cursors.is_empty());
        assert_eq!(persistent.monitoring_enabled, Some(true));
        assert_eq!(persistent.pending_monitoring_notice, Some(true));
    }

    #[test]
    fn monitoring_notice_retries_after_queue_rejection() {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = config();
        config.runtime.state_dir = temporary.path().into();
        let queue = DeliveryQueue::open(temporary.path(), 1).unwrap();
        queue
            .enqueue_internal(Severity::Warn, "occupied".into())
            .unwrap();
        let mut persistent = PersistentState {
            monitoring_enabled: Some(true),
            pending_monitoring_notice: Some(true),
            ..Default::default()
        };

        retry_monitoring_notice(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
        );
        assert_eq!(persistent.pending_monitoring_notice, Some(true));
        let (path, _) = queue.oldest().unwrap().unwrap();
        queue.acknowledge(&path).unwrap();

        retry_monitoring_notice(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
        );
        assert_eq!(persistent.pending_monitoring_notice, None);
        let (_, message) = queue.oldest().unwrap().unwrap();
        assert_eq!(message.severity, Severity::Ok);
        assert!(message.text.contains("alertd 监控已开启"));
    }

    #[test]
    fn disabled_cycle_does_not_collect_advance_state_or_send_daily_report() {
        let mut config = config();
        config.runtime.enabled = false;
        let queue_root = tempfile::tempdir().unwrap();
        let queue = DeliveryQueue::open(queue_root.path(), 16).unwrap();
        let mut persistent = PersistentState::default();
        persistent
            .checks
            .insert("memory".into(), CheckState::default());
        persistent
            .journal_cursors
            .insert("journal".into(), "saved".into());
        let before = serde_json::to_value(&persistent).unwrap();
        let mut context = CollectContext::default();
        context
            .journal_cursors
            .insert("journal".into(), "live".into());
        context
            .pending_journal_cursors
            .insert("journal".into(), "pending".into());

        let observations = run_monitoring_cycle(
            &config,
            test_report_context(),
            &mut persistent,
            &mut context,
            &queue,
            false,
        );

        assert_eq!(serde_json::to_value(&persistent).unwrap(), before);
        assert_eq!(queue.pending_count().unwrap(), 0);
        assert!(observations.is_empty());
        assert_eq!(
            context.journal_cursors.get("journal").map(String::as_str),
            Some("live")
        );
        assert_eq!(
            context
                .pending_journal_cursors
                .get("journal")
                .map(String::as_str),
            Some("pending")
        );
    }
}
