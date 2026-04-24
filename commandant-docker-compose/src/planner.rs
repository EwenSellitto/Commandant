use docker_compose_spec::{DockerCompose, NetworkConfig};
use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::error::Result;
use crate::model::ComposeProject;
use crate::planner_exposure as exposure;
use crate::planner_flatten as flatten;
use crate::planner_runtime as runtime;
use crate::proxy::{ProxyConfig, prepare_proxy_session};

pub(crate) const APP_NETWORK_NAME: &str = "commandant_app";

#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    pub app_network: NetworkPlan,
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
    pub exposure: Option<ServiceExposure>,
    pub exposed_ports: Vec<u16>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceExposure {
    pub enabled: bool,
    pub tag: String,
    pub hostname: String,
    pub backend_port: u16,
    pub source: TagSource,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagSource {
    ExplicitTag,
    AutoTag,
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
    pub primary: bool,
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
    build_plan_with_proxy(project, active_profiles, &ProxyConfig::default())
}

pub fn build_plan_with_proxy(
    project: &ComposeProject,
    active_profiles: &[String],
    proxy_config: &ProxyConfig,
) -> Result<ExecutionPlan> {
    validate_compose_features(project.compose())?;
    let project_name = runtime::derive_project_name(project.base_dir());
    let active = active_service_names(project.compose(), active_profiles);
    let ordered = topological_service_order(project.compose(), &active)?;

    let mut services = Vec::with_capacity(ordered.len());
    for service_name in ordered {
        let service =
            project
                .service(&service_name)
                .ok_or_else(|| crate::Error::UnknownService {
                    service: service_name.clone(),
                })?;
        services.push(flatten::build_service_plan(
            project.base_dir(),
            project.compose(),
            &service_name,
            service,
        )?);
    }

    let mut plan = ExecutionPlan {
        app_network: NetworkPlan {
            compose_name: APP_NETWORK_NAME.to_string(),
            runtime_name: runtime::derive_app_network_runtime_name(
                project.base_dir(),
                project_name.as_deref(),
            ),
            driver: "bridge".to_string(),
            external: false,
        },
        networks: Vec::new(),
        volumes: Vec::new(),
        services,
        proxy: None,
    };

    for service in &mut plan.services {
        service.networks.insert(
            0,
            ServiceNetworkAttachment {
                compose_name: plan.app_network.compose_name.clone(),
                runtime_name: plan.app_network.runtime_name.clone(),
                aliases: Vec::new(),
                primary: true,
            },
        );
        for network in service.networks.iter_mut().skip(1) {
            network.primary = false;
        }
    }

    exposure::resolve_service_exposure(&mut plan, proxy_config)?;
    let proxy = prepare_proxy_session(&mut plan, proxy_config)?;
    plan.proxy = proxy;

    let required_networks = plan
        .services
        .iter()
        .flat_map(|service| service.networks.iter())
        .filter(|network| network.compose_name != APP_NETWORK_NAME)
        .map(|network| network.compose_name.as_str())
        .collect::<BTreeSet<_>>();
    let required_volumes = plan
        .services
        .iter()
        .flat_map(|service| service.mounts.iter())
        .filter(|mount| mount.kind == MountKind::Volume && !mount.anonymous)
        .map(|mount| {
            mount
                .compose_source
                .as_deref()
                .unwrap_or(mount.source.as_str())
        })
        .collect::<BTreeSet<_>>();

    let mut networks = Vec::with_capacity(required_networks.len() + 1);
    networks.push(plan.app_network.clone());
    for name in required_networks {
        networks.push(runtime::build_network_plan_with_project_name(
            project.compose(),
            name,
            project_name.as_deref(),
        )?);
    }
    plan.networks = networks;

    let mut volumes = Vec::with_capacity(required_volumes.len());
    for name in required_volumes {
        volumes.push(runtime::build_volume_plan_with_project_name(
            project.compose(),
            name,
            project_name.as_deref(),
        )?);
    }
    plan.volumes = volumes;
    Ok(plan)
}

fn validate_compose_features(compose: &DockerCompose) -> Result<()> {
    for (service_name, service) in &compose.services {
        let service_name = service_name.to_string();
        if service.build.is_some() {
            return Err(crate::Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "build",
            });
        }
        if service.deploy.is_some() {
            return Err(crate::Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "deploy",
            });
        }
        if service.configs.is_some() {
            return Err(crate::Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "configs",
            });
        }
        if service.secrets.is_some() {
            return Err(crate::Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "secrets",
            });
        }
        if service.network_mode.is_some() {
            return Err(crate::Error::UnsupportedServiceFeature {
                service: service_name,
                feature: "network_mode",
            });
        }
    }
    for (network_name, network) in &compose.networks {
        if let Some(NetworkConfig { ipam: Some(_), .. }) = network.as_ref() {
            return Err(crate::Error::UnsupportedNetworkFeature {
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
        return Err(crate::Error::DependencyCycle {
            service: service_name.to_string(),
        });
    }
    let service = compose
        .services
        .get(&docker_compose_spec::FieldKey::from(service_name))
        .ok_or_else(|| crate::Error::UnknownService {
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

use docker_compose_spec::ServiceDependsOn;
