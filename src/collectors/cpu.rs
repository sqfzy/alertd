use super::{CollectContext, CollectError};
use crate::{
    config::CheckConfig,
    model::{Observation, Severity},
};
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    total: u64,
    idle: u64,
}

pub fn parse_cpu_times(text: &str) -> Result<BTreeMap<String, CpuTimes>, CollectError> {
    let mut cpus = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        if !name.starts_with("cpu")
            || name == "cpu"
            || !name[3..].bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let values: Vec<u64> = fields
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| CollectError::Invalid(format!("invalid /proc/stat line for {name}")))?;
        if values.len() < 4 {
            return Err(CollectError::Invalid(format!(
                "truncated /proc/stat line for {name}"
            )));
        }
        let total = values
            .iter()
            .take(8)
            .copied()
            .fold(0_u64, u64::saturating_add);
        let idle = values[3].saturating_add(values.get(4).copied().unwrap_or_default());
        cpus.insert(name.into(), CpuTimes { total, idle });
    }
    if cpus.is_empty() {
        return Err(CollectError::Invalid(
            "no logical CPUs in /proc/stat".into(),
        ));
    }
    Ok(cpus)
}

fn calculate_usage(previous: CpuTimes, current: CpuTimes) -> Option<f64> {
    let total = current.total.checked_sub(previous.total)?;
    let idle = current.idle.checked_sub(previous.idle)?;
    (total > 0).then(|| total.saturating_sub(idle) as f64 * 100.0 / total as f64)
}

pub fn collect(
    check: &CheckConfig,
    warn: f64,
    critical: f64,
    context: &mut CollectContext,
) -> Result<Observation, CollectError> {
    let root = context.proc_root.as_deref().unwrap_or(Path::new("/proc"));
    let current = parse_cpu_times(&fs::read_to_string(root.join("stat"))?)?;
    let Some(previous) = context
        .cpu_times
        .insert(check.name.clone(), current.clone())
    else {
        return Ok(Observation::healthy(&check.name, "CPU 已建立采样基线")
            .detail("逻辑 CPU", current.len().to_string()));
    };
    let mut usages = Vec::new();
    for (name, times) in &current {
        if let Some(usage) = previous
            .get(name)
            .and_then(|old| calculate_usage(*old, *times))
        {
            usages.push((name.clone(), usage));
        }
    }
    if usages.is_empty() {
        return Ok(Observation::healthy(&check.name, "CPU 计数器已重建基线")
            .detail("逻辑 CPU", current.len().to_string()));
    }
    let average = usages.iter().map(|(_, value)| value).sum::<f64>() / usages.len() as f64;
    let (highest_name, highest) = usages
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("usages is non-empty");
    let severity = if *highest >= critical {
        Some(Severity::Critical)
    } else if *highest >= warn {
        Some(Severity::Warn)
    } else {
        None
    };
    let summary = format!("CPU 平均 {average:.1}%，最高 {highest_name} {highest:.1}%");
    let observation = severity.map_or_else(
        || Observation::healthy(&check.name, &summary),
        |value| Observation::unhealthy(&check.name, value, &summary),
    );
    let rows = usages
        .chunks(8)
        .map(|chunk| {
            chunk
                .iter()
                .map(|(name, value)| format!("{name} {value:.1}%"))
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(observation.detail("每核", rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, model::ObservationStatus};

    #[test]
    fn parses_and_calculates_each_cpu() {
        let first =
            parse_cpu_times("cpu 0 0 0 0\ncpu0 10 0 10 80 0 0 0 0\ncpu1 20 0 20 60 0 0 0 0\n")
                .unwrap();
        let second =
            parse_cpu_times("cpu 0 0 0 0\ncpu0 20 0 20 160 0 0 0 0\ncpu1 60 0 40 100 0 0 0 0\n")
                .unwrap();
        assert_eq!(calculate_usage(first["cpu0"], second["cpu0"]), Some(20.0));
        assert_eq!(calculate_usage(first["cpu1"], second["cpu1"]), Some(60.0));
    }

    #[test]
    fn counter_regression_rebuilds_baseline() {
        assert_eq!(
            calculate_usage(
                CpuTimes { total: 10, idle: 5 },
                CpuTimes { total: 1, idle: 1 }
            ),
            None
        );
    }

    #[test]
    fn collector_reports_highest_logical_cpu() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(
            temporary.path().join("stat"),
            "cpu0 10 0 10 80 0 0 0 0\ncpu1 10 0 10 80 0 0 0 0\n",
        )
        .unwrap();
        let config: Config = toml::from_str(
            "[[checks]]\nname='cpu'\ntype='cpu'\nwarn_usage_pct=80\ncritical_usage_pct=95",
        )
        .unwrap();
        let check = &config.checks[0];
        let mut context = CollectContext {
            proc_root: Some(temporary.path().into()),
            ..Default::default()
        };
        assert!(matches!(
            collect(check, 80.0, 95.0, &mut context).unwrap().status,
            ObservationStatus::Healthy
        ));
        fs::write(
            temporary.path().join("stat"),
            "cpu0 20 0 20 160 0 0 0 0\ncpu1 110 0 10 80 0 0 0 0\n",
        )
        .unwrap();
        let observation = collect(check, 80.0, 95.0, &mut context).unwrap();
        assert!(matches!(
            observation.status,
            ObservationStatus::Unhealthy(Severity::Critical)
        ));
        assert!(observation.summary.contains("cpu1"));
        assert!(observation.details["每核"].contains("cpu0"));
    }
}
