use super::CollectError;
#[cfg(target_os = "linux")]
use super::command;
use crate::{
    config::{self, CheckConfig, LiveMmInstance},
    model::{Observation, Severity, StatisticsCounters},
};
use chrono::{DateTime, Utc};
use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct EntryState {
    observed_at: DateTime<Utc>,
    operator_enabled: u8,
    entries_enabled: u8,
    runtime_risk_mask: u32,
    reason: u16,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RiskState {
    observed_at: DateTime<Utc>,
    runtime_risk_mask: u32,
    symbol_risk_combined_mask: u32,
    symbol_risk_affected: u32,
    order_guard_account_mask: u32,
    order_guard_symbol_combined_mask: u32,
    order_guard_symbol_affected: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct JournalState {
    unit: String,
    message: String,
    observed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnitState {
    unit: String,
    load: String,
    active: String,
    sub: String,
}

#[derive(Clone, Debug)]
struct StrategyStats {
    snapshot_id: u64,
    observed_at: DateTime<Utc>,
    fields: HashMap<String, String>,
    symbols: Vec<HashMap<String, String>>,
}

pub fn collect(
    check: &CheckConfig,
    instances: &[LiveMmInstance],
    stale_after: &str,
    statistics_stale_after: Option<&str>,
    timeout: Duration,
) -> Result<Observation, CollectError> {
    let stale_after = config::parse_duration(stale_after)
        .map_err(|error| CollectError::Invalid(error.to_string()))?;
    let statistics_stale_after = statistics_stale_after
        .map(config::parse_duration)
        .transpose()
        .map_err(|error| CollectError::Invalid(error.to_string()))?;
    let horizon = statistics_stale_after
        .unwrap_or(stale_after)
        .max(stale_after);
    let units = read_unit_states(instances, timeout)?;
    let rows = read_latest(instances, horizon, timeout)?;
    Ok(evaluate(
        check,
        instances,
        &units,
        &rows,
        Utc::now(),
        statistics_stale_after,
    ))
}

fn evaluate(
    check: &CheckConfig,
    instances: &[LiveMmInstance],
    units: &[UnitState],
    rows: &[JournalState],
    now: DateTime<Utc>,
    statistics_stale_after: Option<Duration>,
) -> Observation {
    let latest = latest_by_unit(rows);
    let latest_risks = latest_risk_by_unit(rows);
    let statistics = latest_statistics_by_unit(rows);
    let units = units
        .iter()
        .map(|state| (state.unit.as_str(), state))
        .collect::<HashMap<_, _>>();
    let mut failures = Vec::new();
    let mut details = Vec::new();

    for instance in instances {
        let mut instance_details = vec![instance.name.clone()];
        let unit = units.get(instance.unit.as_str());
        let unit_active =
            unit.is_some_and(|state| state.load == "loaded" && state.active == "active");
        if !unit_active {
            let state = unit
                .map(|state| format!("{}/{}/{}", state.load, state.active, state.sub))
                .unwrap_or_else(|| "状态缺失".to_owned());
            instance_details.push(format!("入口：unit={} systemd={}", instance.unit, state));
            instance_details.push("风险：unavailable".to_owned());
            instance_details.push("限频：unavailable".to_owned());
            details.push(instance_details.join("\n"));
            failures.push(instance.name.clone());
            continue;
        }

        match latest.get(instance.unit.as_str()) {
            Some(row) => match parse_state(row) {
                Ok(state) => {
                    let age = now
                        .signed_duration_since(row.observed_at)
                        .to_std()
                        .unwrap_or(Duration::ZERO);
                    instance_details.push(format!(
                        "入口：operator={} entries={} reason={} generation={} age={}s unit={}",
                        state.operator_enabled,
                        state.entries_enabled,
                        state.reason,
                        state.generation,
                        age.as_secs(),
                        instance.unit,
                    ));
                    if state.operator_enabled != 1
                        || state.entries_enabled != 1
                        || state.runtime_risk_mask != 0
                    {
                        failures.push(instance.name.clone());
                    }
                }
                Err(error) => {
                    instance_details.push(format!(
                        "入口：unit={} systemd=active {}",
                        instance.unit, error
                    ));
                    failures.push(instance.name.clone());
                }
            },
            None => {
                instance_details.push(format!(
                    "入口：unit={} systemd=active 状态日志未更新（不作为门禁）",
                    instance.unit
                ));
            }
        }

        match latest_risks.get(instance.unit.as_str()) {
            Some(row) => match parse_risk_state(row) {
                Ok(state) => {
                    let age = now
                        .signed_duration_since(state.observed_at)
                        .to_std()
                        .unwrap_or(Duration::ZERO);
                    instance_details.push(format!(
                        "风险：runtime=0x{:08x} symbol=0x{:08x} affected=0x{:08x} age={}s",
                        state.runtime_risk_mask,
                        state.symbol_risk_combined_mask,
                        state.symbol_risk_affected,
                        age.as_secs(),
                    ));
                    instance_details.push(format!(
                        "限频：account=0x{:02x} symbol=0x{:02x} affected=0x{:08x}",
                        state.order_guard_account_mask,
                        state.order_guard_symbol_combined_mask,
                        state.order_guard_symbol_affected,
                    ));
                    if state.runtime_risk_mask != 0
                        || state.symbol_risk_combined_mask != 0
                        || state.order_guard_account_mask != 0
                        || state.order_guard_symbol_combined_mask != 0
                    {
                        failures.push(instance.name.clone());
                    }
                }
                Err(error) => {
                    instance_details.push(format!("风险：risk_state {}", error));
                    instance_details.push("限频：unknown".to_owned());
                    failures.push(instance.name.clone());
                }
            },
            None => {
                instance_details.push("风险：risk_state missing".to_owned());
                instance_details.push("限频：unknown".to_owned());
                failures.push(instance.name.clone());
            }
        }

        if let Some(maximum_age) = statistics_stale_after {
            match statistics.get(instance.unit.as_str()) {
                Some(snapshot) => {
                    let age = now
                        .signed_duration_since(snapshot.observed_at)
                        .to_std()
                        .unwrap_or(Duration::ZERO);
                    if age > maximum_age {
                        failures.push(instance.name.clone());
                        instance_details.push(format!(
                            "统计：stale snapshot_id={} age={}s",
                            snapshot.snapshot_id,
                            age.as_secs()
                        ));
                    }
                }
                None => {
                    failures.push(instance.name.clone());
                    instance_details.push("统计：missing".to_owned());
                }
            }
        }
        details.push(instance_details.join("\n"));
    }

    failures.sort();
    failures.dedup();
    let summary = if failures.is_empty() {
        format!("{} 个 live_mm 交易入口与风险位正常", instances.len())
    } else {
        format!("live_mm 状态异常：{}", failures.join(", "))
    };
    let observation = if failures.is_empty() {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, Severity::Critical, summary)
    };
    // DingTalk Markdown collapses single newlines inside a field. A blank line keeps
    // each documented instance-state record on its own visible line.
    let mut observation = observation.detail("实例状态", details.join("\n\n"));
    if statistics_stale_after.is_some() {
        observation = observation.detail(
            "策略统计",
            format_statistics_report(instances, &statistics, now),
        );
        observation = observation.detail(
            "_statistics_counters",
            statistics_counters(instances, &statistics),
        );
    }
    observation
}

#[cfg(not(target_os = "linux"))]
fn read_unit_states(
    _instances: &[LiveMmInstance],
    _timeout: Duration,
) -> Result<Vec<UnitState>, CollectError> {
    Err(CollectError::Unsupported("systemd requires Linux".into()))
}

#[cfg(target_os = "linux")]
fn read_unit_states(
    instances: &[LiveMmInstance],
    timeout: Duration,
) -> Result<Vec<UnitState>, CollectError> {
    let mut arguments = vec!["show"];
    arguments.extend(instances.iter().map(|instance| instance.unit.as_str()));
    arguments.push("--property=Id,LoadState,ActiveState,SubState");
    let output = command::run("systemctl", &arguments, timeout)?;
    if !output.status.success() {
        return Err(CollectError::Invalid(format!(
            "systemctl show exited {}",
            output.status
        )));
    }
    parse_unit_states(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(any(target_os = "linux", test))]
fn parse_unit_states(text: &str) -> Result<Vec<UnitState>, CollectError> {
    let mut states = Vec::new();
    let mut fields = HashMap::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(unit) = fields.remove("Id") {
                states.push(UnitState {
                    unit,
                    load: fields.remove("LoadState").unwrap_or_default(),
                    active: fields.remove("ActiveState").unwrap_or_default(),
                    sub: fields.remove("SubState").unwrap_or_default(),
                });
            }
            fields.clear();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(CollectError::Invalid("malformed systemctl output".into()));
        };
        fields.insert(key, value.to_owned());
    }
    Ok(states)
}

fn latest_statistics_by_unit(rows: &[JournalState]) -> HashMap<&str, StrategyStats> {
    let mut summaries: HashMap<&str, StrategyStats> = HashMap::new();
    for row in rows {
        if !row.message.contains("event=strategy_stats ") {
            continue;
        }
        let Ok(fields) = fields(&row.message) else {
            continue;
        };
        let Some(snapshot_id) = fields
            .get("snapshot_id")
            .and_then(|value| value.parse().ok())
        else {
            continue;
        };
        if !valid_statistics_fields(&fields) {
            continue;
        }
        match summaries.get(row.unit.as_str()) {
            Some(previous) if previous.observed_at >= row.observed_at => {}
            _ => {
                summaries.insert(
                    row.unit.as_str(),
                    StrategyStats {
                        snapshot_id,
                        observed_at: row.observed_at,
                        fields,
                        symbols: Vec::new(),
                    },
                );
            }
        }
    }
    for row in rows {
        if !row.message.contains("event=strategy_symbol_stats ") {
            continue;
        }
        let Ok(fields) = fields(&row.message) else {
            continue;
        };
        let Some(snapshot) = summaries.get_mut(row.unit.as_str()) else {
            continue;
        };
        if !valid_symbol_fields(&fields) {
            continue;
        }
        if fields
            .get("snapshot_id")
            .is_some_and(|value| value == &snapshot.snapshot_id.to_string())
            && row.observed_at <= snapshot.observed_at
        {
            snapshot.symbols.push(fields);
        }
    }
    summaries
}

fn valid_statistics_fields(fields: &HashMap<String, String>) -> bool {
    const UNSIGNED: &[&str] = &[
        "snapshot_id",
        "account_readable",
        "equity_valid",
        "equity_1e8",
        "equity_age_ms",
        "exposure_valid",
        "nonzero_positions",
        "unpriced_positions",
        "entries_enabled",
        "operator_enabled",
        "active_open",
        "active_maker",
        "active_taker",
        "active_episodes",
        "open_total",
        "close_total",
        "fills_total",
        "place_fail_total",
        "open_delta",
        "close_delta",
        "fills_delta",
        "place_fail_delta",
    ];
    UNSIGNED.iter().all(|name| {
        fields
            .get(*name)
            .is_some_and(|value| value.parse::<u64>().is_ok())
    }) && ["gross_exposure_1e8", "net_exposure_1e8"]
        .iter()
        .all(|name| {
            fields
                .get(*name)
                .is_some_and(|value| value.parse::<i64>().is_ok())
        })
        && fields
            .get("runtime_risk_mask")
            .is_some_and(|value| u32::from_str_radix(value.trim_start_matches("0x"), 16).is_ok())
}

fn statistics_counters(
    instances: &[LiveMmInstance],
    statistics: &HashMap<&str, StrategyStats>,
) -> String {
    let counters = instances
        .iter()
        .filter_map(|instance| {
            let snapshot = statistics.get(instance.unit.as_str())?;
            let fields = &snapshot.fields;
            Some((
                instance.name.clone(),
                StatisticsCounters {
                    observed_at: snapshot.observed_at,
                    snapshot_id: snapshot.snapshot_id,
                    open_total: fields.get("open_total")?.parse().ok()?,
                    close_total: fields.get("close_total")?.parse().ok()?,
                    fills_total: fields.get("fills_total")?.parse().ok()?,
                    place_fail_total: fields.get("place_fail_total")?.parse().ok()?,
                },
            ))
        })
        .collect::<BTreeMap<_, _>>();
    serde_json::to_string(&counters).unwrap_or_else(|_| "{}".into())
}

fn valid_symbol_fields(fields: &HashMap<String, String>) -> bool {
    fields.get("coin").is_some_and(|coin| !coin.is_empty())
        && [
            "snapshot_id",
            "priced",
            "account_age_ms",
            "active_open",
            "active_maker",
            "active_taker",
            "active_episodes",
        ]
        .iter()
        .all(|name| {
            fields
                .get(*name)
                .is_some_and(|value| value.parse::<u64>().is_ok())
        })
        && ["position_1e8", "signed_exposure_1e8"].iter().all(|name| {
            fields
                .get(*name)
                .is_some_and(|value| value.parse::<i64>().is_ok())
        })
}

fn fields(message: &str) -> Result<HashMap<String, String>, String> {
    let mut result = HashMap::new();
    for part in message.split_ascii_whitespace() {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        if !name.is_empty() && !value.is_empty() {
            result.insert(name.to_owned(), value.to_owned());
        }
    }
    if result.is_empty() {
        Err("no structured fields".into())
    } else {
        Ok(result)
    }
}

fn number(fields: &HashMap<String, String>, name: &str) -> String {
    fields.get(name).cloned().unwrap_or_else(|| "?".into())
}

fn usdt(fields: &HashMap<String, String>, valid: &str, value: &str) -> String {
    if fields.get(valid).is_none_or(|flag| flag != "1") {
        return "unknown".into();
    }
    fields
        .get(value)
        .and_then(|raw| raw.parse::<i64>().ok())
        .map(|raw| format!("{:.2} U", raw as f64 / 100_000_000.0))
        .unwrap_or_else(|| "unknown".into())
}

fn format_statistics_report(
    instances: &[LiveMmInstance],
    statistics: &HashMap<&str, StrategyStats>,
    now: DateTime<Utc>,
) -> String {
    let mut output = Vec::new();
    let mut available = 0_u64;
    let mut entries = 0_u64;
    let mut risks = 0_u64;
    let mut total_equity = 0_i128;
    let mut total_gross = 0_i128;
    let mut total_net = 0_i128;
    let mut totals_valid = true;
    for instance in instances {
        let Some(snapshot) = statistics.get(instance.unit.as_str()) else {
            totals_valid = false;
            continue;
        };
        available += 1;
        let fields = &snapshot.fields;
        entries += u64::from(number(fields, "entries_enabled") == "1");
        risks += u64::from(number(fields, "runtime_risk_mask") != "00000000");
        if fields.get("equity_valid").is_none_or(|value| value != "1")
            || fields
                .get("exposure_valid")
                .is_none_or(|value| value != "1")
        {
            totals_valid = false;
            continue;
        }
        total_equity += fields["equity_1e8"].parse::<i128>().unwrap_or_default();
        total_gross += fields["gross_exposure_1e8"]
            .parse::<i128>()
            .unwrap_or_default();
        total_net += fields["net_exposure_1e8"]
            .parse::<i128>()
            .unwrap_or_default();
    }
    output.push("**总体**".to_owned());
    output.push(format!(
        "统计：{available}/{}｜入口：{entries}/{}｜风险：{risks}",
        instances.len(),
        instances.len(),
    ));
    output.push(if totals_valid {
        format!(
            "资产：权益 {:.2} U｜gross {:.2} U｜net {:.2} U",
            total_equity as f64 / 100_000_000.0,
            total_gross as f64 / 100_000_000.0,
            total_net as f64 / 100_000_000.0,
        )
    } else {
        "资产：unknown（存在缺失或无法定价样本）".into()
    });
    for instance in instances {
        let Some(snapshot) = statistics.get(instance.unit.as_str()) else {
            output.push(format!("**{}**\n\n状态：统计不可用", instance.name));
            continue;
        };
        let fields = &snapshot.fields;
        let age = now
            .signed_duration_since(snapshot.observed_at)
            .num_seconds()
            .max(0);
        let window = statistics_window(fields);
        output.push(format!(
            "**{}**\n\n状态：入口 {}｜风险 0x{}｜样本 {}s\n\n账户：权益 {}｜gross {}｜net {}\n\n活动订单：开仓 {}｜Maker平仓 {}｜Taker平仓 {}｜Episodes {}\n\n近{}：open {}｜close {}｜fills {}｜fail {}",
            instance.name,
            if number(fields, "entries_enabled") == "1" { "开" } else { "关" },
            number(fields, "runtime_risk_mask"),
            age,
            usdt(fields, "equity_valid", "equity_1e8"),
            usdt(fields, "exposure_valid", "gross_exposure_1e8"),
            usdt(fields, "exposure_valid", "net_exposure_1e8"),
            number(fields, "active_open"), number(fields, "active_maker"),
            number(fields, "active_taker"), number(fields, "active_episodes"),
            window,
            number(fields, "open_delta"), number(fields, "close_delta"),
            number(fields, "fills_delta"), number(fields, "place_fail_delta"),
        ));
        if !snapshot.symbols.is_empty() {
            output.push(format!("活动币种（{}）：", snapshot.symbols.len()));
            for symbol in &snapshot.symbols {
                output.push(format!(
                    "- {}｜仓位 {}｜敞口 {}\n  活动订单：开仓 {}｜Maker平仓 {}｜Taker平仓 {}\n  Episodes：{}｜账户数据年龄：{}ms",
                    number(symbol, "coin").to_uppercase(),
                    fixed_1e8(symbol, "position_1e8"),
                    usdt(symbol, "priced", "signed_exposure_1e8"),
                    number(symbol, "active_open"),
                    number(symbol, "active_maker"),
                    number(symbol, "active_taker"),
                    number(symbol, "active_episodes"),
                    number(symbol, "account_age_ms"),
                ));
            }
        }
    }
    output.join("\n\n")
}

fn statistics_window(fields: &HashMap<String, String>) -> String {
    let milliseconds = fields
        .get("delta_window_ms")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(30_000);
    if milliseconds % 1_000 == 0 {
        format!("{}s", milliseconds / 1_000)
    } else {
        format!("{:.3}s", milliseconds as f64 / 1_000.0)
    }
}

fn fixed_1e8(fields: &HashMap<String, String>, name: &str) -> String {
    let Ok(value) = number(fields, name).parse::<i128>() else {
        return "unknown".into();
    };
    let sign = if value < 0 { "-" } else { "" };
    let absolute = value.unsigned_abs();
    let integer = absolute / 100_000_000;
    let fraction = absolute % 100_000_000;
    if fraction == 0 {
        return format!("{sign}{integer}");
    }
    let fraction = format!("{fraction:08}").trim_end_matches('0').to_owned();
    format!("{sign}{integer}.{fraction}")
}

fn latest_by_unit(rows: &[JournalState]) -> HashMap<&str, &JournalState> {
    let mut latest: HashMap<&str, &JournalState> = HashMap::new();
    for row in rows {
        if !row.message.contains("[live_mm_host]") || !row.message.contains("entries_enabled=") {
            continue;
        }
        match latest.get(row.unit.as_str()) {
            Some(previous) if previous.observed_at >= row.observed_at => {}
            _ => {
                latest.insert(row.unit.as_str(), row);
            }
        }
    }
    latest
}

fn latest_risk_by_unit(rows: &[JournalState]) -> HashMap<&str, &JournalState> {
    let mut latest: HashMap<&str, &JournalState> = HashMap::new();
    for row in rows {
        if !row.message.contains("event=risk_state ") {
            continue;
        }
        match latest.get(row.unit.as_str()) {
            Some(previous) if previous.observed_at >= row.observed_at => {}
            _ => {
                latest.insert(row.unit.as_str(), row);
            }
        }
    }
    latest
}

fn parse_state(row: &JournalState) -> Result<EntryState, String> {
    Ok(EntryState {
        observed_at: row.observed_at,
        operator_enabled: field(&row.message, "operator_enabled")?,
        entries_enabled: field(&row.message, "entries_enabled")?,
        runtime_risk_mask: hex_field(&row.message, "runtime_risk_mask")?,
        reason: field(&row.message, "reason")?,
        generation: field(&row.message, "generation")?,
    })
}

fn parse_risk_state(row: &JournalState) -> Result<RiskState, String> {
    Ok(RiskState {
        observed_at: row.observed_at,
        runtime_risk_mask: hex_field(&row.message, "runtime_risk_mask")?,
        symbol_risk_combined_mask: hex_field(&row.message, "symbol_risk_combined_mask")?,
        symbol_risk_affected: hex_field(&row.message, "symbol_risk_affected")?,
        order_guard_account_mask: hex_field(&row.message, "order_guard_account_mask")?,
        order_guard_symbol_combined_mask: hex_field(
            &row.message,
            "order_guard_symbol_combined_mask",
        )?,
        order_guard_symbol_affected: hex_field(&row.message, "order_guard_symbol_affected")?,
    })
}

fn field<T: std::str::FromStr>(message: &str, name: &str) -> Result<T, String> {
    let prefix = format!("{name}=");
    let value = message
        .split_ascii_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .ok_or_else(|| format!("字段缺失 {name}"))?;
    value
        .parse()
        .map_err(|_| format!("字段非法 {name}={value}"))
}

fn hex_field(message: &str, name: &str) -> Result<u32, String> {
    let prefix = format!("{name}=");
    let value = message
        .split_ascii_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .ok_or_else(|| format!("字段缺失 {name}"))?;
    u32::from_str_radix(value.trim_start_matches("0x"), 16)
        .map_err(|_| format!("字段非法 {name}={value}"))
}

#[cfg(not(target_os = "linux"))]
fn read_latest(
    _instances: &[LiveMmInstance],
    _stale_after: Duration,
    _timeout: Duration,
) -> Result<Vec<JournalState>, CollectError> {
    Err(CollectError::Unsupported("journald requires Linux".into()))
}

#[cfg(target_os = "linux")]
fn read_latest(
    instances: &[LiveMmInstance],
    stale_after: Duration,
    timeout: Duration,
) -> Result<Vec<JournalState>, CollectError> {
    let since = format!("-{}s", stale_after.as_secs().saturating_add(1));
    let mut arguments = vec!["--no-pager", "--quiet", "--output=json", "--since", &since];
    for instance in instances {
        arguments.push("--unit");
        arguments.push(&instance.unit);
    }
    let output = command::run("journalctl", &arguments, timeout)?;
    if !output.status.success() {
        return Err(CollectError::Invalid(format!(
            "journalctl exited {}",
            output.status
        )));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(parse_journal_json)
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn parse_journal_json(line: &str) -> Result<JournalState, CollectError> {
    let value: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| CollectError::Invalid(format!("invalid journal JSON: {error}")))?;
    let unit = value
        .get("_SYSTEMD_UNIT")
        .and_then(|value| value.as_str())
        .ok_or_else(|| CollectError::Invalid("journal row missing _SYSTEMD_UNIT".into()))?;
    let message = value
        .get("MESSAGE")
        .and_then(|value| value.as_str())
        .ok_or_else(|| CollectError::Invalid("journal row missing MESSAGE".into()))?;
    let micros = value
        .get("__REALTIME_TIMESTAMP")
        .and_then(|value| value.as_str())
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| CollectError::Invalid("journal row missing timestamp".into()))?;
    let observed_at = DateTime::from_timestamp_micros(micros)
        .ok_or_else(|| CollectError::Invalid("journal row has invalid timestamp".into()))?;
    Ok(JournalState {
        unit: unit.into(),
        message: message.into(),
        observed_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CheckKind;
    use chrono::TimeZone;

    fn check() -> CheckConfig {
        CheckConfig {
            name: "live-mm-entry".into(),
            enabled: true,
            severity: Severity::Critical,
            pending_for: None,
            recover_for: None,
            runbook: None,
            delivery_route: "default".into(),
            kind: CheckKind::LiveMmEntry {
                instances: instances(),
                stale_after: "20s".into(),
                statistics_stale_after: None,
                statistics_report_every: "off".into(),
            },
        }
    }

    fn instances() -> Vec<LiveMmInstance> {
        ["live_mm1", "live_mm2", "live_mm3", "live_mm4"]
            .into_iter()
            .map(|name| LiveMmInstance {
                name: name.into(),
                unit: format!("{name}.service"),
            })
            .collect()
    }

    fn active_units() -> Vec<UnitState> {
        instances()
            .into_iter()
            .map(|instance| UnitState {
                unit: instance.unit,
                load: "loaded".into(),
                active: "active".into(),
                sub: "running".into(),
            })
            .collect()
    }

    fn row(name: &str, seconds: i64, operator: u8, entries: u8, risk: &str) -> JournalState {
        JournalState {
            unit: format!("{name}.service"),
            message: format!(
                "[live_mm_host] progress entries_enabled={entries} operator_enabled={operator} runtime_risk_mask={risk} reason=2 generation=7"
            ),
            observed_at: Utc.timestamp_opt(seconds, 0).unwrap(),
        }
    }

    fn risk_row(
        name: &str,
        seconds: i64,
        runtime: &str,
        symbol: &str,
        account_guard: &str,
        symbol_guard: &str,
    ) -> JournalState {
        JournalState {
            unit: format!("{name}.service"),
            message: format!(
                "[INFO] event=risk_state runtime_risk_mask={runtime} symbol_risk_combined_mask={symbol} symbol_risk_affected=00000000 order_guard_account_mask={account_guard} order_guard_symbol_combined_mask={symbol_guard} order_guard_symbol_affected=00000000"
            ),
            observed_at: Utc.timestamp_opt(seconds, 0).unwrap(),
        }
    }

    fn healthy_risk_rows(seconds: i64) -> Vec<JournalState> {
        instances()
            .iter()
            .map(|instance| risk_row(&instance.name, seconds, "00000000", "00000000", "00", "00"))
            .collect()
    }

    #[test]
    fn reports_all_instances_healthy() {
        let mut rows = instances()
            .iter()
            .map(|instance| row(&instance.name, 100, 1, 1, "00000000"))
            .collect::<Vec<_>>();
        rows.extend(healthy_risk_rows(100));
        let observation = evaluate(
            &check(),
            &instances(),
            &active_units(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            None,
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Healthy
        ));
    }

    #[test]
    fn parses_systemd_activity_for_each_unit() {
        let states = parse_unit_states(
            "Id=live_mm1.service\nLoadState=loaded\nActiveState=active\nSubState=running\n\n\
             Id=live_mm2.service\nLoadState=loaded\nActiveState=failed\nSubState=failed\n",
        )
        .unwrap();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].active, "active");
        assert_eq!(states[1].active, "failed");
    }

    #[test]
    fn reports_risk_operator_and_effective_entry_failures() {
        let mut rows = vec![
            row("live_mm1", 100, 1, 1, "00000000"),
            row("live_mm2", 100, 1, 0, "00000002"),
            row("live_mm3", 100, 0, 0, "00000000"),
            row("live_mm4", 100, 1, 1, "00000000"),
        ];
        rows.extend(healthy_risk_rows(100));
        let observation = evaluate(
            &check(),
            &instances(),
            &active_units(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            None,
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Unhealthy(Severity::Critical)
        ));
        assert!(observation.summary.contains("live_mm2"));
        assert!(observation.summary.contains("live_mm3"));
    }

    #[test]
    fn reports_each_nonzero_risk_mask() {
        let cases = [
            ("00000002", "00000000", "00", "00", "runtime=0x00000002"),
            (
                "00000000",
                "00000004",
                "00",
                "00",
                "风险：runtime=0x00000000 symbol=0x00000004",
            ),
            (
                "00000000",
                "00000000",
                "14",
                "00",
                "限频：account=0x14 symbol=0x00",
            ),
            (
                "00000000",
                "00000000",
                "00",
                "08",
                "限频：account=0x00 symbol=0x08",
            ),
        ];
        for (runtime, symbol, account_guard, symbol_guard, detail) in cases {
            let mut rows = instances()
                .iter()
                .map(|instance| row(&instance.name, 100, 1, 1, "00000000"))
                .collect::<Vec<_>>();
            rows.extend(healthy_risk_rows(100));
            rows.push(risk_row(
                "live_mm2",
                101,
                runtime,
                symbol,
                account_guard,
                symbol_guard,
            ));
            let observation = evaluate(
                &check(),
                &instances(),
                &active_units(),
                &rows,
                Utc.timestamp_opt(105, 0).unwrap(),
                None,
            );
            assert!(matches!(
                observation.status,
                crate::model::ObservationStatus::Unhealthy(Severity::Critical)
            ));
            assert!(observation.summary.contains("live_mm2"));
            assert!(observation.details["实例状态"].contains(detail));
        }
    }

    #[test]
    fn reports_missing_risk_snapshot() {
        let rows = instances()
            .iter()
            .map(|instance| row(&instance.name, 100, 1, 1, "00000000"))
            .collect::<Vec<_>>();
        let observation = evaluate(
            &check(),
            &instances(),
            &active_units(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            None,
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Unhealthy(Severity::Critical)
        ));
        assert!(observation.details["实例状态"].contains("risk_state missing"));
    }

    #[test]
    fn active_units_do_not_fail_when_state_log_is_absent_or_old() {
        let mut rows = vec![row("live_mm1", 70, 1, 1, "00000000")];
        rows.extend(healthy_risk_rows(100));
        let observation = evaluate(
            &check(),
            &instances(),
            &active_units(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            None,
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Healthy
        ));
        let details = &observation.details["实例状态"];
        assert!(details.contains("systemd=active 状态日志未更新（不作为门禁）"));
    }

    #[test]
    fn reports_inactive_unit_and_malformed_fresh_state() {
        let rows = vec![JournalState {
            unit: "live_mm2.service".into(),
            message: "[live_mm_host] entries_enabled=unknown".into(),
            observed_at: Utc.timestamp_opt(100, 0).unwrap(),
        }];
        let mut units = active_units();
        units[2].active = "failed".into();
        units[2].sub = "failed".into();
        let observation = evaluate(
            &check(),
            &instances(),
            &units,
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            None,
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Unhealthy(Severity::Critical)
        ));
        let details = &observation.details["实例状态"];
        assert!(details.contains("字段缺失 operator_enabled"));
        assert!(details.contains("live_mm3\n入口：unit=live_mm3.service systemd=loaded/failed/failed\n风险：unavailable\n限频：unavailable"));
    }

    #[test]
    fn accepts_only_complete_statistics_snapshot_and_formats_active_symbol() {
        let mut rows = instances()
            .iter()
            .map(|instance| row(&instance.name, 100, 1, 1, "00000000"))
            .collect::<Vec<_>>();
        rows.extend(healthy_risk_rows(100));
        for instance in instances() {
            rows.push(JournalState {
                unit: instance.unit.clone(),
                message: "[INFO] event=strategy_symbol_stats snapshot_id=7 coin=APE position_1e8=100000000 signed_exposure_1e8=125000000 priced=1 account_age_ms=3 active_open=1 active_maker=0 active_taker=0 active_episodes=1".into(),
                observed_at: Utc.timestamp_opt(101, 0).unwrap(),
            });
            rows.push(JournalState {
                unit: instance.unit,
                message: "[INFO] event=strategy_stats snapshot_id=7 account_readable=1 equity_valid=1 equity_1e8=10000000000 equity_age_ms=3 exposure_valid=1 gross_exposure_1e8=125000000 net_exposure_1e8=125000000 nonzero_positions=1 unpriced_positions=0 entries_enabled=1 operator_enabled=1 runtime_risk_mask=00000000 active_open=1 active_maker=0 active_taker=0 active_episodes=1 open_total=10 close_total=9 fills_total=8 place_fail_total=0 delta_window_ms=30241 open_delta=1 close_delta=0 fills_delta=0 place_fail_delta=0".into(),
                observed_at: Utc.timestamp_opt(102, 0).unwrap(),
            });
        }
        let observation = evaluate(
            &check(),
            &instances(),
            &active_units(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            Some(Duration::from_secs(90)),
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Healthy
        ));
        let report = &observation.details["策略统计"];
        assert!(report.contains("**总体**\n\n统计：4/4｜入口：4/4｜风险：0"));
        assert!(report.contains("**live_mm1**\n\n状态：入口 开｜风险 0x00000000｜样本 3s"));
        assert!(report.contains("近30.241s：open 1｜close 0｜fills 0｜fail 0"));
        assert!(report.contains(
            "活动币种（1）：\n\n- APE｜仓位 1｜敞口 1.25 U\n  活动订单：开仓 1｜Maker平仓 0｜Taker平仓 0\n  Episodes：1｜账户数据年龄：3ms"
        ));
        assert!(observation.details["_statistics_counters"].contains("\"open_total\":10"));
    }

    #[test]
    fn rejects_statistics_without_commit_marker() {
        let mut rows = instances()
            .iter()
            .map(|instance| row(&instance.name, 100, 1, 1, "00000000"))
            .collect::<Vec<_>>();
        rows.extend(healthy_risk_rows(100));
        rows.push(JournalState {
            unit: "live_mm1.service".into(),
            message: "[INFO] event=strategy_symbol_stats snapshot_id=9 coin=APE".into(),
            observed_at: Utc.timestamp_opt(102, 0).unwrap(),
        });
        let observation = evaluate(
            &check(),
            &instances(),
            &active_units(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            Some(Duration::from_secs(90)),
        );
        assert!(matches!(
            observation.status,
            crate::model::ObservationStatus::Unhealthy(Severity::Critical)
        ));
        assert!(observation.details["实例状态"].contains("统计：missing"));
    }

    #[test]
    fn groups_entry_and_risk_lines_by_instance() {
        let mut rows = instances()
            .iter()
            .map(|instance| row(&instance.name, 100, 1, 1, "00000000"))
            .collect::<Vec<_>>();
        rows.extend(healthy_risk_rows(100));
        let observation = evaluate(
            &check(),
            &instances(),
            &active_units(),
            &rows,
            Utc.timestamp_opt(105, 0).unwrap(),
            None,
        );
        let details = &observation.details["实例状态"];
        assert!(details.contains(
            "live_mm1\n入口：operator=1 entries=1 reason=2 generation=7 age=5s unit=live_mm1.service\n风险：runtime=0x00000000 symbol=0x00000000 affected=0x00000000 age=5s\n限频：account=0x00 symbol=0x00 affected=0x00000000\n\nlive_mm2"
        ));
    }

    #[test]
    fn parses_journal_json_identity_message_and_timestamp() {
        let row = parse_journal_json(
            r#"{"_SYSTEMD_UNIT":"live-mm-v0.service","MESSAGE":"[live_mm_host] progress entries_enabled=1 operator_enabled=1 runtime_risk_mask=00000000 reason=2 generation=7","__REALTIME_TIMESTAMP":"1000000"}"#,
        )
        .unwrap();
        assert_eq!(row.unit, "live-mm-v0.service");
        assert_eq!(row.observed_at, Utc.timestamp_opt(1, 0).unwrap());
        assert!(row.message.contains("runtime_risk_mask=00000000"));
    }
}
