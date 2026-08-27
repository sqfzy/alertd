//! 严格 TOML 配置 POD、范围校验和钉钉环境密钥解析。

use crate::model::Severity;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

fn default_interval() -> String {
    "30s".into()
}
fn default_state_dir() -> PathBuf {
    "/var/lib/alertd".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_command_timeout() -> String {
    "3s".into()
}
fn default_pending() -> String {
    "90s".into()
}
fn default_recover() -> String {
    "60s".into()
}
fn default_warn_repeat() -> String {
    "30m".into()
}
fn default_critical_repeat() -> String {
    "10m".into()
}
fn default_daily() -> Option<String> {
    Some("02:00".into())
}
fn default_token_env() -> String {
    "ALERTD_DINGTALK_TOKEN".into()
}
fn default_secret_env() -> String {
    "ALERTD_DINGTALK_SECRET".into()
}
fn default_timeout() -> String {
    "3s".into()
}
fn default_capacity() -> usize {
    1024
}
fn default_queue_warn_pct() -> u8 {
    80
}
fn default_failure_report_after() -> u32 {
    3
}
fn default_retry_initial() -> String {
    "5s".into()
}
fn default_retry_max() -> String {
    "5m".into()
}
fn default_true() -> bool {
    true
}
fn default_min_count() -> u32 {
    1
}
fn default_severity() -> Severity {
    Severity::Critical
}
fn default_probe() -> ShmProbe {
    ShmProbe::Exists
}
fn default_collect_failures() -> u32 {
    3
}
fn default_minimum_size_bytes() -> u64 {
    1
}
fn default_warn_inode_used_pct() -> f64 {
    80.0
}
fn default_critical_inode_used_pct() -> f64 {
    90.0
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
/// alertd 的完整运行配置；未知字段在反序列化阶段即被拒绝。
pub struct Config {
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub alarm: AlarmConfig,
    #[serde(default)]
    pub delivery: DeliveryConfig,
    #[serde(default)]
    pub checks: Vec<CheckConfig>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub host: Option<String>,
    pub ip: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_command_timeout")]
    pub command_timeout: String,
}
impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: None,
            ip: None,
            interval: default_interval(),
            state_dir: default_state_dir(),
            log_level: default_log_level(),
            command_timeout: default_command_timeout(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AlarmConfig {
    #[serde(default = "default_pending")]
    pub pending_for: String,
    #[serde(default = "default_recover")]
    pub recover_for: String,
    #[serde(default = "default_warn_repeat")]
    pub warn_repeat: String,
    #[serde(default = "default_critical_repeat")]
    pub critical_repeat: String,
    #[serde(default = "default_daily")]
    pub daily_report_at: Option<String>,
    #[serde(default = "default_collect_failures")]
    pub collect_fail_after_n: u32,
}
impl Default for AlarmConfig {
    fn default() -> Self {
        Self {
            pending_for: default_pending(),
            recover_for: default_recover(),
            warn_repeat: default_warn_repeat(),
            critical_repeat: default_critical_repeat(),
            daily_report_at: default_daily(),
            collect_fail_after_n: default_collect_failures(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DeliveryConfig {
    #[serde(default = "default_token_env")]
    pub token_env: String,
    #[serde(default = "default_secret_env")]
    pub secret_env: String,
    #[serde(default = "default_timeout")]
    pub timeout: String,
    #[serde(default = "default_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_queue_warn_pct")]
    pub queue_warn_pct: u8,
    #[serde(default = "default_failure_report_after")]
    pub failure_report_after: u32,
    #[serde(default = "default_retry_initial")]
    pub retry_initial: String,
    #[serde(default = "default_retry_max")]
    pub retry_max: String,
    pub at_all_on_critical: bool,
}
impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            token_env: default_token_env(),
            secret_env: default_secret_env(),
            timeout: default_timeout(),
            queue_capacity: default_capacity(),
            queue_warn_pct: default_queue_warn_pct(),
            failure_report_after: default_failure_report_after(),
            retry_initial: default_retry_initial(),
            retry_max: default_retry_max(),
            at_all_on_critical: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
/// 每类 check 的严格、互斥配置载荷。
pub enum CheckKind {
    Process {
        cmdline_contains: String,
        #[serde(default = "default_min_count")]
        min_count: u32,
    },
    Shm {
        path: String,
        #[serde(default = "default_probe")]
        probe: ShmProbe,
        #[serde(default)]
        require_progress: bool,
        stale_after: Option<String>,
        offset: Option<u64>,
        endian: Option<Endian>,
        magic: Option<u32>,
        layout_version: Option<u16>,
    },
    Journal {
        units: Vec<String>,
        #[serde(default)]
        ignore_contains: Vec<String>,
        rules: Vec<JournalRule>,
    },
    Systemd {
        units: Vec<String>,
    },
    LatestFile {
        directory: PathBuf,
        prefix: String,
        #[serde(default)]
        suffix: String,
        stale_after: String,
        #[serde(default = "default_minimum_size_bytes")]
        minimum_size_bytes: u64,
    },
    MetricsFile {
        path: PathBuf,
        stale_after: String,
        metrics: Vec<MetricRule>,
    },
    MetricsShm {
        path: String,
        abi_hash: Option<ShmAbiHash>,
        metrics: Vec<ShmMetricRule>,
    },
    Disk {
        mount: PathBuf,
        warn_used_pct: f64,
        critical_used_pct: f64,
        #[serde(default = "default_warn_inode_used_pct")]
        warn_inode_used_pct: f64,
        #[serde(default = "default_critical_inode_used_pct")]
        critical_inode_used_pct: f64,
    },
    Memory {
        warn_available_pct: f64,
        critical_available_pct: f64,
    },
    Cpu {
        warn_usage_pct: f64,
        critical_usage_pct: f64,
    },
    TimeSync {
        warn_offset: String,
        critical_offset: String,
    },
    Network {
        interfaces: Vec<String>,
        warn_errors_per_second: f64,
        critical_errors_per_second: f64,
        warn_drops_per_second: f64,
        critical_drops_per_second: f64,
    },
    SystemTuning,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CheckConfig {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_severity")]
    pub severity: Severity,
    pub pending_for: Option<String>,
    pub recover_for: Option<String>,
    pub runbook: Option<String>,
    #[serde(flatten)]
    pub kind: CheckKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ShmProbe {
    Exists,
    U64Counter,
    GconfV2,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Endian {
    #[default]
    Little,
    Big,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct JournalRule {
    pub contains: String,
    pub severity: Severity,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MetricRule {
    pub key: String,
    pub critical_below: Option<f64>,
    pub warn_below: Option<f64>,
    pub warn_above: Option<f64>,
    pub critical_above: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShmAbiHash {
    pub offset: u64,
    pub expected_hex: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShmMetricRule {
    pub key: String,
    pub offset: u64,
    pub value_type: ShmValueType,
    #[serde(default)]
    pub endian: Endian,
    pub critical_below: Option<f64>,
    pub warn_below: Option<f64>,
    pub warn_above: Option<f64>,
    pub critical_above: Option<f64>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShmValueType {
    U8,
    U16,
    U32,
    U64,
    I8,
    I16,
    I32,
    I64,
    F32,
    F64,
}

impl ShmValueType {
    pub const fn width(self) -> u64 {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug)]
pub struct LoadedConfig {
    pub config: Config,
    pub source_sha256: String,
}

pub fn load_config(path: &Path) -> Result<Config, ConfigError> {
    Ok(load_config_with_sha256(path)?.config)
}

pub fn load_config_with_sha256(path: &Path) -> Result<LoadedConfig, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.into(),
        source,
    })?;
    let config: Config = toml::from_str(&text)?;
    validate_config(&config)?;
    Ok(LoadedConfig {
        config,
        source_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
    })
}

pub fn parse_duration(value: &str) -> Result<Duration, ConfigError> {
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| ConfigError::Invalid(format!("duration {value:?} needs a unit")))?;
    let number: u64 = value[..split]
        .parse()
        .map_err(|_| ConfigError::Invalid(format!("invalid duration {value:?}")))?;
    let seconds = match &value[split..] {
        "ms" => return Ok(Duration::from_millis(number)),
        "s" => Some(number),
        "m" => number.checked_mul(60),
        "h" => number.checked_mul(3600),
        _ => None,
    }
    .ok_or_else(|| ConfigError::Invalid(format!("invalid duration {value:?}")))?;
    Ok(Duration::from_secs(seconds))
}

fn duration_range(
    name: &str,
    value: &str,
    min: Duration,
    max: Duration,
) -> Result<Duration, ConfigError> {
    let duration = parse_duration(value)?;
    if duration < min || duration > max {
        return Err(ConfigError::Invalid(format!(
            "{name}={value} outside {min:?}..={max:?}"
        )));
    }
    Ok(duration)
}

pub fn validate_config(config: &Config) -> Result<(), ConfigError> {
    let interval = duration_range(
        "runtime.interval",
        &config.runtime.interval,
        Duration::from_secs(5),
        Duration::from_secs(3600),
    )?;
    duration_range(
        "runtime.command_timeout",
        &config.runtime.command_timeout,
        Duration::from_millis(200),
        Duration::from_secs(30),
    )?;
    if let Some(host) = &config.runtime.host {
        if host.is_empty() || host.len() > 128 || host.chars().any(char::is_control) {
            return Err(ConfigError::Invalid(
                "runtime.host must contain 1..=128 printable bytes".into(),
            ));
        }
    }
    if let Some(ip) = &config.runtime.ip {
        ip.parse::<IpAddr>().map_err(|_| {
            ConfigError::Invalid("runtime.ip must be a valid IPv4 or IPv6 address".into())
        })?;
    }
    if !config.runtime.state_dir.is_absolute() {
        return Err(ConfigError::Invalid(
            "runtime.state_dir must be absolute".into(),
        ));
    }
    if !matches!(
        config.runtime.log_level.as_str(),
        "trace" | "debug" | "info" | "warn" | "error"
    ) {
        return Err(ConfigError::Invalid(
            "runtime.log_level must be trace/debug/info/warn/error".into(),
        ));
    }
    duration_range(
        "alarm.pending_for",
        &config.alarm.pending_for,
        Duration::ZERO,
        Duration::from_secs(86400),
    )?;
    duration_range(
        "alarm.recover_for",
        &config.alarm.recover_for,
        Duration::ZERO,
        Duration::from_secs(86400),
    )?;
    duration_range(
        "alarm.warn_repeat",
        &config.alarm.warn_repeat,
        Duration::from_secs(60),
        Duration::from_secs(86400),
    )?;
    duration_range(
        "alarm.critical_repeat",
        &config.alarm.critical_repeat,
        Duration::from_secs(60),
        Duration::from_secs(86400),
    )?;
    if !(1..=100).contains(&config.alarm.collect_fail_after_n) {
        return Err(ConfigError::Invalid(
            "alarm.collect_fail_after_n outside 1..=100".into(),
        ));
    }
    if let Some(value) = &config.alarm.daily_report_at {
        validate_clock(value)?;
    }
    duration_range(
        "delivery.timeout",
        &config.delivery.timeout,
        Duration::from_millis(200),
        Duration::from_secs(30),
    )?;
    let initial = duration_range(
        "delivery.retry_initial",
        &config.delivery.retry_initial,
        Duration::from_secs(1),
        Duration::from_secs(3600),
    )?;
    duration_range(
        "delivery.retry_max",
        &config.delivery.retry_max,
        initial,
        Duration::from_secs(86400),
    )?;
    if !(16..=65536).contains(&config.delivery.queue_capacity) {
        return Err(ConfigError::Invalid(
            "delivery.queue_capacity outside 16..=65536".into(),
        ));
    }
    if !(50..=95).contains(&config.delivery.queue_warn_pct) {
        return Err(ConfigError::Invalid(
            "delivery.queue_warn_pct outside 50..=95".into(),
        ));
    }
    if !(1..=100).contains(&config.delivery.failure_report_after) {
        return Err(ConfigError::Invalid(
            "delivery.failure_report_after outside 1..=100".into(),
        ));
    }
    let mut names = HashSet::new();
    for check in &config.checks {
        if check.name.is_empty() || check.name.len() > 128 || !names.insert(&check.name) {
            return Err(ConfigError::Invalid(format!(
                "check name {:?} is empty, too long, or duplicated",
                check.name
            )));
        }
        if check.severity == Severity::Ok {
            return Err(ConfigError::Invalid(format!(
                "check {} severity cannot be ok",
                check.name
            )));
        }
        if let Some(value) = &check.pending_for {
            duration_range(
                "checks.pending_for",
                value,
                Duration::ZERO,
                Duration::from_secs(86400),
            )?;
        }
        if let Some(value) = &check.recover_for {
            duration_range(
                "checks.recover_for",
                value,
                Duration::ZERO,
                Duration::from_secs(86400),
            )?;
        }
        if let Some(url) = &check.runbook {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ConfigError::Invalid(format!(
                    "check {} runbook must be HTTP(S)",
                    check.name
                )));
            }
        }
        validate_check(check, interval)?;
    }
    Ok(())
}

fn validate_clock(value: &str) -> Result<(), ConfigError> {
    if value == "off" {
        return Ok(());
    }
    let (h, m) = value
        .split_once(':')
        .ok_or_else(|| ConfigError::Invalid("daily_report_at must be HH:MM".into()))?;
    let (h, m): (u8, u8) = (
        h.parse()
            .map_err(|_| ConfigError::Invalid("invalid report hour".into()))?,
        m.parse()
            .map_err(|_| ConfigError::Invalid("invalid report minute".into()))?,
    );
    if h > 23 || m > 59 {
        return Err(ConfigError::Invalid(
            "daily_report_at outside 00:00..23:59".into(),
        ));
    }
    Ok(())
}

fn validate_check(check: &CheckConfig, interval: Duration) -> Result<(), ConfigError> {
    match &check.kind {
        CheckKind::Process {
            cmdline_contains,
            min_count,
        } if cmdline_contains.is_empty() || !(1..=1024).contains(min_count) => {
            Err(ConfigError::Invalid(format!(
                "check {} has invalid process matcher/count",
                check.name
            )))
        }
        CheckKind::Shm {
            path,
            probe,
            require_progress,
            stale_after,
            offset,
            endian,
            magic,
            layout_version,
        } => {
            if !path.starts_with('/') || path[1..].contains('/') {
                return Err(ConfigError::Invalid(format!(
                    "check {} has invalid POSIX SHM name",
                    check.name
                )));
            }
            if *probe == ShmProbe::U64Counter && (offset.is_none() || endian.is_none()) {
                return Err(ConfigError::Invalid(format!(
                    "check {} u64_counter needs offset and endian",
                    check.name
                )));
            }
            if *probe == ShmProbe::GconfV2 && (magic.is_none() || layout_version.is_none()) {
                return Err(ConfigError::Invalid(format!(
                    "check {} gconf_v2 needs explicit magic and layout_version",
                    check.name
                )));
            }
            if *require_progress {
                let value = stale_after.as_ref().ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "check {} progress probe needs stale_after",
                        check.name
                    ))
                })?;
                if parse_duration(value)? < interval {
                    return Err(ConfigError::Invalid(format!(
                        "check {} stale_after is shorter than interval",
                        check.name
                    )));
                }
            }
            Ok(())
        }
        CheckKind::Journal {
            units,
            ignore_contains,
            rules,
        } if units.is_empty()
            || rules.is_empty()
            || units.iter().any(String::is_empty)
            || ignore_contains.iter().any(String::is_empty)
            || rules
                .iter()
                .any(|r| r.contains.is_empty() || r.severity == Severity::Ok) =>
        {
            Err(ConfigError::Invalid(format!(
                "check {} needs journal units, non-empty filters, and non-empty warn/critical rules",
                check.name
            )))
        }
        CheckKind::Systemd { units }
            if units.is_empty()
                || units.len() > 64
                || units.iter().any(|unit| unit.is_empty() || unit.len() > 255) =>
        {
            Err(ConfigError::Invalid(format!(
                "check {} needs 1..=64 non-empty systemd units",
                check.name
            )))
        }
        CheckKind::LatestFile {
            directory,
            prefix,
            suffix,
            stale_after,
            minimum_size_bytes,
        } => {
            if !directory.is_absolute()
                || prefix.is_empty()
                || prefix.len() > 255
                || suffix.len() > 255
                || *minimum_size_bytes == 0
                || *minimum_size_bytes > 1_u64 << 40
            {
                return Err(ConfigError::Invalid(format!(
                    "check {} has invalid latest_file path, matcher, or size",
                    check.name
                )));
            }
            duration_range(
                "checks.latest_file.stale_after",
                stale_after,
                interval,
                Duration::from_secs(86_400),
            )?;
            Ok(())
        }
        CheckKind::MetricsFile {
            path,
            stale_after,
            metrics,
        } => {
            validate_metric_rules(check, path.is_absolute(), metrics)?;
            duration_range(
                "checks.metrics_file.stale_after",
                stale_after,
                interval,
                Duration::from_secs(86_400),
            )?;
            Ok(())
        }
        CheckKind::MetricsShm {
            path,
            abi_hash,
            metrics,
        } => validate_metrics_shm(check, path, abi_hash.as_ref(), metrics),
        CheckKind::Disk {
            mount,
            warn_used_pct,
            critical_used_pct,
            warn_inode_used_pct,
            critical_inode_used_pct,
        } if !mount.is_absolute()
            || !valid_upper_thresholds(*warn_used_pct, *critical_used_pct)
            || !valid_upper_thresholds(*warn_inode_used_pct, *critical_inode_used_pct) =>
        {
            Err(ConfigError::Invalid(format!(
                "check {} has invalid disk config",
                check.name
            )))
        }
        CheckKind::Memory {
            warn_available_pct,
            critical_available_pct,
        } if !(*critical_available_pct >= 0.0
            && critical_available_pct < warn_available_pct
            && *warn_available_pct < 100.0) =>
        {
            Err(ConfigError::Invalid(format!(
                "check {} has invalid memory thresholds",
                check.name
            )))
        }
        CheckKind::Cpu {
            warn_usage_pct,
            critical_usage_pct,
        } if !valid_upper_thresholds(*warn_usage_pct, *critical_usage_pct) => Err(
            ConfigError::Invalid(format!("check {} has invalid CPU thresholds", check.name)),
        ),
        CheckKind::TimeSync {
            warn_offset,
            critical_offset,
        } => {
            let warn = duration_range(
                "checks.time_sync.warn_offset",
                warn_offset,
                Duration::from_millis(1),
                Duration::from_secs(1),
            )?;
            duration_range(
                "checks.time_sync.critical_offset",
                critical_offset,
                warn + Duration::from_millis(1),
                Duration::from_secs(1),
            )?;
            Ok(())
        }
        CheckKind::Network {
            interfaces,
            warn_errors_per_second,
            critical_errors_per_second,
            warn_drops_per_second,
            critical_drops_per_second,
        } => {
            let unique: HashSet<_> = interfaces.iter().collect();
            if interfaces.is_empty()
                || interfaces.len() > 64
                || unique.len() != interfaces.len()
                || interfaces.iter().any(|name| {
                    name.is_empty()
                        || name.len() > 15
                        || name.contains('/')
                        || name.chars().any(char::is_whitespace)
                })
                || !valid_rate_thresholds(*warn_errors_per_second, *critical_errors_per_second)
                || !valid_rate_thresholds(*warn_drops_per_second, *critical_drops_per_second)
            {
                return Err(ConfigError::Invalid(format!(
                    "check {} has invalid network interfaces or thresholds",
                    check.name
                )));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn valid_upper_thresholds(warn: f64, critical: f64) -> bool {
    warn.is_finite() && critical.is_finite() && warn > 0.0 && warn < critical && critical <= 100.0
}

fn valid_rate_thresholds(warn: f64, critical: f64) -> bool {
    warn.is_finite() && critical.is_finite() && warn >= 0.0 && warn < critical
}

fn validate_metric_rules(
    check: &CheckConfig,
    valid_path: bool,
    metrics: &[MetricRule],
) -> Result<(), ConfigError> {
    let unique: HashSet<_> = metrics.iter().map(|metric| &metric.key).collect();
    if !valid_path
        || metrics.is_empty()
        || metrics.len() > 64
        || unique.len() != metrics.len()
        || metrics.iter().any(|metric| {
            invalid_metric_fields(
                &metric.key,
                metric.critical_below,
                metric.warn_below,
                metric.warn_above,
                metric.critical_above,
            )
        })
    {
        return Err(ConfigError::Invalid(format!(
            "check {} has invalid metrics path, keys, or thresholds",
            check.name
        )));
    }
    Ok(())
}

fn validate_metrics_shm(
    check: &CheckConfig,
    path: &str,
    abi_hash: Option<&ShmAbiHash>,
    metrics: &[ShmMetricRule],
) -> Result<(), ConfigError> {
    let unique: HashSet<_> = metrics.iter().map(|metric| &metric.key).collect();
    let invalid_abi = abi_hash.is_some_and(|abi| {
        abi.expected_hex.len() < 2
            || abi.expected_hex.len() > 128
            || abi.expected_hex.len() % 2 != 0
            || !abi
                .expected_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || abi
                .offset
                .checked_add((abi.expected_hex.len() / 2) as u64)
                .is_none()
    });
    let invalid_metric = metrics.iter().any(|metric| {
        invalid_metric_fields(
            &metric.key,
            metric.critical_below,
            metric.warn_below,
            metric.warn_above,
            metric.critical_above,
        ) || metric
            .offset
            .checked_add(metric.value_type.width())
            .is_none()
    });
    if !valid_posix_shm_name(path)
        || metrics.is_empty()
        || metrics.len() > 64
        || unique.len() != metrics.len()
        || invalid_abi
        || invalid_metric
    {
        return Err(ConfigError::Invalid(format!(
            "check {} has invalid metrics_shm path, ABI hash, metrics, or thresholds",
            check.name
        )));
    }
    Ok(())
}

fn invalid_metric_fields(
    key: &str,
    critical_below: Option<f64>,
    warn_below: Option<f64>,
    warn_above: Option<f64>,
    critical_above: Option<f64>,
) -> bool {
    key.is_empty()
        || key.len() > 128
        || [critical_below, warn_below, warn_above, critical_above]
            .into_iter()
            .flatten()
            .any(|value| !value.is_finite())
        || matches!(
            (critical_below, warn_below),
            (Some(critical), Some(warn)) if critical >= warn
        )
        || matches!(
            (warn_above, critical_above),
            (Some(warn), Some(critical)) if warn >= critical
        )
        || lower_and_upper_overlap(critical_below, warn_below, warn_above, critical_above)
}

fn lower_and_upper_overlap(
    critical_below: Option<f64>,
    warn_below: Option<f64>,
    warn_above: Option<f64>,
    critical_above: Option<f64>,
) -> bool {
    [critical_below, warn_below]
        .into_iter()
        .flatten()
        .any(|lower| {
            [warn_above, critical_above]
                .into_iter()
                .flatten()
                .any(|upper| lower >= upper)
        })
}

fn valid_posix_shm_name(path: &str) -> bool {
    path.len() > 1
        && path.len() <= 255
        && path.starts_with('/')
        && !path[1..].contains('/')
        && !path.contains('\0')
}

pub fn resolve_dingtalk_credentials(
    config: &DeliveryConfig,
) -> Result<(String, String), ConfigError> {
    let token = std::env::var(&config.token_env).map_err(|_| {
        ConfigError::Invalid(format!("environment {} is missing", config.token_env))
    })?;
    let secret = std::env::var(&config.secret_env).map_err(|_| {
        ConfigError::Invalid(format!("environment {} is missing", config.secret_env))
    })?;
    if token.is_empty() || secret.is_empty() {
        return Err(ConfigError::Invalid(
            "DingTalk credentials cannot be empty".into(),
        ));
    }
    Ok((token, secret))
}
