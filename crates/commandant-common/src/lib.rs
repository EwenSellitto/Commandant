//! Building blocks shared by the orchestrator, the worker and the CLI.

pub mod dirs;
pub mod fs;
pub mod link;
pub mod lookup;
pub mod time;

pub const DEFAULT_PORT: u16 = 7400;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
