use std::collections::BTreeMap;

use bollard::models::{
    ContainerCreateBody, EndpointSettings, HealthConfig, HostConfig, NetworkingConfig, PortBinding,
    RestartPolicy as DockerRestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::CreateContainerOptionsBuilder;
use lmrc_docker::{ContainerRef, DockerClient};

use crate::error::Result;
use crate::model::ComposeProject;
use crate::planner::{
    ExecutionPlan, NetworkPlan, ResolvedHealthcheck, RestartPolicy, ServicePlan, VolumePlan,
    build_plan_with_proxy,
};
use crate::proxy::ProxyConfig;

pub struct ComposeExecutor {
    client: DockerClient,
}

pub struct RunningProject<'a> {
    pub plan: ExecutionPlan,
    pub networks: BTreeMap<String, String>,
    pub volumes: BTreeMap<String, String>,
    pub containers: BTreeMap<String, ContainerRef<'a>>,
}

impl ComposeExecutor {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: DockerClient::new()?,
        })
    }

    pub fn from_client(client: DockerClient) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &DockerClient {
        &self.client
    }

    pub fn plan(
        &self,
        project: &ComposeProject,
        active_profiles: &[String],
    ) -> Result<ExecutionPlan> {
        self.plan_with_proxy(project, active_profiles, &ProxyConfig::default())
    }

    pub fn plan_with_proxy(
        &self,
        project: &ComposeProject,
        active_profiles: &[String],
        proxy_config: &ProxyConfig,
    ) -> Result<ExecutionPlan> {
        build_plan_with_proxy(project, active_profiles, proxy_config)
    }

    pub async fn up<'a>(
        &'a self,
        project: &ComposeProject,
        active_profiles: &[String],
    ) -> Result<RunningProject<'a>> {
        self.up_with_proxy(project, active_profiles, &ProxyConfig::default())
            .await
    }

    pub async fn up_with_proxy<'a>(
        &'a self,
        project: &ComposeProject,
        active_profiles: &[String],
        proxy_config: &ProxyConfig,
    ) -> Result<RunningProject<'a>> {
        let plan = self.plan_with_proxy(project, active_profiles, proxy_config)?;
        self.client.ping().await?;

        let mut runtime_networks = Vec::with_capacity(plan.networks.len() + 1);
        runtime_networks.push(plan.app_network.clone());
        runtime_networks.extend(plan.networks.iter().cloned());

        let (networks, created_network_ids) = self.ensure_networks(&runtime_networks).await?;
        let (volumes, created_volume_names) = match self.ensure_volumes(&plan.volumes).await {
            Ok(result) => result,
            Err(error) => {
                self.remove_networks(&created_network_ids).await;
                return Err(error);
            }
        };

        let mut containers = BTreeMap::new();
        for service in &plan.services {
            let result: Result<ContainerRef<'a>> = async {
                self.ensure_image(&service.image).await?;
                let container = self.create_container(service).await?;
                container.start().await?;

                for attachment in service.networks.iter().skip(1) {
                    self.client
                        .networks()
                        .get(&attachment.runtime_name)
                        .connect(container.id())
                        .await?;
                }

                Ok(container)
            }
            .await;

            let container = match result {
                Ok(container) => container,
                Err(error) => {
                    self.cleanup_partial(
                        &created_network_ids,
                        &created_volume_names,
                        &containers,
                    )
                        .await;
                    return Err(error);
                }
            };

            containers.insert(service.name.to_string(), container);
        }

        Ok(RunningProject {
            plan,
            networks,
            volumes,
            containers,
        })
    }

    pub async fn down(&self, running: RunningProject<'_>) -> Result<()> {
        for (service_name, container) in running.containers.iter().rev() {
            let remove_anonymous_volumes = running
                .plan
                .service(service_name)
                .map(|service| service.mounts.iter().any(|mount| mount.anonymous))
                .unwrap_or(false);
            let _ = container.stop(Some(10)).await;
            let _ = container.remove(true, remove_anonymous_volumes).await;
        }

        for network in running.plan.networks.iter().rev() {
            if !network.external {
                let _ = self
                    .client
                    .networks()
                    .get(&network.runtime_name)
                    .remove()
                    .await;
            }
        }

        Ok(())
    }

    async fn ensure_image(&self, image: &str) -> Result<()> {
        if self.client.images().get(image).inspect().await.is_ok() {
            return Ok(());
        }

        self.client.images().pull(image, None).await?;
        Ok(())
    }

    async fn ensure_networks(
        &self,
        plans: &[NetworkPlan],
    ) -> Result<(BTreeMap<String, String>, Vec<String>)> {
        let existing = self.client.networks().list().await?;
        let mut known = BTreeMap::new();
        for network in existing {
            if let (Some(name), Some(id)) = (network.name, network.id) {
                known.insert(name, id);
            }
        }

        let mut created = BTreeMap::new();
        let mut created_ids = Vec::new();
        for plan in plans {
            let id = if let Some(id) = known.get(&plan.runtime_name) {
                id.clone()
            } else if plan.external {
                plan.runtime_name.to_string()
            } else {
                let id = match self
                    .client
                    .networks()
                    .create(&plan.runtime_name, &plan.driver)
                    .await
                {
                    Ok(id) => id,
                    Err(error) => {
                        self.remove_networks(&created_ids).await;
                        return Err(error.into());
                    }
                };
                created_ids.push(id.clone());
                known.insert(plan.runtime_name.to_string(), id.clone());
                id
            };
            created.insert(plan.compose_name.to_string(), id);
        }

        Ok((created, created_ids))
    }

    async fn ensure_volumes(
        &self,
        plans: &[VolumePlan],
    ) -> Result<(BTreeMap<String, String>, Vec<String>)> {
        let existing = self.client.volumes().list().await?;
        let mut known = existing
            .into_iter()
            .map(|volume| {
                let name = volume.name;
                (name.clone(), name)
            })
            .collect::<BTreeMap<_, _>>();

        let mut created = BTreeMap::new();
        let mut created_names = Vec::new();
        for plan in plans {
            let name = if let Some(name) = known.get(&plan.runtime_name) {
                name.clone()
            } else if plan.external {
                plan.runtime_name.to_string()
            } else {
                let volume = match self.client.volumes().create(&plan.runtime_name).await {
                    Ok(volume) => volume,
                    Err(error) => {
                        self.remove_volumes(&created_names).await;
                        return Err(error.into());
                    }
                };
                let name = volume.name;
                created_names.push(name.clone());
                known.insert(name.clone(), name.clone());
                name
            };
            created.insert(plan.compose_name.to_string(), name);
        }

        Ok((created, created_names))
    }

    async fn create_container<'a>(&'a self, service: &ServicePlan) -> Result<ContainerRef<'a>> {
        let env = if service.environment.is_empty() {
            None
        } else {
            Some(
                service
                    .environment
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect::<Vec<_>>(),
            )
        };

        let labels = if service.labels.is_empty() {
            None
        } else {
            Some(
                service
                    .labels
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            )
        };

        let exposed_ports = if service.ports.is_empty() {
            None
        } else {
            Some(
                service
                    .ports
                    .iter()
                    .map(|port| {
                        (
                            format!("{}/{}", port.container_port, port.protocol),
                            Default::default(),
                        )
                    })
                    .collect(),
            )
        };

        let port_bindings = if service.ports.is_empty() {
            None
        } else {
            Some(
                service
                    .ports
                    .iter()
                    .map(|port| {
                        (
                            format!("{}/{}", port.container_port, port.protocol),
                            Some(vec![PortBinding {
                                host_ip: Some(
                                    port.host_ip
                                        .clone()
                                        .unwrap_or_else(|| "0.0.0.0".to_string()),
                                ),
                                host_port: Some(port.host_port.to_string()),
                            }]),
                        )
                    })
                    .collect(),
            )
        };

        let binds = if service.mounts.is_empty() {
            None
        } else {
            Some(
                service
                    .mounts
                    .iter()
                    .map(|mount| {
                        let mut binding = if mount.source.is_empty() {
                            mount.target.clone()
                        } else {
                            format!("{}:{}", mount.source, mount.target)
                        };
                        if mount.read_only {
                            binding.push_str(":ro");
                        }
                        binding
                    })
                    .collect::<Vec<_>>(),
            )
        };

        let networking_config = service.networks.first().map(|network| NetworkingConfig {
            endpoints_config: Some(
                [(
                    network.runtime_name.clone(),
                    EndpointSettings {
                        aliases: if network.aliases.is_empty() {
                            None
                        } else {
                            Some(network.aliases.clone())
                        },
                        ..Default::default()
                    },
                )]
                .into_iter()
                .collect(),
            ),
        });

        let host_config = HostConfig {
            binds,
            port_bindings,
            restart_policy: docker_restart_policy(service.restart),
            memory: service.memory_limit_bytes,
            cpu_shares: service.cpu_shares,
            privileged: service.privileged.then_some(true),
            auto_remove: service.auto_remove.then_some(true),
            ..Default::default()
        };

        let config = ContainerCreateBody {
            image: Some(service.image.clone()),
            env,
            cmd: service.command.clone(),
            entrypoint: service.entrypoint.clone(),
            working_dir: service.working_dir.clone(),
            labels,
            exposed_ports,
            networking_config,
            healthcheck: service.healthcheck.as_ref().map(to_docker_healthcheck),
            host_config: Some(host_config),
            ..Default::default()
        };

        let response = self
            .client
            .inner()
            .create_container(
                Some(
                    CreateContainerOptionsBuilder::new()
                        .name(&service.container_name)
                        .build(),
                ),
                config,
            )
            .await
            .map_err(|error| {
                lmrc_docker::DockerError::ContainerOperationFailed(error.to_string())
            })?;

        Ok(self.client.containers().get(response.id))
    }

    async fn cleanup_partial<'a>(
        &self,
        created_network_ids: &[String],
        created_volume_names: &[String],
        containers: &BTreeMap<String, ContainerRef<'a>>,
    ) {
        for container in containers.values().rev() {
            let _ = container.stop(Some(10)).await;
            let _ = container.remove(true, true).await;
        }

        self.remove_networks(created_network_ids).await;
        self.remove_volumes(created_volume_names).await;
    }

    async fn remove_networks(&self, ids: &[String]) {
        for id in ids.iter().rev() {
            let _ = self.client.networks().get(id).remove().await;
        }
    }

    async fn remove_volumes(&self, names: &[String]) {
        for name in names.iter().rev() {
            let _ = self.client.volumes().get(name).remove(true).await;
        }
    }
}

fn docker_restart_policy(restart: RestartPolicy) -> Option<DockerRestartPolicy> {
    match restart {
        RestartPolicy::Always => Some(DockerRestartPolicy {
            name: Some(RestartPolicyNameEnum::ALWAYS),
            maximum_retry_count: None,
        }),
        RestartPolicy::OnFailure => Some(DockerRestartPolicy {
            name: Some(RestartPolicyNameEnum::ON_FAILURE),
            maximum_retry_count: None,
        }),
        RestartPolicy::UnlessStopped => Some(DockerRestartPolicy {
            name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
            maximum_retry_count: None,
        }),
        RestartPolicy::No => None,
    }
}

fn to_docker_healthcheck(healthcheck: &ResolvedHealthcheck) -> HealthConfig {
    HealthConfig {
        test: healthcheck.test.clone(),
        interval: healthcheck.interval_ns,
        timeout: healthcheck.timeout_ns,
        retries: healthcheck.retries,
        start_period: healthcheck.start_period_ns,
        start_interval: healthcheck.start_interval_ns,
    }
}
