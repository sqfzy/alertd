use super::CollectError;
use crate::{config::CheckConfig, model::Observation};
#[cfg(target_os = "linux")]
use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
struct UnitState {
    name: String,
    load: String,
    active: String,
    sub: String,
}

#[cfg(any(target_os = "linux", test))]
fn parse_states(text: &str) -> Result<Vec<UnitState>, CollectError> {
    let mut states = Vec::new();
    let mut current = UnitState {
        name: String::new(),
        load: String::new(),
        active: String::new(),
        sub: String::new(),
    };
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !current.name.is_empty() {
                states.push(current);
                current = UnitState {
                    name: String::new(),
                    load: String::new(),
                    active: String::new(),
                    sub: String::new(),
                };
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(CollectError::Invalid("malformed systemctl output".into()));
        };
        match key {
            "Id" => current.name = value.into(),
            "LoadState" => current.load = value.into(),
            "ActiveState" => current.active = value.into(),
            "SubState" => current.sub = value.into(),
            _ => {}
        }
    }
    Ok(states)
}

#[cfg(target_os = "linux")]
fn read_states(units: &[String]) -> Result<Vec<UnitState>, CollectError> {
    let output = Command::new("systemctl")
        .arg("show")
        .args(units)
        .args(["--property=Id,LoadState,ActiveState,SubState"])
        .output()?;
    if !output.status.success() {
        return Err(CollectError::Invalid(format!(
            "systemctl show exited {}",
            output.status
        )));
    }
    parse_states(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(target_os = "linux"))]
fn read_states(_units: &[String]) -> Result<Vec<UnitState>, CollectError> {
    Err(CollectError::Unsupported("systemd requires Linux".into()))
}

pub fn collect(check: &CheckConfig, units: &[String]) -> Result<Observation, CollectError> {
    let states = read_states(units)?;
    if states.len() != units.len() {
        return Err(CollectError::Invalid(format!(
            "systemctl returned {}/{} units",
            states.len(),
            units.len()
        )));
    }
    let unhealthy: Vec<String> = states
        .iter()
        .filter(|state| state.load != "loaded" || state.active != "active")
        .map(|state| {
            format!(
                "{}={}/{}/{}",
                state.name, state.load, state.active, state.sub
            )
        })
        .collect();
    if unhealthy.is_empty() {
        return Ok(Observation::healthy(
            &check.name,
            format!("systemd units active {}/{}", states.len(), units.len()),
        )
        .detail("units", units.join(", ")));
    }
    Ok(Observation::unhealthy(
        &check.name,
        check.severity,
        format!("systemd units 异常 {}/{}", unhealthy.len(), units.len()),
    )
    .detail("异常", unhealthy.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_active_and_failed_units() {
        let states = parse_states(
            "Id=a.service\nLoadState=loaded\nActiveState=active\nSubState=running\n\n\
             Id=b.service\nLoadState=loaded\nActiveState=failed\nSubState=failed\n",
        )
        .unwrap();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].active, "active");
        assert_eq!(states[1].active, "failed");
    }
}
