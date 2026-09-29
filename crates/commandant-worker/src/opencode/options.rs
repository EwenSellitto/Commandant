//! What the agent offers: its agents, models and their thinking efforts.

use anyhow::Result;
use commandant_proto::{AgentChoice, AgentOptions, ModelChoice};

use super::Opencode;
use super::api::{Agent, Providers};

/// OpenCode's own default agent.
const BUILD: &str = "build";

/// Answers a ListAgentOptions; failures go in the answer's `error`.
pub async fn list(opencode: &Opencode, request_id: String) -> AgentOptions {
    let options = gather(opencode).await.unwrap_or_else(|e| AgentOptions {
        error: format!("{e:#}"),
        ..Default::default()
    });
    AgentOptions {
        request_id,
        ..options
    }
}

async fn gather(opencode: &Opencode) -> Result<AgentOptions> {
    let api = opencode.api().await?;
    let (agents, providers, config) =
        tokio::try_join!(api.agents(), api.providers(), api.config())?;
    let agents = promptable(agents);
    let default_agent = match config.default_agent {
        Some(name) if !name.is_empty() => name,
        _ if agents.iter().any(|a| a.name == BUILD) => BUILD.into(),
        _ => agents.first().map(|a| a.name.clone()).unwrap_or_default(),
    };
    Ok(AgentOptions {
        agents,
        models: models(providers),
        default_agent,
        default_model: config.model.unwrap_or_default(),
        ..Default::default()
    })
}

/// The agents a prompt can use: not subagents, not OpenCode's hidden helpers.
fn promptable(agents: Vec<Agent>) -> Vec<AgentChoice> {
    agents
        .into_iter()
        .filter(|a| a.mode != "subagent" && a.hidden != Some(true))
        .map(|a| AgentChoice {
            name: a.name,
            description: a.description.unwrap_or_default(),
        })
        .collect()
}

/// Every usable model, by provider then name, efforts from least to most.
fn models(providers: Providers) -> Vec<ModelChoice> {
    let mut choices = Vec::new();
    for provider in providers.providers {
        let mut models: Vec<_> = provider
            .models
            .into_values()
            .filter(|m| m.status.as_deref() != Some("deprecated"))
            .collect();
        models.sort_by(|a, b| a.name.cmp(&b.name));
        choices.extend(models.into_iter().map(|model| {
            let mut variants: Vec<_> = model.variants.unwrap_or_default().into_keys().collect();
            variants.sort_by_key(|v| effort_rank(v));
            ModelChoice {
                id: format!("{}/{}", provider.id, model.id),
                name: model.name,
                provider: provider.name.clone(),
                variants,
                context: model.limit.and_then(|l| l.context).unwrap_or_default(),
            }
        }));
    }
    choices
}

/// Orders the usual effort names; others go last, as they came.
fn effort_rank(variant: &str) -> usize {
    const ORDER: [&str; 8] = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "thinking",
    ];
    ORDER
        .iter()
        .position(|&v| v == variant)
        .unwrap_or(ORDER.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_primary_agents_and_orders_models_and_efforts() {
        let agents: Vec<Agent> = serde_json::from_value(json!([
            { "name": "build", "mode": "primary", "description": null, "hidden": null },
            { "name": "explore", "mode": "subagent" },
            { "name": "title", "mode": "primary", "hidden": true },
            { "name": "plan", "mode": "all", "description": "Plan mode." },
        ]))
        .unwrap();
        let names: Vec<_> = promptable(agents).into_iter().map(|a| a.name).collect();
        assert_eq!(names, ["build", "plan"]);

        let providers: Providers = serde_json::from_value(json!({ "providers": [{
            "id": "acme", "name": "Acme", "models": {
                "z1": { "id": "z1", "name": "Zeta", "variants": { "high": {}, "low": {}, "max": {} } },
                "a1": { "id": "a1", "name": "Alpha" },
                "old": { "id": "old", "name": "Old", "status": "deprecated" },
            }
        }]}))
        .unwrap();
        let models = models(providers);
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["acme/a1", "acme/z1"]);
        assert_eq!(models[1].variants, ["low", "high", "max"]);
        assert_eq!(models[1].provider, "Acme");
    }
}
