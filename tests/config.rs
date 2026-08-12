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
