use alertd::config::{self, Config};

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
fn validates_live_mm_entry_contract() {
    let text = r#"
[runtime]
interval = "5s"

[[checks]]
name = "live-mm-entry"
type = "live_mm_entry"
stale_after = "20s"
instances = [
  { name = "live_mm1", unit = "live-mm-v0.service" },
  { name = "live_mm2", unit = "live-mm-v0@account8.service" },
]
"#;
    let config: Config = toml::from_str(text).unwrap();
    config::validate_config(&config).unwrap();

    let duplicate: Config =
        toml::from_str(&text.replace("name = \"live_mm2\"", "name = \"live_mm1\"")).unwrap();
    assert!(config::validate_config(&duplicate).is_err());

    let too_short: Config = toml::from_str(&text.replace("20s", "4s")).unwrap();
    assert!(config::validate_config(&too_short).is_err());

    let too_long: Config = toml::from_str(&text.replace("20s", "301s")).unwrap();
    assert!(config::validate_config(&too_long).is_err());
}

#[test]
fn unsigned_delivery_does_not_require_a_signing_secret() {
    let config: Config = toml::from_str(
        r#"
[delivery]
signed = false
token_env = "ALERTD_TEST_UNSIGNED_TOKEN"
secret_env = "ALERTD_TEST_MISSING_SECRET"
"#,
    )
    .unwrap();
    unsafe { std::env::set_var("ALERTD_TEST_UNSIGNED_TOKEN", "token") };
    let credentials = config::resolve_dingtalk_credentials(&config.delivery).unwrap();
    unsafe { std::env::remove_var("ALERTD_TEST_UNSIGNED_TOKEN") };
    assert_eq!(credentials, ("token".into(), None));
}

#[test]
fn signed_delivery_remains_the_default() {
    let config: Config = toml::from_str("").unwrap();
    assert!(config.delivery.signed);
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
