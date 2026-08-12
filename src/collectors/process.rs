use super::{CollectContext, CollectError};
use crate::{
    config::CheckConfig,
    model::{Observation, Severity},
};
use std::{fs, path::Path};

pub fn count_matches(root: &Path, needle: &str) -> Result<u32, CollectError> {
    let mut count = 0;
    for entry in fs::read_dir(root)? {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            continue;
        }
        let bytes = match fs::read(entry.path().join("cmdline")) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let cmdline = String::from_utf8_lossy(&bytes).replace('\0', " ");
        if cmdline.contains(needle) {
            count += 1;
        }
    }
    Ok(count)
}

pub fn collect(
    check: &CheckConfig,
    needle: &str,
    min_count: u32,
    context: &CollectContext,
) -> Result<Observation, CollectError> {
    let root = context.proc_root.as_deref().unwrap_or(Path::new("/proc"));
    let count = count_matches(root, needle)?;
    let summary = format!("进程匹配 {count}/{min_count}");
    let observation = if count >= min_count {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, check.severity.max(Severity::Warn), summary)
    };
    Ok(observation
        .detail("匹配", needle)
        .detail("数量", format!("{count}/{min_count}")))
}
