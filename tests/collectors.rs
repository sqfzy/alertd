use alertd::{
    collectors::{memory::parse_meminfo, process::count_matches},
    model::Severity,
};
use std::fs;

#[test]
fn parses_available_memory() {
    assert_eq!(
        parse_meminfo("MemTotal: 1000 kB\nMemAvailable: 250 kB\n").unwrap(),
        (1000, 250)
    );
    assert!(parse_meminfo("MemTotal: 1000 kB\n").is_err());
    assert!(Severity::Critical > Severity::Warn);
}

#[test]
fn counts_process_fixtures_and_ignores_non_pid_entries() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("12")).unwrap();
    fs::write(
        temp.path().join("12/cmdline"),
        b"/usr/bin/app\0--role\0market",
    )
    .unwrap();
    fs::create_dir(temp.path().join("self")).unwrap();
    assert_eq!(count_matches(temp.path(), "--role market").unwrap(), 1);
    assert_eq!(count_matches(temp.path(), "private").unwrap(), 0);
}
