//! systemd 查询在后台运行；主采集循环只消费本模块的内存缓存。
use super::{CollectError, command};
use crate::{
    config::{self, CheckConfig, CheckKind, Config},
    model::{Observation, Severity},
};
use chrono::{DateTime, Utc};
use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

#[derive(Clone)]
struct Cache {
    success: Option<DateTime<Utc>>,
    states: Vec<(String, String, String, String)>,
    error: Option<String>,
}
pub struct Workers {
    cache: Arc<RwLock<HashMap<String, Cache>>>,
    stops: HashMap<String, Arc<AtomicBool>>,
    joins: Vec<thread::JoinHandle<()>>,
    timeout: Duration,
}
pub struct Read {
    pub service: Option<Observation>,
    pub collector: Observation,
    pub daily: Observation,
}
fn settings(check: &CheckConfig, c: &Config) -> Option<(Vec<String>, Duration, Duration)> {
    let CheckKind::Systemd {
        units,
        probe_interval,
        stale_after,
    } = &check.kind
    else {
        return None;
    };
    Some((
        units.clone(),
        config::parse_duration(
            probe_interval
                .as_deref()
                .unwrap_or(&c.runtime.systemd_probe_interval),
        )
        .unwrap(),
        config::parse_duration(
            stale_after
                .as_deref()
                .unwrap_or(&c.runtime.systemd_stale_after),
        )
        .unwrap(),
    ))
}
impl Workers {
    pub fn new(c: &Config) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            stops: HashMap::new(),
            joins: Vec::new(),
            timeout: config::parse_duration(&c.runtime.command_timeout).unwrap(),
        }
    }
    pub fn reconcile(&mut self, c: &Config, enabled: bool) {
        let wanted: HashMap<_, _> = if enabled {
            c.checks
                .iter()
                .filter(|x| x.enabled)
                .filter_map(|x| settings(x, c).map(|v| (x.name.clone(), v)))
                .collect()
        } else {
            HashMap::new()
        };
        let removed: Vec<_> = self
            .stops
            .keys()
            .filter(|n| !wanted.contains_key(*n))
            .cloned()
            .collect();
        for n in removed {
            self.stops
                .remove(&n)
                .unwrap()
                .store(true, Ordering::Relaxed);
            self.cache.write().unwrap().remove(&n);
        }
        for (n, (units, period, _)) in wanted {
            if self.stops.contains_key(&n) {
                continue;
            }
            let stop = Arc::new(AtomicBool::new(false));
            self.stops.insert(n.clone(), stop.clone());
            self.cache.write().unwrap().insert(
                n.clone(),
                Cache {
                    success: None,
                    states: vec![],
                    error: None,
                },
            );
            let cache = self.cache.clone();
            let timeout = self.timeout;
            self.joins.push(thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match probe(&units, timeout, &stop) {
                        Ok(states) => {
                            let mut lock = cache.write().unwrap();
                            if let Some(x) = lock.get_mut(&n) {
                                x.success = Some(Utc::now());
                                x.states = states;
                                x.error = None
                            }
                        }
                        Err(CollectError::Cancelled) => return,
                        Err(e) => {
                            if let Some(x) = cache.write().unwrap().get_mut(&n) {
                                x.error = Some(e.to_string())
                            };
                            tracing::warn!(check=%n,error=%e,"systemd worker probe failed")
                        }
                    }
                    let end = std::time::Instant::now() + period;
                    while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < end {
                        thread::sleep(Duration::from_millis(100))
                    }
                }
            }));
        }
    }
    pub fn clear(&mut self) {
        for s in self.stops.values() {
            s.store(true, Ordering::Relaxed)
        }
        self.stops.clear();
        self.cache.write().unwrap().clear()
    }
    pub fn shutdown(&mut self) {
        self.clear();
        for j in self.joins.drain(..) {
            let _ = j.join();
        }
    }
    pub fn read(&self, check: &CheckConfig, c: &Config) -> Read {
        let (units, _, stale) = settings(check, c).unwrap();
        let entry = self.cache.read().unwrap().get(&check.name).cloned();
        let fresh = entry
            .as_ref()
            .and_then(|x| x.success)
            .is_some_and(|t| (Utc::now() - t).to_std().unwrap_or_default() <= stale);
        if fresh {
            let x = entry.unwrap();
            let bad: Vec<_> = x
                .states
                .iter()
                .filter(|(_, l, a, _)| l != "loaded" || a != "active")
                .map(|(n, l, a, s)| format!("{n}={l}/{a}/{s}"))
                .collect();
            let mut o = if bad.is_empty() {
                Observation::healthy(
                    &check.name,
                    format!("systemd units active {}/{}", x.states.len(), units.len()),
                )
            } else {
                Observation::unhealthy(
                    &check.name,
                    check.severity,
                    format!("systemd units 异常 {}/{}", bad.len(), units.len()),
                )
                .detail("异常", bad.join(", "))
            };
            o = o.detail("units", units.join(", "));
            if let Some(e) = x.error {
                o = o
                    .detail(
                        "采集状态",
                        format!(
                            "degraded：沿用 {} 秒前状态",
                            (Utc::now() - x.success.unwrap()).num_seconds()
                        ),
                    )
                    .detail("最近采集错误", e)
            }
            return Read {
                service: Some(o.clone()),
                daily: o,
                collector: Observation::healthy(
                    &format!("{}/collector", check.name),
                    "systemd 状态采集正常",
                ),
            };
        }
        let reason = entry
            .and_then(|x| x.error)
            .unwrap_or_else(|| "等待首个 systemd 状态快照".into());
        Read {
            service: None,
            daily: Observation::healthy(&check.name, "systemd 状态过期")
                .detail("_systemd_cache", "stale")
                .detail("采集状态", reason.clone()),
            collector: Observation::unhealthy(
                &format!("{}/collector", check.name),
                Severity::Warn,
                "systemd 状态过期/监控盲区",
            )
            .detail("原因", reason),
        }
    }
}
fn probe(
    units: &[String],
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<Vec<(String, String, String, String)>, CollectError> {
    let mut a = vec!["show"];
    a.extend(units.iter().map(String::as_str));
    a.push("--property=Id,LoadState,ActiveState,SubState");
    let o = command::run_cancellable("systemctl", &a, timeout, stop)?;
    if !o.status.success() {
        return Err(CollectError::Invalid(format!(
            "systemctl show exited {}",
            o.status
        )));
    }
    let mut out = Vec::new();
    let mut row = (String::new(), String::new(), String::new(), String::new());
    for l in String::from_utf8_lossy(&o.stdout)
        .lines()
        .chain(std::iter::once(""))
    {
        if l.is_empty() {
            if !row.0.is_empty() {
                out.push(row);
                row = (String::new(), String::new(), String::new(), String::new())
            }
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            return Err(CollectError::Invalid("malformed systemctl output".into()));
        };
        match k {
            "Id" => row.0 = v.into(),
            "LoadState" => row.1 = v.into(),
            "ActiveState" => row.2 = v.into(),
            "SubState" => row.3 = v.into(),
            _ => {}
        }
    }
    if out.len() != units.len() {
        return Err(CollectError::Invalid(format!(
            "systemctl returned {}/{} units",
            out.len(),
            units.len()
        )));
    }
    Ok(out)
}
