use crate::{
    config::parse_duration,
    model::{AlertEvent, CheckState, Observation, ObservationStatus, Severity, Transition},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct AlarmPolicy {
    pub pending_for: Duration,
    pub warn_repeat: Duration,
    pub critical_repeat: Duration,
}

impl AlarmPolicy {
    pub fn from_strings(
        pending: &str,
        warn: &str,
        critical: &str,
    ) -> Result<Self, crate::config::ConfigError> {
        Ok(Self {
            pending_for: parse_duration(pending)?,
            warn_repeat: parse_duration(warn)?,
            critical_repeat: parse_duration(critical)?,
        })
    }
}

fn elapsed(now: DateTime<Utc>, since: DateTime<Utc>) -> Duration {
    (now - since).to_std().unwrap_or(Duration::ZERO)
}

pub fn evaluate(
    state: &mut CheckState,
    observation: &Observation,
    policy: &AlarmPolicy,
    runbook: Option<String>,
) -> Option<AlertEvent> {
    match observation.status {
        ObservationStatus::Healthy => resolve(state, observation, runbook),
        ObservationStatus::Unhealthy(severity) => {
            fire_or_repeat(state, observation, severity, policy, runbook)
        }
    }
}

fn resolve(
    state: &mut CheckState,
    observation: &Observation,
    runbook: Option<String>,
) -> Option<AlertEvent> {
    state.pending_since = None;
    let started_at = state.firing_since.take()?;
    let severity = state.severity;
    state.last_sent_at = Some(observation.observed_at);
    state.severity = Severity::Ok;
    Some(event(
        observation,
        severity,
        Transition::Resolved,
        started_at,
        runbook,
    ))
}

fn fire_or_repeat(
    state: &mut CheckState,
    observation: &Observation,
    severity: Severity,
    policy: &AlarmPolicy,
    runbook: Option<String>,
) -> Option<AlertEvent> {
    let now = observation.observed_at;
    if state.firing_since.is_none() {
        let pending = *state.pending_since.get_or_insert(now);
        if elapsed(now, pending) < policy.pending_for {
            return None;
        }
        state.firing_since = Some(pending);
        state.last_sent_at = Some(now);
        state.severity = severity;
        return Some(event(
            observation,
            severity,
            Transition::Firing,
            pending,
            runbook,
        ));
    }
    let started = state.firing_since.unwrap_or(now);
    let escalated = severity > state.severity;
    let repeat = match severity {
        Severity::Critical => policy.critical_repeat,
        _ => policy.warn_repeat,
    };
    let due = state
        .last_sent_at
        .is_none_or(|sent| elapsed(now, sent) >= repeat);
    state.severity = severity;
    if !escalated && !due {
        return None;
    }
    state.last_sent_at = Some(now);
    Some(event(
        observation,
        severity,
        Transition::Repeating,
        started,
        runbook,
    ))
}

fn event(
    observation: &Observation,
    severity: Severity,
    transition: Transition,
    started_at: DateTime<Utc>,
    runbook: Option<String>,
) -> AlertEvent {
    AlertEvent {
        check_name: observation.check_name.clone(),
        severity,
        transition,
        started_at,
        observed_at: observation.observed_at,
        summary: observation.summary.clone(),
        details: observation.details.clone(),
        runbook,
    }
}

pub fn collection_failure_observation(
    check_name: &str,
    failures: u32,
    threshold: u32,
    error: &str,
) -> Observation {
    if failures < threshold {
        return Observation::healthy(&format!("{check_name}/collector"), "采集器暂时失败")
            .detail("连续失败", failures.to_string());
    }
    Observation::unhealthy(
        &format!("{check_name}/collector"),
        Severity::Warn,
        "监控采集盲区",
    )
    .detail("连续失败", failures.to_string())
    .detail("错误", error)
}

pub fn shift_time(observation: &mut Observation, seconds: i64) {
    observation.observed_at += ChronoDuration::seconds(seconds);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> AlarmPolicy {
        AlarmPolicy {
            pending_for: Duration::from_secs(10),
            warn_repeat: Duration::from_secs(30),
            critical_repeat: Duration::from_secs(20),
        }
    }

    #[test]
    fn waits_fires_escalates_repeats_and_recovers() {
        let mut state = CheckState::default();
        let mut observation = Observation::unhealthy("book", Severity::Warn, "stale");
        assert!(evaluate(&mut state, &observation, &policy(), None).is_none());
        shift_time(&mut observation, 10);
        assert_eq!(
            evaluate(&mut state, &observation, &policy(), None)
                .unwrap()
                .transition,
            Transition::Firing
        );
        shift_time(&mut observation, 1);
        observation.status = ObservationStatus::Unhealthy(Severity::Critical);
        assert_eq!(
            evaluate(&mut state, &observation, &policy(), None)
                .unwrap()
                .transition,
            Transition::Repeating
        );
        shift_time(&mut observation, 10);
        assert!(evaluate(&mut state, &observation, &policy(), None).is_none());
        shift_time(&mut observation, 10);
        assert_eq!(
            evaluate(&mut state, &observation, &policy(), None)
                .unwrap()
                .transition,
            Transition::Repeating
        );
        observation.status = ObservationStatus::Healthy;
        assert_eq!(
            evaluate(&mut state, &observation, &policy(), None)
                .unwrap()
                .transition,
            Transition::Resolved
        );
    }
}
