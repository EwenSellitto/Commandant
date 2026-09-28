//! The chat's state, and how keys and task events change it.

use std::time::Instant;

use commandant_proto::task_event::Event as TaskEvent;
use commandant_proto::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What the worker prefixes its notes with.
const NOTE_PREFIX: &str = "[opencode] ";
/// Lines moved by PageUp / PageDown.
const PAGE: u16 = 10;

/// Sent with every prompt; `session_id` fills in after the first reply.
pub struct Settings {
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub agent: String,
}

/// Something for the event loop to do.
pub enum Action {
    Send(PromptRequest),
    Cancel(String),
    Quit,
}

/// What background tasks report to the UI.
pub enum Message {
    Task(TaskEvent),
    /// The prompt couldn't be sent, or its stream broke.
    Failed(String),
    Node(NodeInfo),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Agent,
    /// A tool the agent used.
    Tool,
    Error,
    /// A remark from commandant itself.
    Info,
}

pub struct Entry {
    pub role: Role,
    pub text: String,
}

pub enum Activity {
    Idle,
    Working {
        /// Known once the orchestrator has started the task.
        task_id: Option<String>,
        since: Instant,
        cancelling: bool,
    },
}

pub struct App {
    pub node: NodeInfo,
    pub settings: Settings,
    pub thread: Vec<Entry>,
    pub activity: Activity,
    pub input: Input,
    /// How many lines the thread is scrolled up from the bottom.
    pub scroll: u16,
    /// Bytes of a UTF-8 character split across output chunks, per stream.
    partial_stdout: Vec<u8>,
    partial_stderr: Vec<u8>,
    /// A note line still waiting for its newline.
    partial_note: String,
}

impl App {
    pub fn new(node: NodeInfo, settings: Settings) -> Self {
        Self {
            node,
            settings,
            thread: Vec::new(),
            activity: Activity::Idle,
            input: Input::default(),
            scroll: 0,
            partial_stdout: Vec::new(),
            partial_stderr: Vec::new(),
            partial_note: String::new(),
        }
    }

    pub fn harness(&self) -> Option<&str> {
        self.node.harnesses.first().map(String::as_str)
    }

    pub fn running_task(&self) -> Option<String> {
        match &self.activity {
            Activity::Working { task_id, .. } => task_id.clone(),
            Activity::Idle => None,
        }
    }

    pub fn on_input(&mut self, event: Event) -> Option<Action> {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
            Event::Paste(text) => {
                // The prompt is a single line.
                self.input.insert_str(&text.replace(['\r', '\n'], " "));
                None
            }
            _ => None,
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c' | 'd') if ctrl => return Some(Action::Quit),
            KeyCode::Char('u') if ctrl => self.input.clear(),
            KeyCode::Char(c) => self.input.insert(c),
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => return self.cancel(),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(PAGE),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(PAGE),
            _ => {}
        }
        None
    }

    /// Sends the prompt, or runs a `/command`.
    fn submit(&mut self) -> Option<Action> {
        let text = self.input.text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        match text.as_str() {
            "/quit" | "/exit" => return Some(Action::Quit),
            "/new" => {
                self.input.clear();
                self.thread.clear();
                self.settings.session_id.clear();
                self.info("started a new session");
                return None;
            }
            _ => {}
        }
        if matches!(self.activity, Activity::Working { .. }) {
            // Keep the text; it can be sent once the agent is done.
            return None;
        }
        self.input.clear();
        self.scroll = 0;
        self.push(Role::User, &text);
        self.activity = Activity::Working {
            task_id: None,
            since: Instant::now(),
            cancelling: false,
        };
        Some(Action::Send(PromptRequest {
            node: self.node.id.clone(),
            prompt: text,
            session_id: self.settings.session_id.clone(),
            cwd: self.settings.cwd.clone(),
            model: self.settings.model.clone(),
            agent: self.settings.agent.clone(),
        }))
    }

    fn cancel(&mut self) -> Option<Action> {
        let Activity::Working {
            task_id: Some(task_id),
            cancelling,
            ..
        } = &mut self.activity
        else {
            return None;
        };
        if *cancelling {
            return None;
        }
        *cancelling = true;
        Some(Action::Cancel(task_id.clone()))
    }

    pub fn on_message(&mut self, message: Message) {
        match message {
            Message::Node(node) => self.node = node,
            Message::Failed(error) => {
                self.flush_output();
                self.push(Role::Error, &error);
                self.activity = Activity::Idle;
            }
            Message::Task(TaskEvent::Started(started)) => {
                if let Activity::Working { task_id, .. } = &mut self.activity {
                    *task_id = Some(started.task_id);
                }
            }
            Message::Task(TaskEvent::Output(output)) => {
                if output.stream() == OutputStream::Stderr {
                    let text = decode(&mut self.partial_stderr, &output.data);
                    self.note(&text);
                } else {
                    let text = decode(&mut self.partial_stdout, &output.data);
                    self.append(Role::Agent, &text);
                }
            }
            Message::Task(TaskEvent::Finished(finished)) => self.finish(finished),
        }
    }

    fn finish(&mut self, finished: TaskFinished) {
        self.flush_output();
        if !finished.session_id.is_empty() {
            self.settings.session_id = finished.session_id;
        }
        if finished.cancelled {
            self.info("cancelled");
        } else if !finished.error.is_empty() {
            self.push(Role::Error, &finished.error);
        } else if finished.exit_code != Some(0) {
            self.push(Role::Error, "the agent failed");
        }
        self.activity = Activity::Idle;
    }

    /// The worker reports tools and errors as `[opencode] …` lines.
    fn note(&mut self, text: &str) {
        self.partial_note.push_str(text);
        while let Some(end) = self.partial_note.find('\n') {
            let line: String = self.partial_note.drain(..=end).collect();
            let line = line.trim_end();
            match line.strip_prefix(NOTE_PREFIX) {
                Some(tool) => self.push(Role::Tool, tool),
                None if !line.is_empty() => self.push(Role::Error, line),
                None => {}
            }
        }
    }

    fn flush_output(&mut self) {
        self.partial_stdout.clear();
        self.partial_stderr.clear();
        let rest = std::mem::take(&mut self.partial_note);
        self.note(&format!("{rest}\n"));
    }

    fn info(&mut self, text: &str) {
        self.push(Role::Info, text);
    }

    fn push(&mut self, role: Role, text: &str) {
        self.thread.push(Entry {
            role,
            text: text.to_string(),
        });
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

/// A single-line text field. `cursor` counts characters, not bytes.
#[derive(Default)]
pub struct Input {
    pub text: String,
    pub cursor: usize,
}

impl Input {
    fn byte_index(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map_or(self.text.len(), |(i, _)| i)
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_index();
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn insert_str(&mut self, s: &str) {
        let at = self.byte_index();
        self.text.insert_str(at, s);
        self.cursor += s.chars().count();
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.delete();
        }
    }

    fn delete(&mut self) {
        let at = self.byte_index();
        if at < self.text.len() {
            self.text.remove(at);
        }
    }

    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    fn home(&mut self) {
        self.cursor = 0;
    }

    fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let node = NodeInfo {
            id: "n1".into(),
            name: "w1".into(),
            harnesses: vec!["opencode".into()],
            ..Default::default()
        };
        let settings = Settings {
            session_id: String::new(),
            cwd: String::new(),
            model: String::new(),
            agent: String::new(),
        };
        App::new(node, settings)
    }

    fn output(stream: OutputStream, data: &[u8]) -> Message {
        Message::Task(TaskEvent::Output(TaskOutput {
            stream: stream as i32,
            data: data.to_vec(),
            ..Default::default()
        }))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_turn_streams_into_the_thread_and_keeps_the_session() {
        let mut app = app();
        type_text(&mut app, "hi");
        let Some(Action::Send(request)) = app.on_key(KeyEvent::from(KeyCode::Enter)) else {
            panic!("Enter should send");
        };
        assert_eq!(
            (request.prompt.as_str(), request.node.as_str()),
            ("hi", "n1")
        );
        assert!(app.input.text.is_empty());

        // Typing on while the agent works is fine, sending isn't.
        type_text(&mut app, "next");
        assert!(app.on_key(KeyEvent::from(KeyCode::Enter)).is_none());
        assert_eq!(app.input.text, "next");

        app.on_message(Message::Task(TaskEvent::Started(TaskStarted {
            task_id: "t1".into(),
            ..Default::default()
        })));
        // "é" split across two chunks.
        app.on_message(output(OutputStream::Stdout, b"Hello caf\xc3"));
        app.on_message(output(OutputStream::Stderr, b"[opencode] write a.txt\n"));
        app.on_message(output(OutputStream::Stdout, b"\xa9 done"));
        app.on_message(Message::Task(TaskEvent::Finished(TaskFinished {
            task_id: "t1".into(),
            exit_code: Some(0),
            session_id: "ses_1".into(),
            ..Default::default()
        })));

        let thread: Vec<_> = app
            .thread
            .iter()
            .map(|e| (e.role, e.text.as_str()))
            .collect();
        assert_eq!(
            thread,
            [
                (Role::User, "hi"),
                (Role::Agent, "Hello caf"),
                (Role::Tool, "write a.txt"),
                (Role::Agent, "é done"),
            ]
        );
        assert!(matches!(app.activity, Activity::Idle));

        // The next prompt continues the session.
        let Some(Action::Send(request)) = app.on_key(KeyEvent::from(KeyCode::Enter)) else {
            panic!("Enter should send once idle");
        };
        assert_eq!(request.session_id, "ses_1");
    }

    #[test]
    fn esc_cancels_once_the_task_is_known() {
        let mut app = app();
        type_text(&mut app, "hi");
        app.on_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.on_key(KeyEvent::from(KeyCode::Esc)).is_none());
        app.on_message(Message::Task(TaskEvent::Started(TaskStarted {
            task_id: "t1".into(),
            ..Default::default()
        })));
        assert!(matches!(
            app.on_key(KeyEvent::from(KeyCode::Esc)),
            Some(Action::Cancel(id)) if id == "t1"
        ));
        assert!(app.on_key(KeyEvent::from(KeyCode::Esc)).is_none());
    }

    #[test]
    fn input_edits_by_character() {
        let mut input = Input::default();
        for c in "héllo".chars() {
            input.insert(c);
        }
        input.home();
        input.right();
        input.delete();
        input.end();
        input.backspace();
        assert_eq!(input.text, "hll");
        assert_eq!(input.cursor, 3);
    }
}
