use crate::model::Severity;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct MetricValue {
    pub key: String,
    pub rendered: String,
    pub severity: Severity,
    pub threshold: Option<f64>,
}

pub(super) fn evaluate_metric(
    key: &str,
    rendered: String,
    number: f64,
    warn_above: Option<f64>,
    critical_above: Option<f64>,
) -> MetricValue {
    let (severity, threshold) = metric_severity(number, warn_above, critical_above);
    MetricValue {
        key: key.into(),
        rendered,
        severity,
        threshold,
    }
}

pub(super) fn highest_severity(values: &[MetricValue]) -> Severity {
    values
        .iter()
        .map(|value| value.severity)
        .max()
        .unwrap_or_default()
}

pub(super) fn metrics_summary(severity: Severity, healthy: &str, values: &[MetricValue]) -> String {
    if severity == Severity::Ok {
        return healthy.into();
    }
    let exceeded = values
        .iter()
        .filter(|value| value.severity != Severity::Ok)
        .map(|value| {
            format!(
                "{}={}（阈值 {}）",
                value.key,
                value.rendered,
                value.threshold.expect("non-OK metric has threshold")
            )
        })
        .collect::<Vec<_>>()
        .join("，");
    format!("指标越线：{exceeded}")
}

pub(super) fn render_metrics(values: &[MetricValue]) -> String {
    values
        .iter()
        .map(|value| format!("{}={}", value.key, value.rendered))
        .collect::<Vec<_>>()
        .join("\n")
}

fn metric_severity(
    value: f64,
    warn_above: Option<f64>,
    critical_above: Option<f64>,
) -> (Severity, Option<f64>) {
    if critical_above.is_some_and(|threshold| value >= threshold) {
        return (Severity::Critical, critical_above);
    }
    if warn_above.is_some_and(|threshold| value >= threshold) {
        return (Severity::Warn, warn_above);
    }
    (Severity::Ok, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_trigger_at_exact_boundaries_and_remain_optional() {
        assert_eq!(
            evaluate_metric("warn", "80".into(), 80.0, Some(80.0), Some(120.0)).severity,
            Severity::Warn
        );
        assert_eq!(
            evaluate_metric("critical", "120".into(), 120.0, Some(80.0), Some(120.0)).severity,
            Severity::Critical
        );
        assert_eq!(
            evaluate_metric("report", "1".into(), f64::MAX, None, None).severity,
            Severity::Ok
        );
    }
}
