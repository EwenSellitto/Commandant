//! gRPC contract shared by the orchestrator, workers and the CLI.

pub mod v1 {
    tonic::include_proto!("commandant.v1");
}

pub use v1::*;

use tonic::metadata::{Ascii, MetadataValue};
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Status};

/// Lets each payload be wrapped in its envelope with `.into()`, e.g.
/// `WorkerMsg::from(Heartbeat {})` instead of spelling out the `oneof`.
macro_rules! envelope {
    ($envelope:ident.$field:ident, $module:ident::$oneof:ident { $($variant:ident($payload:ty)),* $(,)? }) => {
        $(
            impl From<$payload> for $envelope {
                fn from(payload: $payload) -> Self {
                    Self { $field: Some($module::$oneof::$variant(payload)) }
                }
            }
        )*
    };
}

envelope!(WorkerMsg.msg, worker_msg::Msg {
    Hello(Hello),
    Heartbeat(Heartbeat),
    Output(TaskOutput),
    Finished(TaskFinished),
    Options(AgentOptions),
});

envelope!(OrchestratorMsg.msg, orchestrator_msg::Msg {
    Welcome(Welcome),
    Run(RunTask),
    Cancel(CancelTask),
    Prompt(AgentPrompt),
    ListOptions(ListAgentOptions),
});

envelope!(TaskEvent.event, task_event::Event {
    Started(TaskStarted),
    Output(TaskOutput),
    Finished(TaskFinished),
});

/// Adds `authorization: Bearer <token>` to every request.
#[derive(Clone)]
pub struct BearerAuth(MetadataValue<Ascii>);

impl BearerAuth {
    pub fn new(token: &str) -> anyhow::Result<Self> {
        Ok(Self(format!("Bearer {token}").parse()?))
    }
}

impl Interceptor for BearerAuth {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        req.metadata_mut().insert("authorization", self.0.clone());
        Ok(req)
    }
}

pub type ControlClient = control_client::ControlClient<InterceptedService<Channel, BearerAuth>>;

/// Connects to an orchestrator's Control service as an admin.
pub async fn connect_control(addr: &str, token: &str) -> anyhow::Result<ControlClient> {
    let addr = commandant_common::link::prefer_loopback(addr).await;
    let channel = Endpoint::from_shared(addr)?.connect().await?;
    Ok(control_client::ControlClient::with_interceptor(
        channel,
        BearerAuth::new(token)?,
    ))
}
