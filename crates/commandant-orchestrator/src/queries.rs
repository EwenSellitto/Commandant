//! Questions put to workers that expect one answer, matched by request id.

use std::collections::HashMap;
use std::sync::Mutex;

use commandant_proto::AgentOptions;
use tokio::sync::oneshot;

#[derive(Default)]
pub struct Queries {
    pending: Mutex<HashMap<String, oneshot::Sender<AgentOptions>>>,
}

impl Queries {
    /// Opens a question; the answer arrives on the receiver.
    pub fn open(&self) -> (String, oneshot::Receiver<AgentOptions>) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(request_id.clone(), tx);
        (request_id, rx)
    }

    /// Hands a worker's answer to whoever asked, if they are still waiting.
    pub fn answer(&self, options: AgentOptions) {
        let asker = self.pending.lock().unwrap().remove(&options.request_id);
        if let Some(asker) = asker {
            let _ = asker.send(options);
        }
    }

    /// Gives up on a question that went unanswered.
    pub fn close(&self, request_id: &str) {
        self.pending.lock().unwrap().remove(request_id);
    }
}
