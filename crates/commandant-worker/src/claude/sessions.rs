//! Claude Code's saved sessions: one JSON-lines transcript per session, in a
//! folder per project under `~/.claude/projects`.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use commandant_proto::{AgentSession, HistoryEntry};
use serde_json::Value;

use super::stream::{blocks, tool_line};
use crate::harness::keep_latest;

/// How many sessions a listing shows.
const LIMIT: usize = 50;

/// Where Claude Code keeps its transcripts.
pub fn projects() -> Result<PathBuf> {
    let config = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => std::env::home_dir()
            .context("no home directory")?
            .join(".claude"),
    };
    Ok(config.join("projects"))
}

/// The latest sessions, in every project.
// ponytail: reads each listed transcript whole; keep a summary per file if
// big ones make listing slow.
pub fn list(projects: &Path) -> Result<Vec<AgentSession>> {
    let mut found = Vec::new();
    for project in read_dir(projects) {
        for file in read_dir(&project) {
            if file.extension().is_some_and(|e| e == "jsonl") {
                let modified = file.metadata().and_then(|m| m.modified()).ok();
                found.push((modified.unwrap_or(UNIX_EPOCH), file));
            }
        }
    }
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    found.truncate(LIMIT);
    Ok(found
        .into_iter()
        .filter_map(|(modified, file)| {
            let text = std::fs::read_to_string(&file).ok()?;
            let mut session = summary(&text)?;
            session.id = file.file_stem()?.to_string_lossy().into_owned();
            session.updated = modified
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            Some(session)
        })
        .collect())
}

/// What a session's transcript says about it; `None` if nothing was said.
fn summary(text: &str) -> Option<AgentSession> {
    let mut session = AgentSession::default();
    let (mut named, mut prompted) = (String::new(), String::new());
    for entry in entries(text) {
        if session.directory.is_empty()
            && let Some(cwd) = entry["cwd"].as_str()
        {
            session.directory = cwd.to_string();
        }
        match entry["type"].as_str().unwrap_or_default() {
            "custom-title" => named = text_of(&entry["customTitle"]),
            "ai-title" if named.is_empty() => named = text_of(&entry["aiTitle"]),
            "user" if prompted.is_empty() => prompted = prompt(&entry).unwrap_or_default(),
            "assistant" => {
                if let Some(model) = entry["message"]["model"].as_str() {
                    session.model = model.to_string();
                }
            }
            _ => {}
        }
    }
    if session.directory.is_empty() {
        return None;
    }
    session.title = if named.is_empty() { prompted } else { named };
    Some(session)
}

/// What was said in `session_id`, oldest first.
pub fn history(projects: &Path, session_id: &str) -> Result<Vec<HistoryEntry>> {
    let text = std::fs::read_to_string(transcript(projects, session_id)?)?;
    let mut said = Vec::new();
    let mut push = |role: &str, text: &str| {
        let text = text.trim();
        if !text.is_empty() {
            said.push(HistoryEntry {
                role: role.into(),
                text: text.into(),
            });
        }
    };
    for entry in entries(&text) {
        match entry["type"].as_str().unwrap_or_default() {
            "user" => {
                if let Some(prompt) = prompt(&entry) {
                    push("user", &prompt);
                }
            }
            "assistant" => {
                for block in blocks(&entry["message"]) {
                    match block["type"].as_str().unwrap_or_default() {
                        "text" => push("agent", block["text"].as_str().unwrap_or_default()),
                        "thinking" => {
                            push("thinking", block["thinking"].as_str().unwrap_or_default())
                        }
                        "tool_use" => push(
                            "tool",
                            &tool_line(block["name"].as_str().unwrap_or_default(), &block["input"]),
                        ),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(keep_latest(said))
}

/// The directory a session works in.
pub fn directory(projects: &Path, session_id: &str) -> Result<PathBuf> {
    let text = std::fs::read_to_string(transcript(projects, session_id)?)?;
    summary(&text)
        .map(|s| PathBuf::from(s.directory))
        .with_context(|| format!("session {session_id} doesn't say where it works"))
}

fn transcript(projects: &Path, session_id: &str) -> Result<PathBuf> {
    let name = format!("{session_id}.jsonl");
    read_dir(projects)
        .into_iter()
        .map(|project| project.join(&name))
        .find(|file| file.is_file())
        .with_context(|| format!("no Claude Code session {session_id} on this node"))
}

/// The transcript's entries of the main conversation, not its subagents'.
fn entries(text: &str) -> impl Iterator<Item = Value> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["isSidechain"] != true)
}

/// What the user typed, if this entry is a prompt: not a tool's result, nor
/// something Claude Code added (`isMeta`, or tagged like `<command-name>`).
fn prompt(entry: &Value) -> Option<String> {
    if entry["isMeta"] == true {
        return None;
    }
    let content = &entry["message"]["content"];
    let text = match content.as_str() {
        Some(text) => text.to_string(),
        None => blocks(&entry["message"])
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string())
}

fn text_of(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn read_dir(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSCRIPT: &str = r#"{"type":"user","cwd":"/src/app","isMeta":true,"message":{"content":"<local-command-caveat>x</local-command-caveat>"}}
{"type":"user","cwd":"/src/app","message":{"role":"user","content":"fix the parser"}}
{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"thinking","thinking":"hmm"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/src/app/p.rs"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"..."}]}}
{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"a subagent"}]}}
{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"Fixed.\n"}]}}
{"type":"ai-title","aiTitle":"Parser fix"}
not json
"#;

    #[test]
    fn a_transcript_gives_the_session_and_what_was_said() {
        let session = summary(TRANSCRIPT).unwrap();
        assert_eq!(
            (
                session.title.as_str(),
                session.directory.as_str(),
                session.model.as_str()
            ),
            ("Parser fix", "/src/app", "claude-opus-5-5")
        );

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("-src-app");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("abc.jsonl"), TRANSCRIPT).unwrap();
        // Not a transcript of anything that was said.
        std::fs::write(project.join("empty.jsonl"), "{}\n").unwrap();

        let listed = list(dir.path()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "abc");
        assert!(listed[0].updated > 0);
        assert_eq!(
            directory(dir.path(), "abc").unwrap(),
            PathBuf::from("/src/app")
        );
        assert!(directory(dir.path(), "nope").is_err());

        let said: Vec<_> = history(dir.path(), "abc")
            .unwrap()
            .into_iter()
            .map(|e| (e.role, e.text))
            .collect();
        let expected = [
            ("user", "fix the parser"),
            ("thinking", "hmm"),
            ("tool", "Read /src/app/p.rs"),
            ("agent", "Fixed."),
        ];
        assert_eq!(said, expected.map(|(r, t)| (r.to_string(), t.to_string())));
    }
}
