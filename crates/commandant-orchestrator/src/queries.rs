//! Questions put to workers that expect one answer, matched by request id.

use std::collections::HashMap;
use std::sync::Mutex;

use commandant_proto::{AgentOptions, AgentSessions};
use tokio::sync::oneshot;

/// What a worker answers a question with.
pub enum Answer {
    Options(AgentOptions),
    Sessions(AgentSessions),
}

impl Answer {
    fn request_id(&self) -> &str {
        match self {
            Self::Options(options) => &options.request_id,
            Self::Sessions(sessions) => &sessions.request_id,
        }
    }
}

#[derive(Default)]
pub struct Queries {
    pending: Mutex<HashMap<String, oneshot::Sender<Answer>>>,
}

impl Queries {
    /// Opens a question; the answer arrives on the receiver.
    pub fn open(&self) -> (String, oneshot::Receiver<Answer>) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(request_id.clone(), tx);
        (request_id, rx)
    }

    /// Hands a worker's answer to whoever asked, if they are still waiting.
    pub fn answer(&self, answer: Answer) {
        let asker = self.pending.lock().unwrap().remove(answer.request_id());
        if let Some(asker) = asker {
            let _ = asker.send(answer);
        }
    }

    /// Gives up on a question that went unanswered.
    pub fn close(&self, request_id: &str) {
        self.pending.lock().unwrap().remove(request_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(request_id: &str) -> Answer {
        Answer::Options(AgentOptions {
            request_id: request_id.into(),
            ..Default::default()
        })
    }

    #[test]
    fn answers_reach_whoever_asked_and_nobody_else() {
        let queries = Queries::default();
        let (options_id, mut options_rx) = queries.open();
        let (sessions_id, mut sessions_rx) = queries.open();
        assert_ne!(options_id, sessions_id);

        // Answers to unknown or abandoned questions go nowhere.
        queries.answer(options("someone-else"));
        let (closed_id, mut closed_rx) = queries.open();
        queries.close(&closed_id);
        queries.answer(options(&closed_id));
        assert!(closed_rx.try_recv().is_err());

        queries.answer(Answer::Sessions(AgentSessions {
            request_id: sessions_id.clone(),
            ..Default::default()
        }));
        queries.answer(options(&options_id));
        assert!(
            matches!(sessions_rx.try_recv(), Ok(Answer::Sessions(s)) if s.request_id == sessions_id)
        );
        assert!(
            matches!(options_rx.try_recv(), Ok(Answer::Options(o)) if o.request_id == options_id)
        );

        // Each question takes one answer.
        queries.answer(options(&options_id));
        assert!(queries.pending.lock().unwrap().is_empty());
    }
}
