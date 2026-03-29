use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use docker_compose_spec::{
    Command, ComposeService, DockerCompose, FieldKey, IntOrStr, Network, NetworkConfig,
    NonEmptyKey, ServiceNetwork, ServiceNetworks, ServicePort, ServiceVolume, VecOrMap,
    VecOrMapVal,
};

use crate::error::{Error, Result};
use crate::parser::{self, parse_str};

#[derive(Debug, Clone)]
pub struct ComposeProject {
    compose: DockerCompose,
    base_dir: PathBuf,
}

impl ComposeProject {
    pub fn from_yaml_strl_str(yaml: &str) -> Result<Self> {
        Ok(Self {
            compose: parse_str(yaml)?,
            base_dir: PathBuf::from("."),
        })
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let (compose, base_dir) = parser::parse_file(path)?;
        Ok(Self { compose, base_dir })
    }

    pub fn new(compose: DockerCompose, base_dir: impl Into<PathBuf>) -> Self {
        Self {
            compose,
            base_dir: base_dir.into(),
        }
    }

    pub fn compose(&self) -> &DockerCompose {
        &self.compose
    }

    pub fn compose_mut(&mut self) -> &mut DockerCompose {
        &mut self.compose
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn to_yaml(&self) -> Result<String> {
        parser::to_yaml(&self.compose)
    }

    pub fn service(&self, service: &str) -> Option<&ComposeService> {
        self.compose.services.get(&FieldKey::from(service))
    }

    pub fn service_mut(&mut self, service: &str) -> Result<&mut ComposeService> {
        self.compose
            .services
            .get_mut(&FieldKey::from(service))
            .ok_or_else(|| Error::UnknownService {
                service: service.to_string(),
            })
    }

    pub fn set_image(&mut self, service: &str, image: impl Into<String>) -> Result<()> {
        self.service_mut(service)?.image = Some(image.into());
        Ok(())
    }

    pub fn set_command(&mut self, service: &str, command: Vec<String>) -> Result<()> {
        self.service_mut(service)?.command = Some(Command::Vec(command));
        Ok(())
    }

    pub fn set_entrypoint(&mut self, service: &str, entrypoint: Vec<String>) -> Result<()> {
        self.service_mut(service)?.entrypoint = Some(Command::Vec(entrypoint));
        Ok(())
    }

    pub fn set_env(
        &mut self,
        service: &str,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<()> {
        let service = self.service_mut(service)?;
        let key = NonEmptyKey::from(key.into().as_str());
        let value = VecOrMapVal::String(value.into());

        let env = service
            .environment
            .get_or_insert_with(|| VecOrMap::Map(BTreeMap::new()));
        match env {
            VecOrMap::Map(entries) => {
                entries.insert(key, value);
            }
            VecOrMap::Vec(values) => {
                let needle = format!("{}=", key.as_str());
                values.retain(|entry| !entry.starts_with(&needle));
                values.push(format!("{}={}", key.as_str(), stringify_env_value(&value)));
            }
        }

        Ok(())
    }

    pub fn remove_env(&mut self, service: &str, key: &str) -> Result<()> {
        let service = self.service_mut(service)?;
        if let Some(env) = service.environment.as_mut() {
            match env {
                VecOrMap::Map(entries) => {
                    entries.remove(&NonEmptyKey::from(key));
                }
                VecOrMap::Vec(values) => {
                    let needle = format!("{key}=");
                    values.retain(|entry| !entry.starts_with(&needle) && entry != key);
                }
            }
        }
        Ok(())
    }

    pub fn set_ports(&mut self, service: &str, ports: Vec<PortMapping>) -> Result<()> {
        let service = self.service_mut(service)?;
        service.ports = Some(ports.into_iter().map(Into::into).collect());
        Ok(())
    }

    pub fn add_port(&mut self, service: &str, port: PortMapping) -> Result<()> {
        let service = self.service_mut(service)?;
        service.ports.get_or_insert_with(Vec::new).push(port.into());
        Ok(())
    }

    pub fn attach_network(&mut self, service: &str, network: &str) -> Result<()> {
        self.compose
            .networks
            .entry(FieldKey::from(network))
            .or_insert_with(|| Network::from(Option::<NetworkConfig>::None));

        let service = self.service_mut(service)?;
        match service
            .networks
            .get_or_insert_with(|| ServiceNetworks::Vec(Vec::new()))
        {
            ServiceNetworks::Vec(networks) => {
                if !networks.iter().any(|existing| existing == network) {
                    networks.push(network.to_string());
                }
            }
            ServiceNetworks::Map(networks) => {
                networks
                    .entry(FieldKey::from(network))
                    .or_insert_with(default_service_network);
            }
        }

        Ok(())
    }

    pub fn set_network_aliases(
        &mut self,
        service: &str,
        network: &str,
        aliases: Vec<String>,
    ) -> Result<()> {
        self.attach_network(service, network)?;
        let service = self.service_mut(service)?;

        let networks = match service.networks.as_mut() {
            Some(ServiceNetworks::Map(networks)) => networks,
            Some(ServiceNetworks::Vec(existing)) => {
                let mapped = existing
                    .iter()
                    .map(|name| (FieldKey::from(name.as_str()), default_service_network()))
                    .collect();
                service.networks = Some(ServiceNetworks::Map(mapped));
                match service.networks.as_mut() {
                    Some(ServiceNetworks::Map(networks)) => networks,
                    _ => unreachable!(),
                }
            }
            None => unreachable!(),
        };

        networks
            .entry(FieldKey::from(network))
            .or_insert_with(default_service_network)
            .aliases = if aliases.is_empty() {
            None
        } else {
            Some(aliases)
        };

        Ok(())
    }

    pub fn add_named_volume(
        &mut self,
        service: &str,
        source: impl Into<String>,
        target: impl Into<String>,
        read_only: bool,
    ) -> Result<()> {
        let source = source.into();
        self.ensure_volume(&source);
        let service = self.service_mut(service)?;
        service
            .volumes
            .get_or_insert_with(Vec::new)
            .push(build_service_volume(
                "volume",
                Some(source),
                target.into(),
                read_only,
            ));

        Ok(())
    }

    pub fn add_bind_mount(
        &mut self,
        service: &str,
        source: impl Into<String>,
        target: impl Into<String>,
        read_only: bool,
    ) -> Result<()> {
        let service = self.service_mut(service)?;
        service
            .volumes
            .get_or_insert_with(Vec::new)
            .push(build_service_volume(
                "bind",
                Some(source.into()),
                target.into(),
                read_only,
            ));

        Ok(())
    }

    pub fn enable_profile(&mut self, service: &str, profile: impl Into<String>) -> Result<()> {
        let service = self.service_mut(service)?;
        let profile = profile.into();
        let profiles = service.profiles.get_or_insert_with(Vec::new);
        if !profiles.iter().any(|existing| existing == &profile) {
            profiles.push(profile);
        }
        Ok(())
    }

    pub fn disable_profile(&mut self, service: &str, profile: &str) -> Result<()> {
        let service = self.service_mut(service)?;
        if let Some(profiles) = service.profiles.as_mut() {
            profiles.retain(|existing| existing != profile);
            if profiles.is_empty() {
                service.profiles = None;
            }
        }
        Ok(())
    }

    fn ensure_volume(&mut self, source: &str) {
        self.compose
            .volumes
            .entry(FieldKey::from(source))
            .or_insert_with(|| {
                docker_compose_spec::Volume::from(docker_compose_spec::VolumeConfig {
                    driver: None,
                    driver_opts: BTreeMap::new(),
                    external: None,
                    labels: None,
                    name: None,
                })
            });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
    pub host_ip: Option<String>,
}

impl PortMapping {
    pub fn tcp(host_port: u16, container_port: u16) -> Self {
        Self {
            host_port,
            container_port,
            protocol: "tcp".to_string(),
            host_ip: None,
        }
    }
}

impl From<PortMapping> for ServicePort {
    fn from(value: PortMapping) -> Self {
        ServicePort::Object {
            app_protocol: None,
            host_ip: value.host_ip,
            mode: None,
            name: None,
            protocol: Some(value.protocol),
            published: Some(IntOrStr::Integer(i64::from(value.host_port))),
            target: Some(IntOrStr::Integer(i64::from(value.container_port))),
        }
    }
}

fn default_service_network() -> ServiceNetwork {
    ServiceNetwork {
        aliases: None,
        driver_opts: BTreeMap::new(),
        ipv4_address: None,
        ipv6_address: None,
        link_local_ips: None,
        mac_address: None,
        priority: None,
    }
}

fn stringify_env_value(value: &VecOrMapVal) -> String {
    match value {
        VecOrMapVal::Null => String::new(),
        VecOrMapVal::Boolean(value) => value.to_string(),
        VecOrMapVal::Number(value) => value.to_string(),
        VecOrMapVal::String(value) => value.clone(),
    }
}

fn build_service_volume(
    type_: &str,
    source: Option<String>,
    target: String,
    read_only: bool,
) -> ServiceVolume {
    ServiceVolume::Object {
        bind: None,
        consistency: None,
        read_only: read_only.then_some(docker_compose_spec::BoolOrStr::Boolean(true)),
        source,
        target: Some(target),
        tmpfs: None,
        type_: type_.to_string(),
        volume: Box::default(),
    }
}
