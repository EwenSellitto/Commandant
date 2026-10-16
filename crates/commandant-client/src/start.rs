//! Where a front end starts: `tui` and `gui` take the same arguments, and
//! give the core the same defaults and first chat.

use anyhow::{Result, bail};
use commandant_client_core::{Intent, Settings};
use commandant_common::lookup::{self, Match};
use commandant_proto::NodeInfo;

use crate::cli::UiArgs;

impl UiArgs {
    /// What new chats start with.
    pub fn defaults(&self) -> Settings {
        Settings {
            session_id: String::new(),
            cwd: self.cwd.clone().unwrap_or_default(),
            model: self.model.clone().unwrap_or_default(),
            agent: self.agent.clone().unwrap_or_default(),
            effort: self.effort.clone().unwrap_or_default(),
        }
    }

    /// The chat to open first, when a node is named.
    pub fn first_open(&self, nodes: &[NodeInfo]) -> Result<Option<Intent>> {
        let Some(needle) = self.node.as_deref() else {
            return Ok(None);
        };
        Ok(Some(Intent::OpenSession {
            node: find_node(nodes, needle)?.id,
            session: self.session.clone(),
        }))
    }
}

/// The node named by `needle`: a name, an id or an id prefix.
fn find_node(nodes: &[NodeInfo], needle: &str) -> Result<NodeInfo> {
    match lookup::find_named(nodes.to_vec(), needle, |n| &n.name, |n| &n.id) {
        Match::One(node) => Ok(node),
        Match::Ambiguous => bail!("{needle:?} is ambiguous"),
        Match::None => bail!("no node matches {needle:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Vec<NodeInfo> {
        ["box", "laptop"]
            .into_iter()
            .map(|name| NodeInfo {
                id: format!("id-{name}"),
                name: name.into(),
                ..Default::default()
            })
            .collect()
    }

    fn args(node: Option<&str>, session: Option<&str>) -> UiArgs {
        UiArgs {
            node: node.map(Into::into),
            session: session.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn a_named_node_is_opened_first() {
        let open = args(Some("laptop"), Some("ses_1")).first_open(&nodes());
        assert_eq!(
            open.unwrap(),
            Some(Intent::OpenSession {
                node: "id-laptop".into(),
                session: Some("ses_1".into()),
            })
        );
    }

    #[test]
    fn nothing_is_opened_when_no_node_is_named() {
        assert_eq!(args(None, None).first_open(&nodes()).unwrap(), None);
    }

    #[test]
    fn a_node_that_is_not_there_is_an_error() {
        let Err(e) = args(Some("desk"), None).first_open(&nodes()) else {
            panic!("opened a node that is not there");
        };
        assert_eq!(e.to_string(), "no node matches \"desk\"");
    }

    #[test]
    fn the_defaults_are_what_was_passed() {
        let args = UiArgs {
            cwd: Some("/src".into()),
            effort: Some("high".into()),
            ..Default::default()
        };
        let defaults = args.defaults();
        assert_eq!(
            (defaults.cwd.as_str(), defaults.effort.as_str()),
            ("/src", "high")
        );
        assert!(defaults.model.is_empty());
    }
}
