//! Client settings: flags/env first, then `~/.config/commandant/config.toml`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use commandant_common::{dirs, fs};
use commandant_proto::{ControlClient, connect_control};
use serde::{Deserialize, Serialize};

pub const DEFAULT_ADDR: &str = "http://127.0.0.1:7400";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct FileConfig {
    pub addr: Option<String>,
    pub token: Option<String>,
}

pub fn load() -> Result<FileConfig> {
    let path = dirs::client_config()?;
    match fs::read_optional(&path)? {
        Some(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        None => Ok(FileConfig::default()),
    }
}

pub fn save(config: &FileConfig) -> Result<PathBuf> {
    let path = dirs::client_config()?;
    fs::write_private(&path, &toml::to_string(config)?)?;
    Ok(path)
}

/// Resolved client connection settings.
pub struct Client {
    pub addr: String,
    pub token: String,
}

impl Client {
    pub async fn connect(&self) -> Result<ControlClient> {
        connect_control(&self.addr, &self.token)
            .await
            .with_context(|| format!("connecting to {}", self.addr))
    }
}

pub fn resolve(
    addr: Option<String>,
    token: Option<String>,
    token_file: Option<PathBuf>,
) -> Result<Client> {
    let file = load()?;
    let token = match (token, token_file, file.token) {
        (Some(t), _, _) => t,
        (None, Some(path), _) => fs::read_trimmed(&path).context("reading the token file")?,
        (None, None, Some(t)) => t,
        (None, None, None) => {
            bail!("no admin token: pass --token, set COMMANDANT_TOKEN, or run `commandant login`")
        }
    };
    let addr = addr.or(file.addr).unwrap_or_else(|| DEFAULT_ADDR.into());
    Ok(Client { addr, token })
}
