//! Building blocks shared by the orchestrator, the worker and the CLI.

pub mod dirs;
pub mod fs;
pub mod harness;
pub mod link;
pub mod lookup;
pub mod time;

pub const DEFAULT_PORT: u16 = 7400;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `bytes` random bytes from the OS, in hex.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("OS random number generator unavailable");
    hex::encode(buf)
}

/// `value`, or `default` when it is empty.
pub fn or<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() { default } else { value }
}
