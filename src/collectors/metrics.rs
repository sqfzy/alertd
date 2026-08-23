use crate::model::Severity;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct MetricValue {
    pub key: String,
    pub rendered: String,
    pub severity: Severity,
    violation: Option<ThresholdViolation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThresholdDirection {
    Below,
    Above,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThresholdViolation {
    direction: ThresholdDirection,
    threshold: f64,
}

pub(super) fn evaluate_metric(
    key: &str,
    rendered: String,
    number: f64,
    critical_below: Option<f64>,
    warn_below: Option<f64>,
    warn_above: Option<f64>,
    critical_above: Option<f64>,
) -> MetricValue {
    let (severity, violation) = metric_severity(
        number,
        critical_below,
        warn_below,
        warn_above,
        critical_above,
    );
    MetricValue {
        key: key.into(),
        rendered,
        severity,
        violation,
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
            let violation = value
                .violation
                .expect("non-OK metric has threshold violation");
            format!(
                "{}={}（{} {} {}限 {}）",
                value.key,
                value.rendered,
                comparison(violation.direction),
                value.severity.label(),
                direction_label(violation.direction),
                violation.threshold,
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
    critical_below: Option<f64>,
    warn_below: Option<f64>,
    warn_above: Option<f64>,
    critical_above: Option<f64>,
) -> (Severity, Option<ThresholdViolation>) {
    if let Some(threshold) = critical_below.filter(|threshold| value <= *threshold) {
        return threshold_result(Severity::Critical, ThresholdDirection::Below, threshold);
    }
    if let Some(threshold) = critical_above.filter(|threshold| value >= *threshold) {
        return threshold_result(Severity::Critical, ThresholdDirection::Above, threshold);
    }
    if let Some(threshold) = warn_below.filter(|threshold| value <= *threshold) {
        return threshold_result(Severity::Warn, ThresholdDirection::Below, threshold);
    }
    if let Some(threshold) = warn_above.filter(|threshold| value >= *threshold) {
        return threshold_result(Severity::Warn, ThresholdDirection::Above, threshold);
    }
    (Severity::Ok, None)
}

fn threshold_result(
    severity: Severity,
    direction: ThresholdDirection,
    threshold: f64,
) -> (Severity, Option<ThresholdViolation>) {
    (
        severity,
        Some(ThresholdViolation {
            direction,
            threshold,
        }),
    )
}

fn comparison(direction: ThresholdDirection) -> &'static str {
    match direction {
        ThresholdDirection::Below => "≤",
        ThresholdDirection::Above => "≥",
    }
}

fn direction_label(direction: ThresholdDirection) -> &'static str {
    match direction {
        ThresholdDirection::Below => "下",
        ThresholdDirection::Above => "上",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upper_thresholds_trigger_at_exact_boundaries_and_remain_optional() {
        assert_eq!(
            evaluate_metric(
                "warn",
                "80".into(),
                80.0,
                None,
                None,
                Some(80.0),
                Some(120.0)
            )
            .severity,
            Severity::Warn
        );
        assert_eq!(
            evaluate_metric(
                "critical",
                "120".into(),
                120.0,
                None,
                None,
                Some(80.0),
                Some(120.0)
            )
            .severity,
            Severity::Critical
        );
        assert_eq!(
            evaluate_metric("report", "1".into(), f64::MAX, None, None, None, None).severity,
            Severity::Ok
        );
    }

    #[test]
    fn lower_thresholds_trigger_at_exact_boundaries_and_handle_negatives() {
        let warn = evaluate_metric(
            "temperature",
            "10".into(),
            10.0,
            Some(5.0),
            Some(10.0),
            Some(90.0),
            Some(100.0),
        );
        let critical = evaluate_metric(
            "temperature",
            "-5".into(),
            -5.0,
            Some(-5.0),
            Some(0.0),
            None,
            None,
        );
        assert_eq!(warn.severity, Severity::Warn);
        assert_eq!(critical.severity, Severity::Critical);
        assert_eq!(
            metrics_summary(Severity::Warn, "正常", &[warn]),
            "指标越线：temperature=10（≤ WARN 下限 10）"
        );
        assert_eq!(
            metrics_summary(Severity::Critical, "正常", &[critical]),
            "指标越线：temperature=-5（≤ CRITICAL 下限 -5）"
        );
    }

    #[test]
    fn mixed_directions_report_highest_severity_and_healthy_band() {
        let low = evaluate_metric(
            "temperature",
            "5".into(),
            5.0,
            Some(5.0),
            Some(10.0),
            Some(90.0),
            Some(100.0),
        );
        let high = evaluate_metric(
            "queue",
            "90".into(),
            90.0,
            None,
            None,
            Some(90.0),
            Some(100.0),
        );
        let healthy = evaluate_metric(
            "inside",
            "50".into(),
            50.0,
            Some(5.0),
            Some(10.0),
            Some(90.0),
            Some(100.0),
        );
        assert_eq!(
            highest_severity(&[low.clone(), high.clone(), healthy]),
            Severity::Critical
        );
        assert_eq!(
            metrics_summary(Severity::Critical, "正常", &[low, high]),
            "指标越线：temperature=5（≤ CRITICAL 下限 5），queue=90（≥ WARN 上限 90）"
        );
    }
}
