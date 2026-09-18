use super::{
    CollectContext, CollectError,
    metrics::{MetricValue, evaluate_metric, highest_severity, metrics_summary, render_metrics},
};
use crate::{
    config::{CheckConfig, Endian, ShmAbiHash, ShmMetricRule, ShmValueType},
    model::{Observation, Severity},
};
use std::{
    fs::File,
    io::ErrorKind,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
};

#[tracing::instrument(
    name = "collect_metrics_shm",
    skip(check, abi_hash, metrics, context),
    fields(check = %check.name, shm = name),
    err
)]
pub fn collect(
    check: &CheckConfig,
    name: &str,
    abi_hash: Option<&ShmAbiHash>,
    metrics: &[ShmMetricRule],
    context: &CollectContext,
) -> Result<Observation, CollectError> {
    let path = shm_file(context.shm_root.as_deref(), name);
    let Some(file) = open_shm(&path)? else {
        return Ok(missing_observation(check, name));
    };
    let size = file.metadata()?.len();
    if size == 0 {
        return Ok(empty_observation(check, name));
    }
    if let Some(abi_hash) = abi_hash {
        let expected = decode_hex(&abi_hash.expected_hex)?;
        let actual = read_at(&file, size, abi_hash.offset, expected.len())?;
        if actual != expected {
            return Ok(abi_mismatch_observation(
                check,
                name,
                &abi_hash.expected_hex,
                &actual,
            ));
        }
    }
    let values = read_metrics(&file, size, metrics)?;
    Ok(metrics_observation(check, name, size, abi_hash, &values))
}

fn shm_file(root: Option<&Path>, name: &str) -> PathBuf {
    root.unwrap_or(Path::new("/dev/shm"))
        .join(name.trim_start_matches('/'))
}

fn open_shm(path: &Path) -> Result<Option<File>, CollectError> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_metrics(
    file: &File,
    size: u64,
    metrics: &[ShmMetricRule],
) -> Result<Vec<MetricValue>, CollectError> {
    metrics
        .iter()
        .map(|metric| read_metric(file, size, metric))
        .collect()
}

fn read_metric(
    file: &File,
    size: u64,
    metric: &ShmMetricRule,
) -> Result<MetricValue, CollectError> {
    let bytes = read_at(
        file,
        size,
        metric.offset,
        metric.value_type.width() as usize,
    )?;
    let (number, rendered) = decode_number(&bytes, metric.value_type, metric.endian)?;
    Ok(evaluate_metric(
        &metric.key,
        rendered,
        number,
        metric.critical_below,
        metric.warn_below,
        metric.warn_above,
        metric.critical_above,
    ))
}

fn read_at(file: &File, size: u64, offset: u64, width: usize) -> Result<Vec<u8>, CollectError> {
    let end = offset
        .checked_add(width as u64)
        .ok_or_else(|| CollectError::Invalid(format!("SHM offset {offset} overflows")))?;
    if end > size {
        return Err(CollectError::Invalid(format!(
            "SHM range {offset}..{end} exceeds size {size}"
        )));
    }
    let mut bytes = vec![0; width];
    file.read_exact_at(&mut bytes, offset)?;
    Ok(bytes)
}

fn decode_number(
    bytes: &[u8],
    value_type: ShmValueType,
    endian: Endian,
) -> Result<(f64, String), CollectError> {
    macro_rules! integer {
        ($type:ty, $width:expr) => {{
            let raw: [u8; $width] = bytes.try_into().expect("validated numeric width");
            let value = match endian {
                Endian::Little => <$type>::from_le_bytes(raw),
                Endian::Big => <$type>::from_be_bytes(raw),
            };
            (value as f64, value.to_string())
        }};
    }
    let decoded = match value_type {
        ShmValueType::U8 => (f64::from(bytes[0]), bytes[0].to_string()),
        ShmValueType::U16 => integer!(u16, 2),
        ShmValueType::U32 => integer!(u32, 4),
        ShmValueType::U64 => integer!(u64, 8),
        ShmValueType::I8 => {
            let value = bytes[0] as i8;
            (f64::from(value), value.to_string())
        }
        ShmValueType::I16 => integer!(i16, 2),
        ShmValueType::I32 => integer!(i32, 4),
        ShmValueType::I64 => integer!(i64, 8),
        ShmValueType::F32 => integer!(f32, 4),
        ShmValueType::F64 => integer!(f64, 8),
    };
    if !decoded.0.is_finite() {
        return Err(CollectError::Invalid(format!(
            "SHM {value_type:?} value is not finite"
        )));
    }
    Ok(decoded)
}

fn decode_hex(value: &str) -> Result<Vec<u8>, CollectError> {
    if value.is_empty() || value.len() % 2 != 0 {
        return Err(CollectError::Invalid(
            "ABI hex must have a non-zero even length".into(),
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)
                .map_err(|error| CollectError::Invalid(format!("invalid ABI hex: {error}")))?;
            u8::from_str_radix(text, 16)
                .map_err(|error| CollectError::Invalid(format!("invalid ABI hex: {error}")))
        })
        .collect()
}

fn missing_observation(check: &CheckConfig, name: &str) -> Observation {
    Observation::unhealthy(&check.name, check.severity, "指标 SHM 不存在").detail("对象", name)
}

fn empty_observation(check: &CheckConfig, name: &str) -> Observation {
    Observation::unhealthy(&check.name, check.severity, "指标 SHM 为空").detail("对象", name)
}

fn abi_mismatch_observation(
    check: &CheckConfig,
    name: &str,
    expected: &str,
    actual: &[u8],
) -> Observation {
    Observation::unhealthy(&check.name, check.severity, "指标 SHM ABI hash 不匹配")
        .detail("对象", name)
        .detail("预期 ABI", expected.to_ascii_lowercase())
        .detail("实际 ABI", encode_hex(actual))
}

fn metrics_observation(
    check: &CheckConfig,
    name: &str,
    size: u64,
    abi_hash: Option<&ShmAbiHash>,
    values: &[MetricValue],
) -> Observation {
    let severity = highest_severity(values);
    let summary = metrics_summary(severity, "SHM 指标正常", values);
    let observation = if severity == Severity::Ok {
        Observation::healthy(&check.name, summary)
    } else {
        Observation::unhealthy(&check.name, severity, summary)
    };
    let observation = observation
        .detail("对象", name)
        .detail("大小", size.to_string())
        .detail("指标", render_metrics(values));
    if abi_hash.is_some() {
        observation.detail("ABI", "匹配")
    } else {
        observation
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, model::ObservationStatus};
    use std::fs;

    fn check() -> CheckConfig {
        let config: Config = toml::from_str(
            r#"
[[checks]]
name = "shm-latency"
type = "metrics_shm"
path = "/market-metrics"
abi_hash = { offset = 0, expected_hex = "27C6096D" }
metrics = [
  { key = "latest", offset = 8, value_type = "u64", critical_below = 5, warn_below = 10 },
  { key = "p99", offset = 16, value_type = "f64", warn_above = 80, critical_above = 120 },
  { key = "depth", offset = 24, value_type = "i32", endian = "big", critical_above = 1000 },
]
"#,
        )
        .unwrap();
        config.checks.into_iter().next().unwrap()
    }

    fn contract(check: &CheckConfig) -> (Option<&ShmAbiHash>, &[ShmMetricRule]) {
        let crate::config::CheckKind::MetricsShm {
            abi_hash, metrics, ..
        } = &check.kind
        else {
            panic!("expected metrics_shm check");
        };
        (abi_hash.as_ref(), metrics)
    }

    fn fixture() -> Vec<u8> {
        let mut bytes = vec![0; 32];
        bytes[0..4].copy_from_slice(&[0x27, 0xc6, 0x09, 0x6d]);
        bytes[8..16].copy_from_slice(&17_u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&120.5_f64.to_le_bytes());
        bytes[24..28].copy_from_slice(&1000_i32.to_be_bytes());
        bytes
    }

    #[test]
    fn reads_abi_and_reports_all_values_at_highest_severity() {
        let temporary = tempfile::tempdir().unwrap();
        fs::write(temporary.path().join("market-metrics"), fixture()).unwrap();
        let check = check();
        let (abi_hash, metrics) = contract(&check);
        let context = CollectContext {
            shm_root: Some(temporary.path().into()),
            ..Default::default()
        };

        let observation = collect(&check, "/market-metrics", abi_hash, metrics, &context).unwrap();

        assert_eq!(
            observation.status,
            ObservationStatus::Unhealthy(Severity::Critical)
        );
        assert_eq!(observation.details["ABI"], "匹配");
        assert_eq!(
            observation.details["指标"],
            "latest=17\np99=120.5\ndepth=1000"
        );
    }

    #[test]
    fn lower_thresholds_flow_through_shm_collection() {
        let temporary = tempfile::tempdir().unwrap();
        let mut bytes = fixture();
        bytes[8..16].copy_from_slice(&10_u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&50_f64.to_le_bytes());
        bytes[24..28].copy_from_slice(&0_i32.to_be_bytes());
        fs::write(temporary.path().join("market-metrics"), bytes).unwrap();
        let check = check();
        let (abi_hash, metrics) = contract(&check);
        let context = CollectContext {
            shm_root: Some(temporary.path().into()),
            ..Default::default()
        };

        let observation = collect(&check, "/market-metrics", abi_hash, metrics, &context).unwrap();

        assert_eq!(
            observation.status,
            ObservationStatus::Unhealthy(Severity::Warn)
        );
        assert!(observation.summary.contains("latest=10（≤ WARN 下限 10）"));
    }

    #[test]
    fn missing_empty_and_abi_mismatch_are_direct_observations() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("market-metrics");
        let check = check();
        let (abi_hash, metrics) = contract(&check);
        let context = CollectContext {
            shm_root: Some(temporary.path().into()),
            ..Default::default()
        };
        assert!(
            collect(&check, "/market-metrics", abi_hash, metrics, &context)
                .unwrap()
                .summary
                .contains("不存在")
        );
        fs::write(&path, []).unwrap();
        assert!(
            collect(&check, "/market-metrics", abi_hash, metrics, &context)
                .unwrap()
                .summary
                .contains("为空")
        );
        let mut bytes = fixture();
        bytes[0] = 0;
        fs::write(&path, bytes).unwrap();
        let observation = collect(&check, "/market-metrics", abi_hash, metrics, &context).unwrap();
        assert!(observation.summary.contains("ABI hash 不匹配"));
        assert_eq!(observation.details["实际 ABI"], "00c6096d");
    }

    #[test]
    fn decodes_every_integer_and_float_in_both_endian_modes() {
        macro_rules! assert_decoded {
            ($bytes:expr, $kind:expr, $endian:expr, $rendered:expr) => {
                assert_eq!(decode_number(&$bytes, $kind, $endian).unwrap().1, $rendered)
            };
        }
        assert_decoded!([255], ShmValueType::U8, Endian::Little, "255");
        assert_decoded!([255], ShmValueType::I8, Endian::Big, "-1");
        assert_decoded!(
            513_u16.to_le_bytes(),
            ShmValueType::U16,
            Endian::Little,
            "513"
        );
        assert_decoded!(513_u16.to_be_bytes(), ShmValueType::U16, Endian::Big, "513");
        assert_decoded!(
            70_000_u32.to_le_bytes(),
            ShmValueType::U32,
            Endian::Little,
            "70000"
        );
        assert_decoded!(
            70_000_u32.to_be_bytes(),
            ShmValueType::U32,
            Endian::Big,
            "70000"
        );
        assert_decoded!(
            u64::MAX.to_be_bytes(),
            ShmValueType::U64,
            Endian::Big,
            "18446744073709551615"
        );
        assert_decoded!(
            u64::MAX.to_le_bytes(),
            ShmValueType::U64,
            Endian::Little,
            "18446744073709551615"
        );
        assert_decoded!((-2_i16).to_be_bytes(), ShmValueType::I16, Endian::Big, "-2");
        assert_decoded!(
            (-2_i16).to_le_bytes(),
            ShmValueType::I16,
            Endian::Little,
            "-2"
        );
        assert_decoded!(
            (-3_i32).to_le_bytes(),
            ShmValueType::I32,
            Endian::Little,
            "-3"
        );
        assert_decoded!((-3_i32).to_be_bytes(), ShmValueType::I32, Endian::Big, "-3");
        assert_decoded!((-4_i64).to_be_bytes(), ShmValueType::I64, Endian::Big, "-4");
        assert_decoded!(
            (-4_i64).to_le_bytes(),
            ShmValueType::I64,
            Endian::Little,
            "-4"
        );
        assert_decoded!(
            1.25_f32.to_le_bytes(),
            ShmValueType::F32,
            Endian::Little,
            "1.25"
        );
        assert_decoded!(
            1.25_f32.to_be_bytes(),
            ShmValueType::F32,
            Endian::Big,
            "1.25"
        );
        assert_decoded!(
            (-2.5_f64).to_be_bytes(),
            ShmValueType::F64,
            Endian::Big,
            "-2.5"
        );
        assert_decoded!(
            (-2.5_f64).to_le_bytes(),
            ShmValueType::F64,
            Endian::Little,
            "-2.5"
        );
    }

    #[test]
    fn rejects_non_finite_values_and_out_of_bounds_ranges() {
        assert!(decode_number(&f64::NAN.to_le_bytes(), ShmValueType::F64, Endian::Little).is_err());
        assert!(
            decode_number(&f32::INFINITY.to_be_bytes(), ShmValueType::F32, Endian::Big).is_err()
        );
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("short");
        fs::write(&path, [0; 4]).unwrap();
        let file = File::open(path).unwrap();
        assert!(read_at(&file, 4, 1, 8).is_err());
        assert!(decode_hex("").is_err());
        assert!(decode_hex("abc").is_err());
    }

    #[test]
    fn no_abi_reads_metrics_and_truncated_abi_is_a_collector_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("metrics");
        fs::write(&path, 42_u64.to_le_bytes()).unwrap();
        let check = check();
        let rule = ShmMetricRule {
            key: "value".into(),
            offset: 0,
            value_type: ShmValueType::U64,
            endian: Endian::Little,
            critical_below: None,
            warn_below: None,
            warn_above: None,
            critical_above: None,
        };
        let context = CollectContext {
            shm_root: Some(temporary.path().into()),
            ..Default::default()
        };
        let observation = collect(&check, "/metrics", None, &[rule], &context).unwrap();
        assert_eq!(observation.status, ObservationStatus::Healthy);
        assert_eq!(observation.details["指标"], "value=42");

        let truncated = ShmAbiHash {
            offset: 4,
            expected_hex: "0011223344556677".into(),
        };
        assert!(collect(&check, "/metrics", Some(&truncated), &[], &context).is_err());
    }

    #[test]
    fn opened_descriptor_remains_on_original_inode_after_path_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("metrics");
        let replacement = temporary.path().join("replacement");
        fs::write(&path, 1_u64.to_le_bytes()).unwrap();
        let file = File::open(&path).unwrap();
        fs::write(&replacement, 2_u64.to_le_bytes()).unwrap();
        fs::rename(&replacement, &path).unwrap();
        let rule = ShmMetricRule {
            key: "value".into(),
            offset: 0,
            value_type: ShmValueType::U64,
            endian: Endian::Little,
            critical_below: None,
            warn_below: None,
            warn_above: None,
            critical_above: None,
        };
        let values = read_metrics(&file, 8, &[rule]).unwrap();
        assert_eq!(values[0].rendered, "1");
    }
}
