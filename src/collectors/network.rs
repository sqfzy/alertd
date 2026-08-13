use super::{CollectContext, CollectError};
use crate::{
    config::CheckConfig,
    model::{Observation, Severity},
};
use chrono::{DateTime, Utc};
use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counters {
    errors: u64,
    drops: u64,
}

#[derive(Clone, Debug)]
pub struct NetworkSample {
    counters: Counters,
    observed_at: DateTime<Utc>,
}

fn read_number(path: &Path) -> Result<u64, CollectError> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| CollectError::Invalid(format!("invalid counter {}", path.display())))
}

fn read_counters(root: &Path) -> Result<Counters, CollectError> {
    let statistics = root.join("statistics");
    Ok(Counters {
        errors: read_number(&statistics.join("rx_errors"))?
            .saturating_add(read_number(&statistics.join("tx_errors"))?),
        drops: read_number(&statistics.join("rx_dropped"))?
            .saturating_add(read_number(&statistics.join("tx_dropped"))?),
    })
}

fn counter_rate(previous: u64, current: u64, seconds: f64) -> f64 {
    current
        .checked_sub(previous)
        .map_or(0.0, |delta| delta as f64 / seconds.max(0.001))
}

fn reached(value: f64, threshold: f64) -> bool {
    if threshold == 0.0 {
        value > 0.0
    } else {
        value >= threshold
    }
}

#[allow(clippy::too_many_arguments)]
pub fn collect(
    check: &CheckConfig,
    interfaces: &[String],
    warn_errors: f64,
    critical_errors: f64,
    warn_drops: f64,
    critical_drops: f64,
    context: &mut CollectContext,
) -> Result<Observation, CollectError> {
    let sys_root = context.sys_root.as_deref().unwrap_or(Path::new("/sys"));
    let now = Utc::now();
    let mut severity = Severity::Ok;
    let mut details = Vec::new();
    for interface in interfaces {
        let root = sys_root.join("class/net").join(interface);
        if !root.exists() {
            return Err(CollectError::Invalid(format!(
                "interface {interface} is missing"
            )));
        }
        let operstate = fs::read_to_string(root.join("operstate"))?
            .trim()
            .to_string();
        let carrier = fs::read_to_string(root.join("carrier"))?.trim().to_string();
        let counters = read_counters(&root)?;
        let key = format!("{}:{interface}", check.name);
        let previous = context.network_samples.insert(
            key,
            NetworkSample {
                counters,
                observed_at: now,
            },
        );
        let (errors, drops) = previous.map_or((0.0, 0.0), |sample| {
            let seconds = (now - sample.observed_at)
                .to_std()
                .unwrap_or_default()
                .as_secs_f64();
            (
                counter_rate(sample.counters.errors, counters.errors, seconds),
                counter_rate(sample.counters.drops, counters.drops, seconds),
            )
        });
        let current = if operstate != "up"
            || carrier != "1"
            || reached(errors, critical_errors)
            || reached(drops, critical_drops)
        {
            Severity::Critical
        } else if reached(errors, warn_errors) || reached(drops, warn_drops) {
            Severity::Warn
        } else {
            Severity::Ok
        };
        severity = severity.max(current);
        details.push(format!(
            "{interface} {operstate}/carrier={carrier} errors={errors:.2}/s drops={drops:.2}/s"
        ));
    }
    let summary = if severity == Severity::Ok {
        format!("网络接口正常 {}/{}", interfaces.len(), interfaces.len())
    } else {
        "网络链路或计数器异常".into()
    };
    let observation = if severity == Severity::Ok {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, severity, summary)
    };
    Ok(observation.detail("接口", details.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, model::ObservationStatus};
    use chrono::Duration as ChronoDuration;

    #[test]
    fn calculates_rates_and_resets_on_counter_regression() {
        assert_eq!(counter_rate(10, 20, 2.0), 5.0);
        assert_eq!(counter_rate(20, 10, 2.0), 0.0);
    }

    #[test]
    fn reports_link_down_and_counter_rates() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("class/net/ens5");
        fs::create_dir_all(root.join("statistics")).unwrap();
        fs::write(root.join("operstate"), "down\n").unwrap();
        fs::write(root.join("carrier"), "0\n").unwrap();
        for name in ["rx_errors", "tx_errors", "rx_dropped", "tx_dropped"] {
            fs::write(root.join("statistics").join(name), "0\n").unwrap();
        }
        let config: Config = toml::from_str("[[checks]]\nname='net'\ntype='network'\ninterfaces=['ens5']\nwarn_errors_per_second=1\ncritical_errors_per_second=10\nwarn_drops_per_second=100\ncritical_drops_per_second=1000").unwrap();
        let mut context = CollectContext {
            sys_root: Some(temporary.path().into()),
            ..Default::default()
        };
        let down = collect(
            &config.checks[0],
            &["ens5".into()],
            1.0,
            10.0,
            100.0,
            1000.0,
            &mut context,
        )
        .unwrap();
        assert!(matches!(
            down.status,
            ObservationStatus::Unhealthy(Severity::Critical)
        ));

        fs::write(root.join("operstate"), "up\n").unwrap();
        fs::write(root.join("carrier"), "1\n").unwrap();
        context.network_samples.insert(
            "net:ens5".into(),
            NetworkSample {
                counters: Counters::default(),
                observed_at: Utc::now() - ChronoDuration::seconds(1),
            },
        );
        fs::write(root.join("statistics/rx_errors"), "10\n").unwrap();
        let rates = collect(
            &config.checks[0],
            &["ens5".into()],
            1.0,
            10.0,
            100.0,
            1000.0,
            &mut context,
        )
        .unwrap();
        assert!(matches!(rates.status, ObservationStatus::Unhealthy(_)));
    }
}
