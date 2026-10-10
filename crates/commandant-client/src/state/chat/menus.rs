//! What a chat offers to choose from for its settings, the agent's
//! commands and MCP servers, and what choosing any of its choices does.

use std::sync::Arc;

use commandant_proto::*;

use super::Chat;
use crate::state::{Ask, Choice, Choices, Choose, Edit, Effect, Outcome, Wanted};
use commandant_common::or;

/// What a chat offers to choose from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Menu {
    Agent,
    Model,
    Effort,
    /// One of the agent's commands or skills, to fill in.
    Command,
    /// An MCP server, to connect or disconnect.
    Mcp,
}

impl Menu {
    fn title(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Model => "Model",
            Self::Effort => "Thinking effort",
            Self::Command => "Commands and skills",
            Self::Mcp => "MCP servers (Enter connects or disconnects)",
        }
    }
}

impl Chat {
    pub(super) fn open_menu(&mut self, menu: Menu, filter: &str) -> Option<Effect> {
        let Some(options) = self.options.clone() else {
            // It opens when they come.
            self.pending = Some((menu, filter.to_string()));
            if self.fetching_options {
                self.info("the node is still saying what its agent offers; this opens when it has");
                return None;
            }
            return self.ask_for_options();
        };
        let (choices, current): (Vec<_>, _) = match menu {
            Menu::Agent => {
                let choices = options
                    .agents
                    .iter()
                    .map(|a| Choice::new(Choose::Agent(a.name.clone()), &a.name, &a.description))
                    .collect();
                (choices, Some(Choose::Agent(self.agent().to_string())))
            }
            Menu::Model => {
                let default = or(&options.default_model, "the agent picks");
                let models = options.models.iter().map(|m| {
                    let detail = format!("{} · {}", m.provider, m.id);
                    Choice::new(Choose::Model(m.id.clone()), &m.name, detail)
                });
                let choices = std::iter::once(Choice::new(
                    Choose::Model(String::new()),
                    "Default",
                    default,
                ))
                .chain(models)
                .collect();
                (choices, Some(Choose::Model(self.settings.model.clone())))
            }
            Menu::Effort => {
                let efforts = self.efforts_or_explain()?;
                let default =
                    Choice::new(Choose::Effort(String::new()), "Default", "the model's own");
                let choices = std::iter::once(default)
                    .chain(
                        efforts
                            .iter()
                            .map(|e| Choice::new(Choose::Effort(e.clone()), e, "")),
                    )
                    .collect();
                (choices, Some(Choose::Effort(self.settings.effort.clone())))
            }
            Menu::Command => {
                if options.loading {
                    self.info("the agent's commands are still loading (its MCP servers are connecting); try again in a moment");
                    return None;
                }
                if options.commands.is_empty() {
                    self.info("the agent has no commands or skills");
                    return None;
                }
                let choices = options
                    .commands
                    .iter()
                    .map(|c| {
                        let detail = match c.description.as_str() {
                            "" => c.source.clone(),
                            text => format!("{} · {text}", c.source),
                        };
                        Choice::new(
                            Choose::Command(c.name.clone()),
                            format!("/{}", c.name),
                            detail,
                        )
                    })
                    .collect();
                (choices, None)
            }
            Menu::Mcp => {
                if options.loading {
                    self.info(
                        "the agent's MCP servers are still connecting; try again in a moment",
                    );
                    return None;
                }
                if options.mcp_servers.is_empty() {
                    self.info("the agent has no MCP servers configured");
                    return None;
                }
                let choices = options
                    .mcp_servers
                    .iter()
                    .map(|m| Choice::new(Choose::Mcp(m.name.clone()), &m.name, mcp_status(m)))
                    .collect();
                (choices, None)
            }
        };
        let choices = Choices::new(menu.title(), choices, current).with_filter(filter);
        self.put(choices);
        None
    }

    /// The current model's efforts, or `None` after saying why there are none.
    pub(super) fn efforts_or_explain(&mut self) -> Option<Vec<String>> {
        match self.efforts() {
            Some([]) => {
                let model = self.model().to_string();
                self.info(&format!("{model} has no thinking efforts"));
                None
            }
            Some(efforts) => Some(efforts.to_vec()),
            None => {
                self.info(
                    "pick a model with /model first (the default one is known after a reply)",
                );
                None
            }
        }
    }

    /// Does what was chosen from the chat's question, which closes.
    pub(in crate::state) fn choose(&mut self, choice: Choose) -> Outcome {
        self.ask = None;
        match choice {
            Choose::Agent(agent) => self.settings.agent = agent,
            Choose::Effort(effort) => self.settings.effort = effort,
            Choose::Model(model) => {
                self.settings.model = model;
                // An effort the new model doesn't know would be refused.
                let effort = &self.settings.effort;
                if !effort.is_empty() && self.efforts().is_some_and(|e| !e.contains(effort)) {
                    let model = self.model().to_string();
                    let effort = std::mem::take(&mut self.settings.effort);
                    self.info(&format!(
                        "{model} has no {effort:?} effort; back to its default"
                    ));
                }
            }
            // Ready for its arguments.
            Choose::Command(name) => {
                return Outcome {
                    prompt: Some((self.id, Edit::Insert(format!("/{name} ")))),
                    ..Default::default()
                };
            }
            Choose::Provider(id) => self.open_sign_in(&id),
            Choose::SignIn {
                provider,
                oauth: None,
            } => self.ask_for(Wanted::ApiKey { provider }),
            Choose::SignIn {
                provider,
                oauth: Some(index),
            } => {
                let doing = format!("starting to sign in to {}…", self.provider_name(&provider));
                let start = auth_action::Action::OauthStart(index);
                return self
                    .authenticate(provider, Some(index), doing, start)
                    .into();
            }
            Choose::SignOut(provider) => {
                let doing = format!("signing out of {}…", self.provider_name(&provider));
                let action = auth_action::Action::SignOut(true);
                return self.authenticate(provider, None, doing, action).into();
            }
            Choose::NewCopy(repository) => return self.new_copy(repository).into(),
            Choose::Join(copy) => self.work_in(&copy.path, &copy.id),
            Choose::Mcp(name) => {
                let connect = self.options.as_ref().is_some_and(|o| {
                    o.mcp_servers
                        .iter()
                        .any(|m| m.name == name && m.status != "connected")
                });
                let doing = if connect {
                    "connecting"
                } else {
                    "disconnecting"
                };
                self.info(&format!("{doing} {name}…"));
                return Effect::SwitchMcp {
                    chat: self.id,
                    node: self.node.id.clone(),
                    name,
                    connect,
                }
                .into();
            }
            // The app's own choices.
            Choose::NewSession | Choose::Chat(_) | Choose::Saved(_) | Choose::Harness { .. } => {}
        }
        Outcome::default()
    }

    /// Leaves the chat's question unanswered.
    pub(in crate::state) fn dismiss(&mut self) {
        if let Some(Ask::Enter { .. }) = self.ask.take() {
            self.info("not signed in");
        }
    }

    /// How switching an MCP server went, which this chat asked for.
    pub fn mcp_switched(&mut self, name: &str, switched: &Result<Arc<AgentOptions>, String>) {
        match switched {
            Ok(options) => {
                if let Some(server) = options.mcp_servers.iter().find(|m| m.name == name) {
                    self.info(&format!("{name}: {}", mcp_status(server)));
                }
            }
            Err(e) => self.error(&format!("couldn't switch {name}: {e}")),
        }
    }
}

/// `connected`, or `failed: why`.
fn mcp_status(server: &McpServer) -> String {
    match server.error.as_str() {
        "" => server.status.clone(),
        error => format!("{}: {error}", server.status),
    }
}
