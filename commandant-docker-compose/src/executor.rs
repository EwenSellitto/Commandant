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
    ExecutionPlan, MountKind, NetworkPlan, ResolvedHealthcheck, RestartPolicy, ServicePlan,
    VolumePlan, build_plan,
};

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
        build_plan(project, active_profiles)
    }

    pub async fn up<'a>(
        &'a self,
        project: &ComposeProject,
        active_profiles: &[String],
    ) -> Result<RunningProject<'a>> {
        let plan = self.plan(project, active_profiles)?;
        self.client.ping().await?;

        let networks = self.ensure_networks(&plan.networks).await?;
        let volumes = self.ensure_volumes(&plan.volumes).await?;

        let mut containers = BTreeMap::new();
        for service in &plan.services {
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

    async fn ensure_networks(&self, plans: &[NetworkPlan]) -> Result<BTreeMap<String, String>> {
        let existing = self.client.networks().list().await?;
        let mut known = existing
            .into_iter()
            .filter_map(|network| Some((network.name?, network.id?)))
            .collect::<BTreeMap<_, _>>();

        let mut created = BTreeMap::new();
        for plan in plans {
            let id = if let Some(id) = known.get(&plan.runtime_name) {
                id.clone()
            } else if plan.external {
                plan.runtime_name.to_string()
            } else {
                let id = self
                    .client
                    .networks()
                    .create(&plan.runtime_name, &plan.driver)
                    .await?;
                known.insert(plan.runtime_name.to_string(), id.clone());
                id
            };
            created.insert(plan.compose_name.to_string(), id);
        }

        Ok(created)
    }

    async fn ensure_volumes(&self, plans: &[VolumePlan]) -> Result<BTreeMap<String, String>> {
        let existing = self.client.volumes().list().await?;
        let mut known = existing
            .into_iter()
            .map(|volume| {
                let name = volume.name;
                (name.clone(), name)
            })
            .collect::<BTreeMap<_, _>>();

        let mut created = BTreeMap::new();
        for plan in plans {
            let name = if let Some(name) = known.get(&plan.runtime_name) {
                name.clone()
            } else if plan.external {
                plan.runtime_name.to_string()
            } else {
                let volume = self.client.volumes().create(&plan.runtime_name).await?;
                let name = volume.name;
                known.insert(name.clone(), name.clone());
                name
            };
            created.insert(plan.compose_name.to_string(), name);
        }

        Ok(created)
    }

    async fn create_container<'a>(&'a self, service: &ServicePlan) -> Result<ContainerRef<'a>> {
        let env = (!service.environment.is_empty()).then(|| {
            service
                .environment
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
        });

        let labels = (!service.labels.is_empty()).then(|| {
            service
                .labels
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        });

        let exposed_ports = (!service.ports.is_empty()).then(|| {
            service
                .ports
                .iter()
                .map(|port| {
                    (
                        format!("{}/{}", port.container_port, port.protocol),
                        Default::default(),
                    )
                })
                .collect()
        });

        let port_bindings = (!service.ports.is_empty()).then(|| {
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
                .collect()
        });

        let binds = (!service.mounts.is_empty()).then(|| {
            service
                .mounts
                .iter()
                .map(|mount| match mount.kind {
                    MountKind::Bind | MountKind::Volume => {
                        let mut binding = if mount.source.is_empty() {
                            mount.target.clone()
                        } else {
                            format!("{}:{}", mount.source, mount.target)
                        };
                        if mount.read_only {
                            binding.push_str(":ro");
                        }
                        binding
                    }
                })
                .collect::<Vec<_>>()
        });

        let networking_config = service.networks.first().map(|network| NetworkingConfig {
            endpoints_config: Some(
                [(
                    network.runtime_name.clone(),
                    EndpointSettings {
                        aliases: (!network.aliases.is_empty()).then(|| network.aliases.clone()),
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
