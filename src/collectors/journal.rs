//! Journald event collector backed by one long-lived `journalctl` reader per check.
//!
//! The worker owns process I/O and applies backpressure; runtime owns matching,
//! alert state, and the durable cursor commit.

use super::{CollectContext, CollectError};
use crate::{
    config::{CheckConfig, JournalRule},
    model::{Observation, Severity},
};
use chrono::{DateTime, SecondsFormat, Utc};
#[cfg(any(target_os = "linux", test))]
use serde_json::Value;
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::{
    io::{self, BufRead, BufReader, Read},
    process::{Command, Stdio},
    sync::mpsc::sync_channel,
    thread,
    time::{Duration, Instant},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError},
    },
    thread::JoinHandle,
};
use tracing::{info, warn};

#[cfg(target_os = "linux")]
const BATCH_SIZE: usize = 128;
#[cfg(target_os = "linux")]
const CHANNEL_CAPACITY: usize = 4;
#[cfg(target_os = "linux")]
const RETRY_INITIAL: Duration = Duration::from_secs(1);
#[cfg(target_os = "linux")]
const RETRY_MAX: Duration = Duration::from_secs(60);
#[cfg(target_os = "linux")]
const BATCH_FLUSH_WINDOW: Duration = Duration::from_millis(100);
#[cfg(target_os = "linux")]
const STDERR_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
struct JournalBatch {
    cursor: String,
    entries: Vec<JournalEntry>,
}

#[derive(Debug, PartialEq, Eq)]
struct JournalEntry {
    cursor: Option<String>,
    message: String,
    unit: Option<String>,
    occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq)]
struct JournalSample {
    severity: Severity,
    unit: Option<String>,
    rule: String,
    occurred_at: Option<DateTime<Utc>>,
    message: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct JournalMatches {
    severity: Severity,
    hits: u64,
    ignored: u64,
    warn_hits: u64,
    critical_hits: u64,
    sample: Option<JournalSample>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum WorkerMessage {
    Batch {
        batch: JournalBatch,
        acknowledge: SyncSender<()>,
    },
    Failure(String),
}

struct PendingBatch {
    batch: JournalBatch,
    acknowledge: SyncSender<()>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct ReaderFailure {
    message: String,
    committed_cursor: Option<String>,
}

pub(crate) struct JournalWorker {
    units: Vec<String>,
    receiver: Receiver<WorkerMessage>,
    stopping: Arc<AtomicBool>,
    child_pid: Arc<AtomicI32>,
    join: Option<JoinHandle<()>>,
    pending: Option<PendingBatch>,
}

fn match_messages(
    entries: &[JournalEntry],
    ignore_contains: &[String],
    rules: &[JournalRule],
) -> JournalMatches {
    let mut matches = JournalMatches::default();
    for entry in entries {
        if ignore_contains
            .iter()
            .any(|ignored| entry.message.contains(ignored))
        {
            matches.ignored += 1;
            continue;
        }
        for rule in rules {
            if entry.message.contains(&rule.contains) {
                matches.hits += 1;
                matches.severity = matches.severity.max(rule.severity);
                match rule.severity {
                    Severity::Warn => matches.warn_hits += 1,
                    Severity::Critical => matches.critical_hits += 1,
                    Severity::Ok => {}
                }
                if matches
                    .sample
                    .as_ref()
                    .is_none_or(|sample| rule.severity > sample.severity)
                {
                    matches.sample = Some(JournalSample {
                        severity: rule.severity,
                        unit: entry.unit.clone(),
                        rule: rule.contains.clone(),
                        occurred_at: entry.occurred_at,
                        message: truncate_sample(&entry.message),
                    });
                }
            }
        }
    }
    matches
}

#[cfg(any(target_os = "linux", test))]
fn parse_entry(line: &str) -> Option<JournalEntry> {
    let value: Value = serde_json::from_str(line).ok()?;
    Some(JournalEntry {
        cursor: value
            .get("__CURSOR")
            .or_else(|| value.get("_CURSOR"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        message: value.get("MESSAGE")?.as_str()?.to_owned(),
        unit: value
            .get("_SYSTEMD_UNIT")
            .and_then(Value::as_str)
            .map(str::to_owned),
        occurred_at: value
            .get("__REALTIME_TIMESTAMP")
            .and_then(parse_realtime_timestamp),
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_realtime_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    let micros = value
        .as_str()
        .and_then(|raw| raw.parse::<i64>().ok())
        .or_else(|| value.as_i64())?;
    DateTime::from_timestamp_micros(micros)
}

fn truncate_sample(message: &str) -> String {
    let mut characters = message.chars();
    let prefix: String = characters.by_ref().take(240).collect();
    if characters.next().is_none() {
        return prefix;
    }
    let mut truncated: String = prefix.chars().take(239).collect();
    truncated.push('…');
    truncated
}

pub fn collect(
    check: &CheckConfig,
    units: &[String],
    ignore_contains: &[String],
    rules: &[JournalRule],
    context: &mut CollectContext,
) -> Result<Observation, CollectError> {
    ensure_worker(&check.name, units, context)?;
    let worker = context
        .journal_workers
        .get_mut(&check.name)
        .expect("journal worker exists after ensure_worker");
    worker.poll()?;
    let Some(batch) = worker.pending.as_ref() else {
        return Ok(Observation::healthy(&check.name, "journal 无新行"));
    };
    let matches = match_messages(&batch.batch.entries, ignore_contains, rules);
    if matches.ignored > 0 {
        info!(
            check = %check.name,
            read = batch.batch.entries.len(),
            ignored = matches.ignored,
            "journal messages filtered"
        );
    }
    context
        .pending_journal_cursors
        .insert(check.name.clone(), batch.batch.cursor.clone());
    let summary = format!(
        "journal 新增 {} 行，规则命中 {} 次",
        batch.batch.entries.len(),
        matches.hits
    );
    let observation = if matches.hits == 0 {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, matches.severity, summary)
    };
    let mut observation = observation
        .event_counts(matches.warn_hits, matches.critical_hits)
        .detail("本批读取", batch.batch.entries.len().to_string())
        .detail("本次命中", matches.hits.to_string());
    if let Some(sample) = matches.sample {
        observation = observation
            .detail("服务", sample.unit.unwrap_or_else(|| "未知".into()))
            .detail("命中规则", sample.rule)
            .detail("日志", sample.message);
        if let Some(occurred_at) = sample.occurred_at {
            observation = observation.detail(
                "日志时间",
                occurred_at.to_rfc3339_opts(SecondsFormat::Micros, true),
            );
        }
    }
    Ok(observation)
}

pub(crate) fn acknowledge(check_name: &str, context: &mut CollectContext) -> bool {
    let Some(mut worker) = context.journal_workers.remove(check_name) else {
        return true;
    };
    let Some(pending) = worker.pending.take() else {
        context.journal_workers.insert(check_name.into(), worker);
        return true;
    };
    // The worker cannot read the next batch until this acknowledgement arrives;
    // this keeps queue acceptance ahead of the durable cursor commit.
    if pending.acknowledge.send(()).is_ok() {
        context.journal_workers.insert(check_name.into(), worker);
        true
    } else {
        worker.stop();
        warn!(
            check = check_name,
            "journal worker acknowledgement failed; restarting"
        );
        false
    }
}

pub(crate) fn reconcile_workers(checks: &[CheckConfig], context: &mut CollectContext) {
    let active: std::collections::HashMap<&str, &[String]> = checks
        .iter()
        .filter(|check| check.enabled)
        .filter_map(|check| match &check.kind {
            crate::config::CheckKind::Journal { units, .. } => {
                Some((check.name.as_str(), units.as_slice()))
            }
            _ => None,
        })
        .collect();
    let stale: Vec<String> = context
        .journal_workers
        .iter()
        .filter(|(name, worker)| {
            active
                .get(name.as_str())
                .is_none_or(|units| *units != worker.units)
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in stale {
        if let Some(mut worker) = context.journal_workers.remove(&name) {
            worker.stop();
            info!(check = %name, "journal worker stopped after configuration change");
        }
    }
}

pub(crate) fn stop_workers(context: &mut CollectContext) {
    for (name, mut worker) in context.journal_workers.drain() {
        worker.stop();
        info!(check = %name, "journal worker stopped");
    }
}

fn ensure_worker(
    check_name: &str,
    units: &[String],
    context: &mut CollectContext,
) -> Result<(), CollectError> {
    let needs_restart = context
        .journal_workers
        .get(check_name)
        .is_some_and(|worker| worker.units != units || worker.is_finished());
    if needs_restart {
        if let Some(mut worker) = context.journal_workers.remove(check_name) {
            worker.stop();
            info!(check = check_name, "journal worker restarted");
        }
    }
    if context.journal_workers.contains_key(check_name) {
        return Ok(());
    }
    let cursor = context.journal_cursors.get(check_name).cloned();
    let worker = JournalWorker::start(units.to_vec(), cursor)?;
    context
        .journal_workers
        .insert(check_name.to_owned(), worker);
    Ok(())
}

impl JournalWorker {
    fn start(units: Vec<String>, cursor: Option<String>) -> Result<Self, CollectError> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (units, cursor);
            Err(CollectError::Unsupported("journald requires Linux".into()))
        }
        #[cfg(target_os = "linux")]
        {
            let (sender, receiver) = sync_channel(CHANNEL_CAPACITY);
            let stopping = Arc::new(AtomicBool::new(false));
            let child_pid = Arc::new(AtomicI32::new(-1));
            let thread_stopping = stopping.clone();
            let thread_pid = child_pid.clone();
            let thread_units = units.clone();
            let cursor_mode = if cursor.is_some() {
                "after-cursor"
            } else {
                "since-now"
            };
            let join = thread::Builder::new()
                .name(format!("alertd-journal-{units:?}"))
                .spawn(move || {
                    worker_loop(thread_units, cursor, sender, thread_stopping, thread_pid)
                })
                .map_err(CollectError::Io)?;
            info!(units = ?units, cursor_mode, "journal worker started");
            Ok(Self {
                units,
                receiver,
                stopping,
                child_pid,
                join: Some(join),
                pending: None,
            })
        }
    }

    fn poll(&mut self) -> Result<(), CollectError> {
        if self.pending.is_some() {
            return Ok(());
        }
        match self.receiver.try_recv() {
            Ok(WorkerMessage::Batch { batch, acknowledge }) => {
                info!(entries = batch.entries.len(), last_cursor = %batch.cursor, "journal batch received");
                self.pending = Some(PendingBatch { batch, acknowledge });
                Ok(())
            }
            Ok(WorkerMessage::Failure(error)) => Err(CollectError::Invalid(error)),
            Err(TryRecvError::Empty) => Ok(()),
            Err(TryRecvError::Disconnected) => Err(CollectError::Invalid(
                "journal worker channel disconnected".into(),
            )),
        }
    }

    fn is_finished(&self) -> bool {
        self.join.as_ref().is_some_and(JoinHandle::is_finished)
    }

    fn stop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        kill_child_group(self.child_pid.load(Ordering::Relaxed));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(target_os = "linux")]
fn worker_loop(
    units: Vec<String>,
    mut cursor: Option<String>,
    sender: SyncSender<WorkerMessage>,
    stopping: Arc<AtomicBool>,
    child_pid: Arc<AtomicI32>,
) {
    let mut retry = RETRY_INITIAL;
    while !stopping.load(Ordering::Relaxed) {
        match run_reader(&units, cursor.as_deref(), &sender, &stopping, &child_pid) {
            Ok(next_cursor) => {
                cursor = next_cursor.or(cursor);
                retry = RETRY_INITIAL;
            }
            Err(failure) => {
                cursor = failure.committed_cursor.or(cursor);
                warn!(units = ?units, error = %failure.message, retry = ?retry, "journal worker reader failed");
                if !send_failure(&sender, failure.message, &stopping) {
                    break;
                }
                sleep_interruptibly(retry, &stopping);
                retry = retry.saturating_mul(2).min(RETRY_MAX);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn run_reader(
    units: &[String],
    cursor: Option<&str>,
    sender: &SyncSender<WorkerMessage>,
    stopping: &AtomicBool,
    child_pid: &AtomicI32,
) -> Result<Option<String>, ReaderFailure> {
    let mut arguments = vec![
        "--follow",
        "--lines=all",
        "--no-pager",
        "--quiet",
        "--output=json",
    ];
    for unit in units {
        arguments.push("--unit");
        arguments.push(unit);
    }
    if let Some(cursor) = cursor {
        arguments.push("--after-cursor");
        arguments.push(cursor);
    } else {
        arguments.push("--since");
        arguments.push("now");
    }
    let mut command = Command::new("journalctl");
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| ReaderFailure {
        message: error.to_string(),
        committed_cursor: None,
    })?;
    child_pid.store(child.id() as i32, Ordering::Relaxed);
    info!(units = ?units, cursor_mode = ?if cursor.is_some() { "after-cursor" } else { "since-now" }, "journalctl reader process started");
    let stdout = child.stdout.take().ok_or_else(|| ReaderFailure {
        message: "journalctl stdout was not piped".to_owned(),
        committed_cursor: None,
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| ReaderFailure {
        message: "journalctl stderr was not piped".to_owned(),
        committed_cursor: None,
    })?;
    let stderr_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        while bytes.len() < STDERR_LIMIT {
            let size = match stderr.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(size) => size,
            };
            bytes.extend_from_slice(&buffer[..size.min(STDERR_LIMIT - bytes.len())]);
        }
        String::from_utf8_lossy(&bytes).trim().to_owned()
    });
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut entries = Vec::with_capacity(BATCH_SIZE);
    let mut latest_cursor = None;
    let mut committed_cursor = None;
    let mut flush_deadline: Option<Instant> = None;
    let read_error = loop {
        line.clear();
        let timeout = flush_deadline.map_or(-1, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32
        });
        match read_line_ready(&mut reader, &mut line, timeout) {
            Ok(None) => {
                let batch_cursor = latest_cursor.clone();
                let acknowledged = send_batch(sender, &mut entries, batch_cursor.clone(), stopping)
                    .map_err(|error| ReaderFailure {
                        message: error,
                        committed_cursor: committed_cursor.clone(),
                    })?;
                if !acknowledged {
                    kill_child_group(child_pid.load(Ordering::Relaxed));
                    break None;
                }
                committed_cursor = batch_cursor;
                latest_cursor = None;
                flush_deadline = None;
            }
            Ok(Some(0)) => break None,
            Ok(Some(_)) => {
                let Some(entry) = parse_entry(&line) else {
                    warn!("journal worker skipped malformed JSON entry");
                    continue;
                };
                let Some(entry_cursor) = entry.cursor.clone() else {
                    warn!("journal worker skipped entry without cursor");
                    continue;
                };
                latest_cursor = Some(entry_cursor);
                entries.push(entry);
                if flush_deadline.is_none() {
                    flush_deadline = Some(Instant::now() + BATCH_FLUSH_WINDOW);
                }
                if entries.len() >= BATCH_SIZE {
                    let batch_cursor = latest_cursor.clone();
                    let acknowledged =
                        send_batch(sender, &mut entries, batch_cursor.clone(), stopping).map_err(
                            |error| ReaderFailure {
                                message: error,
                                committed_cursor: committed_cursor.clone(),
                            },
                        )?;
                    if !acknowledged {
                        kill_child_group(child_pid.load(Ordering::Relaxed));
                        break None;
                    }
                    committed_cursor = batch_cursor;
                    latest_cursor = None;
                    flush_deadline = None;
                }
            }
            Err(error) => break Some(error.to_string()),
        }
    };
    if read_error.is_some() {
        kill_child_group(child_pid.load(Ordering::Relaxed));
    }
    if !entries.is_empty() && !stopping.load(Ordering::Relaxed) {
        let batch_cursor = latest_cursor.clone();
        let acknowledged = send_batch(sender, &mut entries, batch_cursor.clone(), stopping)
            .map_err(|error| ReaderFailure {
                message: error,
                committed_cursor: committed_cursor.clone(),
            })?;
        if !acknowledged {
            kill_child_group(child_pid.load(Ordering::Relaxed));
            return Ok(None);
        }
        committed_cursor = batch_cursor;
    }
    let status = child.wait().map_err(|error| ReaderFailure {
        message: error.to_string(),
        committed_cursor: committed_cursor.clone(),
    })?;
    child_pid.store(-1, Ordering::Relaxed);
    let stderr = stderr_thread.join().unwrap_or_default();
    if stopping.load(Ordering::Relaxed) {
        return Ok(None);
    }
    if let Some(error) = read_error {
        return Err(ReaderFailure {
            message: format!("journalctl output read failed: {error}"),
            committed_cursor,
        });
    }
    let detail = if stderr.is_empty() {
        format!("journalctl exited {status}")
    } else {
        format!("journalctl exited {status}: {stderr}")
    };
    if status.success() {
        Err(ReaderFailure {
            message: format!("journalctl follow ended unexpectedly: {detail}"),
            committed_cursor,
        })
    } else {
        Err(ReaderFailure {
            message: detail,
            committed_cursor,
        })
    }
}

#[cfg(target_os = "linux")]
fn read_line_ready(
    reader: &mut BufReader<std::process::ChildStdout>,
    line: &mut String,
    timeout_ms: i32,
) -> io::Result<Option<usize>> {
    if !reader.buffer().is_empty() {
        return reader.read_line(line).map(Some);
    }
    let mut descriptor = libc::pollfd {
        fd: reader.get_ref().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result >= 0 {
            return if result == 0 {
                Ok(None)
            } else {
                reader.read_line(line).map(Some)
            };
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "linux")]
fn send_failure(sender: &SyncSender<WorkerMessage>, error: String, stopping: &AtomicBool) -> bool {
    let mut message = WorkerMessage::Failure(error);
    loop {
        if stopping.load(Ordering::Relaxed) {
            return false;
        }
        match sender.try_send(message) {
            Ok(()) => return true,
            Err(std::sync::mpsc::TrySendError::Full(next)) => {
                message = next;
                thread::sleep(Duration::from_millis(100));
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

#[cfg(target_os = "linux")]
fn send_batch(
    sender: &SyncSender<WorkerMessage>,
    entries: &mut Vec<JournalEntry>,
    cursor: Option<String>,
    stopping: &AtomicBool,
) -> Result<bool, String> {
    let Some(cursor) = cursor else {
        entries.clear();
        return Ok(true);
    };
    let batch = JournalBatch {
        cursor,
        entries: std::mem::take(entries),
    };
    let (acknowledge, confirmation) = sync_channel(0);
    let mut message = WorkerMessage::Batch { batch, acknowledge };
    let blocked_since = Instant::now();
    loop {
        if stopping.load(Ordering::Relaxed) {
            return Ok(false);
        }
        match sender.try_send(message) {
            Ok(()) => {
                let blocked_ms = blocked_since.elapsed().as_millis();
                if blocked_ms >= 100 {
                    info!(blocked_ms, "journal worker channel backpressure");
                }
                break;
            }
            Err(std::sync::mpsc::TrySendError::Full(next)) => {
                message = next;
                thread::sleep(Duration::from_millis(100));
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                return Err("journal runtime channel disconnected".to_owned());
            }
        }
    }
    loop {
        if stopping.load(Ordering::Relaxed) {
            return Ok(false);
        }
        match confirmation.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => return Ok(true),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(false),
        }
    }
}

#[cfg(target_os = "linux")]
fn sleep_interruptibly(duration: Duration, stopping: &AtomicBool) {
    let deadline = std::time::Instant::now() + duration;
    while !stopping.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
}

fn kill_child_group(pid: i32) {
    #[cfg(unix)]
    if pid > 0 {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(message: &str) -> JournalEntry {
        JournalEntry {
            cursor: None,
            message: message.into(),
            unit: None,
            occurred_at: None,
        }
    }

    #[test]
    fn counts_warn_and_critical_substrings() {
        let rules = vec![
            JournalRule {
                contains: "WARN".into(),
                severity: Severity::Warn,
            },
            JournalRule {
                contains: "ERROR".into(),
                severity: Severity::Critical,
            },
        ];
        let matches = match_messages(
            &[entry("WARN first"), entry("normal"), entry("ERROR failed")],
            &[],
            &rules,
        );
        assert_eq!(matches.hits, 2);
        assert_eq!(matches.warn_hits, 1);
        assert_eq!(matches.critical_hits, 1);
        assert_eq!(matches.severity, Severity::Critical);
        assert_eq!(matches.sample.unwrap().message, "ERROR failed");
    }

    #[test]
    fn ignores_messages_before_matching_alert_rules() {
        let rules = vec![
            JournalRule {
                contains: "WARN".into(),
                severity: Severity::Warn,
            },
            JournalRule {
                contains: "ERROR".into(),
                severity: Severity::Critical,
            },
        ];
        let matches = match_messages(
            &[
                entry("ERROR expected during shutdown"),
                entry("error expected during shutdown"),
                entry("WARN retrying"),
                entry("ERROR failed"),
            ],
            &["expected during shutdown".into(), "not present".into()],
            &rules,
        );

        assert_eq!(matches.ignored, 2);
        assert_eq!(matches.hits, 2);
        assert_eq!(matches.warn_hits, 1);
        assert_eq!(matches.critical_hits, 1);
        assert_eq!(matches.severity, Severity::Critical);
        assert_eq!(matches.sample.unwrap().message, "ERROR failed");
    }

    #[test]
    fn ignore_matching_is_case_sensitive() {
        let rules = vec![JournalRule {
            contains: "ERROR".into(),
            severity: Severity::Critical,
        }];
        let matches = match_messages(&[entry("ERROR failed")], &["error failed".into()], &rules);

        assert_eq!(matches.ignored, 0);
        assert_eq!(matches.hits, 1);
    }

    #[test]
    fn fully_filtered_batch_has_no_alert_occurrences() {
        let rules = vec![JournalRule {
            contains: "ERROR".into(),
            severity: Severity::Critical,
        }];
        let matches = match_messages(
            &[entry("ERROR expected during shutdown")],
            &["expected during shutdown".into()],
            &rules,
        );

        assert_eq!(matches.ignored, 1);
        assert_eq!(matches.hits, 0);
        assert_eq!(matches.warn_hits, 0);
        assert_eq!(matches.critical_hits, 0);
        assert_eq!(matches.severity, Severity::Ok);
        assert!(matches.sample.is_none());
    }

    #[test]
    fn parses_journal_metadata_and_tolerates_missing_optional_fields() {
        let parsed = parse_entry(
            r#"{"__CURSOR":"s=cursor","MESSAGE":"fatal\ncontinued","_SYSTEMD_UNIT":"market.service","__REALTIME_TIMESTAMP":"1787605818149168"}"#,
        )
        .unwrap();
        assert_eq!(parsed.cursor.as_deref(), Some("s=cursor"));
        assert_eq!(parsed.message, "fatal\ncontinued");
        assert_eq!(parsed.unit.as_deref(), Some("market.service"));
        assert_eq!(
            parsed.occurred_at.unwrap().timestamp_micros(),
            1_787_605_818_149_168
        );

        let missing = parse_entry(r#"{"MESSAGE":"WARN only"}"#).unwrap();
        assert_eq!(missing.cursor, None);
        assert_eq!(missing.unit, None);
        assert_eq!(missing.occurred_at, None);
        let invalid_time =
            parse_entry(r#"{"MESSAGE":"WARN only","__REALTIME_TIMESTAMP":"invalid"}"#).unwrap();
        assert_eq!(invalid_time.occurred_at, None);
        assert!(parse_entry("not json").is_none());
        assert!(parse_entry(r#"{"_SYSTEMD_UNIT":"market.service"}"#).is_none());
    }

    #[test]
    fn highest_severity_sample_keeps_its_unit_rule_and_time() {
        let occurred_at = DateTime::from_timestamp_micros(1_787_605_818_149_168).unwrap();
        let entries = [
            JournalEntry {
                cursor: None,
                message: "WARN retrying".into(),
                unit: Some("first.service".into()),
                occurred_at: None,
            },
            JournalEntry {
                cursor: None,
                message: "ERROR fatal".into(),
                unit: Some("critical.service".into()),
                occurred_at: Some(occurred_at),
            },
            JournalEntry {
                cursor: None,
                message: "ERROR later".into(),
                unit: Some("later.service".into()),
                occurred_at: None,
            },
        ];
        let rules = [
            JournalRule {
                contains: "WARN".into(),
                severity: Severity::Warn,
            },
            JournalRule {
                contains: "ERROR".into(),
                severity: Severity::Critical,
            },
        ];

        let sample = match_messages(&entries, &[], &rules).sample.unwrap();
        assert_eq!(sample.unit.as_deref(), Some("critical.service"));
        assert_eq!(sample.rule, "ERROR");
        assert_eq!(sample.occurred_at, Some(occurred_at));
        assert_eq!(sample.message, "ERROR fatal");
    }

    #[test]
    fn sample_truncation_is_unicode_safe_and_marks_truncation() {
        let exact = "延".repeat(240);
        assert_eq!(truncate_sample(&exact), exact);

        let truncated = truncate_sample(&"延".repeat(241));
        assert_eq!(truncated.chars().count(), 240);
        assert!(truncated.ends_with('…'));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn batch_is_not_released_until_runtime_acknowledges_it() {
        let (sender, receiver) = sync_channel(1);
        let stopping = AtomicBool::new(false);
        let mut entries = vec![entry("ERROR failed")];
        entries[0].cursor = Some("s=cursor".into());
        let join = thread::spawn(move || {
            send_batch(&sender, &mut entries, Some("s=cursor".into()), &stopping).unwrap()
        });
        let WorkerMessage::Batch { batch, acknowledge } = receiver.recv().unwrap() else {
            panic!("expected journal batch");
        };
        assert_eq!(batch.cursor, "s=cursor");
        assert_eq!(batch.entries.len(), 1);
        assert!(!join.is_finished());
        acknowledge.send(()).unwrap();
        assert!(join.join().unwrap());
    }
}
