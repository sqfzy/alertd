use super::CollectError;
use crate::{
    config::{CheckConfig, parse_duration},
    model::Observation,
};
use std::{fs, path::Path, time::SystemTime};

struct LatestFile {
    name: String,
    size: u64,
    modified: SystemTime,
}

fn find_latest(
    directory: &Path,
    prefix: &str,
    suffix: &str,
) -> Result<Option<LatestFile>, CollectError> {
    let mut latest: Option<LatestFile> = None;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(prefix) || !name.ends_with(suffix) {
            continue;
        }
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let candidate = LatestFile {
            name,
            size: metadata.len(),
            modified: metadata.modified()?,
        };
        if latest
            .as_ref()
            .is_none_or(|current| candidate.modified > current.modified)
        {
            latest = Some(candidate);
        }
    }
    Ok(latest)
}

pub fn collect(
    check: &CheckConfig,
    directory: &Path,
    prefix: &str,
    suffix: &str,
    stale_after: &str,
    minimum_size_bytes: u64,
) -> Result<Observation, CollectError> {
    let Some(latest) = find_latest(directory, prefix, suffix)? else {
        return Ok(
            Observation::unhealthy(&check.name, check.severity, "没有匹配的活动文件")
                .detail("目录", directory.display().to_string())
                .detail("匹配", format!("{prefix}*{suffix}")),
        );
    };
    if latest.size < minimum_size_bytes {
        return Ok(Observation::unhealthy(
            &check.name,
            check.severity,
            format!("最新文件过小：{} 字节", latest.size),
        )
        .detail("文件", latest.name)
        .detail("最小大小", minimum_size_bytes.to_string()));
    }
    let age = SystemTime::now()
        .duration_since(latest.modified)
        .unwrap_or_default();
    let stale =
        parse_duration(stale_after).map_err(|error| CollectError::Invalid(error.to_string()))?;
    let summary = if age >= stale {
        format!("最新文件已 {} 秒没有更新", age.as_secs())
    } else {
        format!("最新文件正常更新，距今 {} 秒", age.as_secs())
    };
    let observation = if age >= stale {
        Observation::unhealthy(&check.name, check.severity, summary)
    } else {
        Observation::healthy(&check.name, summary)
    };
    Ok(observation
        .detail("文件", directory.join(latest.name).display().to_string())
        .detail("大小", latest.size.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, model::ObservationStatus};
    use std::io::Write;

    fn check() -> crate::config::CheckConfig {
        let config: Config = toml::from_str(
            r#"
[[checks]]
name = "file"
type = "latest_file"
directory = "/tmp"
prefix = "raw_"
suffix = ".bin"
stale_after = "10s"
minimum_size_bytes = 2
"#,
        )
        .unwrap();
        config.checks.into_iter().next().unwrap()
    }

    #[test]
    fn selects_latest_matching_non_directory() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("raw_1.bin"), b"one").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(temporary.path().join("raw_2.bin"), b"two").unwrap();
        fs::File::create(temporary.path().join("other.bin"))
            .unwrap()
            .write_all(b"ignored")
            .unwrap();
        let latest = find_latest(temporary.path(), "raw_", ".bin")
            .unwrap()
            .unwrap();
        assert_eq!(latest.name, "raw_2.bin");
        assert_eq!(latest.size, 3);
    }

    #[test]
    fn reports_missing_small_and_stale_files() {
        let temporary = tempfile::tempdir().unwrap();
        let missing = collect(&check(), temporary.path(), "raw_", ".bin", "10s", 2).unwrap();
        assert!(matches!(missing.status, ObservationStatus::Unhealthy(_)));

        fs::write(temporary.path().join("raw_1.bin"), b"x").unwrap();
        let small = collect(&check(), temporary.path(), "raw_", ".bin", "10s", 2).unwrap();
        assert!(matches!(small.status, ObservationStatus::Unhealthy(_)));

        fs::write(temporary.path().join("raw_1.bin"), b"xx").unwrap();
        let stale = collect(&check(), temporary.path(), "raw_", ".bin", "0s", 2).unwrap();
        assert!(matches!(stale.status, ObservationStatus::Unhealthy(_)));
    }
}
