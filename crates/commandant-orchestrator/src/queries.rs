//! Questions put to workers that expect one answer, matched by request id.

use std::collections::HashMap;
use std::sync::Mutex;

use commandant_proto::worker_msg::Msg;
use tokio::sync::oneshot;

use crate::registry::ConnId;

/// A question waiting for its answer.
struct Asked {
    /// The worker connection it was put to. Only it may answer.
    to: ConnId,
    asker: oneshot::Sender<Msg>,
}

#[derive(Default)]
pub struct Queries {
    pending: Mutex<HashMap<String, Asked>>,
}

impl Queries {
    /// Opens a question for the worker on connection `to`; the answer arrives
    /// on the receiver.
    pub fn open(&self, to: ConnId) -> (String, oneshot::Receiver<Msg>) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (asker, rx) = oneshot::channel();
        let asked = Asked { to, asker };
        self.pending
            .lock()
            .unwrap()
            .insert(request_id.clone(), asked);
        (request_id, rx)
    }

    /// Hands an answer from the worker on connection `from` to whoever asked
    /// it, if they are still waiting.
    pub fn answer(&self, from: ConnId, answer: Msg) {
        let Some(request_id) = answer.request_id() else {
            return;
        };
        let asked = {
            let mut pending = self.pending.lock().unwrap();
            if pending.get(request_id).is_none_or(|asked| asked.to != from) {
                return;
            }
            pending.remove(request_id).expect("just found")
        };
        let _ = asked.asker.send(answer);
    }

    /// Gives up on a question that went unanswered.
    pub fn close(&self, request_id: &str) {
        self.pending.lock().unwrap().remove(request_id);
    }
}

#[cfg(test)]
mod tests {
    use commandant_proto::{AgentOptions, AgentSessions};

    use super::*;

    fn options(request_id: &str) -> Msg {
        Msg::Options(AgentOptions {
            request_id: request_id.into(),
            ..Default::default()
        })
    }

    #[test]
    fn answers_reach_whoever_asked_and_nobody_else() {
        let queries = Queries::default();
        let (options_id, mut options_rx) = queries.open(1);
        let (sessions_id, mut sessions_rx) = queries.open(1);
        assert_ne!(options_id, sessions_id);

        // Answers to unknown or abandoned questions go nowhere.
        queries.answer(1, options("someone-else"));
        let (closed_id, mut closed_rx) = queries.open(1);
        queries.close(&closed_id);
        queries.answer(1, options(&closed_id));
        assert!(closed_rx.try_recv().is_err());
        // Nor do answers from a worker that wasn't asked.
        queries.answer(2, options(&options_id));
        assert!(options_rx.try_recv().is_err());

        queries.answer(
            1,
            Msg::Sessions(AgentSessions {
                request_id: sessions_id.clone(),
                ..Default::default()
            }),
        );
        queries.answer(1, options(&options_id));
        assert!(
            matches!(sessions_rx.try_recv(), Ok(Msg::Sessions(s)) if s.request_id == sessions_id)
        );
        assert!(matches!(options_rx.try_recv(), Ok(Msg::Options(o)) if o.request_id == options_id));

        // Each question takes one answer.
        queries.answer(1, options(&options_id));
        assert!(queries.pending.lock().unwrap().is_empty());
    }
}
