use crate::{
    config::parse_duration,
    model::{AlertEvent, CheckState, Observation, ObservationStatus, Severity, Transition},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct AlarmPolicy {
    pub pending_for: Duration,
    pub recover_for: Duration,
    pub warn_repeat: Duration,
    pub critical_repeat: Duration,
}

impl AlarmPolicy {
    pub fn from_strings(
        pending: &str,
        recover: &str,
        warn: &str,
        critical: &str,
    ) -> Result<Self, crate::config::ConfigError> {
        Ok(Self {
            pending_for: parse_duration(pending)?,
            recover_for: parse_duration(recover)?,
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
    if observation.event {
        return evaluate_event(state, observation, policy, runbook);
    }
    match observation.status {
        ObservationStatus::Healthy => resolve(state, observation, policy, runbook),
        ObservationStatus::Unhealthy(severity) => {
            fire_or_repeat(state, observation, severity, policy, runbook)
        }
    }
}

fn resolve(
    state: &mut CheckState,
    observation: &Observation,
    policy: &AlarmPolicy,
    runbook: Option<String>,
) -> Option<AlertEvent> {
    state.pending_since = None;
    let started_at = state.firing_since?;
    let recovering = *state
        .recovering_since
        .get_or_insert(observation.observed_at);
    if elapsed(observation.observed_at, recovering) < policy.recover_for {
        return None;
    }
    state.firing_since = None;
    state.recovering_since = None;
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
    state.recovering_since = None;
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

fn evaluate_event(
    state: &mut CheckState,
    observation: &Observation,
    policy: &AlarmPolicy,
    runbook: Option<String>,
) -> Option<AlertEvent> {
    state.daily_warn_count = state
        .daily_warn_count
        .saturating_add(observation.warn_occurrences);
    state.daily_critical_count = state
        .daily_critical_count
        .saturating_add(observation.critical_occurrences);
    let ObservationStatus::Unhealthy(severity) = observation.status else {
        return None;
    };
    let occurrences = observation
        .warn_occurrences
        .saturating_add(observation.critical_occurrences)
        .max(1);
    state.event_window_count = state.event_window_count.saturating_add(occurrences);
    let repeat = match severity {
        Severity::Critical => policy.critical_repeat,
        _ => policy.warn_repeat,
    };
    let escalated = severity > state.event_severity;
    let due = state
        .last_sent_at
        .is_none_or(|sent| elapsed(observation.observed_at, sent) >= repeat);
    if !escalated && !due {
        return None;
    }
    let mut event = event(
        observation,
        severity,
        Transition::Event,
        observation.observed_at,
        runbook,
    );
    event
        .details
        .insert("窗口累计".into(), state.event_window_count.to_string());
    state.event_window_count = 0;
    state.last_sent_at = Some(observation.observed_at);
    state.event_severity = severity;
    Some(event)
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
            recover_for: Duration::from_secs(10),
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
        assert!(evaluate(&mut state, &observation, &policy(), None).is_none());
        shift_time(&mut observation, 10);
        assert_eq!(
            evaluate(&mut state, &observation, &policy(), None)
                .unwrap()
                .transition,
            Transition::Resolved
        );
    }

    #[test]
    fn event_checks_aggregate_without_resolving() {
        let mut state = CheckState::default();
        let mut event =
            Observation::unhealthy("journal", Severity::Warn, "WARN").event_counts(2, 0);
        let first = evaluate(&mut state, &event, &policy(), None).unwrap();
        assert_eq!(first.transition, Transition::Event);
        assert_eq!(first.details["窗口累计"], "2");

        shift_time(&mut event, 1);
        assert!(evaluate(&mut state, &event, &policy(), None).is_none());
        let mut empty = Observation::healthy("journal", "no new lines").event_counts(0, 0);
        shift_time(&mut empty, 1);
        assert!(evaluate(&mut state, &empty, &policy(), None).is_none());
        assert!(state.firing_since.is_none());
        assert_eq!(state.severity, Severity::Ok);
        assert_eq!(state.event_severity, Severity::Warn);
        assert_eq!(state.daily_warn_count, 4);

        shift_time(&mut event, 30);
        let repeated = evaluate(&mut state, &event, &policy(), None).unwrap();
        assert_eq!(repeated.details["窗口累计"], "4");
    }
}
