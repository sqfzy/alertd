use crate::{
    alarm::{self, AlarmPolicy},
    collectors::{self, CollectContext},
    config::{self, CheckConfig, Config},
    delivery::{dingtalk::DingTalkClient, queue::DeliveryQueue},
    model::{CheckState, Observation, Severity},
    report,
    state::{self, PersistentState},
};
use chrono::Local;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
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

pub fn run(options: RuntimeOptions) -> Result<(), RuntimeError> {
    let mut config = config::load_config(&options.config_path)?;
    let host = resolve_host(&config);
    let mut persistent = state::load(&config.runtime.state_dir)?;
    let queue = DeliveryQueue::open(&config.runtime.state_dir, config.delivery.queue_capacity)?;
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
    let delivery_worker =
        dingtalk.map(|client| start_delivery_worker(queue.clone(), client, &config, stop.clone()));
    let mut context = CollectContext {
        journal_cursors: persistent.journal_cursors.clone(),
        ..Default::default()
    };
    let mut observations = Vec::new();
    info!(host, "alertd started");
    while !stop.load(Ordering::Relaxed) {
        if reload.swap(false, Ordering::Relaxed) {
            reload_config(&options.config_path, &mut config, &mut persistent);
        }
        observations.clear();
        run_checks(
            &config,
            &host,
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
            &host,
            &mut persistent,
            &queue,
            options.dry_run,
            &observations,
        );
        if let Err(error) = state::save(&config.runtime.state_dir, &persistent) {
            error!(%error, "cannot persist runtime state");
        }
        sleep_interruptibly(
            config::parse_duration(&config.runtime.interval)?,
            &stop,
            &reload,
        );
    }
    info!("alertd stopping");
    if let Some(worker) = delivery_worker {
        let _ = worker.join();
    }
    Ok(())
}

pub fn send_test(config: &Config, dry_run: bool) -> Result<(), RuntimeError> {
    let text = format!(
        "🟢 OK · alertd 测试\n\n主机：{}\n状态：配置与钉钉投递正常",
        resolve_host(config)
    );
    if dry_run {
        println!("{text}");
    } else {
        build_client(config)?.send(&text, false)?;
    }
    Ok(())
}

fn run_checks(
    config: &Config,
    host: &str,
    persistent: &mut PersistentState,
    context: &mut CollectContext,
    queue: &DeliveryQueue,
    dry_run: bool,
    observations: &mut Vec<Observation>,
) {
    let global_policy = AlarmPolicy::from_strings(
        &config.alarm.pending_for,
        &config.alarm.warn_repeat,
        &config.alarm.critical_repeat,
    )
    .expect("validated config");
    for check in config.checks.iter().filter(|check| check.enabled) {
        match collectors::collect(check, context) {
            Ok(observation) => {
                resolve_collector_alarm(check, persistent, &global_policy, host, queue, dry_run);
                let state = persistent.checks.entry(check.name.clone()).or_default();
                state.collection_failures = 0;
                let accepted = process_observation(
                    check,
                    observation.clone(),
                    state,
                    &global_policy,
                    host,
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
                    host,
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
    host: &str,
    queue: &DeliveryQueue,
    dry_run: bool,
) {
    let name = format!("{}/collector", check.name);
    let Some(mut state) = persistent.checks.remove(&name) else {
        return;
    };
    let observation = Observation::healthy(&name, "监控采集恢复");
    process_observation(check, observation, &mut state, policy, host, queue, dry_run);
    persistent.checks.insert(name, state);
}

fn process_observation(
    check: &CheckConfig,
    observation: Observation,
    state: &mut CheckState,
    global: &AlarmPolicy,
    host: &str,
    queue: &DeliveryQueue,
    dry_run: bool,
) -> bool {
    debug!(check = %observation.check_name, summary = %observation.summary, "observation");
    let mut policy = *global;
    if let Some(pending) = &check.pending_for {
        policy.pending_for = config::parse_duration(pending).expect("validated config");
    }
    if matches!(check.kind, config::CheckKind::Journal { .. }) {
        policy.pending_for = Duration::ZERO;
    }
    let previous = state.clone();
    let Some(event) = alarm::evaluate(state, &observation, &policy, check.runbook.clone()) else {
        return true;
    };
    let text = report::format_alert(host, &event);
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

fn drain_once(queue: &DeliveryQueue, client: &DingTalkClient) -> bool {
    match queue.oldest() {
        Ok(Some((path, message))) => {
            match client.send(&message.text, message.severity == Severity::Critical) {
                Ok(()) => {
                    if let Err(error) = queue.acknowledge(&path) {
                        error!(%error, "delivered alert could not be acknowledged; duplicate is possible");
                        return false;
                    }
                    true
                }
                Err(error) => {
                    warn!(%error, id = %message.id, "delivery failed; message remains queued");
                    false
                }
            }
        }
        Ok(None) => true,
        Err(error) => {
            error!(%error, "cannot read delivery queue");
            false
        }
    }
}

fn start_delivery_worker(
    queue: DeliveryQueue,
    client: DingTalkClient,
    config: &Config,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    let initial = config::parse_duration(&config.delivery.retry_initial).expect("validated config");
    let maximum = config::parse_duration(&config.delivery.retry_max).expect("validated config");
    thread::spawn(move || {
        let mut retry = initial;
        while !stop.load(Ordering::Relaxed) {
            if drain_once(&queue, &client) {
                retry = initial;
                sleep_worker(Duration::from_millis(500), &stop);
            } else {
                sleep_worker(retry, &stop);
                retry = retry.saturating_mul(2).min(maximum);
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
    host: &str,
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
        report::format_daily(host, observations),
        dry_run,
    ) {
        persistent.last_daily_date = Some(date);
    }
}

fn reload_config(path: &Path, config: &mut Config, persistent: &mut PersistentState) {
    match config::load_config(path) {
        Ok(next) => {
            persistent.checks.retain(|name, _| {
                next.checks.iter().any(|check| {
                    &check.name == name || format!("{}/collector", check.name) == *name
                })
            });
            *config = next;
            info!("configuration reloaded");
        }
        Err(error) => {
            error!(%error, "configuration reload rejected; old configuration remains active")
        }
    }
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

fn sleep_interruptibly(duration: Duration, stop: &AtomicBool, reload: &AtomicBool) {
    let deadline = std::time::Instant::now() + duration;
    while !stop.load(Ordering::Relaxed)
        && !reload.load(Ordering::Relaxed)
        && std::time::Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(200));
    }
}
