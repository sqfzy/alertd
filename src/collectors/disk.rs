use super::CollectError;
use crate::{
    config::CheckConfig,
    model::{Observation, Severity},
};
use std::{ffi::CString, path::Path};

#[cfg(unix)]
fn usage(path: &Path) -> Result<(u64, u64), CollectError> {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| CollectError::Invalid("mount contains NUL".into()))?;
    let mut data = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), data.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let data = unsafe { data.assume_init() };
    let total = u64::from(data.f_blocks).saturating_mul(data.f_frsize);
    let available = u64::from(data.f_bavail).saturating_mul(data.f_frsize);
    Ok((total, available))
}

pub fn collect(
    check: &CheckConfig,
    mount: &Path,
    warn: f64,
    critical: f64,
) -> Result<Observation, CollectError> {
    let (total, available) = usage(mount)?;
    if total == 0 {
        return Err(CollectError::Invalid("filesystem has zero blocks".into()));
    }
    let used_pct = (total - available) as f64 * 100.0 / total as f64;
    let status = if used_pct >= critical {
        Some(Severity::Critical)
    } else if used_pct >= warn {
        Some(Severity::Warn)
    } else {
        None
    };
    let summary = format!("磁盘已用 {used_pct:.1}%");
    let observation = status.map_or_else(
        || Observation::healthy(&check.name, &summary),
        |severity| Observation::unhealthy(&check.name, severity, &summary),
    );
    Ok(observation
        .detail("挂载点", mount.display().to_string())
        .detail("剩余", format!("{} MiB", available / 1024 / 1024)))
}
