//! The agent's saved sessions, so a client can pick one up again.

use anyhow::Result;
use commandant_proto::{AgentSession, HistoryEntry};

use super::Opencode;
use super::api::{SavedMessage, SavedSession};
use crate::harness::keep_latest;

/// How many sessions a listing shows.
const LIMIT: usize = 50;

/// The latest saved sessions.
pub async fn list(opencode: &Opencode) -> Result<Vec<AgentSession>> {
    let saved = opencode.api().await?.sessions(LIMIT).await?;
    Ok(sessions(saved, |id| opencode.busy.contains(id)))
}

/// What was said in `session_id`, oldest first.
pub async fn history(opencode: &Opencode, session_id: &str) -> Result<Vec<HistoryEntry>> {
    let messages = opencode.api().await?.messages(session_id).await?;
    Ok(entries(messages))
}

/// The prompts, replies, thinking and tool calls, as the chat shows them live.
fn entries(messages: Vec<SavedMessage>) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    for message in messages {
        let user = message.info.role == "user";
        for part in message.parts {
            let (role, text) = match (part.kind.as_str(), part.state) {
                ("text", _) if part.synthetic == Some(true) => continue,
                ("text", _) if user => ("user", part.text),
                ("text", _) => ("agent", part.text),
                ("reasoning", _) => ("thinking", part.text),
                ("tool", Some(state)) if state.status == "completed" => (
                    "tool",
                    format!("{} {}", part.tool, state.title.unwrap_or_default()),
                ),
                ("tool", Some(state)) if state.status == "error" => (
                    "tool",
                    format!("{} failed: {}", part.tool, state.error.unwrap_or_default()),
                ),
                _ => continue,
            };
            let text = text.trim().to_string();
            if !text.is_empty() {
                entries.push(HistoryEntry {
                    role: role.into(),
                    text,
                });
            }
        }
    }
    keep_latest(entries)
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
    fn history_keeps_what_the_chat_shows() {
        let messages: Vec<SavedMessage> = serde_json::from_value(json!([
            { "info": { "role": "user" }, "parts": [
                { "type": "text", "text": "fix it" },
                { "type": "text", "text": "<file contents>", "synthetic": true },
            ]},
            { "info": { "role": "assistant" }, "parts": [
                { "type": "step-start" },
                { "type": "reasoning", "text": "hmm" },
                { "type": "tool", "tool": "bash", "state": { "status": "completed", "title": "ls" } },
                { "type": "tool", "tool": "edit", "state": { "status": "error", "error": "denied" } },
                { "type": "tool", "tool": "read", "state": { "status": "running" } },
                { "type": "text", "text": "done\n" },
            ]},
        ]))
        .unwrap();
        let entries: Vec<_> = entries(messages)
            .into_iter()
            .map(|e| (e.role, e.text))
            .collect();
        let expected = [
            ("user", "fix it"),
            ("thinking", "hmm"),
            ("tool", "bash ls"),
            ("tool", "edit failed: denied"),
            ("agent", "done"),
        ];
        assert_eq!(
            entries,
            expected.map(|(r, t)| (r.to_string(), t.to_string()))
        );
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
