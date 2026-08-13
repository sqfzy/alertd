use super::CollectError;
use crate::{
    config::CheckConfig,
    model::{Observation, Severity},
};
use std::{ffi::CString, path::Path};

struct DiskUsage {
    total: u64,
    available: u64,
    inodes: Option<(u64, u64)>,
}

fn to_u64<T: TryInto<u64>>(value: T) -> u64 {
    value.try_into().ok().unwrap_or_default()
}

fn classify(
    used_pct: f64,
    inode_used_pct: Option<f64>,
    warn: f64,
    critical: f64,
    inode_warn: f64,
    inode_critical: f64,
) -> Option<Severity> {
    if used_pct >= critical || inode_used_pct.is_some_and(|value| value >= inode_critical) {
        Some(Severity::Critical)
    } else if used_pct >= warn || inode_used_pct.is_some_and(|value| value >= inode_warn) {
        Some(Severity::Warn)
    } else {
        None
    }
}

#[cfg(unix)]
fn usage(path: &Path) -> Result<DiskUsage, CollectError> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| CollectError::Invalid("mount contains NUL".into()))?;
    let mut data = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), data.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let data = unsafe { data.assume_init() };
    let total = to_u64(data.f_blocks).saturating_mul(to_u64(data.f_frsize));
    let available = to_u64(data.f_bavail).saturating_mul(to_u64(data.f_frsize));
    let inode_total = to_u64(data.f_files);
    let inodes = (inode_total > 0).then_some((inode_total, to_u64(data.f_favail)));
    Ok(DiskUsage {
        total,
        available,
        inodes,
    })
}

pub fn collect(
    check: &CheckConfig,
    mount: &Path,
    warn: f64,
    critical: f64,
    inode_warn: f64,
    inode_critical: f64,
) -> Result<Observation, CollectError> {
    let usage = usage(mount)?;
    let (total, available, inodes) = (usage.total, usage.available, usage.inodes);
    if total == 0 {
        return Err(CollectError::Invalid("filesystem has zero blocks".into()));
    }
    let used_pct = (total - available) as f64 * 100.0 / total as f64;
    let inode_used_pct = inodes
        .map(|(total, available)| total.saturating_sub(available) as f64 * 100.0 / total as f64);
    let status = classify(
        used_pct,
        inode_used_pct,
        warn,
        critical,
        inode_warn,
        inode_critical,
    );
    let inode_summary = inode_used_pct.map_or_else(|| "N/A".into(), |value| format!("{value:.1}%"));
    let summary = format!("磁盘已用 {used_pct:.1}%，inode {inode_summary}");
    let observation = status.map_or_else(
        || Observation::healthy(&check.name, &summary),
        |severity| Observation::unhealthy(&check.name, severity, &summary),
    );
    Ok(observation
        .detail("挂载点", mount.display().to_string())
        .detail("剩余", format!("{} MiB", available / 1024 / 1024))
        .detail("inode 已用", inode_summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_and_inode_compete_at_boundaries() {
        assert_eq!(
            classify(80.0, Some(20.0), 80.0, 90.0, 80.0, 90.0),
            Some(Severity::Warn)
        );
        assert_eq!(
            classify(10.0, Some(90.0), 80.0, 90.0, 80.0, 90.0),
            Some(Severity::Critical)
        );
        assert_eq!(classify(10.0, None, 80.0, 90.0, 80.0, 90.0), None);
    }
}
