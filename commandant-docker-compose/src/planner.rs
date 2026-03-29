use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use docker_compose_spec::{
    BoolOrStr, Command, ComposeService, DockerCompose, FieldKey, Healthcheck, IntOrStr,
    NetworkConfig, NumOrStr, Restart, ServiceDependsOn, ServiceNetwork, ServiceNetworks,
    ServicePort, ServiceVolume, VecOrMap, VecOrMapVal, VecOrStr,
};

use crate::error::{Error, Result};
use crate::model::ComposeProject;
use crate::proxy::{prepare_proxy_session, ProxyConfig};

#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    pub networks: Vec<NetworkPlan>,
    pub volumes: Vec<VolumePlan>,
    pub services: Vec<ServicePlan>,
    pub proxy: Option<crate::proxy::ProxySession>,
}

#[derive(Debug, Clone)]
pub struct NetworkPlan {
    pub compose_name: String,
    pub runtime_name: String,
    pub driver: String,
    pub external: bool,
}

#[derive(Debug, Clone)]
pub struct VolumePlan {
    pub compose_name: String,
    pub runtime_name: String,
    pub external: bool,
}

#[derive(Debug, Clone)]
pub struct ServicePlan {
    pub name: String,
    pub image: String,
    pub container_name: String,
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub environment: BTreeMap<String, String>,
    pub working_dir: Option<String>,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub ports: Vec<ResolvedPort>,
    pub mounts: Vec<ResolvedMount>,
    pub networks: Vec<ServiceNetworkAttachment>,
    pub healthcheck: Option<ResolvedHealthcheck>,
    pub restart: RestartPolicy,
    pub memory_limit_bytes: Option<i64>,
    pub cpu_shares: Option<i64>,
    pub privileged: bool,
    pub auto_remove: bool,
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedPort {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
    pub host_ip: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedMount {
    pub compose_source: Option<String>,
    pub source: String,
    pub target: String,
    pub kind: MountKind,
    pub read_only: bool,
    pub anonymous: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountKind {
    Bind,
    Volume,
}

#[derive(Debug, Clone)]
pub struct ServiceNetworkAttachment {
    pub compose_name: String,
    pub runtime_name: String,
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHealthcheck {
    pub test: Option<Vec<String>>,
    pub interval_ns: Option<i64>,
    pub timeout_ns: Option<i64>,
    pub retries: Option<i64>,
    pub start_period_ns: Option<i64>,
    pub start_interval_ns: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    No,
    Always,
    OnFailure,
    UnlessStopped,
}

impl ExecutionPlan {
    pub fn service(&self, name: &str) -> Option<&ServicePlan> {
        self.services.iter().find(|service| service.name == name)
    }
}

pub fn build_plan(project: &ComposeProject, active_profiles: &[String]) -> Result<ExecutionPlan> {
    validate_compose_features(project.compose())?;

    let project_name = derive_project_name(project.base_dir());
    let active = active_service_names(project.compose(), active_profiles);
    let ordered = topological_service_order(project.compose(), &active)?;

    let mut services = Vec::with_capacity(ordered.len());

    for service_name in ordered {
        let service = project
            .service(&service_name)
            .ok_or_else(|| Error::UnknownService {
                service: service_name.clone(),
            })?;

        let plan = build_service_plan(
            project.base_dir(),
            project.compose(),
            &service_name,
            service,
        )?;
        services.push(plan);
    }

    let mut plan = ExecutionPlan {
        networks: Vec::new(),
        volumes: Vec::new(),
        services,
        proxy: None,
    };

    let proxy_config = ProxyConfig::default();
    let proxy = prepare_proxy_session(&mut plan, &proxy_config)?;
    plan.proxy = proxy;

    let required_networks = plan
        .services
        .iter()
        .flat_map(|service| {
            service
                .networks
                .iter()
                .map(|network| network.compose_name.as_str())
        })
        .filter(|name| *name != "commandant_proxy")
        .collect::<BTreeSet<_>>();

    let required_volumes = plan
        .services
        .iter()
        .flat_map(|service| {
            service.mounts.iter().filter_map(|mount| {
                (mount.kind == MountKind::Volume && !mount.anonymous).then(|| {
                    mount
                        .compose_source
                        .as_deref()
                        .unwrap_or(mount.source.as_str())
                })
            })
        })
        .collect::<BTreeSet<_>>();

    let networks = required_networks
        .into_iter()
        .map(|name| {
            build_network_plan_with_project_name(project.compose(), name, project_name.as_deref())
        })
        .collect::<Result<Vec<_>>>()?;

    let volumes = required_volumes
        .into_iter()
        .map(|name| {
            build_volume_plan_with_project_name(project.compose(), name, project_name.as_deref())
        })
        .collect::<Result<Vec<_>>>()?;

    plan.networks = networks;
    plan.volumes = volumes;

    if let Some(proxy) = plan.proxy.as_ref() {
        if !plan
            .networks
            .iter()
            .any(|network| network.runtime_name == proxy.network_runtime_name)
        {
            plan.networks.push(NetworkPlan {
                compose_name: proxy.network_compose_name.clone(),
                runtime_name: proxy.network_runtime_name.clone(),
                driver: "bridge".to_string(),
                external: false,
            });
        }
    }

    Ok(plan)
}

fn validate_compose_features(compose: &DockerCompose) -> Result<()> {
    for (service_name, service) in &compose.services {
        let service_name = service_name.to_string();

        if service.build.is_some() {
            return Err(Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "build",
            });
        }
        if service.deploy.is_some() {
            return Err(Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "deploy",
            });
        }
        if service.configs.is_some() {
            return Err(Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "configs",
            });
        }
        if service.secrets.is_some() {
            return Err(Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "secrets",
            });
        }
        if service.network_mode.is_some() {
            return Err(Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "network_mode",
            });
        }
    }

    for (network_name, network) in &compose.networks {
        if let Some(NetworkConfig { ipam: Some(_), .. }) = network.as_ref() {
            return Err(Error::UnsupportedNetworkFeature {
                network: network_name.to_string(),
                feature: "ipam",
            });
        }
    }

    Ok(())
}

fn active_service_names(compose: &DockerCompose, active_profiles: &[String]) -> HashSet<String> {
    let active_profiles = active_profiles
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    compose
        .services
        .iter()
        .filter(|(_, service)| match service.profiles.as_ref() {
            None => true,
            Some(profiles) => profiles
                .iter()
                .any(|profile| active_profiles.contains(profile.as_str())),
        })
        .map(|(name, _)| name.to_string())
        .collect()
}

fn topological_service_order(
    compose: &DockerCompose,
    active_services: &HashSet<String>,
) -> Result<Vec<String>> {
    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    let mut order = Vec::with_capacity(active_services.len());

    for service in active_services {
        visit_service(
            compose,
            service,
            active_services,
            &mut visited,
            &mut visiting,
            &mut order,
        )?;
    }

    Ok(order)
}

fn visit_service(
    compose: &DockerCompose,
    service_name: &str,
    active_services: &HashSet<String>,
    visited: &mut HashSet<String>,
    visiting: &mut HashSet<String>,
    order: &mut Vec<String>,
) -> Result<()> {
    if visited.contains(service_name) {
        return Ok(());
    }

    if !visiting.insert(service_name.to_string()) {
        return Err(Error::DependencyCycle {
            service: service_name.to_string(),
        });
    }

    let service = compose
        .services
        .get(&FieldKey::from(service_name))
        .ok_or_else(|| Error::UnknownService {
            service: service_name.to_string(),
        })?;

    match service.depends_on.as_ref() {
        Some(ServiceDependsOn::Vec(values)) => {
            for dependency in values {
                if active_services.contains(dependency.as_str()) {
                    visit_service(
                        compose,
                        dependency,
                        active_services,
                        visited,
                        visiting,
                        order,
                    )?;
                }
            }
        }
        Some(ServiceDependsOn::Map(values)) => {
            for dependency in values.keys() {
                if active_services.contains(dependency.as_str()) {
                    visit_service(
                        compose,
                        dependency.as_str(),
                        active_services,
                        visited,
                        visiting,
                        order,
                    )?;
                }
            }
        }
        None => {}
    }

    visiting.remove(service_name);
    visited.insert(service_name.to_string());
    order.push(service_name.to_string());

    Ok(())
}

fn build_network_plan_with_project_name(
    compose: &DockerCompose,
    name: &str,
    project_name: Option<&str>,
) -> Result<NetworkPlan> {
    let network =
        compose
            .networks
            .get(&FieldKey::from(name))
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

fn build_volume_plan_with_project_name(
    compose: &DockerCompose,
    name: &str,
    project_name: Option<&str>,
) -> Result<VolumePlan> {
    let volume =
        compose
            .volumes
            .get(&FieldKey::from(name))
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

fn build_service_plan(
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
    let auto_remove = false;
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
        ports,
        mounts,
        networks,
        healthcheck,
        restart,
        memory_limit_bytes,
        cpu_shares,
        privileged,
        auto_remove,
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
    let mut resolved = Vec::new();
    for port in ports.into_iter().flatten() {
        resolved.push(parse_service_port(service_name, port)?);
    }
    Ok(resolved)
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
            let port = *value as u16;
            Ok(ResolvedPort {
                host_port: port,
                container_port: port,
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
    let mut mounts = Vec::new();
    for volume in volumes.into_iter().flatten() {
        mounts.push(parse_service_volume(
            base_dir,
            compose,
            service_name,
            volume,
            project_name,
        )?);
    }
    Ok(mounts)
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

    if let Some(config) = config {
        if config.ipv4_address.is_some()
            || config.ipv6_address.is_some()
            || config.mac_address.is_some()
        {
            return Err(Error::UnsupportedServiceFeature {
                service: service_name.to_string(),
                feature: "service network static addressing",
            });
        }
    }

    Ok(ServiceNetworkAttachment {
        compose_name: network_name.to_string(),
        runtime_name: network_plan.runtime_name,
        aliases: config
            .and_then(|config| config.aliases.clone())
            .unwrap_or_default(),
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
    let Some(value) = value else {
        return Ok(None);
    };

    let duration = humantime::parse_duration(value).map_err(|_| Error::InvalidVolumeMapping {
        service: "<healthcheck>".to_string(),
        value: value.to_string(),
    })?;

    Ok(Some(duration_to_ns(duration)))
}

fn duration_to_ns(duration: Duration) -> i64 {
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
        docker_compose_spec::NumOrStr::Number(value) => Some(*value as i64),
        docker_compose_spec::NumOrStr::String(value) => value.parse().ok(),
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
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() >= 3 && parts[0].contains('.') {
        (Some(parts[0].to_string()), &value[parts[0].len() + 1..])
    } else {
        (None, value)
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

fn derive_project_name(base_dir: &Path) -> Option<String> {
    let name = base_dir.file_name()?.to_str()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn prefixed_runtime_name(project_name: Option<&str>, name: &str) -> String {
    match project_name {
        Some(project_name) if !project_name.is_empty() => format!("{project_name}_{name}"),
        _ => name.to_string(),
    }
}
