use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::naming::sanitize_hostname_component;
use crate::planner::{ExecutionPlan, ServiceExposure, TagSource};
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
        let Some(expose) = service.labels.get(COMMANDANT_EXPOSE_LABEL) else {
            continue;
        };
        if !matches!(expose.as_str(), "1" | "true" | "yes" | "on") {
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
            None => service
                .ports
                .first()
                .map(|port| port.container_port)
                .ok_or_else(|| Error::MissingExposurePort {
                    service: service.name.clone(),
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
    }

    Ok(())
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
