//! Signing OpenCode in to model providers: an API key, or one of the OAuth
//! methods some providers have.

use std::collections::HashMap;

use anyhow::{Result, bail};
use commandant_proto::{
    AuthAction, AuthMethod, ModelProvider, OauthFinish, ProviderAuthResult, auth_action,
};

use super::Opencode;
use super::api;

/// Every provider, the signed-in ones first, then by name.
pub async fn providers(opencode: &Opencode) -> Result<Vec<ModelProvider>> {
    let api = opencode.api().await?;
    let (all, methods) = tokio::try_join!(api.all_providers(), api.auth_methods())?;
    Ok(listed(all, methods))
}

fn listed(
    all: api::AllProviders,
    mut methods: HashMap<String, Vec<api::AuthMethod>>,
) -> Vec<ModelProvider> {
    let mut providers: Vec<ModelProvider> = all
        .all
        .into_iter()
        .map(|p| {
            let methods = match methods.remove(&p.id) {
                Some(methods) => methods
                    .into_iter()
                    .enumerate()
                    .map(|(index, m)| AuthMethod {
                        oauth: m.kind == "oauth",
                        label: m.label,
                        index: index as u32,
                    })
                    .collect(),
                None => vec![AuthMethod {
                    label: "API key".into(),
                    ..Default::default()
                }],
            };
            ModelProvider {
                connected: all.connected.contains(&p.id),
                id: p.id,
                name: p.name,
                methods,
            }
        })
        .collect();
    providers.sort_by_key(|p| (!p.connected, p.name.to_lowercase()));
    providers
}

/// Does one step of signing in or out.
pub async fn authenticate(
    opencode: &Opencode,
    provider: &str,
    action: Option<AuthAction>,
) -> Result<ProviderAuthResult> {
    let api = opencode.api().await?;
    let mut result = ProviderAuthResult::default();
    match action.and_then(|a| a.action) {
        Some(auth_action::Action::ApiKey(key)) => {
            if key.trim().is_empty() {
                bail!("the API key is empty");
            }
            api.set_api_key(provider, key.trim()).await?;
        }
        Some(auth_action::Action::OauthStart(index)) => {
            let methods = api.auth_methods().await?;
            let Some(method) = methods.get(provider).and_then(|m| m.get(index as usize)) else {
                bail!("{provider} has no sign-in method {index}");
            };
            let authorization = api
                .oauth_authorize(provider, index, &inputs(method))
                .await?;
            result.url = authorization.url;
            result.instructions = authorization.instructions;
            result.needs_code = authorization.method == "code";
            // Nothing has changed yet.
            return Ok(result);
        }
        Some(auth_action::Action::OauthFinish(OauthFinish { index, code })) => {
            api.oauth_callback(provider, index, code.trim()).await?;
        }
        Some(auth_action::Action::SignOut(_)) => api.remove_auth(provider).await?,
        None => bail!("nothing to do: no sign-in action given"),
    }
    opencode.credentials_changed();
    Ok(result)
}

/// Answers to what a method asks before starting.
// ponytail: takes each select's first choice and leaves text out (GitHub.com
// over Enterprise, say); ask the user when someone needs the others.
fn inputs(method: &api::AuthMethod) -> HashMap<String, String> {
    method
        .prompts
        .iter()
        .filter_map(|p| Some((p.key.clone(), p.options.first()?.value.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn providers_list_how_to_sign_in_signed_in_first() {
        let all = serde_json::from_value(json!({
            "all": [
                { "id": "zeta", "name": "Zeta" },
                { "id": "openai", "name": "OpenAI" },
                { "id": "acme", "name": "acme" },
            ],
            "connected": ["zeta"],
            "default": {},
        }))
        .unwrap();
        let methods = serde_json::from_value(json!({
            "openai": [
                { "type": "oauth", "label": "ChatGPT (browser)" },
                { "type": "api", "label": "Manually enter API Key" },
            ],
        }))
        .unwrap();
        let providers = listed(all, methods);
        let ids: Vec<_> = providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["zeta", "acme", "openai"]);
        assert!(providers[0].connected && !providers[1].connected);
        let openai = &providers[2].methods;
        assert_eq!(
            openai
                .iter()
                .map(|m| (m.oauth, m.index))
                .collect::<Vec<_>>(),
            [(true, 0), (false, 1)]
        );
        assert_eq!(providers[1].methods[0].label, "API key");
        assert!(!providers[1].methods[0].oauth);
    }

    #[test]
    fn selects_take_their_first_choice() {
        let method: api::AuthMethod = serde_json::from_value(json!({
            "type": "oauth", "label": "Login", "prompts": [
                { "type": "select", "key": "deploymentType", "message": "Where?",
                  "options": [{ "label": "GitHub.com", "value": "github.com" },
                              { "label": "Enterprise", "value": "enterprise" }] },
                { "type": "text", "key": "enterpriseUrl", "message": "URL?" },
            ],
        }))
        .unwrap();
        assert_eq!(
            inputs(&method),
            HashMap::from([("deploymentType".to_string(), "github.com".to_string())])
        );
    }
}
