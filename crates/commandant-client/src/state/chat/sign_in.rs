//! Signing a node's agent in to or out of a model provider, from a chat.

use commandant_proto::*;

use super::Chat;
use crate::state::{Ask, Choice, Choices, Choose, Effect, Outcome, Wanted};

/// A step of signing in to a provider the node is at.
pub(super) struct SigningIn {
    provider: String,
    /// Set while it starts OAuth method `start`.
    start: Option<u32>,
    /// What it is doing, to show meanwhile.
    pub(super) doing: String,
}

impl Chat {
    /// Sends the API key or code `text` holds, if one is asked for. It is
    /// kept out of the thread.
    pub(in crate::state) fn enter(&mut self, text: &str) -> Outcome {
        let text = text.trim();
        let wanted = match self.ask.take() {
            Some(Ask::Enter { wanted, .. }) if !text.is_empty() => wanted,
            asked => {
                self.ask = asked;
                return Outcome::default();
            }
        };
        let (provider, action) = match wanted {
            Wanted::ApiKey { provider } => (provider, auth_action::Action::ApiKey(text.into())),
            Wanted::Code { provider, index } => {
                let finish = OauthFinish {
                    index,
                    code: text.into(),
                };
                (provider, auth_action::Action::OauthFinish(finish))
            }
        };
        let doing = format!("signing in to {}…", self.provider_name(&provider));
        self.authenticate(provider, None, doing, action).into()
    }

    /// Asks for the line `wanted`, saying where to find it; a key is
    /// never shown.
    pub(super) fn ask_for(&mut self, wanted: Wanted) {
        let (label, secret) = match &wanted {
            Wanted::ApiKey { provider } => {
                let label = format!("the API key for {}", self.provider_name(provider));
                self.info(&format!("paste {label} and press Enter (Esc to cancel)"));
                (label, true)
            }
            Wanted::Code { .. } => {
                self.info("then paste the code it shows here");
                ("the code the page shows".to_string(), false)
            }
        };
        self.put(Ask::Enter {
            label,
            secret,
            wanted,
        });
    }

    /// Has the node do a step of signing in, and waits for it.
    pub(super) fn authenticate(
        &mut self,
        provider: String,
        start: Option<u32>,
        doing: String,
        action: auth_action::Action,
    ) -> Effect {
        self.signing_in = Some(SigningIn {
            provider: provider.clone(),
            start,
            doing,
        });
        Effect::Authenticate {
            chat: self.id,
            node: self.node.id.clone(),
            provider,
            action: AuthAction {
                action: Some(action),
            },
        }
    }

    pub(super) fn provider_name(&self, id: &str) -> String {
        let provider = self.providers.iter().find(|p| p.id == id);
        provider.map_or_else(|| id.to_string(), |p| p.name.clone())
    }

    /// Asks the node for its providers, to pick one when they come.
    pub(super) fn list_providers(&mut self, filter: &str) -> Option<Effect> {
        if self.listing_providers.is_some() {
            return None;
        }
        self.listing_providers = Some(filter.to_string());
        Some(Effect::FetchProviders {
            chat: self.id,
            node: self.node.id.clone(),
        })
    }

    pub(super) fn open_providers(&mut self, filter: &str) {
        let choices = self
            .providers
            .iter()
            .map(|p| {
                let detail = match p.connected {
                    true => format!("signed in · {}", p.id),
                    false => p.id.clone(),
                };
                Choice::new(Choose::Provider(p.id.clone()), &p.name, detail)
            })
            .collect();
        let choices = Choices::new("Providers (Enter signs in or out)", choices, None);
        self.put(choices.with_filter(filter));
    }

    /// The ways to sign in to `provider`, and out if it is signed in.
    pub(super) fn open_sign_in(&mut self, id: &str) {
        let Some(provider) = self.providers.iter().find(|p| p.id == id) else {
            return;
        };
        let mut choices: Vec<_> = provider
            .methods
            .iter()
            .map(|m| {
                let choose = Choose::SignIn {
                    provider: id.to_string(),
                    oauth: m.oauth.then_some(m.index),
                };
                let detail = if m.oauth { "OAuth" } else { "paste a key" };
                Choice::new(choose, &m.label, detail)
            })
            .collect();
        if provider.connected {
            let detail = format!("forget {}'s credentials on this node", provider.name);
            choices.push(Choice::new(
                Choose::SignOut(id.to_string()),
                "Sign out",
                detail,
            ));
        }
        self.put(Choices::new("Sign in with", choices, None));
    }

    /// Takes in how a step of signing in went, which may call for the next.
    pub(super) fn signed_in(
        &mut self,
        result: Result<ProviderAuthResult, String>,
    ) -> Option<Effect> {
        // Esc'd meanwhile, or not this chat's.
        let SigningIn {
            provider, start, ..
        } = self.signing_in.take()?;
        let name = self.provider_name(&provider);
        let result = match result {
            Ok(result) => result,
            Err(e) => {
                self.error(&format!("couldn't sign in to {name}: {e}"));
                return None;
            }
        };
        let Some(index) = start else {
            self.info(&format!("{name}: done; its models follow"));
            return Some(Effect::fetch_options(&self.node.id));
        };
        // OAuth has started: say where to go.
        if !result.instructions.is_empty() {
            self.info(&result.instructions);
        }
        self.info(&format!("open {}", result.url));
        if result.needs_code {
            self.ask_for(Wanted::Code { provider, index });
            return None;
        }
        // A redirect back to the node's loopback only works in a browser there.
        if result.url.contains("localhost") || result.url.contains("127.0.0.1") {
            self.info(&format!(
                "that page sends the browser back to {}: sign in from a browser there, or pick a headless method",
                self.node.name
            ));
        }
        let finish = OauthFinish {
            index,
            code: String::new(),
        };
        let doing = format!("waiting for you to sign in to {name}…");
        Some(self.authenticate(
            provider,
            None,
            doing,
            auth_action::Action::OauthFinish(finish),
        ))
    }
}
