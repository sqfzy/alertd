use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Ok,
    Warn,
    Critical,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warn => "WARN",
            Self::Critical => "CRITICAL",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationStatus {
    Healthy,
    Unhealthy(Severity),
}

#[derive(Clone, Debug)]
pub struct Observation {
    pub check_name: String,
    pub status: ObservationStatus,
    pub summary: String,
    pub details: BTreeMap<String, String>,
    pub observed_at: DateTime<Utc>,
    pub event: bool,
    pub warn_occurrences: u64,
    pub critical_occurrences: u64,
}

impl Observation {
    pub fn healthy(name: &str, summary: impl Into<String>) -> Self {
        Self {
            check_name: name.into(),
            status: ObservationStatus::Healthy,
            summary: summary.into(),
            details: BTreeMap::new(),
            observed_at: Utc::now(),
            event: false,
            warn_occurrences: 0,
            critical_occurrences: 0,
        }
    }

    pub fn unhealthy(name: &str, severity: Severity, summary: impl Into<String>) -> Self {
        Self {
            check_name: name.into(),
            status: ObservationStatus::Unhealthy(severity),
            summary: summary.into(),
            details: BTreeMap::new(),
            observed_at: Utc::now(),
            event: false,
            warn_occurrences: 0,
            critical_occurrences: 0,
        }
    }

    pub fn detail(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    pub fn event_counts(mut self, warn: u64, critical: u64) -> Self {
        self.event = true;
        self.warn_occurrences = warn;
        self.critical_occurrences = critical;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    Firing,
    Repeating,
    Resolved,
    Event,
}

#[derive(Clone, Debug)]
pub struct AlertEvent {
    pub check_name: String,
    pub severity: Severity,
    pub transition: Transition,
    pub started_at: DateTime<Utc>,
    pub observed_at: DateTime<Utc>,
    pub summary: String,
    pub details: BTreeMap<String, String>,
    pub runbook: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CheckState {
    pub pending_since: Option<DateTime<Utc>>,
    pub firing_since: Option<DateTime<Utc>>,
    pub last_sent_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recovering_since: Option<DateTime<Utc>>,
    pub severity: Severity,
    pub collection_failures: u32,
    #[serde(default)]
    pub event_window_count: u64,
    #[serde(default)]
    pub event_severity: Severity,
    #[serde(default)]
    pub daily_warn_count: u64,
    #[serde(default)]
    pub daily_critical_count: u64,
}
