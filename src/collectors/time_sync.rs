use super::{CollectError, command};
use crate::{
    config::{CheckConfig, parse_duration},
    model::{Observation, Severity},
};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
struct Tracking {
    offset_seconds: f64,
    leap_status: String,
}

fn parse_tracking(text: &str) -> Result<Tracking, CollectError> {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| CollectError::Invalid("chronyc tracking output is empty".into()))?;
    let fields: Vec<_> = line.split(',').map(str::trim).collect();
    if fields.len() < 14 {
        return Err(CollectError::Invalid(format!(
            "chronyc tracking CSV has {} fields, expected at least 14",
            fields.len()
        )));
    }
    let offset_seconds = fields[4]
        .parse::<f64>()
        .map_err(|_| CollectError::Invalid("chronyc System time is invalid".into()))?;
    if !offset_seconds.is_finite() {
        return Err(CollectError::Invalid(
            "chronyc System time is not finite".into(),
        ));
    }
    Ok(Tracking {
        offset_seconds,
        leap_status: fields[13].into(),
    })
}

fn leap_is_normal(value: &str) -> bool {
    value.eq_ignore_ascii_case("normal") || value == "0"
}

fn classify(tracking: &Tracking, warn: Duration, critical: Duration) -> Option<Severity> {
    let offset = Duration::from_secs_f64(tracking.offset_seconds.abs());
    if !leap_is_normal(&tracking.leap_status) || offset >= critical {
        Some(Severity::Critical)
    } else if offset >= warn {
        Some(Severity::Warn)
    } else {
        None
    }
}

pub fn collect(
    check: &CheckConfig,
    warn_offset: &str,
    critical_offset: &str,
    command_timeout: Duration,
) -> Result<Observation, CollectError> {
    let output = command::run("chronyc", &["-c", "tracking"], command_timeout)?;
    if !output.status.success() {
        return Err(CollectError::Invalid(format!(
            "chronyc tracking exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let tracking = parse_tracking(&String::from_utf8_lossy(&output.stdout))?;
    let warn =
        parse_duration(warn_offset).map_err(|error| CollectError::Invalid(error.to_string()))?;
    let critical = parse_duration(critical_offset)
        .map_err(|error| CollectError::Invalid(error.to_string()))?;
    let severity = classify(&tracking, warn, critical);
    let offset_ms = tracking.offset_seconds * 1000.0;
    let summary = format!("时钟偏差 {offset_ms:+.3} ms，Leap {}", tracking.leap_status);
    let observation = severity.map_or_else(
        || Observation::healthy(&check.name, &summary),
        |value| Observation::unhealthy(&check.name, value, &summary),
    );
    Ok(observation
        .detail("偏差", format!("{offset_ms:+.3} ms"))
        .detail("Leap", tracking.leap_status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_positive_and_negative_csv_offsets() {
        let prefix = "A9FEA97B,169.254.169.123,4,1786617046.727857844,";
        let suffix =
            ",0.000000704,0.000001603,8.643,0.001,0.034,0.000308092,0.000232773,16.0,Normal\n";
        assert_eq!(
            parse_tracking(&format!("{prefix}-0.005{suffix}"))
                .unwrap()
                .offset_seconds,
            -0.005
        );
        assert!(leap_is_normal("Normal"));
        assert!(!leap_is_normal("Not synchronised"));
    }

    #[test]
    fn classifies_offset_boundaries_and_unsynchronised_state() {
        let tracking = Tracking {
            offset_seconds: -0.001,
            leap_status: "Normal".into(),
        };
        assert_eq!(
            classify(
                &tracking,
                Duration::from_millis(1),
                Duration::from_millis(5)
            ),
            Some(Severity::Warn)
        );
        let unsynchronised = Tracking {
            offset_seconds: 0.0,
            leap_status: "Not synchronised".into(),
        };
        assert_eq!(
            classify(
                &unsynchronised,
                Duration::from_millis(1),
                Duration::from_millis(5)
            ),
            Some(Severity::Critical)
        );
    }
}
