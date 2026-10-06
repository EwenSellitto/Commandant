//! Building blocks shared by the orchestrator, the worker and the CLI.

pub mod dirs;
pub mod fs;
pub mod harness;
pub mod link;
pub mod lookup;
pub mod time;

pub const DEFAULT_PORT: u16 = 7400;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `value`, or `default` when it is empty.
pub fn or<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() { default } else { value }
}
