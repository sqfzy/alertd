use alertd::config::{self, Config};
use sha2::{Digest, Sha256};

fn valid() -> &'static str {
    r#"
[runtime]
state_dir = "/tmp/alertd"
[[checks]]
name = "root"
type = "disk"
mount = "/"
warn_used_pct = 80
critical_used_pct = 90
"#
}

#[test]
fn accepts_minimal_config() {
    let config: Config = toml::from_str(valid()).unwrap();
    config::validate_config(&config).unwrap();
}

#[test]
fn complete_example_matches_strict_schema() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("config/alertd.toml.example");

    let config = config::load_config(&path).expect("complete example must remain valid");

    assert_eq!(config.checks.len(), 13);
}

#[test]
fn loaded_config_hashes_exact_source_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("alertd.toml");
    std::fs::write(&path, valid()).unwrap();

    let loaded = config::load_config_with_sha256(&path).unwrap();

    assert_eq!(
        loaded.source_sha256,
        format!("{:x}", Sha256::digest(valid().as_bytes()))
    );
}

#[test]
fn rejects_unknown_keys_and_duplicates() {
    assert!(toml::from_str::<Config>(&valid().replace("state_dir", "unknown")).is_err());
    let duplicated = format!(
        "{}\n[[checks]]\nname='root'\ntype='memory'\nwarn_available_pct=20\ncritical_available_pct=10",
        valid()
    );
    let config: Config = toml::from_str(&duplicated).unwrap();
    assert!(config::validate_config(&config).is_err());
}

#[test]
fn rejects_invalid_thresholds_and_progress_contract() {
    let bad_disk: Config =
        toml::from_str(&valid().replace("critical_used_pct = 90", "critical_used_pct = 70"))
            .unwrap();
    assert!(config::validate_config(&bad_disk).is_err());
    let bad_shm: Config = toml::from_str("[[checks]]\nname='x'\ntype='shm'\npath='/x'\nprobe='u64_counter'\nrequire_progress=true\nstale_after='1s'").unwrap();
    assert!(config::validate_config(&bad_shm).is_err());
}

#[test]
fn validates_optional_runtime_ip() {
    let valid_ip: Config = toml::from_str(&valid().replace(
        "state_dir = \"/tmp/alertd\"",
        "state_dir = \"/tmp/alertd\"\nip = \"52.221.32.231\"",
    ))
    .unwrap();
    config::validate_config(&valid_ip).unwrap();

    let invalid_ip: Config = toml::from_str(&valid().replace(
        "state_dir = \"/tmp/alertd\"",
        "state_dir = \"/tmp/alertd\"\nip = \"primary\"",
    ))
    .unwrap();
    assert!(config::validate_config(&invalid_ip).is_err());
}

#[test]
fn validates_systemd_and_latest_file_contracts() {
    let text = r#"
[runtime]
interval = "10s"

[[checks]]
name = "units"
type = "systemd"
units = ["app.service", "archive.timer"]

[[checks]]
name = "raw"
type = "latest_file"
directory = "/var/lib/app/raw"
prefix = "raw_"
suffix = ".bin"
stale_after = "20s"
minimum_size_bytes = 384
"#;
    let valid_checks: Config = toml::from_str(text).unwrap();
    config::validate_config(&valid_checks).unwrap();

    let bad_path: Config =
        toml::from_str(&text.replace("/var/lib/app/raw", "relative/raw")).unwrap();
    assert!(config::validate_config(&bad_path).is_err());

    let bad_age: Config =
        toml::from_str(&text.replace("stale_after = \"20s\"", "stale_after = \"5s\"")).unwrap();
    assert!(config::validate_config(&bad_age).is_err());
}

#[test]
fn validates_optional_journal_filters() {
    let text = r#"
[[checks]]
name = "journal"
type = "journal"
units = ["app.service"]
ignore_contains = ["expected during shutdown"]
rules = [{ contains = "ERROR", severity = "critical" }]
"#;
    let config: Config = toml::from_str(text).unwrap();
    config::validate_config(&config).unwrap();

    let without_filters: Config =
        toml::from_str(&text.replace("ignore_contains = [\"expected during shutdown\"]\n", ""))
            .unwrap();
    config::validate_config(&without_filters).unwrap();

    let empty_filter: Config =
        toml::from_str(&text.replace("expected during shutdown", "")).unwrap();
    assert!(config::validate_config(&empty_filter).is_err());
}

#[test]
fn validates_metrics_file_contract() {
    let text = r#"
[runtime]
interval = "10s"

[[checks]]
name = "latency"
type = "metrics_file"
path = "/run/market/latency.metrics.json"
stale_after = "30s"
metrics = [
  { key = "latest" },
  { key = "p99", warn_above = 80, critical_above = 120 },
  { key = "max", critical_above = 500 },
]
"#;
    let config: Config = toml::from_str(text).unwrap();
    config::validate_config(&config).unwrap();

    for invalid in [
        text.replace("/run/market/latency.metrics.json", "relative.json"),
        text.replace("stale_after = \"30s\"", "stale_after = \"5s\""),
        text.replace(
            "{ key = \"max\", critical_above = 500 },",
            "{ key = \"p99\" },",
        ),
        text.replace("warn_above = 80", "warn_above = 120"),
        text.replace("warn_above = 80", "warn_above = nan"),
        text.replace("{ key = \"latest\" },", "{ key = \"\" },"),
        text.replace(
            "{ key = \"latest\" },",
            &format!("{{ key = \"{}\" }},", "x".repeat(129)),
        ),
    ] {
        let config: Config = toml::from_str(&invalid).unwrap();
        assert!(config::validate_config(&config).is_err());
    }

    let empty = text.replace(
        "metrics = [\n  { key = \"latest\" },\n  { key = \"p99\", warn_above = 80, critical_above = 120 },\n  { key = \"max\", critical_above = 500 },\n]",
        "metrics = []",
    );
    assert!(config::validate_config(&toml::from_str(&empty).unwrap()).is_err());
    assert!(
        toml::from_str::<Config>(&text.replace(
            "{ key = \"latest\" },",
            "{ key = \"latest\", unknown = 1 },"
        ))
        .is_err()
    );
}

#[test]
fn validates_metrics_shm_contract() {
    let text = r#"
[[checks]]
name = "latency-shm"
type = "metrics_shm"
path = "/market-metrics"
abi_hash = { offset = 0, expected_hex = "27c6096D" }
metrics = [
  { key = "u8", offset = 8, value_type = "u8" },
  { key = "u16", offset = 16, value_type = "u16", endian = "big" },
  { key = "u32", offset = 24, value_type = "u32" },
  { key = "u64", offset = 32, value_type = "u64" },
  { key = "i8", offset = 40, value_type = "i8" },
  { key = "i16", offset = 48, value_type = "i16" },
  { key = "i32", offset = 56, value_type = "i32", critical_above = 1000 },
  { key = "i64", offset = 64, value_type = "i64" },
  { key = "f32", offset = 72, value_type = "f32" },
  { key = "f64", offset = 80, value_type = "f64", warn_above = 80, critical_above = 120 },
]
"#;
    let config: Config = toml::from_str(text).unwrap();
    config::validate_config(&config).unwrap();
    let config::CheckKind::MetricsShm { metrics, .. } = &config.checks[0].kind else {
        panic!("expected metrics_shm check");
    };
    assert_eq!(metrics[0].endian, config::Endian::Little);
    assert_eq!(metrics[1].endian, config::Endian::Big);

    for invalid in [
        text.replace("/market-metrics", "market-metrics"),
        text.replace("/market-metrics", "/market/metrics"),
        text.replace("27c6096D", ""),
        text.replace("27c6096D", "abc"),
        text.replace("27c6096D", "zz"),
        text.replace("{ key = \"u16\"", "{ key = \"u8\""),
        text.replace("warn_above = 80", "warn_above = 120"),
        text.replace("critical_above = 1000", "critical_above = nan"),
    ] {
        let config: Config = toml::from_str(&invalid).unwrap();
        assert!(
            config::validate_config(&config).is_err(),
            "accepted: {invalid}"
        );
    }

    let no_abi = text.replace(
        "abi_hash = { offset = 0, expected_hex = \"27c6096D\" }\n",
        "",
    );
    config::validate_config(&toml::from_str(&no_abi).unwrap()).unwrap();

    let mut overflowing_abi = config.clone();
    let config::CheckKind::MetricsShm { abi_hash, .. } = &mut overflowing_abi.checks[0].kind else {
        panic!("expected metrics_shm check");
    };
    abi_hash.as_mut().unwrap().offset = u64::MAX;
    assert!(config::validate_config(&overflowing_abi).is_err());

    let mut overflowing_metric = config.clone();
    let config::CheckKind::MetricsShm { metrics, .. } = &mut overflowing_metric.checks[0].kind
    else {
        panic!("expected metrics_shm check");
    };
    metrics.last_mut().unwrap().offset = u64::MAX;
    assert!(config::validate_config(&overflowing_metric).is_err());

    let no_metrics = text.replace(&text[text.find("metrics = [").unwrap()..], "metrics = []\n");
    assert!(config::validate_config(&toml::from_str(&no_metrics).unwrap()).is_err());
    assert!(
        toml::from_str::<Config>(&text.replace(
            "{ key = \"u8\", offset = 8, value_type = \"u8\" },",
            "{ key = \"u8\", offset = 8, value_type = \"u8\", unknown = true },"
        ))
        .is_err()
    );
}

#[test]
fn accepts_tickfeat_production_config() {
    let config = config::load_config(std::path::Path::new("config/tickfeat-bn-spot.toml")).unwrap();
    assert_eq!(config.checks.len(), 14);
}

#[test]
fn validates_host_health_checks_and_new_global_ranges() {
    let text = r#"
[runtime]
command_timeout = "3s"

[alarm]
recover_for = "60s"

[delivery]
queue_warn_pct = 80
failure_report_after = 3

[[checks]]
name = "cpu"
type = "cpu"
warn_usage_pct = 80
critical_usage_pct = 95

[[checks]]
name = "clock"
type = "time_sync"
warn_offset = "1ms"
critical_offset = "5ms"

[[checks]]
name = "network"
type = "network"
interfaces = ["ens5", "ens7"]
warn_errors_per_second = 1
critical_errors_per_second = 10
warn_drops_per_second = 100
critical_drops_per_second = 1000

[[checks]]
name = "tuning"
type = "system_tuning"
"#;
    let config: Config = toml::from_str(text).unwrap();
    config::validate_config(&config).unwrap();

    let bad_cpu: Config =
        toml::from_str(&text.replace("critical_usage_pct = 95", "critical_usage_pct = 70"))
            .unwrap();
    assert!(config::validate_config(&bad_cpu).is_err());
    let duplicate_interface: Config = toml::from_str(&text.replace(
        "interfaces = [\"ens5\", \"ens7\"]",
        "interfaces = [\"ens5\", \"ens5\"]",
    ))
    .unwrap();
    assert!(config::validate_config(&duplicate_interface).is_err());
    let bad_offset: Config =
        toml::from_str(&text.replace("critical_offset = \"5ms\"", "critical_offset = \"1ms\""))
            .unwrap();
    assert!(config::validate_config(&bad_offset).is_err());
}
