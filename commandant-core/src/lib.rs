use serde::{Deserialize, Serialize};

/// Shared configuration for Commandant components
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    pub server_url: String,
    pub log_level: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "commandant".to_string(),
            server_url: "http://localhost:3000".to_string(),
            log_level: "info".to_string(),
        }
    }
}

/// Shared result type for Commandant operations
pub type Result<T> = std::result::Result<T, Error>;

/// Shared error type for Commandant operations
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
}

/// Common utility functions
pub mod utils {
    /// Generate a unique identifier
    pub fn generate_id() -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        format!("cmdt-{}", duration.as_millis())
    }
}

/// Docker module
pub mod browser;
/// Docker module
pub mod docker;
/// Engine module
pub mod engine;
/// Git module
pub mod git;
/// Worktrunk module
pub mod worktrunk;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_id() {
        use std::thread;
        use std::time::Duration;

        let id1 = utils::generate_id();
        thread::sleep(Duration::from_millis(1));
        let id2 = utils::generate_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("cmdt-"));
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.name, "commandant");
        assert_eq!(config.server_url, "http://localhost:3000");
    }
}
