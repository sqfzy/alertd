use crate::{
    alarm::{self, AlarmPolicy},
    collectors::{self, CollectContext},
    config::{self, CheckConfig, Config, LoadedConfig},
    delivery::{dingtalk::DingTalkClient, queue::DeliveryQueue},
    identity::{self, RuntimeIdentity},
    maintenance::{self, MaintenancePhase},
    model::{CheckState, Observation, Severity},
    report,
    state::{self, PersistentState},
    systemd_notify,
};
use chrono::{Local, Utc};
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
    maintenance_error: Option<String>,
}

pub fn run(options: RuntimeOptions) -> Result<(), RuntimeError> {
    let LoadedConfig {
        mut config,
        source_sha256,
    } = options.loaded_config;
    let initial_identity = identity::load_runtime_identity(&config, source_sha256)?;
    let mut persistent = state::load(&config.runtime.state_dir)?;
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
        "alertd started"
    );
    notify_systemd("READY=1");
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
        let maintenance_active = update_maintenance(
            &config,
            report_context,
            &mut persistent,
            &queue,
            options.dry_run,
            &mut health,
        );
        let observations = run_checks(
            &config,
            report_context,
            &mut persistent,
            &mut context,
            &queue,
            options.dry_run,
            maintenance_active,
        );
        persistent
            .journal_cursors
            .clone_from(&context.journal_cursors);
        if !maintenance_active {
            maybe_daily(
                &config,
                report_context,
                &mut persistent,
                &queue,
                options.dry_run,
                &observations,
            );
        }
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

fn update_maintenance(
    config: &Config,
    context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    queue: &DeliveryQueue,
    dry_run: bool,
    health: &mut RuntimeHealth,
) -> bool {
    let window = match maintenance::load(&config.runtime.state_dir) {
        Ok(window) => {
            health.maintenance_error = None;
            window
        }
        Err(error_value) => {
            report_maintenance_error(context, queue, dry_run, health, &error_value.to_string());
            return false;
        }
    };
    let Some(window) = window else {
        return false;
    };
    match maintenance::phase(&window, Utc::now()) {
        MaintenancePhase::Active => {
            notify_maintenance_start(
                context,
                persistent,
                queue,
                dry_run,
                &config.runtime.state_dir,
                &window,
            );
            true
        }
        MaintenancePhase::Ending => {
            notify_maintenance_end(
                context,
                persistent,
                queue,
                dry_run,
                &config.runtime.state_dir,
                &window,
            );
            if persistent.maintenance_end_notice_id.as_deref() == Some(window.id.as_str()) {
                if let Err(error_value) =
                    maintenance::remove_if_id(&config.runtime.state_dir, &window.id)
                {
                    error!(
                        error = %error_value,
                        id = %window.id,
                        "cannot remove completed maintenance window"
                    );
                }
            }
            false
        }
    }
}

fn notify_maintenance_start(
    context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    queue: &DeliveryQueue,
    dry_run: bool,
    state_dir: &Path,
    window: &maintenance::MaintenanceWindow,
) {
    if persistent.maintenance_start_notice_id.as_deref() == Some(window.id.as_str()) {
        return;
    }
    let detail = format!(
        "原因：{}\n自动恢复：{}",
        window.reason,
        maintenance::format_time(window.until)
    );
    if enqueue_internal(
        queue,
        Severity::Warn,
        report::format_internal(context, Severity::Warn, "维护窗口已开始", &detail),
        dry_run,
    ) {
        persistent.maintenance_start_notice_id = Some(window.id.clone());
        persist_maintenance_notice_state(state_dir, persistent);
        info!(id = %window.id, until = %window.until, reason = %window.reason, "maintenance window active");
    }
}

fn notify_maintenance_end(
    context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    queue: &DeliveryQueue,
    dry_run: bool,
    state_dir: &Path,
    window: &maintenance::MaintenanceWindow,
) {
    if persistent.maintenance_end_notice_id.as_deref() == Some(window.id.as_str()) {
        return;
    }
    let (title, end_kind) = if window.cancelled_at.is_some() {
        ("维护窗口已人工取消", "人工取消")
    } else {
        ("维护窗口已自动结束", "到达预定时间")
    };
    let detail = format!("原因：{}\n结束方式：{end_kind}", window.reason);
    if enqueue_internal(
        queue,
        Severity::Ok,
        report::format_internal(context, Severity::Ok, title, &detail),
        dry_run,
    ) {
        persistent.maintenance_end_notice_id = Some(window.id.clone());
        persist_maintenance_notice_state(state_dir, persistent);
        info!(id = %window.id, reason = %window.reason, "maintenance window ended");
    }
}

fn persist_maintenance_notice_state(state_dir: &Path, persistent: &PersistentState) {
    if let Err(error_value) = state::save(state_dir, persistent) {
        error!(
            error = %error_value,
            "cannot immediately persist maintenance notification identity"
        );
    }
}

fn report_maintenance_error(
    context: report::ReportContext<'_>,
    queue: &DeliveryQueue,
    dry_run: bool,
    health: &mut RuntimeHealth,
    detail: &str,
) {
    if health.maintenance_error.as_deref() == Some(detail) {
        debug!(
            error = detail,
            "maintenance state remains invalid; monitoring is active"
        );
        return;
    }
    error!(
        error = detail,
        "maintenance state invalid; monitoring remains active"
    );
    if enqueue_internal(
        queue,
        Severity::Warn,
        report::format_internal(
            context,
            Severity::Warn,
            "维护窗口状态无效，监控已 fail-open",
            detail,
        ),
        dry_run,
    ) {
        health.maintenance_error = Some(detail.to_owned());
    }
}

fn run_checks(
    config: &Config,
    report_context: report::ReportContext<'_>,
    persistent: &mut PersistentState,
    context: &mut CollectContext,
    queue: &DeliveryQueue,
    dry_run: bool,
    maintenance_active: bool,
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
                if maintenance_active {
                    record_suppressed_observation(check, context, &observation);
                    observations.push(observation);
                    continue;
                }
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
                if maintenance_active {
                    warn!(
                        check = %check.name,
                        error = %error_value,
                        "collector failed during maintenance; alarm state unchanged"
                    );
                    continue;
                }
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

fn record_suppressed_observation(
    check: &CheckConfig,
    context: &mut CollectContext,
    observation: &Observation,
) {
    debug!(
        check = %observation.check_name,
        summary = %observation.summary,
        "observation suppressed by maintenance window"
    );
    if let Some(cursor) = context.pending_journal_cursors.remove(&check.name) {
        context.journal_cursors.insert(check.name.clone(), cursor);
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
    fn maintenance_notices_are_deduplicated_and_window_is_removed() {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = config();
        config.runtime.state_dir = temporary.path().into();
        let queue = DeliveryQueue::open(temporary.path(), 16).unwrap();
        let mut persistent = PersistentState::default();
        let mut health = RuntimeHealth::default();
        let until = (Utc::now() + chrono::Duration::hours(1)).fixed_offset();
        let window =
            maintenance::start(temporary.path(), until, "deploy".into(), Utc::now()).unwrap();

        assert!(update_maintenance(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
            &mut health,
        ));
        assert_eq!(
            persistent.maintenance_start_notice_id,
            Some(window.id.clone())
        );
        assert_eq!(queue.pending_count().unwrap(), 1);
        assert!(update_maintenance(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
            &mut health,
        ));
        assert_eq!(queue.pending_count().unwrap(), 1);

        maintenance::cancel(temporary.path()).unwrap();
        assert!(!update_maintenance(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
            &mut health,
        ));
        assert_eq!(persistent.maintenance_end_notice_id, Some(window.id));
        assert_eq!(queue.pending_count().unwrap(), 2);
        assert!(maintenance::load(temporary.path()).unwrap().is_none());
    }

    #[test]
    fn invalid_maintenance_state_fails_open_and_warns_once() {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = config();
        config.runtime.state_dir = temporary.path().into();
        std::fs::write(temporary.path().join("maintenance.json"), "invalid").unwrap();
        let queue = DeliveryQueue::open(temporary.path(), 16).unwrap();
        let mut persistent = PersistentState::default();
        let mut health = RuntimeHealth::default();

        for _ in 0..2 {
            assert!(!update_maintenance(
                &config,
                test_report_context(),
                &mut persistent,
                &queue,
                false,
                &mut health,
            ));
        }
        assert_eq!(queue.pending_count().unwrap(), 1);
    }

    #[test]
    fn maintenance_start_notice_retries_after_queue_rejection() {
        let temporary = tempfile::tempdir().unwrap();
        let mut config = config();
        config.runtime.state_dir = temporary.path().into();
        let queue = DeliveryQueue::open(temporary.path(), 1).unwrap();
        queue
            .enqueue_internal(Severity::Warn, "occupied".into())
            .unwrap();
        let mut persistent = PersistentState::default();
        let mut health = RuntimeHealth::default();
        let until = (Utc::now() + chrono::Duration::hours(1)).fixed_offset();
        let window =
            maintenance::start(temporary.path(), until, "deploy".into(), Utc::now()).unwrap();

        assert!(update_maintenance(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
            &mut health,
        ));
        assert_eq!(persistent.maintenance_start_notice_id, None);
        let (path, _) = queue.oldest().unwrap().unwrap();
        queue.acknowledge(&path).unwrap();

        assert!(update_maintenance(
            &config,
            test_report_context(),
            &mut persistent,
            &queue,
            false,
            &mut health,
        ));
        assert_eq!(persistent.maintenance_start_notice_id, Some(window.id));
    }

    #[test]
    fn maintenance_collection_preserves_alarm_state_and_commits_cursor() {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::write(
            temporary.path().join("meminfo"),
            "MemTotal: 1000 kB\nMemAvailable: 50 kB\n",
        )
        .unwrap();
        let config = config();
        let queue_root = tempfile::tempdir().unwrap();
        let queue = DeliveryQueue::open(queue_root.path(), 16).unwrap();
        let mut state = CheckState {
            pending_since: Some(Utc::now()),
            collection_failures: 2,
            ..Default::default()
        };
        state.daily_critical_count = 9;
        let mut persistent = PersistentState::default();
        persistent.checks.insert("memory".into(), state);
        let before = serde_json::to_value(&persistent.checks).unwrap();
        let mut context = CollectContext {
            proc_root: Some(temporary.path().into()),
            ..Default::default()
        };
        let observations = run_checks(
            &config,
            test_report_context(),
            &mut persistent,
            &mut context,
            &queue,
            false,
            true,
        );
        assert_eq!(serde_json::to_value(&persistent.checks).unwrap(), before);
        assert_eq!(queue.pending_count().unwrap(), 0);
        assert_eq!(observations.len(), 1);

        context
            .pending_journal_cursors
            .insert("memory".into(), "cursor-after-maintenance".into());
        record_suppressed_observation(
            &config.checks[0],
            &mut context,
            &Observation::healthy("memory", "suppressed"),
        );
        assert_eq!(
            context.journal_cursors.get("memory").map(String::as_str),
            Some("cursor-after-maintenance")
        );

        std::fs::remove_file(temporary.path().join("meminfo")).unwrap();
        let observations = run_checks(
            &config,
            test_report_context(),
            &mut persistent,
            &mut context,
            &queue,
            false,
            true,
        );
        assert_eq!(serde_json::to_value(&persistent.checks).unwrap(), before);
        assert!(observations.is_empty());
    }
}
