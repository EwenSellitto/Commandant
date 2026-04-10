use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::docker_paths::{common_docker_socket_candidates, first_matching_path};
use crate::error::Result;
use crate::planner::{ExecutionPlan, ResolvedPort, ServiceNetworkAttachment, ServicePlan};

const PROXY_SERVICE_NAME: &str = "__commandant_traefik";

#[derive(Debug, Clone)]
pub struct ProxyConfig {
    pub enabled: bool,
    pub domain_suffix: String,
    pub host_port: u16,
    pub traefik_image: String,
    pub name: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            domain_suffix: "localhost".to_string(),
            host_port: 80,
            traefik_image: "traefik:v3.6.12".to_string(),
            name: "commandant-traefik".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProxyRoute {
    pub service_name: String,
    pub hostname: String,
    pub backend_port: u16,
}

#[derive(Debug, Clone)]
pub struct ProxySession {
    pub session_id: String,
    pub network_compose_name: String,
    pub network_runtime_name: String,
    pub container_name: String,
    pub host_port: u16,
    pub routes: Vec<ProxyRoute>,
    pub traefik_image: String,
}

pub fn prepare_proxy_session(
    plan: &mut ExecutionPlan,
    config: &ProxyConfig,
) -> Result<Option<ProxySession>> {
    if !config.enabled {
        return Ok(None);
    }

    let session_id = session_id();
    let network_compose_name = "commandant_proxy".to_string();
    let network_runtime_name = format!("commandant-proxy-{session_id}");
    let container_name = format!("{}-{session_id}", config.name);

    if !plan
        .networks
        .iter()
        .any(|network| network.compose_name == network_compose_name)
    {
        plan.networks.push(crate::planner::NetworkPlan {
            compose_name: network_compose_name.clone(),
            runtime_name: network_runtime_name.clone(),
            driver: "bridge".to_string(),
            external: false,
        });
    }

    let mut routes = Vec::new();
    for service in &mut plan.services {
        if service.name == PROXY_SERVICE_NAME {
            continue;
        }
        if let Some(route) = prepare_service_route(service, &network_runtime_name, config)? {
            routes.push(route);
        }
    }

    plan.services.insert(
        0,
        build_traefik_service(&network_runtime_name, &container_name, config),
    );

    Ok(Some(ProxySession {
        session_id,
        network_compose_name,
        network_runtime_name,
        container_name,
        host_port: config.host_port,
        routes,
        traefik_image: config.traefik_image.clone(),
    }))
}

fn build_traefik_service(
    network_runtime_name: &str,
    container_name: &str,
    config: &ProxyConfig,
) -> ServicePlan {
    ServicePlan {
        name: PROXY_SERVICE_NAME.to_string(),
        image: config.traefik_image.clone(),
        container_name: container_name.to_string(),
        command: Some(vec![
            "traefik".to_string(),
            "--providers.docker=true".to_string(),
            "--providers.docker.exposedbydefault=false".to_string(),
            format!("--providers.docker.network={network_runtime_name}"),
            "--providers.docker.endpoint=unix:///var/run/docker.sock".to_string(),
            "--entrypoints.web.address=:80".to_string(),
            "--api.dashboard=false".to_string(),
            "--ping=true".to_string(),
            "--ping.entrypoint=web".to_string(),
            "--log.level=INFO".to_string(),
        ]),
        entrypoint: None,
        environment: BTreeMap::new(),
        working_dir: None,
        hostname: None,
        user: None,
        labels: BTreeMap::new(),
        ports: vec![ResolvedPort {
            host_port: config.host_port,
            container_port: 80,
            protocol: "tcp".to_string(),
            host_ip: Some("127.0.0.1".to_string()),
        }],
        mounts: vec![crate::planner::ResolvedMount {
            compose_source: Some("/var/run/docker.sock".to_string()),
            source: detect_docker_socket().unwrap_or_else(|| "/var/run/docker.sock".to_string()),
            target: "/var/run/docker.sock".to_string(),
            kind: crate::planner::MountKind::Bind,
            read_only: true,
            anonymous: false,
        }],
        networks: vec![ServiceNetworkAttachment {
            compose_name: "commandant_proxy".to_string(),
            runtime_name: network_runtime_name.to_string(),
            aliases: vec![config.name.clone()],
        }],
        healthcheck: None,
        restart: crate::planner::RestartPolicy::No,
        memory_limit_bytes: None,
        cpu_shares: None,
        privileged: false,
        auto_remove: false,
        depends_on: Vec::new(),
    }
}

fn prepare_service_route(
    service: &mut ServicePlan,
    network_runtime_name: &str,
    config: &ProxyConfig,
) -> Result<Option<ProxyRoute>> {
    ensure_proxy_network(service, network_runtime_name);

    let Some(port) = service.ports.first().cloned() else {
        return Ok(None);
    };

    let hostname = format!(
        "{}.{}",
        sanitize_hostname_component(&service.name),
        config.domain_suffix
    );
    let router_id = sanitize_label_component(&format!("{}_{}", service.name, "router"));
    let service_id = sanitize_label_component(&format!("{}_{}", service.name, "service"));

    service
        .labels
        .insert("traefik.enable".to_string(), "true".to_string());
    service.labels.insert(
        "traefik.docker.network".to_string(),
        network_runtime_name.to_string(),
    );
    service.labels.insert(
        format!("traefik.http.routers.{router_id}.rule"),
        format!("Host(`{hostname}`)"),
    );
    service.labels.insert(
        format!("traefik.http.routers.{router_id}.entrypoints"),
        "web".to_string(),
    );
    service.labels.insert(
        format!("traefik.http.routers.{router_id}.service"),
        service_id.clone(),
    );
    service.labels.insert(
        format!("traefik.http.services.{service_id}.loadbalancer.server.port"),
        port.container_port.to_string(),
    );

    Ok(Some(ProxyRoute {
        service_name: service.name.clone(),
        hostname,
        backend_port: port.container_port,
    }))
}

fn ensure_proxy_network(service: &mut ServicePlan, network_runtime_name: &str) {
    let attachment = ServiceNetworkAttachment {
        compose_name: "commandant_proxy".to_string(),
        runtime_name: network_runtime_name.to_string(),
        aliases: Vec::new(),
    };

    if !service
        .networks
        .iter()
        .any(|network| network.runtime_name == attachment.runtime_name)
    {
        service.networks.push(attachment);
    }
}

fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn sanitize_hostname_component(value: &str) -> String {
    let mut output = String::new();
    let mut previous_dash = false;

    for character in value.chars().flat_map(|character| character.to_lowercase()) {
        let is_valid = character.is_ascii_lowercase() || character.is_ascii_digit();
        if is_valid {
            output.push(character);
            previous_dash = false;
        } else if !previous_dash {
            output.push('-');
            previous_dash = true;
        }
    }

    output.trim_matches('-').to_string()
}

fn sanitize_label_component(value: &str) -> String {
    sanitize_hostname_component(value).replace('-', "_")
}

fn detect_docker_socket() -> Option<String> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    detect_docker_socket_at(home)
}

fn detect_docker_socket_at(home: Option<std::path::PathBuf>) -> Option<String> {
    let home = home?;
    let candidates = common_docker_socket_candidates(&home, false);

    first_matching_path(candidates, is_socket_file).map(|path| path.display().to_string())
}

fn is_socket_file(path: &std::path::Path) -> bool {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                metadata.file_type().is_socket()
            }

            #[cfg(not(unix))]
            {
                metadata.is_file()
            }
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RestartPolicy;
    use std::collections::BTreeMap;

    fn service(
        name: &str,
        ports: Vec<ResolvedPort>,
        networks: Vec<ServiceNetworkAttachment>,
    ) -> ServicePlan {
        ServicePlan {
            name: name.to_string(),
            image: "nginx:latest".to_string(),
            container_name: format!("container_{name}"),
            command: None,
            entrypoint: None,
            environment: BTreeMap::new(),
            working_dir: None,
            hostname: None,
            user: None,
            labels: BTreeMap::new(),
            ports,
            mounts: Vec::new(),
            networks,
            healthcheck: None,
            restart: RestartPolicy::No,
            memory_limit_bytes: None,
            cpu_shares: None,
            privileged: false,
            auto_remove: false,
            depends_on: Vec::new(),
        }
    }

    fn base_plan() -> ExecutionPlan {
        ExecutionPlan {
            networks: vec![crate::planner::NetworkPlan {
                compose_name: "app".to_string(),
                runtime_name: "project_app".to_string(),
                driver: "bridge".to_string(),
                external: false,
            }],
            volumes: Vec::new(),
            services: vec![
                service(
                    "API.Service_1",
                    vec![ResolvedPort {
                        host_port: 8080,
                        container_port: 9000,
                        protocol: "tcp".to_string(),
                        host_ip: None,
                    }],
                    vec![ServiceNetworkAttachment {
                        compose_name: "app".to_string(),
                        runtime_name: "project_app".to_string(),
                        aliases: vec!["api.local".to_string()],
                    }],
                ),
                service(
                    "worker",
                    Vec::new(),
                    vec![ServiceNetworkAttachment {
                        compose_name: "app".to_string(),
                        runtime_name: "project_app".to_string(),
                        aliases: Vec::new(),
                    }],
                ),
            ],
            proxy: None,
        }
    }

    #[test]
    fn sanitizes_hostname_components() {
        assert_eq!(
            sanitize_hostname_component("API.Service_1"),
            "api-service-1"
        );
        assert_eq!(sanitize_label_component("API.Service_1"), "api_service_1");
    }

    #[test]
    fn prepare_proxy_session_adds_traefik_and_routes_backend_services() {
        let mut plan = base_plan();
        let config = ProxyConfig {
            enabled: true,
            domain_suffix: "example.test".to_string(),
            host_port: 8088,
            traefik_image: "traefik:test".to_string(),
            name: "commandant-traefik".to_string(),
        };

        let session = prepare_proxy_session(&mut plan, &config)
            .expect("prepare proxy session")
            .expect("proxy session enabled");

        assert!(session.session_id.contains('-'));
        assert_eq!(session.network_compose_name, "commandant_proxy");
        assert_eq!(session.host_port, 8088);
        assert_eq!(session.traefik_image, "traefik:test");
        assert_eq!(plan.networks.len(), 2);
        assert_eq!(plan.services[0].name, PROXY_SERVICE_NAME);

        let traefik = &plan.services[0];
        assert_eq!(traefik.image, "traefik:test");
        assert_eq!(traefik.ports[0].host_port, 8088);
        assert_eq!(traefik.ports[0].container_port, 80);
        assert_eq!(traefik.ports[0].host_ip.as_deref(), Some("127.0.0.1"));
        assert!(traefik.container_name.starts_with("commandant-traefik-"));
        assert_eq!(traefik.networks[0].compose_name, "commandant_proxy");
        assert_eq!(
            traefik.networks[0].runtime_name,
            session.network_runtime_name
        );
        assert_eq!(
            traefik.networks[0].aliases,
            vec!["commandant-traefik".to_string()]
        );
        assert!(traefik
            .command
            .as_ref()
            .expect("traefik command")
            .contains(&format!(
                "--providers.docker.network={}",
                session.network_runtime_name
            )));

        let api = plan
            .services
            .iter()
            .find(|service| service.name == "API.Service_1")
            .expect("api service exists");
        assert_eq!(api.networks[0].runtime_name, "project_app");
        assert_eq!(api.networks[1].runtime_name, session.network_runtime_name);
        assert_eq!(
            api.labels.get("traefik.enable").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            api.labels.get("traefik.docker.network").map(String::as_str),
            Some(session.network_runtime_name.as_str())
        );
        assert_eq!(
            api.labels
                .get("traefik.http.routers.api_service_1_router.rule")
                .map(String::as_str),
            Some("Host(`api-service-1.example.test`)")
        );
        assert_eq!(
            api.labels
                .get("traefik.http.routers.api_service_1_router.entrypoints")
                .map(String::as_str),
            Some("web")
        );
        assert_eq!(
            api.labels
                .get("traefik.http.routers.api_service_1_router.service")
                .map(String::as_str),
            Some("api_service_1_service")
        );
        assert_eq!(
            api.labels
                .get("traefik.http.services.api_service_1_service.loadbalancer.server.port")
                .map(String::as_str),
            Some("9000")
        );

        let worker = plan
            .services
            .iter()
            .find(|service| service.name == "worker")
            .expect("worker service exists");
        assert!(worker.labels.is_empty());
        assert_eq!(worker.networks.len(), 2);
        assert_eq!(
            worker.networks[1].runtime_name,
            session.network_runtime_name
        );
        assert!(session.routes.iter().any(|route| {
            route.service_name == "API.Service_1"
                && route.hostname == "api-service-1.example.test"
                && route.backend_port == 9000
        }));
        assert_eq!(session.routes.len(), 1);
    }

    #[test]
    fn prepare_proxy_session_is_noop_when_disabled() {
        let mut plan = base_plan();
        let config = ProxyConfig {
            enabled: false,
            ..ProxyConfig::default()
        };

        let session = prepare_proxy_session(&mut plan, &config).expect("prepare proxy session");

        assert!(session.is_none());
        assert_eq!(plan.networks.len(), 1);
        assert_eq!(plan.services.len(), 2);
        assert!(plan
            .services
            .iter()
            .all(|service| service.labels.is_empty()));
        assert!(plan.services.iter().all(|service| {
            service
                .networks
                .iter()
                .all(|network| network.runtime_name != "commandant-proxy")
        }));
    }

    #[test]
    fn detects_only_real_socket_paths() {
        let temp_home =
            std::env::temp_dir().join(format!("commandant-proxy-test-{}", std::process::id()));
        let docker_dir = temp_home.join(".docker/run");
        let _ = std::fs::create_dir_all(&docker_dir);
        let socket_path = docker_dir.join("docker.sock");

        let _listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .expect("create unix socket fixture");

        assert_eq!(
            detect_docker_socket_at(Some(temp_home.clone())),
            Some(socket_path.display().to_string())
        );

        let _ = std::fs::remove_dir_all(&temp_home);
    }
}
