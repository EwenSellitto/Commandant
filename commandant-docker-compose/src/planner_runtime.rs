use std::path::Path;
use std::process::Command as ProcessCommand;

use docker_compose_spec::DockerCompose;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::planner::{NetworkPlan, VolumePlan, APP_NETWORK_NAME};

pub(crate) fn derive_project_name(base_dir: &Path) -> Option<String> {
    let name = base_dir.file_name()?.to_str()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

pub(crate) fn derive_app_network_runtime_name(
    base_dir: &Path,
    project_name: Option<&str>,
) -> String {
    let suffix = compose_runtime_suffix(base_dir);
    match project_name {
        Some(project_name) if !project_name.is_empty() => {
            format!("{project_name}_{APP_NETWORK_NAME}_{suffix}")
        }
        _ => format!("{APP_NETWORK_NAME}_{suffix}"),
    }
}

pub(crate) fn build_network_plan_with_project_name(
    compose: &DockerCompose,
    name: &str,
    project_name: Option<&str>,
) -> Result<NetworkPlan> {
    let network = compose
        .networks
        .get(&docker_compose_spec::FieldKey::from(name))
        .ok_or_else(|| Error::UnknownNetwork {
            network: name.to_string(),
        })?;

    let runtime_name = network
        .as_ref()
        .and_then(|config| config.name.clone())
        .unwrap_or_else(|| prefixed_runtime_name(project_name, name));
    let driver = network
        .as_ref()
        .and_then(|config| config.driver.clone())
        .unwrap_or_else(|| "bridge".to_string());
    let external = network
        .as_ref()
        .and_then(|config| config.external.as_ref())
        .is_some();

    Ok(NetworkPlan {
        compose_name: name.to_string(),
        runtime_name,
        driver,
        external,
    })
}

pub(crate) fn build_volume_plan_with_project_name(
    compose: &DockerCompose,
    name: &str,
    project_name: Option<&str>,
) -> Result<VolumePlan> {
    let volume = compose
        .volumes
        .get(&docker_compose_spec::FieldKey::from(name))
        .ok_or_else(|| Error::UnknownVolume {
            volume: name.to_string(),
        })?;

    let runtime_name = volume
        .as_ref()
        .and_then(|config| config.name.clone())
        .unwrap_or_else(|| prefixed_runtime_name(project_name, name));
    let external = volume
        .as_ref()
        .and_then(|config| config.external.as_ref())
        .is_some();

    Ok(VolumePlan {
        compose_name: name.to_string(),
        runtime_name,
        external,
    })
}

pub(crate) fn sanitize_runtime_component(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut previous_underscore = false;

    for character in value.chars().flat_map(|character| character.to_lowercase()) {
        let is_valid = character.is_ascii_lowercase() || character.is_ascii_digit();
        if is_valid {
            output.push(character);
            previous_underscore = false;
        } else if !previous_underscore {
            output.push('_');
            previous_underscore = true;
        }
    }

    output.trim_matches('_').to_string()
}

pub(crate) fn prefixed_runtime_name(project_name: Option<&str>, name: &str) -> String {
    match project_name {
        Some(project_name) if !project_name.is_empty() => format!("{project_name}_{name}"),
        _ => name.to_string(),
    }
}

fn compose_runtime_suffix(base_dir: &Path) -> String {
    git_branch_name(base_dir)
        .map(|branch| sanitize_runtime_component(&branch))
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string().replace('-', ""))
}

fn git_branch_name(base_dir: &Path) -> Option<String> {
    let output = ProcessCommand::new("git")
        .args([
            "-C",
            base_dir.to_str()?,
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8(output.stdout).ok()?.trim().to_string();
    match branch.as_str() {
        "" | "HEAD" => None,
        _ => Some(branch),
    }
}
