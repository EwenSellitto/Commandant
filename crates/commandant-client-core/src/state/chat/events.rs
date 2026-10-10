//! What a chat's task events and background answers make of it.

use std::time::Duration;

use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::*;

use super::{Activity, Chat, Entry, Message, Role, Unseen, format::*};
use crate::state::Effect;

impl Chat {
    /// Takes in what a background task reports, which may call for an effect.
    pub fn on_message(&mut self, message: Message) -> Option<Effect> {
        match message {
            Message::Node(node) => self.node = node,
            Message::Options(options) => {
                // Every chat on the node hears; one that was waiting explains.
                let waiting = std::mem::take(&mut self.fetching_options);
                let pending = self.pending.take();
                match options {
                    Ok(options) => {
                        self.options = Some(options);
                        if let Some((menu, filter)) = pending {
                            return self.open_menu(menu, &filter);
                        }
                    }
                    Err(e) if waiting || pending.is_some() => {
                        self.error(&format!("couldn't list the agent's options: {e}"))
                    }
                    Err(_) => {}
                }
            }
            Message::Providers(providers) => {
                let filter = self.listing_providers.take().unwrap_or_default();
                match providers {
                    Ok(providers) => {
                        self.providers = providers;
                        self.open_providers(&filter);
                    }
                    Err(e) => self.error(&format!("couldn't list the providers: {e}")),
                }
            }
            Message::Auth(result) => return self.signed_in(result),
            Message::Project(ready) => {
                let repository = self.preparing.take().unwrap_or_default();
                match ready {
                    Ok(ready) => self.work_in(&ready.path, &ready.id),
                    Err(e) => self.error(&format!("couldn't get {repository}: {e}")),
                }
            }
            Message::Projects(projects) => {
                self.listing_projects = false;
                match projects {
                    Ok(projects) => self.open_projects(projects),
                    Err(e) => self.error(&format!("couldn't list the projects: {e}")),
                }
            }
            Message::History(history) => {
                self.loading_history = false;
                match history {
                    Ok(entries) => {
                        let agent = self.agent().to_string();
                        let earlier = entries.into_iter().map(|e| Entry {
                            role: match e.role.as_str() {
                                "user" => Role::User,
                                "thinking" => Role::Thinking,
                                "tool" => Role::Tool,
                                _ => Role::Agent,
                            },
                            text: e.text,
                            agent: agent.clone(),
                        });
                        // Before whatever was said since resuming.
                        self.thread.splice(0..0, earlier);
                    }
                    Err(e) => self.error(&format!(
                        "couldn't load the session's earlier messages: {e}"
                    )),
                }
            }
            Message::Failed(error) => {
                self.flush_output();
                self.error(&error);
                self.activity = Activity::Idle;
                self.unseen = Some(Unseen::Failed);
            }
            Message::Task(TaskEvent::Started(started)) => {
                if let Activity::Working {
                    task_id,
                    cancelling,
                    ..
                } = &mut self.activity
                {
                    *task_id = Some(started.task_id.clone());
                    // Esc was pressed before the task had an id.
                    if *cancelling {
                        return Some(Effect::Cancel(started.task_id));
                    }
                }
            }
            Message::Task(TaskEvent::Output(output)) => match output.stream() {
                OutputStream::Stderr => {
                    let text = decode(&mut self.partial_stderr, &output.data);
                    self.note(&text);
                }
                OutputStream::Reasoning => {
                    let text = decode(&mut self.partial_reasoning, &output.data);
                    self.append(Role::Thinking, &text);
                }
                _ => {
                    let text = decode(&mut self.partial_stdout, &output.data);
                    self.append(Role::Agent, &text);
                }
            },
            Message::Task(TaskEvent::Finished(finished)) => self.finish(finished),
        }
        None
    }

    fn finish(&mut self, finished: TaskFinished) {
        self.flush_output();
        if !finished.session_id.is_empty() {
            self.settings.session_id = finished.session_id.clone();
        }
        if !finished.model.is_empty() {
            self.used_model = finished.model.clone();
        }
        if let Some(usage) = &finished.usage {
            self.spent += usage.cost;
            if usage.context > 0 {
                self.context = usage.context;
            }
        }
        let summary = match &self.activity {
            Activity::Working {
                since,
                agent,
                effort,
                ..
            } => Some(self.summary(&finished, agent, effort, since.elapsed())),
            Activity::Idle => None,
        };
        let mut unseen = Unseen::Failed;
        if finished.cancelled {
            self.info("cancelled");
        } else if !finished.error.is_empty() {
            self.error(&finished.error);
        } else if finished.exit_code != Some(0) {
            self.error("the agent failed");
        } else {
            unseen = Unseen::Done;
        }
        self.unseen = Some(unseen);
        if let Some(summary) = summary {
            self.push(Role::Summary, &summary);
        }
        self.activity = Activity::Idle;
    }

    /// The worker reports tools as `[<harness>] …` lines, and errors as
    /// any other.
    fn note(&mut self, text: &str) {
        let prefix = format!(
            "[{}] ",
            self.node.harnesses.first().map_or("", String::as_str)
        );
        self.partial_note.push_str(text);
        while let Some(end) = self.partial_note.find('\n') {
            let line: String = self.partial_note.drain(..=end).collect();
            let line = line.trim_end();
            match line.strip_prefix(&prefix) {
                Some(tool) => self.push(Role::Tool, tool),
                None if !line.is_empty() => self.error(line),
                None => {}
            }
        }
    }

    /// `build · Claude Sonnet 5 · high · 12.3s · 12.6k in · 184 out · $0.0123`
    fn summary(
        &self,
        finished: &TaskFinished,
        agent: &str,
        effort: &str,
        took: Duration,
    ) -> String {
        // A cancelled turn doesn't say which model it had.
        let model = match finished.model.as_str() {
            "" => self.model(),
            model => model,
        };
        let mut parts = vec![agent.to_string(), self.model_name(model).to_string()];
        if !effort.is_empty() {
            parts.push(effort.to_string());
        }
        parts.push(elapsed(took));
        if let Some(usage) = finished.usage.as_ref().filter(|u| u.input + u.output > 0) {
            let input = usage.input + usage.cache_read + usage.cache_write;
            parts.push(format!("{} in", count(input)));
            parts.push(format!("{} out", count(usage.output + usage.reasoning)));
            if usage.cost > 0.0 {
                parts.push(dollars(usage.cost));
            }
        }
        parts.retain(|p| !p.is_empty());
        parts.join(" · ")
    }

    fn flush_output(&mut self) {
        self.partial_stdout.clear();
        self.partial_stderr.clear();
        self.partial_reasoning.clear();
        let rest = std::mem::take(&mut self.partial_note);
        self.note(&format!("{rest}\n"));
    }

    /// Extends the last entry if it has the same role, so a streamed reply
    /// stays one entry.
    fn append(&mut self, role: Role, text: &str) {
        match self.thread.last_mut() {
            Some(last) if last.role == role => last.text.push_str(text),
            _ => self.push(role, text),
        }
    }
}

/// Decodes output, holding back a character split across chunks in `partial`.
fn decode(partial: &mut Vec<u8>, data: &[u8]) -> String {
    partial.extend_from_slice(data);
    let complete = match std::str::from_utf8(partial) {
        Ok(_) => partial.len(),
        // Invalid bytes, not a cut: let the lossy conversion show them.
        Err(e) if e.error_len().is_some() => partial.len(),
        Err(e) => e.valid_up_to(),
    };
    let rest = partial.split_off(complete);
    let text = String::from_utf8_lossy(partial).into_owned();
    *partial = rest;
    text
}
