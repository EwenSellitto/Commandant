//! Logging in: the first-run screen, where the server's connection link is
//! pasted. The link is saved once connecting to it works.

use commandant_client_core::config::Client;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use super::chat::Input;

#[derive(Default)]
pub struct LogIn {
    /// The link, as pasted.
    pub link: Input,
    /// Why the last link didn't do.
    pub error: Option<String>,
    /// Connecting to the link: the keys wait.
    pub connecting: bool,
}

/// What the screen asks for next.
pub enum Step {
    Wait,
    /// Connect to this server.
    Connect(Client),
    /// Give up on the server being connected to.
    Cancel,
    /// Leave the client.
    Leave,
}

impl LogIn {
    pub fn on_input(&mut self, event: Event) -> Step {
        let key = match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => key,
            Event::Paste(text) if !self.connecting => {
                self.link.paste(&text);
                return Step::Wait;
            }
            _ => return Step::Wait,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c' | 'd') if ctrl => Step::Leave,
            KeyCode::Esc if self.connecting => {
                self.connecting = false;
                Step::Cancel
            }
            KeyCode::Esc => Step::Leave,
            _ if self.connecting => Step::Wait,
            KeyCode::Enter => match Client::from_link(&self.link.text) {
                Ok(client) => {
                    self.error = None;
                    self.connecting = true;
                    Step::Connect(client)
                }
                Err(e) => {
                    self.error = Some(format!("{e:#}"));
                    Step::Wait
                }
            },
            _ => {
                self.link.edit(key);
                Step::Wait
            }
        }
    }

    /// Connecting failed: the link stays, to fix or try again.
    pub fn failed(&mut self, error: &anyhow::Error) {
        self.connecting = false;
        self.error = Some(format!("{error:#}"));
    }
}

#[cfg(test)]
mod tests {
    use commandant_common::link::{Link, Pin};
    use crossterm::event::KeyEvent;

    use super::*;

    fn link() -> Link {
        let admin = format!("cmda_{}", "ab".repeat(16));
        Link::new(&admin, "192.168.1.15:7400").with_pin(Some(Pin::of(b"a certificate")))
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::from(code))
    }

    fn type_text(screen: &mut LogIn, text: &str) {
        for c in text.chars() {
            screen.on_input(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_pasted_link_is_connected_to() {
        let mut screen = LogIn::default();
        screen.on_input(Event::Paste(format!("{}\n", link())));
        let Step::Connect(client) = screen.on_input(key(KeyCode::Enter)) else {
            panic!("the link wasn't connected to");
        };
        assert_eq!(client.addr, link().addr());
        assert!(screen.connecting && screen.error.is_none());
    }

    #[test]
    fn a_bad_link_says_why_and_stays() {
        let mut screen = LogIn::default();
        type_text(&mut screen, "https://box:7400");
        assert!(matches!(screen.on_input(key(KeyCode::Enter)), Step::Wait));
        let error = screen.error.as_deref().expect("an error is shown");
        assert!(error.contains("commandant://"), "{error}");
        assert_eq!(screen.link.text, "https://box:7400", "kept, to fix");
        assert!(!screen.connecting);
    }

    #[test]
    fn a_server_that_cannot_be_reached_says_why_and_stays() {
        let mut screen = LogIn::default();
        screen.on_input(Event::Paste(link().to_string()));
        screen.on_input(key(KeyCode::Enter));
        screen.failed(&anyhow::anyhow!("connecting to box"));
        assert_eq!(screen.error.as_deref(), Some("connecting to box"));
        assert!(!screen.connecting);
        assert!(matches!(
            screen.on_input(key(KeyCode::Enter)),
            Step::Connect(_)
        ));
    }

    #[test]
    fn connecting_can_be_given_up() {
        let mut screen = LogIn::default();
        screen.on_input(Event::Paste(link().to_string()));
        screen.on_input(key(KeyCode::Enter));
        type_text(&mut screen, "x");
        assert_eq!(screen.link.text, link().to_string(), "the keys wait");
        assert!(matches!(screen.on_input(key(KeyCode::Esc)), Step::Cancel));
        assert!(!screen.connecting);
    }

    #[test]
    fn esc_or_ctrl_c_leaves() {
        let mut screen = LogIn::default();
        assert!(matches!(screen.on_input(key(KeyCode::Esc)), Step::Leave));
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(matches!(screen.on_input(Event::Key(ctrl_c)), Step::Leave));
    }
}
