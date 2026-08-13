#[cfg(target_os = "linux")]
use super::command;
use super::{CollectContext, CollectError};
#[cfg(target_os = "linux")]
use crate::model::Severity;
use crate::{config::CheckConfig, model::Observation};
#[cfg(any(target_os = "linux", test))]
use std::collections::BTreeSet;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
const BASELINE_SOURCE_SHA256: &str =
    "27c6096d9b907b8207a5d440cce9c6c6ffce63d90a27ea37fc53870261377da8";

#[cfg(any(target_os = "linux", test))]
fn parse_cpu_list(value: &str) -> Result<BTreeSet<u32>, CollectError> {
    let mut cpus = BTreeSet::new();
    for part in value.trim().split(',').filter(|part| !part.is_empty()) {
        if let Some((start, end)) = part.split_once('-') {
            let start: u32 = start
                .parse()
                .map_err(|_| CollectError::Invalid(format!("invalid CPU list {value:?}")))?;
            let end: u32 = end
                .parse()
                .map_err(|_| CollectError::Invalid(format!("invalid CPU list {value:?}")))?;
            if start > end || end > 65_535 {
                return Err(CollectError::Invalid(format!("invalid CPU range {part:?}")));
            }
            cpus.extend(start..=end);
        } else {
            cpus.insert(
                part.parse()
                    .map_err(|_| CollectError::Invalid(format!("invalid CPU list {value:?}")))?,
            );
        }
    }
    Ok(cpus)
}

#[cfg(any(target_os = "linux", test))]
fn cmdline_value<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline.split_whitespace().find_map(|argument| {
        let (name, value) = argument.split_once('=')?;
        (name == key).then_some(value)
    })
}

#[cfg(any(target_os = "linux", test))]
fn has_flag(cmdline: &str, flag: &str) -> bool {
    cmdline.split_whitespace().any(|argument| argument == flag)
}

#[cfg(any(target_os = "linux", test))]
fn kernel_issues(present: &BTreeSet<u32>, cmdline: &str) -> Vec<String> {
    let expected: BTreeSet<_> = present.iter().copied().filter(|cpu| *cpu != 0).collect();
    let mut issues = Vec::new();
    for key in ["isolcpus", "nohz_full", "rcu_nocbs"] {
        let matches = cmdline_value(cmdline, key)
            .and_then(|value| parse_cpu_list(value).ok())
            .is_some_and(|actual| actual == expected);
        if !matches {
            issues.push(format!("{key} 应为隔离核集合"));
        }
    }
    for (key, expected_value) in [("irqaffinity", "0"), ("mitigations", "off")] {
        if cmdline_value(cmdline, key) != Some(expected_value) {
            issues.push(format!("{key}={expected_value} 缺失"));
        }
    }
    for flag in ["rcu_nocb_poll", "nowatchdog", "nosoftlockup"] {
        if !has_flag(cmdline, flag) {
            issues.push(format!("{flag} 缺失"));
        }
    }
    issues
}

#[cfg(target_os = "linux")]
fn default_route_interface(proc_root: &Path) -> Result<Option<String>, CollectError> {
    let text = fs::read_to_string(proc_root.join("net/route"))?;
    Ok(text.lines().skip(1).find_map(|line| {
        let fields: Vec<_> = line.split_whitespace().collect();
        (fields.get(1) == Some(&"00000000")).then(|| fields[0].to_string())
    }))
}

#[cfg(target_os = "linux")]
fn is_virtual_name(name: &str) -> bool {
    name == "lo"
        || ["docker", "veth", "br-", "virbr", "tun", "tap"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

#[cfg(target_os = "linux")]
fn data_interfaces(sys_root: &Path, default: Option<&str>) -> Result<Vec<PathBuf>, CollectError> {
    let net_root = sys_root.join("class/net");
    let mut interfaces = Vec::new();
    for entry in fs::read_dir(&net_root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if default == Some(name.as_str()) || is_virtual_name(&name) {
            continue;
        }
        let path = entry.path();
        if path.join("device").exists() {
            interfaces.push(path);
        }
    }
    interfaces.sort();
    Ok(interfaces)
}

#[cfg(any(target_os = "linux", test))]
fn mask_is_cpu0(value: &str) -> bool {
    let normalized = value.trim().replace(',', "");
    let normalized = normalized.trim_start_matches('0');
    normalized == "1"
}

#[cfg(target_os = "linux")]
fn check_nic(
    interface: &Path,
    proc_root: &Path,
    issues: &mut Vec<String>,
) -> Result<(), CollectError> {
    let name = interface
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("?");
    let irq_root = interface.join("device/msi_irqs");
    if irq_root.is_dir() {
        for entry in fs::read_dir(&irq_root)? {
            let irq = entry?.file_name().to_string_lossy().into_owned();
            let affinity = proc_root.join("irq").join(&irq).join("smp_affinity_list");
            let value = fs::read_to_string(&affinity)?;
            if parse_cpu_list(&value)? != BTreeSet::from([0]) {
                issues.push(format!("{name} IRQ {irq} affinity={}", value.trim()));
            }
        }
    }
    let queues = interface.join("queues");
    if !queues.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(queues)? {
        let entry = entry?;
        let queue = entry.file_name().to_string_lossy().into_owned();
        let setting = if queue.starts_with("tx-") {
            Some("xps_cpus")
        } else if queue.starts_with("rx-") {
            Some("rps_cpus")
        } else {
            None
        };
        let Some(setting) = setting else { continue };
        let path = entry.path().join(setting);
        if path.exists() {
            let value = fs::read_to_string(&path)?;
            if !mask_is_cpu0(&value) {
                issues.push(format!("{name} {queue}/{setting}={}", value.trim()));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn collect(
    check: &CheckConfig,
    command_timeout: Duration,
    context: &CollectContext,
) -> Result<Observation, CollectError> {
    let proc_root = context.proc_root.as_deref().unwrap_or(Path::new("/proc"));
    let sys_root = context.sys_root.as_deref().unwrap_or(Path::new("/sys"));
    let present = parse_cpu_list(&fs::read_to_string(
        sys_root.join("devices/system/cpu/present"),
    )?)?;
    if present.len() < 2 || !present.contains(&0) {
        return Err(CollectError::Invalid(
            "system_tuning requires CPU0 and at least one isolated CPU".into(),
        ));
    }
    let cmdline = fs::read_to_string(proc_root.join("cmdline"))?;
    let kernel = kernel_issues(&present, &cmdline);
    let rt = fs::read_to_string(proc_root.join("sys/kernel/sched_rt_runtime_us"))?;
    let mut scheduler = Vec::new();
    if rt.trim() != "-1" {
        scheduler.push(format!("kernel.sched_rt_runtime_us={}", rt.trim()));
    }
    let output = command::run(
        "systemctl",
        &["is-active", "irqbalance.service"],
        command_timeout,
    )?;
    let mut irqbalance = Vec::new();
    if output.status.success() {
        irqbalance.push("irqbalance 仍为 active".into());
    } else if !matches!(output.status.code(), Some(3) | Some(4)) {
        return Err(CollectError::Invalid(format!(
            "systemctl is-active irqbalance exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let default = default_route_interface(proc_root)?;
    let interfaces = data_interfaces(sys_root, default.as_deref())?;
    let mut nic = Vec::new();
    for interface in &interfaces {
        check_nic(interface, proc_root, &mut nic)?;
    }
    let mut all = Vec::new();
    for (group, values) in [
        ("内核参数", &kernel),
        ("RT", &scheduler),
        ("irqbalance", &irqbalance),
        ("NIC", &nic),
    ] {
        if !values.is_empty() {
            all.push(format!("{group}: {}", values.join("；")));
        }
    }
    let mut observation = if all.is_empty() {
        Observation::healthy(&check.name, "低延迟运行态基线符合")
    } else {
        Observation::unhealthy(
            &check.name,
            Severity::Critical,
            format!("低延迟基线偏差 {} 项", all.len()),
        )
    };
    observation = observation
        .detail(
            "数据口",
            interfaces
                .iter()
                .filter_map(|path| path.file_name()?.to_str())
                .collect::<Vec<_>>()
                .join(", "),
        )
        .detail("安全权衡", "mitigations=off 关闭 CPU 漏洞缓解");
    observation = observation.detail("基线脚本 SHA-256", BASELINE_SOURCE_SHA256);
    if !all.is_empty() {
        observation = observation.detail("偏差", all.join("\n"));
    }
    Ok(observation)
}

#[cfg(not(target_os = "linux"))]
pub fn collect(
    _check: &CheckConfig,
    _command_timeout: Duration,
    _context: &CollectContext,
) -> Result<Observation, CollectError> {
    Err(CollectError::Unsupported(
        "system_tuning requires Linux".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_equivalent_cpu_sets_and_masks() {
        assert_eq!(
            parse_cpu_list("0-2,4").unwrap(),
            BTreeSet::from([0, 1, 2, 4])
        );
        assert!(mask_is_cpu0("00000000,00000001\n"));
        assert!(!mask_is_cpu0("3"));
    }

    #[test]
    fn recognizes_exact_cmdline_keys() {
        let cmdline = "isolcpus=1-3 rcu_nocb_poll mitigations=off";
        assert_eq!(cmdline_value(cmdline, "isolcpus"), Some("1-3"));
        assert!(has_flag(cmdline, "rcu_nocb_poll"));
        assert!(!has_flag(cmdline, "nowatchdog"));
    }

    #[test]
    fn validates_complete_kernel_baseline_and_reports_missing_items() {
        let present = BTreeSet::from([0, 1, 2, 3]);
        let complete = "isolcpus=1-3 nohz_full=1,2,3 rcu_nocbs=1-3 rcu_nocb_poll irqaffinity=0 mitigations=off nowatchdog nosoftlockup";
        assert!(kernel_issues(&present, complete).is_empty());
        for argument in complete.split_whitespace() {
            let incomplete = complete.replace(argument, "");
            assert!(
                !kernel_issues(&present, &incomplete).is_empty(),
                "{argument}"
            );
        }
    }
}
