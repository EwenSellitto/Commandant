use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::naming::sanitize_hostname_component;
use crate::planner::{ExecutionPlan, ServiceExposure, ServicePlan, TagSource};
use crate::proxy::ProxyConfig;

const COMMANDANT_EXPOSE_LABEL: &str = "com.commandant.expose";
const COMMANDANT_TAG_LABEL: &str = "com.commandant.tag";
const COMMANDANT_PORT_LABEL: &str = "com.commandant.port";

pub(crate) fn resolve_service_exposure(
    plan: &mut ExecutionPlan,
    config: &ProxyConfig,
) -> Result<()> {
    let mut seen = BTreeMap::new();

    for service in &mut plan.services {
        if !should_expose_service(service) {
            continue;
        }

        let raw_tag = service
            .labels
            .get(COMMANDANT_TAG_LABEL)
            .cloned()
            .unwrap_or_else(|| auto_tag(&service.name));
        let tag = sanitize_hostname_component(&raw_tag);
        if tag.is_empty() {
            return Err(Error::InvalidTag {
                service: service.name.clone(),
                tag: raw_tag,
            });
        }

        if let Some(first) = seen.insert(tag.clone(), service.name.clone()) {
            return Err(Error::DuplicateTag {
                tag,
                first,
                second: service.name.clone(),
            });
        }

        let backend_port = match service.labels.get(COMMANDANT_PORT_LABEL) {
            Some(value) => value
                .parse::<u16>()
                .map_err(|_| Error::InvalidExposurePort {
                    service: service.name.clone(),
                    value: value.clone(),
                })?,
            None => service.exposed_ports.first().copied().ok_or_else(|| {
                Error::MissingExposurePort {
                    service: service.name.clone(),
                }
            })?,
        };

        service.exposure = Some(ServiceExposure {
            enabled: true,
            tag: tag.clone(),
            hostname: format!("{tag}.{}", config.domain_suffix),
            backend_port,
            source: if service.labels.contains_key(COMMANDANT_TAG_LABEL) {
                TagSource::ExplicitTag
            } else {
                TagSource::AutoTag
            },
        });
        service.ports.clear();
    }

    Ok(())
}

fn should_expose_service(service: &ServicePlan) -> bool {
    matches!(
        service
            .labels
            .get(COMMANDANT_EXPOSE_LABEL)
            .map(String::as_str),
        Some("1" | "true" | "yes" | "on")
    ) || (!service.labels.contains_key(COMMANDANT_EXPOSE_LABEL)
        && !service.exposed_ports.is_empty())
}

fn auto_tag(service_name: &str) -> String {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    service_name.hash(&mut hasher);
    let suffix = format!("{:x}", hasher.finish());
    format!(
        "{}-{}",
        sanitize_hostname_component(service_name),
        &suffix[..6.min(suffix.len())]
    )
}
