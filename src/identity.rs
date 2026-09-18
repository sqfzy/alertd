use crate::config::Config;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use thiserror::Error;

const MACHINE_ID_PATH: &str = "/etc/machine-id";
const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

#[derive(Clone, Debug)]
pub struct RuntimeIdentity {
    pub host: String,
    pub ip: Option<String>,
    pub system_hostname: String,
    pub machine_sha256: String,
    pub boot_sha256: String,
    pub pid: u32,
    pub config_sha256: String,
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("cannot read system hostname: {0}")]
    Hostname(#[source] std::io::Error),
    #[error("system hostname is empty")]
    EmptyHostname,
    #[error("cannot read identity file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("identity file {0} is empty")]
    Empty(String),
}

pub fn load_runtime_identity(
    config: &Config,
    config_sha256: String,
) -> Result<RuntimeIdentity, IdentityError> {
    let system_hostname = hostname::get()
        .map_err(IdentityError::Hostname)?
        .to_string_lossy()
        .trim()
        .to_owned();
    if system_hostname.is_empty() {
        return Err(IdentityError::EmptyHostname);
    }
    load_runtime_identity_from(
        config,
        config_sha256,
        system_hostname,
        std::process::id(),
        Path::new(MACHINE_ID_PATH),
        Path::new(BOOT_ID_PATH),
    )
}

pub fn update_config_identity(
    identity: &mut RuntimeIdentity,
    config: &Config,
    config_sha256: String,
) {
    identity.host = config
        .runtime
        .host
        .clone()
        .unwrap_or_else(|| identity.system_hostname.clone());
    identity.ip.clone_from(&config.runtime.ip);
    identity.config_sha256 = config_sha256;
}

fn load_runtime_identity_from(
    config: &Config,
    config_sha256: String,
    system_hostname: String,
    pid: u32,
    machine_id_path: &Path,
    boot_id_path: &Path,
) -> Result<RuntimeIdentity, IdentityError> {
    let machine_sha256 = hash_identity_file(machine_id_path)?;
    let boot_sha256 = hash_identity_file(boot_id_path)?;
    let host = config
        .runtime
        .host
        .clone()
        .unwrap_or_else(|| system_hostname.clone());
    Ok(RuntimeIdentity {
        host,
        ip: config.runtime.ip.clone(),
        system_hostname,
        machine_sha256,
        boot_sha256,
        pid,
        config_sha256,
    })
}

fn hash_identity_file(path: &Path) -> Result<String, IdentityError> {
    let raw = fs::read_to_string(path).map_err(|source| IdentityError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let value = raw.trim_end();
    if value.is_empty() {
        return Err(IdentityError::Empty(path.display().to_string()));
    }
    Ok(format!("{:x}", Sha256::digest(value.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        toml::from_str("[runtime]\nhost='role'\nip='127.0.0.1'").unwrap()
    }

    #[test]
    fn hashes_trimmed_identity_files() {
        let temporary = tempfile::tempdir().unwrap();
        let machine = temporary.path().join("machine-id");
        let boot = temporary.path().join("boot-id");
        fs::write(&machine, "machine-value\n").unwrap();
        fs::write(&boot, "boot-value \n").unwrap();

        let identity = load_runtime_identity_from(
            &config(),
            "config-hash".into(),
            "kernel-host".into(),
            42,
            &machine,
            &boot,
        )
        .unwrap();

        assert_eq!(
            identity.machine_sha256,
            format!("{:x}", Sha256::digest(b"machine-value"))
        );
        assert_eq!(
            identity.boot_sha256,
            format!("{:x}", Sha256::digest(b"boot-value"))
        );
        assert_eq!(identity.pid, 42);
    }

    #[test]
    fn rejects_missing_and_empty_identity_files() {
        let temporary = tempfile::tempdir().unwrap();
        let machine = temporary.path().join("machine-id");
        let boot = temporary.path().join("boot-id");
        fs::write(&machine, "\n").unwrap();
        fs::write(&boot, "boot").unwrap();
        assert!(matches!(
            load_runtime_identity_from(&config(), "hash".into(), "host".into(), 1, &machine, &boot),
            Err(IdentityError::Empty(_))
        ));
        fs::remove_file(&machine).unwrap();
        assert!(matches!(
            load_runtime_identity_from(&config(), "hash".into(), "host".into(), 1, &machine, &boot),
            Err(IdentityError::Read { .. })
        ));
    }
}
