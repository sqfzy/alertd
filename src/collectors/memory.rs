use super::{CollectContext, CollectError};
use crate::{
    config::CheckConfig,
    model::{Observation, Severity},
};
use std::{fs, path::Path};

pub fn parse_meminfo(text: &str) -> Result<(u64, u64), CollectError> {
    fn value(text: &str, key: &str) -> Option<u64> {
        text.lines().find_map(|line| {
            line.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
    }
    let total =
        value(text, "MemTotal:").ok_or_else(|| CollectError::Invalid("MemTotal missing".into()))?;
    let available = value(text, "MemAvailable:")
        .ok_or_else(|| CollectError::Invalid("MemAvailable missing".into()))?;
    if total == 0 {
        return Err(CollectError::Invalid("MemTotal is zero".into()));
    }
    Ok((total, available))
}

pub fn collect(
    check: &CheckConfig,
    warn: f64,
    critical: f64,
    context: &CollectContext,
) -> Result<Observation, CollectError> {
    let root = context.proc_root.as_deref().unwrap_or(Path::new("/proc"));
    let (total, available) = parse_meminfo(&fs::read_to_string(root.join("meminfo"))?)?;
    let pct = available as f64 * 100.0 / total as f64;
    let status = if pct <= critical {
        Some(Severity::Critical)
    } else if pct <= warn {
        Some(Severity::Warn)
    } else {
        None
    };
    let summary = format!("可用内存 {pct:.1}%");
    let observation = status.map_or_else(
        || Observation::healthy(&check.name, &summary),
        |severity| Observation::unhealthy(&check.name, severity, &summary),
    );
    Ok(observation
        .detail("可用", format!("{} MiB", available / 1024))
        .detail("总量", format!("{} MiB", total / 1024)))
}
