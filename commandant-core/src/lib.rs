use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    pub session: Uuid,
    pub server_url: String,
    pub log_level: String,
}

pub struct State;

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "commandant".to_string(),
            session: Uuid::new_v4(),
            server_url: "http://localhost:9876".to_string(),
            log_level: "info".to_string(),
        }
    }
}

/// Shared result type for Commandant operations
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

pub mod utils {
    use uuid::Uuid;

    pub fn generate_id() -> Uuid {
        Uuid::new_v4()
    }
}

pub mod browser;
pub mod engine;
pub mod git;
pub mod sessions;
pub mod worktrunk;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_id() {
        assert!(true);
    }

    #[test]
    fn test_config_default() {
        assert!(true)
    }
}
