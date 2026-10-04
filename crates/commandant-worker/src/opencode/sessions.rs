//! The agent's saved sessions, so a client can pick one up again.

use anyhow::Result;
use commandant_proto::AgentSession;

use super::Opencode;
use super::api::SavedSession;

/// How many sessions a listing shows.
const LIMIT: usize = 50;

/// The latest saved sessions.
pub async fn list(opencode: &Opencode) -> Result<Vec<AgentSession>> {
    let saved = opencode.api().await?.sessions(LIMIT).await?;
    Ok(sessions(saved, |id| opencode.is_busy(id)))
}

/// Top-level sessions, latest first; subagents' ones are part of another.
fn sessions(mut saved: Vec<SavedSession>, busy: impl Fn(&str) -> bool) -> Vec<AgentSession> {
    saved.retain(|s| s.parent_id.is_none());
    saved.sort_by_key(|s| std::cmp::Reverse(s.time.updated));
    saved
        .into_iter()
        .map(|s| {
            let (model, variant) = match s.model {
                Some(m) => (
                    format!("{}/{}", m.provider_id, m.id),
                    // What OpenCode records when no effort was chosen.
                    m.variant.filter(|v| v != "default").unwrap_or_default(),
                ),
                None => Default::default(),
            };
            AgentSession {
                busy: busy(&s.id),
                id: s.id,
                title: s.title,
                directory: s.directory,
                updated: s.time.updated / 1000,
                agent: s.agent.unwrap_or_default(),
                model,
                variant,
                cost: s.cost,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn lists_top_level_sessions_latest_first() {
        let saved: Vec<SavedSession> = serde_json::from_value(json!([
            { "id": "ses_old", "title": "old", "directory": "/a", "time": { "created": 1, "updated": 1_000 } },
            { "id": "ses_sub", "parentID": "ses_new", "directory": "/b", "time": { "created": 1, "updated": 9_000 } },
            { "id": "ses_new", "title": "new", "directory": "/b", "agent": "plan", "cost": 0.5,
              "model": { "id": "echo", "providerID": "fake", "variant": "default" },
              "time": { "created": 1, "updated": 5_000 } },
        ]))
        .unwrap();
        let sessions = sessions(saved, |id| id == "ses_new");
        let ids: Vec<_> = sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["ses_new", "ses_old"]);
        let new = &sessions[0];
        assert_eq!(
            (new.agent.as_str(), new.model.as_str(), new.variant.as_str()),
            ("plan", "fake/echo", "")
        );
        assert_eq!((new.updated, new.busy, new.cost), (5, true, 0.5));
        assert!(!sessions[1].busy);
    }

    #[test]
    fn copes_with_sparse_sessions() {
        // Untitled, never prompted, and the nulls OpenCode sends for unset fields.
        let saved: Vec<SavedSession> = serde_json::from_value(json!([
            { "id": "ses_bare", "directory": "/a", "agent": null, "model": null, "time": { "created": 1, "updated": 999 } },
            { "id": "ses_tied", "directory": "/b", "time": { "created": 1, "updated": 999 },
              "model": { "id": "m", "providerID": "p" } },
        ]))
        .unwrap();
        let sessions = sessions(saved, |_| false);
        let bare = sessions.iter().find(|s| s.id == "ses_bare").unwrap();
        assert_eq!(
            (
                bare.title.as_str(),
                bare.agent.as_str(),
                bare.model.as_str(),
                bare.updated
            ),
            ("", "", "", 0)
        );
        let tied = sessions.iter().find(|s| s.id == "ses_tied").unwrap();
        assert_eq!((tied.model.as_str(), tied.variant.as_str()), ("p/m", ""));
        assert!(super::sessions(Vec::new(), |_| true).is_empty());
    }
}
