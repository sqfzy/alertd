use super::{CollectContext, CollectError};
use crate::{
    config::{CheckConfig, Endian, ShmProbe, parse_duration},
    model::{Observation, Severity},
};
use chrono::{DateTime, Utc};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct ProgressState {
    pub fingerprint: u64,
    pub inode: u64,
    pub changed_at: DateTime<Utc>,
}

fn shm_file(root: Option<&Path>, name: &str) -> PathBuf {
    root.unwrap_or(Path::new("/dev/shm"))
        .join(name.trim_start_matches('/'))
}

fn read_u64(bytes: &[u8], offset: usize, endian: Endian) -> Result<u64, CollectError> {
    let raw: [u8; 8] = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| CollectError::Invalid(format!("u64 offset {offset} outside SHM")))?
        .try_into()
        .unwrap();
    Ok(match endian {
        Endian::Little => u64::from_le_bytes(raw),
        Endian::Big => u64::from_be_bytes(raw),
    })
}

fn gconf_fingerprint(
    bytes: &[u8],
    expected_magic: u32,
    expected_version: u16,
) -> Result<u64, CollectError> {
    if bytes.len() < 72 {
        return Err(CollectError::Invalid("gconf_v2 header is truncated".into()));
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
    if magic != expected_magic || version != expected_version {
        return Err(CollectError::Invalid(format!(
            "gconf_v2 header mismatch magic=0x{magic:08x} version={version}"
        )));
    }
    Ok(u64::from_le_bytes(bytes[64..72].try_into().unwrap()))
}

#[allow(clippy::too_many_arguments)]
pub fn collect(
    check: &CheckConfig,
    name: &str,
    probe: ShmProbe,
    require_progress: bool,
    stale_after: Option<&str>,
    offset: Option<u64>,
    endian: Option<Endian>,
    magic: Option<u32>,
    layout_version: Option<u16>,
    context: &mut CollectContext,
) -> Result<Observation, CollectError> {
    let path = shm_file(context.shm_root.as_deref(), name);
    let metadata = fs::metadata(&path)?;
    if metadata.len() == 0 {
        return Ok(Observation::unhealthy(
            &check.name,
            check.severity.max(Severity::Warn),
            "SHM 存在但为空",
        )
        .detail("对象", name));
    }
    if probe == ShmProbe::Exists || !require_progress {
        return Ok(Observation::healthy(&check.name, "SHM 存在且非空")
            .detail("对象", name)
            .detail("大小", metadata.len().to_string()));
    }
    let bytes = fs::read(&path)?;
    let fingerprint = match probe {
        ShmProbe::Exists => metadata.len(),
        ShmProbe::U64Counter => read_u64(
            &bytes,
            offset.unwrap_or_default() as usize,
            endian.unwrap_or(Endian::Little),
        )?,
        ShmProbe::GconfV2 => gconf_fingerprint(
            &bytes,
            magic.ok_or_else(|| CollectError::Invalid("gconf_v2 magic missing".into()))?,
            layout_version
                .ok_or_else(|| CollectError::Invalid("gconf_v2 layout_version missing".into()))?,
        )?,
    };
    let now = Utc::now();
    let entry = context
        .shm_progress
        .entry(check.name.clone())
        .or_insert(ProgressState {
            fingerprint,
            inode: metadata.ino(),
            changed_at: now,
        });
    if entry.fingerprint != fingerprint || entry.inode != metadata.ino() {
        *entry = ProgressState {
            fingerprint,
            inode: metadata.ino(),
            changed_at: now,
        };
    }
    let stale = parse_duration(
        stale_after.ok_or_else(|| CollectError::Invalid("stale_after missing".into()))?,
    )
    .map_err(|error| CollectError::Invalid(error.to_string()))?;
    let age = (now - entry.changed_at).to_std().unwrap_or_default();
    let summary = if age >= stale {
        format!("SHM 已 {} 秒没有推进", age.as_secs())
    } else {
        format!("SHM 正常推进，静止 {} 秒", age.as_secs())
    };
    let observation = if age >= stale {
        Observation::unhealthy(&check.name, check.severity, summary)
    } else {
        Observation::healthy(&check.name, summary)
    };
    Ok(observation
        .detail("对象", name)
        .detail("进度", fingerprint.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_counter_endianness_and_bounds() {
        assert_eq!(
            read_u64(&[1, 0, 0, 0, 0, 0, 0, 0], 0, Endian::Little).unwrap(),
            1
        );
        assert_eq!(
            read_u64(&[0, 0, 0, 0, 0, 0, 0, 1], 0, Endian::Big).unwrap(),
            1
        );
        assert!(read_u64(&[0; 7], 0, Endian::Little).is_err());
    }
}
