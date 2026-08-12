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

const GCONF_HEADER_BYTES: usize = 64;
const GCONF_RING_ENTRIES_OFFSET: usize = 128;
const GCONF_BOARD_KIND: u16 = 1;
const GCONF_BCAST_RING_KIND: u16 = 2;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GconfSegment {
    kind: u16,
    entry_size: usize,
    capacity: usize,
    fingerprint: u64,
}

fn hash_u64(hash: u64, value: u64) -> u64 {
    value.to_le_bytes().into_iter().fold(hash, |current, byte| {
        (current ^ u64::from(byte)).wrapping_mul(1_099_511_628_211)
    })
}

fn gconf_segment(
    bytes: &[u8],
    expected_magic: u32,
    expected_version: u16,
) -> Result<GconfSegment, CollectError> {
    if bytes.len() < GCONF_HEADER_BYTES {
        return Err(CollectError::Invalid("gconf_v2 header is truncated".into()));
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
    if magic != expected_magic || version != expected_version {
        return Err(CollectError::Invalid(format!(
            "gconf_v2 header mismatch magic=0x{magic:08x} version={version}"
        )));
    }
    let kind = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
    let entry_size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let capacity = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    if entry_size < 8 || capacity == 0 {
        return Err(CollectError::Invalid(format!(
            "gconf_v2 invalid entry_size={entry_size} capacity={capacity}"
        )));
    }
    let entries_bytes = entry_size
        .checked_mul(capacity)
        .ok_or_else(|| CollectError::Invalid("gconf_v2 entry bytes overflow".into()))?;
    let fingerprint = match kind {
        GCONF_BCAST_RING_KIND => {
            let expected_size = GCONF_RING_ENTRIES_OFFSET
                .checked_add(entries_bytes)
                .ok_or_else(|| CollectError::Invalid("gconf_v2 ring size overflow".into()))?;
            if bytes.len() != expected_size {
                return Err(CollectError::Invalid(format!(
                    "gconf_v2 ring size mismatch actual={} expected={expected_size}",
                    bytes.len()
                )));
            }
            read_u64(bytes, GCONF_HEADER_BYTES, Endian::Little)?
        }
        GCONF_BOARD_KIND => {
            let slots_offset = bytes
                .len()
                .checked_sub(entries_bytes)
                .filter(|offset| *offset >= GCONF_HEADER_BYTES)
                .ok_or_else(|| {
                    CollectError::Invalid(format!(
                        "gconf_v2 board slots outside file size={} entries={entries_bytes}",
                        bytes.len()
                    ))
                })?;
            let heartbeat = read_u64(bytes, 32, Endian::Little)?;
            (0..capacity).try_fold(
                hash_u64(1_469_598_103_934_665_603, heartbeat),
                |hash, index| {
                    let offset = slots_offset + index * entry_size;
                    Ok::<u64, CollectError>(hash_u64(
                        hash,
                        read_u64(bytes, offset, Endian::Little)?,
                    ))
                },
            )?
        }
        _ => {
            return Err(CollectError::Invalid(format!(
                "gconf_v2 unsupported SegKind={kind}"
            )));
        }
    };
    Ok(GconfSegment {
        kind,
        entry_size,
        capacity,
        fingerprint,
    })
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
    if probe == ShmProbe::Exists {
        return Ok(Observation::healthy(&check.name, "SHM 存在且非空")
            .detail("对象", name)
            .detail("大小", metadata.len().to_string()));
    }
    let bytes = fs::read(&path)?;
    let (fingerprint, contract) = match probe {
        ShmProbe::Exists => unreachable!("exists returned before reading SHM"),
        ShmProbe::U64Counter => (
            read_u64(
                &bytes,
                offset.unwrap_or_default() as usize,
                endian.unwrap_or(Endian::Little),
            )?,
            None,
        ),
        ShmProbe::GconfV2 => {
            let segment = gconf_segment(
                &bytes,
                magic.ok_or_else(|| CollectError::Invalid("gconf_v2 magic missing".into()))?,
                layout_version.ok_or_else(|| {
                    CollectError::Invalid("gconf_v2 layout_version missing".into())
                })?,
            )?;
            (segment.fingerprint, Some(segment))
        }
    };
    if !require_progress {
        let mut observation =
            Observation::healthy(&check.name, "SHM 契约有效").detail("对象", name);
        if let Some(segment) = contract {
            observation = observation
                .detail("SegKind", segment.kind.to_string())
                .detail("entry_size", segment.entry_size.to_string())
                .detail("capacity", segment.capacity.to_string());
        }
        return Ok(observation);
    }
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

    fn header(kind: u16, entry_size: u32, capacity: u32) -> [u8; 64] {
        let mut bytes = [0_u8; 64];
        bytes[0..4].copy_from_slice(&0x4743_4632_u32.to_le_bytes());
        bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&kind.to_le_bytes());
        bytes[8..12].copy_from_slice(&entry_size.to_le_bytes());
        bytes[12..16].copy_from_slice(&capacity.to_le_bytes());
        bytes
    }

    #[test]
    fn fingerprints_ring_head_and_validates_size() {
        let mut bytes = vec![0_u8; 128 + 4 * 64];
        bytes[..64].copy_from_slice(&header(GCONF_BCAST_RING_KIND, 64, 4));
        bytes[64..72].copy_from_slice(&7_u64.to_le_bytes());
        let segment = gconf_segment(&bytes, 0x4743_4632, 2).unwrap();
        assert_eq!(segment.fingerprint, 7);
        bytes.pop();
        assert!(gconf_segment(&bytes, 0x4743_4632, 2).is_err());
    }

    #[test]
    fn fingerprints_board_slots_from_file_tail() {
        let mut bytes = vec![0_u8; 256 + 3 * 64];
        bytes[..64].copy_from_slice(&header(GCONF_BOARD_KIND, 64, 3));
        bytes[32..40].copy_from_slice(&11_u64.to_le_bytes());
        bytes[256..264].copy_from_slice(&2_u64.to_le_bytes());
        bytes[320..328].copy_from_slice(&4_u64.to_le_bytes());
        let first = gconf_segment(&bytes, 0x4743_4632, 2).unwrap();
        bytes[384..392].copy_from_slice(&6_u64.to_le_bytes());
        let second = gconf_segment(&bytes, 0x4743_4632, 2).unwrap();
        assert_ne!(first.fingerprint, second.fingerprint);
    }

    #[test]
    fn rejects_bad_header_kind_and_overflow() {
        let mut bytes = vec![0_u8; 64];
        bytes[..64].copy_from_slice(&header(3, 64, 1));
        assert!(gconf_segment(&bytes, 0x4743_4632, 2).is_err());
        assert!(gconf_segment(&bytes, 0, 2).is_err());
    }
}
