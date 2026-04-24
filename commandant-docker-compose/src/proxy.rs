use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;
use crate::planner::ExecutionPlan;
use crate::proxy_labels::{PROXY_SERVICE_NAME, build_traefik_service, prepare_service_route};

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
    pub tag: String,
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
    let network_compose_name = plan.app_network.compose_name.clone();
    let network_runtime_name = plan.app_network.runtime_name.clone();
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
        if let Some(route) = prepare_service_route(service, &network_runtime_name)? {
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

fn session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}
