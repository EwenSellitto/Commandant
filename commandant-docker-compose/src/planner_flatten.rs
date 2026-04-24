use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use docker_compose_spec::{
    BoolOrStr, Command, ComposeService, DockerCompose, FieldKey, Healthcheck, IntOrStr, NumOrStr,
    Restart, ServiceDependsOn, ServiceNetwork, ServiceNetworks, ServicePort, ServiceVolume,
    VecOrMap, VecOrMapVal, VecOrStr,
};

use crate::error::{Error, Result};
use crate::planner::{
    MountKind, ResolvedHealthcheck, ResolvedMount, ResolvedPort, RestartPolicy,
    ServiceNetworkAttachment, ServicePlan,
};
use crate::planner_runtime::{
    build_network_plan_with_project_name, derive_project_name, prefixed_runtime_name,
};

pub(crate) fn build_service_plan(
    base_dir: &Path,
    compose: &DockerCompose,
    service_name: &str,
    service: &ComposeService,
) -> Result<ServicePlan> {
    let project_name = derive_project_name(base_dir);
    let image = service.image.clone().ok_or_else(|| Error::MissingImage {
        service: service_name.to_string(),
    })?;

    let command = flatten_command(service.command.as_ref());
    let entrypoint = flatten_command(service.entrypoint.as_ref());
    let environment = flatten_service_environment(
        base_dir,
        service.env_file.as_ref(),
        service.environment.as_ref(),
    )?;
    let labels = flatten_labels(service.labels.as_ref());
    let ports = flatten_ports(service_name, service.ports.as_ref())?;
    let exposed_ports = flatten_exposed_ports(service_name, service.expose.as_ref(), &ports)?;
    let mounts = flatten_mounts(
        base_dir,
        compose,
        service_name,
        service.volumes.as_ref(),
        project_name.as_deref(),
    )?;
    let networks = flatten_networks(
        compose,
        service_name,
        service.networks.as_ref(),
        project_name.as_deref(),
    )?;
    let healthcheck = flatten_healthcheck(service.healthcheck.as_ref())?;
    let restart = map_restart(service.restart.as_ref());
    let memory_limit_bytes = parse_memory_limit(service.mem_limit.as_ref())?;
    let cpu_shares = service.cpu_shares.as_ref().and_then(parse_num_or_str_i64);
    let privileged = bool_or_str(service.privileged.as_ref()).unwrap_or(false);
    let depends_on = match service.depends_on.as_ref() {
        Some(ServiceDependsOn::Vec(values)) => values.clone(),
        Some(ServiceDependsOn::Map(values)) => values.keys().map(ToString::to_string).collect(),
        None => Vec::new(),
    };

    Ok(ServicePlan {
        name: service_name.to_string(),
        image,
        container_name: service
            .container_name
            .clone()
            .unwrap_or_else(|| prefixed_runtime_name(project_name.as_deref(), service_name)),
        command,
        entrypoint,
        environment,
        working_dir: service.working_dir.clone(),
        hostname: service.hostname.clone(),
        user: service.user.clone(),
        labels,
        exposure: None,
        exposed_ports,
        ports,
        mounts,
        networks,
        healthcheck,
        restart,
        memory_limit_bytes,
        cpu_shares,
        privileged,
        auto_remove: false,
        depends_on,
    })
}

fn flatten_command(command: Option<&Command>) -> Option<Vec<String>> {
    match command {
        Some(Command::Vec(values)) => Some(values.clone()),
        Some(Command::String(value)) => shell_words::split(value).ok(),
        Some(Command::Null) | None => None,
    }
}

fn flatten_environment(environment: Option<&VecOrMap>) -> BTreeMap<String, String> {
    match environment {
        Some(VecOrMap::Map(values)) => values
            .iter()
            .map(|(key, value)| (key.to_string(), vec_or_map_value_to_string(value)))
            .collect(),
        Some(VecOrMap::Vec(values)) => values
            .iter()
            .map(|entry| match entry.split_once('=') {
                Some((key, value)) => (key.to_string(), value.to_string()),
                None => (entry.to_string(), String::new()),
            })
            .collect(),
        None => BTreeMap::new(),
    }
}

fn flatten_service_environment(
    base_dir: &Path,
    env_files: Option<&docker_compose_spec::EnvFile>,
    environment: Option<&VecOrMap>,
) -> Result<BTreeMap<String, String>> {
    let mut merged = load_default_dotenv(base_dir)?;

    match env_files {
        Some(docker_compose_spec::EnvFile::String(path)) => {
            merged.extend(load_dotenv_file(&resolve_path(base_dir, path))?);
        }
        Some(docker_compose_spec::EnvFile::Vec(paths)) => {
            for entry in paths {
                let path = match entry {
                    docker_compose_spec::EnvFileItem::String(path) => path,
                    docker_compose_spec::EnvFileItem::Object { path, .. } => path,
                };
                merged.extend(load_dotenv_file(&resolve_path(base_dir, path))?);
            }
        }
        None => {}
    }

    merged.extend(flatten_environment(environment));
    Ok(merged)
}

fn load_default_dotenv(base_dir: &Path) -> Result<BTreeMap<String, String>> {
    let default_path = base_dir.join(".env");
    if default_path.exists() {
        load_dotenv_file(&default_path)
    } else {
        Ok(BTreeMap::new())
    }
}

fn load_dotenv_file(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }

    let contents = fs::read_to_string(path).map_err(|source| Error::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;

    let mut values = BTreeMap::new();
    for item in dotenvy::from_read_iter(contents.as_bytes()) {
        let (key, value) = item.map_err(|error| {
            Error::Parse(anyhow::anyhow!(
                "failed to parse env file `{}`: {error}",
                path.display()
            ))
        })?;
        values.insert(key, value);
    }

    Ok(values)
}

fn flatten_labels(labels: Option<&VecOrMap>) -> BTreeMap<String, String> {
    flatten_environment(labels)
}

fn flatten_ports(
    service_name: &str,
    ports: Option<&Vec<ServicePort>>,
) -> Result<Vec<ResolvedPort>> {
    ports
        .into_iter()
        .flatten()
        .map(|port| parse_service_port(service_name, port))
        .collect()
}

fn flatten_exposed_ports(
    service_name: &str,
    expose: Option<&Vec<NumOrStr>>,
    ports: &[ResolvedPort],
) -> Result<Vec<u16>> {
    let mut resolved = ports
        .iter()
        .map(|port| port.container_port)
        .collect::<Vec<_>>();

    for port in expose.into_iter().flatten() {
        let port = parse_exposed_port(service_name, port)?;
        if !resolved.contains(&port) {
            resolved.push(port);
        }
    }

    Ok(resolved)
}

fn parse_exposed_port(service_name: &str, port: &NumOrStr) -> Result<u16> {
    match port {
        NumOrStr::Number(value) => {
            if !value.is_finite()
                || value.fract() != 0.0
                || *value < 0.0
                || *value > u16::MAX as f64
            {
                return Err(Error::InvalidExposurePort {
                    service: service_name.to_string(),
                    value: value.to_string(),
                });
            }
            Ok(*value as u16)
        }
        NumOrStr::String(value) => value
            .parse::<u16>()
            .map_err(|_| Error::InvalidExposurePort {
                service: service_name.to_string(),
                value: value.clone(),
            }),
    }
}

fn parse_service_port(service_name: &str, port: &ServicePort) -> Result<ResolvedPort> {
    match port {
        ServicePort::Object {
            host_ip,
            protocol,
            published,
            target,
            ..
        } => {
            let host_port = published
                .as_ref()
                .and_then(parse_int_or_str_u16)
                .ok_or_else(|| Error::InvalidPortMapping {
                    service: service_name.to_string(),
                    value: format!("{port:?}"),
                })?;
            let container_port =
                target
                    .as_ref()
                    .and_then(parse_int_or_str_u16)
                    .ok_or_else(|| Error::InvalidPortMapping {
                        service: service_name.to_string(),
                        value: format!("{port:?}"),
                    })?;

            Ok(ResolvedPort {
                host_port,
                container_port,
                protocol: protocol.as_deref().unwrap_or("tcp").to_string(),
                host_ip: host_ip.clone(),
            })
        }
        ServicePort::String(value) => parse_short_port(service_name, value),
        ServicePort::Number(value) => {
            let port = *value;
            if !port.is_finite() || port.fract() != 0.0 || port < 0.0 || port > u16::MAX as f64 {
                return Err(Error::InvalidPortMapping {
                    service: service_name.to_string(),
                    value: value.to_string(),
                });
            }
            Ok(ResolvedPort {
                host_port: port as u16,
                container_port: port as u16,
                protocol: "tcp".to_string(),
                host_ip: None,
            })
        }
    }
}

fn parse_short_port(service_name: &str, value: &str) -> Result<ResolvedPort> {
    let (host_ip, rest) = extract_host_ip(value);
    let (ports, protocol) = split_protocol(rest);
    let parts = ports.split(':').collect::<Vec<_>>();

    let (host_port, container_port) = match parts.as_slice() {
        [container] => {
            let container_port = parse_u16(service_name, value, container)?;
            (container_port, container_port)
        }
        [host, container] => (
            parse_u16(service_name, value, host)?,
            parse_u16(service_name, value, container)?,
        ),
        _ => {
            return Err(Error::InvalidPortMapping {
                service: service_name.to_string(),
                value: value.to_string(),
            });
        }
    };

    Ok(ResolvedPort {
        host_port,
        container_port,
        protocol: protocol.to_string(),
        host_ip,
    })
}

fn flatten_mounts(
    base_dir: &Path,
    compose: &DockerCompose,
    service_name: &str,
    volumes: Option<&Vec<ServiceVolume>>,
    project_name: Option<&str>,
) -> Result<Vec<ResolvedMount>> {
    volumes
        .into_iter()
        .flatten()
        .map(|volume| parse_service_volume(base_dir, compose, service_name, volume, project_name))
        .collect()
}

fn parse_service_volume(
    base_dir: &Path,
    compose: &DockerCompose,
    service_name: &str,
    volume: &ServiceVolume,
    project_name: Option<&str>,
) -> Result<ResolvedMount> {
    match volume {
        ServiceVolume::String(value) => {
            parse_short_volume(base_dir, compose, service_name, value, project_name)
        }
        ServiceVolume::Object {
            read_only,
            source,
            target,
            type_,
            ..
        } => {
            let target = target.clone().ok_or_else(|| Error::InvalidVolumeMapping {
                service: service_name.to_string(),
                value: format!("{volume:?}"),
            })?;
            let compose_source = source.clone().unwrap_or_default();
            let source = compose_source.clone();
            let kind = match type_.as_str() {
                "bind" => MountKind::Bind,
                "volume" => MountKind::Volume,
                _ => {
                    return Err(Error::InvalidVolumeMapping {
                        service: service_name.to_string(),
                        value: format!("{volume:?}"),
                    });
                }
            };
            let anonymous = kind == MountKind::Volume && source.is_empty();
            let source = match kind {
                MountKind::Bind => resolve_path(base_dir, &source).display().to_string(),
                MountKind::Volume if anonymous => String::new(),
                MountKind::Volume => {
                    lookup_volume_name_with_project_name(compose, &source, project_name)?
                }
            };

            Ok(ResolvedMount {
                compose_source: (kind == MountKind::Volume)
                    .then_some(compose_source)
                    .filter(|value| !value.is_empty()),
                source,
                target,
                kind,
                read_only: bool_or_str(read_only.as_ref()).unwrap_or(false),
                anonymous,
            })
        }
    }
}

fn parse_short_volume(
    base_dir: &Path,
    compose: &DockerCompose,
    service_name: &str,
    value: &str,
    project_name: Option<&str>,
) -> Result<ResolvedMount> {
    let parts = value.split(':').collect::<Vec<_>>();
    let (source, target, mode) = match parts.as_slice() {
        [target] => (String::new(), (*target).to_string(), ""),
        [source, target] => ((*source).to_string(), (*target).to_string(), ""),
        [source, target, mode] => ((*source).to_string(), (*target).to_string(), *mode),
        _ => {
            return Err(Error::InvalidVolumeMapping {
                service: service_name.to_string(),
                value: value.to_string(),
            });
        }
    };

    let kind = infer_mount_kind(compose, &source, parts.len() == 1);
    let anonymous = kind == MountKind::Volume && source.is_empty();
    let source = match kind {
        MountKind::Bind => resolve_path(base_dir, &source).display().to_string(),
        MountKind::Volume if anonymous => String::new(),
        MountKind::Volume => lookup_volume_name_with_project_name(compose, &source, project_name)?,
    };

    Ok(ResolvedMount {
        compose_source: (kind == MountKind::Volume && !anonymous)
            .then_some(parts[0].to_string())
            .filter(|value| !value.is_empty()),
        source,
        target,
        kind,
        read_only: mode.split(',').any(|flag| flag == "ro"),
        anonymous,
    })
}

fn flatten_networks(
    compose: &DockerCompose,
    service_name: &str,
    networks: Option<&ServiceNetworks>,
    project_name: Option<&str>,
) -> Result<Vec<ServiceNetworkAttachment>> {
    match networks {
        Some(ServiceNetworks::Vec(values)) => values
            .iter()
            .map(|name| build_network_attachment(compose, service_name, name, None, project_name))
            .collect(),
        Some(ServiceNetworks::Map(values)) => values
            .iter()
            .map(|(name, config)| {
                build_network_attachment(
                    compose,
                    service_name,
                    name.as_str(),
                    Some(config),
                    project_name,
                )
            })
            .collect(),
        None => {
            if compose.networks.is_empty() {
                Ok(Vec::new())
            } else if compose.networks.contains_key(&FieldKey::from("default")) {
                Ok(vec![build_network_attachment(
                    compose,
                    service_name,
                    "default",
                    None,
                    project_name,
                )?])
            } else {
                Ok(Vec::new())
            }
        }
    }
}

fn flatten_healthcheck(healthcheck: Option<&Healthcheck>) -> Result<Option<ResolvedHealthcheck>> {
    let Some(healthcheck) = healthcheck else {
        return Ok(None);
    };

    let disabled = bool_or_str(healthcheck.disable.as_ref()).unwrap_or(false);
    if disabled {
        return Ok(Some(ResolvedHealthcheck {
            test: Some(vec!["NONE".to_string()]),
            interval_ns: None,
            timeout_ns: None,
            retries: None,
            start_period_ns: None,
            start_interval_ns: None,
        }));
    }

    Ok(Some(ResolvedHealthcheck {
        test: flatten_healthcheck_test(healthcheck.test.as_ref()),
        interval_ns: parse_duration_ns(healthcheck.interval.as_deref())?,
        timeout_ns: parse_duration_ns(healthcheck.timeout.as_deref())?,
        retries: healthcheck.retries.as_ref().and_then(parse_num_or_str_i64),
        start_period_ns: parse_duration_ns(healthcheck.start_period.as_deref())?,
        start_interval_ns: parse_duration_ns(healthcheck.start_interval.as_deref())?,
    }))
}

fn flatten_healthcheck_test(test: Option<&VecOrStr>) -> Option<Vec<String>> {
    match test {
        Some(VecOrStr::Vec(values)) => Some(values.clone()),
        Some(VecOrStr::String(command)) => Some(vec!["CMD-SHELL".to_string(), command.to_string()]),
        None => None,
    }
}

fn build_network_attachment(
    compose: &DockerCompose,
    service_name: &str,
    network_name: &str,
    config: Option<&ServiceNetwork>,
    project_name: Option<&str>,
) -> Result<ServiceNetworkAttachment> {
    let network_plan = build_network_plan_with_project_name(compose, network_name, project_name)?;
    if config.is_some_and(|config| {
        config.ipv4_address.is_some()
            || config.ipv6_address.is_some()
            || config.mac_address.is_some()
    }) {
        return Err(Error::UnsupportedServiceFeature {
            service: service_name.to_string(),
            feature: "service network static addressing",
        });
    }

    Ok(ServiceNetworkAttachment {
        compose_name: network_name.to_string(),
        runtime_name: network_plan.runtime_name,
        aliases: config
            .and_then(|config| config.aliases.clone())
            .unwrap_or_default(),
        primary: false,
    })
}

fn map_restart(restart: Option<&Restart>) -> RestartPolicy {
    match restart {
        Some(Restart::Always) => RestartPolicy::Always,
        Some(Restart::OnFailure) => RestartPolicy::OnFailure,
        Some(Restart::UnlessStopped) => RestartPolicy::UnlessStopped,
        Some(Restart::No) | None => RestartPolicy::No,
    }
}

fn parse_memory_limit(memory: Option<&docker_compose_spec::NumOrStr>) -> Result<Option<i64>> {
    let Some(memory) = memory else {
        return Ok(None);
    };
    match memory {
        docker_compose_spec::NumOrStr::Number(value) => Ok(Some(*value as i64)),
        docker_compose_spec::NumOrStr::String(value) => parse_memory_string(value).map(Some),
    }
}

fn parse_duration_ns(value: Option<&str>) -> Result<Option<i64>> {
    let Some(value) = value else { return Ok(None) };
    let duration = humantime::parse_duration(value).map_err(|_| Error::InvalidVolumeMapping {
        service: "<healthcheck>".to_string(),
        value: value.to_string(),
    })?;
    Ok(Some(duration_to_ns(duration)))
}

fn duration_to_ns(duration: std::time::Duration) -> i64 {
    duration.as_nanos().min(i64::MAX as u128) as i64
}

fn parse_memory_string(value: &str) -> Result<i64> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(0);
    }
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, suffix) = value.split_at(split);
    let amount = digits
        .parse::<i64>()
        .map_err(|_| Error::InvalidVolumeMapping {
            service: "<memory>".to_string(),
            value: value.to_string(),
        })?;
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1024,
        "m" | "mb" => 1024 * 1024,
        "g" | "gb" => 1024 * 1024 * 1024,
        _ => {
            return Err(Error::InvalidVolumeMapping {
                service: "<memory>".to_string(),
                value: value.to_string(),
            });
        }
    };
    Ok(amount.saturating_mul(multiplier))
}

fn bool_or_str(value: Option<&BoolOrStr>) -> Option<bool> {
    match value {
        Some(BoolOrStr::Boolean(value)) => Some(*value),
        Some(BoolOrStr::String(value)) => value.parse().ok(),
        None => None,
    }
}

fn vec_or_map_value_to_string(value: &VecOrMapVal) -> String {
    match value {
        VecOrMapVal::Null => String::new(),
        VecOrMapVal::Boolean(value) => value.to_string(),
        VecOrMapVal::Number(value) => value.to_string(),
        VecOrMapVal::String(value) => value.clone(),
    }
}

fn parse_num_or_str_i64(value: &NumOrStr) -> Option<i64> {
    match value {
        NumOrStr::Number(value) => Some(*value as i64),
        NumOrStr::String(value) => value.parse().ok(),
    }
}

fn parse_int_or_str_u16(value: &IntOrStr) -> Option<u16> {
    match value {
        IntOrStr::Integer(value) => u16::try_from(*value).ok(),
        IntOrStr::String(value) => value.parse().ok(),
    }
}

fn parse_u16(service_name: &str, original: &str, value: &str) -> Result<u16> {
    value.parse::<u16>().map_err(|_| Error::InvalidPortMapping {
        service: service_name.to_string(),
        value: original.to_string(),
    })
}

fn split_protocol(value: &str) -> (&str, &str) {
    match value.rsplit_once('/') {
        Some((port, protocol)) => (port, protocol),
        None => (value, "tcp"),
    }
}

fn extract_host_ip(value: &str) -> (Option<String>, &str) {
    let mut parts = value.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(host), Some(_), Some(rest)) if host.contains('.') => (Some(host.to_string()), rest),
        _ => (None, value),
    }
}

fn resolve_path(base_dir: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base_dir.join(path)
    }
}

fn infer_mount_kind(compose: &DockerCompose, source: &str, target_only: bool) -> MountKind {
    if target_only {
        MountKind::Volume
    } else if source.is_empty()
        || source.starts_with('.')
        || source.starts_with('/')
        || source.starts_with('~')
    {
        MountKind::Bind
    } else if compose.volumes.contains_key(&FieldKey::from(source)) {
        MountKind::Volume
    } else {
        MountKind::Bind
    }
}

fn lookup_volume_name_with_project_name(
    compose: &DockerCompose,
    source: &str,
    project_name: Option<&str>,
) -> Result<String> {
    let volume = compose
        .volumes
        .get(&FieldKey::from(source))
        .ok_or_else(|| Error::UnknownVolume {
            volume: source.to_string(),
        })?;

    Ok(volume
        .as_ref()
        .and_then(|config| config.name.clone())
        .unwrap_or_else(|| prefixed_runtime_name(project_name, source)))
}
