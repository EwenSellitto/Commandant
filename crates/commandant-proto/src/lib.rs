//! gRPC contract shared by the orchestrator, workers and the CLI.

pub mod v1 {
    tonic::include_proto!("commandant.v1");
}

pub use v1::*;

use std::time::Duration;

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
    Sessions(AgentSessions),
    HarnessStarted(HarnessStarted),
    History(SessionHistory),
    Providers(AgentProviders),
    AuthResult(ProviderAuthResult),
    ProjectReady(ProjectReady),
});

envelope!(OrchestratorMsg.msg, orchestrator_msg::Msg {
    Welcome(Welcome),
    Run(RunTask),
    Cancel(CancelTask),
    Prompt(AgentPrompt),
    ListOptions(ListAgentOptions),
    ListSessions(ListAgentSessions),
    StartHarness(StartHarness),
    GetHistory(GetSessionHistory),
    ListProviders(ListProviders),
    ProviderAuth(ProviderAuth),
    PrepareProject(PrepareProject),
});

/// The capability a worker lists for the harness it hosts: `harness:opencode`.
pub const HOSTS: &str = "harness:";
/// The capability a worker lists for each harness it could start.
pub const CAN_HOST: &str = "can-host:";

/// A worker's answer to one of the orchestrator's questions, matched to it by
/// `request_id`; a non-empty `error` means the question failed.
pub trait Reply: Default + Into<WorkerMsg> + TryFrom<worker_msg::Msg> {
    fn request_id(&self) -> &str;
    fn error(&self) -> &str;
    /// `request_id` and `error`, to fill in.
    fn fields(&mut self) -> (&mut String, &mut String);
}

macro_rules! reply {
    ($($payload:ident => $variant:ident),* $(,)?) => {
        $(
            impl Reply for $payload {
                fn request_id(&self) -> &str {
                    &self.request_id
                }
                fn error(&self) -> &str {
                    &self.error
                }
                fn fields(&mut self) -> (&mut String, &mut String) {
                    (&mut self.request_id, &mut self.error)
                }
            }

            impl TryFrom<worker_msg::Msg> for $payload {
                type Error = worker_msg::Msg;
                fn try_from(msg: worker_msg::Msg) -> Result<Self, Self::Error> {
                    match msg {
                        worker_msg::Msg::$variant(payload) => Ok(payload),
                        other => Err(other),
                    }
                }
            }
        )*

        impl worker_msg::Msg {
            /// The question this answers, if it is an answer.
            pub fn request_id(&self) -> Option<&str> {
                match self {
                    $(Self::$variant(payload) => Some(&payload.request_id),)*
                    _ => None,
                }
            }
        }
    };
}

reply!(
    AgentOptions => Options,
    AgentSessions => Sessions,
    HarnessStarted => HarnessStarted,
    SessionHistory => History,
    AgentProviders => Providers,
    ProviderAuthResult => AuthResult,
    ProjectReady => ProjectReady,
);

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
    Ok(control_client::ControlClient::with_interceptor(
        channel(addr).await?,
        BearerAuth::new(token)?,
    ))
}

/// How long one of an orchestrator's addresses gets to answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Connects to the orchestrator at `addr`, which may list several URLs
/// separated by commas (say its public name and its LAN address): all are
/// tried at once, and the first to answer is used.
pub async fn channel(addr: &str) -> anyhow::Result<Channel> {
    let mut attempts = tokio::task::JoinSet::new();
    for url in commandant_common::link::urls(addr) {
        let url = url.to_string();
        attempts.spawn(async move {
            let local = commandant_common::link::prefer_loopback(&url).await;
            let connected = async {
                Endpoint::from_shared(local)?
                    .connect_timeout(CONNECT_TIMEOUT)
                    .http2_keep_alive_interval(Duration::from_secs(20))
                    .keep_alive_while_idle(true)
                    .connect()
                    .await
                    .map_err(anyhow::Error::from)
            };
            connected.await.map_err(|e| format!("{url}: {e:#}"))
        });
    }
    let mut failures = Vec::new();
    while let Some(attempt) = attempts.join_next().await {
        match attempt? {
            Ok(channel) => return Ok(channel),
            Err(failure) => failures.push(failure),
        }
    }
    match failures.is_empty() {
        true => anyhow::bail!("no orchestrator address given"),
        false => anyhow::bail!("couldn't reach the orchestrator ({})", failures.join("; ")),
    }
}
