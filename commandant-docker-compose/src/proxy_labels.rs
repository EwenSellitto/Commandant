use std::collections::BTreeMap;

use crate::error::Result;
use crate::naming::sanitize_label_component;
use crate::planner::{ResolvedMount, ResolvedPort, ServiceNetworkAttachment, ServicePlan};
use crate::proxy::{ProxyConfig, ProxyRoute};

pub(crate) const PROXY_SERVICE_NAME: &str = "__commandant_traefik";

pub(crate) fn build_traefik_service(
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
        exposure: None,
        ports: vec![ResolvedPort {
            host_port: config.host_port,
            container_port: 80,
            protocol: "tcp".to_string(),
            host_ip: Some("127.0.0.1".to_string()),
        }],
        mounts: vec![ResolvedMount {
            compose_source: Some("/var/run/docker.sock".to_string()),
            source: crate::proxy_socket::detect_docker_socket()
                .unwrap_or_else(|| "/var/run/docker.sock".to_string()),
            target: "/var/run/docker.sock".to_string(),
            kind: crate::planner::MountKind::Bind,
            read_only: true,
            anonymous: false,
        }],
        networks: vec![ServiceNetworkAttachment {
            compose_name: crate::planner::APP_NETWORK_NAME.to_string(),
            runtime_name: network_runtime_name.to_string(),
            aliases: vec![config.name.clone()],
            primary: true,
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

pub(crate) fn prepare_service_route(
    service: &mut ServicePlan,
    network_runtime_name: &str,
) -> Result<Option<ProxyRoute>> {
    ensure_proxy_network(service, network_runtime_name);

    let Some(exposure) = service.exposure.clone() else {
        return Ok(None);
    };
    let hostname = exposure.hostname.clone();
    let router_id = sanitize_label_component(&format!("{}_router", exposure.tag));
    let service_id = sanitize_label_component(&format!("{}_service", exposure.tag));

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
        exposure.backend_port.to_string(),
    );

    Ok(Some(ProxyRoute {
        service_name: service.name.clone(),
        tag: exposure.tag.clone(),
        hostname,
        backend_port: exposure.backend_port,
    }))
}

fn ensure_proxy_network(service: &mut ServicePlan, network_runtime_name: &str) {
    let attachment = ServiceNetworkAttachment {
        compose_name: crate::planner::APP_NETWORK_NAME.to_string(),
        runtime_name: network_runtime_name.to_string(),
        aliases: Vec::new(),
        primary: false,
    };
    if !service
        .networks
        .iter()
        .any(|network| network.runtime_name == attachment.runtime_name)
    {
        service.networks.push(attachment);
    }
}
