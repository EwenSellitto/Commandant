//! Client settings: flags/env first, then `~/.config/commandant/config.toml`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use commandant_common::link::Link;
use commandant_common::{dirs, fs};
use commandant_proto::{ControlClient, connect_control};
use serde::{Deserialize, Serialize};

/// What the config file saves: the server a client talks to by default.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct FileConfig {
    /// The server's address, `host:port`.
    pub addr: Option<String>,
    /// The admin token it takes.
    pub token: Option<String>,
}

impl FileConfig {
    /// Whether this is where `client`'s server is saved.
    fn names(&self, client: &Client) -> bool {
        self.addr.as_ref() == Some(&client.addr) && self.token.as_ref() == Some(&client.token)
    }
}

/// The saved config; empty when there is no file yet.
///
/// # Errors
///
/// If the file can't be read or isn't valid TOML.
pub fn load() -> Result<FileConfig> {
    let path = dirs::client_config()?;
    match fs::read_optional(&path)? {
        Some(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        None => Ok(FileConfig::default()),
    }
}

/// Writes `config` where [`load`] reads it, readable only by its owner; the
/// path it went to.
///
/// # Errors
///
/// If the config directory is unknown or the file can't be written.
pub fn save(config: &FileConfig) -> Result<PathBuf> {
    let path = dirs::client_config()?;
    fs::write_private(&path, &toml::to_string(config)?)?;
    Ok(path)
}

/// Resolved client connection settings.
#[derive(Clone)]
pub struct Client {
    /// The server's address, `host:port`.
    pub addr: String,
    /// The admin token it takes.
    pub token: String,
}

impl Client {
    /// The server a pasted connection link names.
    ///
    /// # Errors
    ///
    /// If `text` isn't a connection link.
    pub fn from_link(text: &str) -> Result<Self> {
        let link: Link = text.parse()?;
        Ok(Self {
            addr: link.addr(),
            token: link.token,
        })
    }

    /// The connection link that names this server, to show again.
    pub fn link(&self) -> String {
        Link::for_addr(&self.token, &self.addr).to_string()
    }

    /// A control client for the server.
    ///
    /// # Errors
    ///
    /// If the server can't be reached or the token is malformed.
    pub async fn connect(&self) -> Result<ControlClient> {
        connect_control(&self.addr, &self.token)
            .await
            .with_context(|| format!("connecting to {}", self.addr))
    }
}

/// Saves the server, so the next client run talks to it.
///
/// # Errors
///
/// As [`save`].
pub fn remember(client: &Client) -> Result<PathBuf> {
    save(&FileConfig {
        addr: Some(client.addr.clone()),
        token: Some(client.token.clone()),
    })
}

/// Forgets the saved server, so the next client run asks for a link; the
/// config file it was saved in, if it was.
///
/// # Errors
///
/// If the config directory is unknown or the file can't be removed.
pub fn forget() -> Result<Option<PathBuf>> {
    let path = dirs::client_config()?;
    Ok(fs::remove(&path)?.then_some(path))
}

/// Forgets the saved server if it is `client`'s, and not one flags or the
/// environment named instead; a config file that can't be read goes too.
///
/// # Errors
///
/// As [`forget`].
pub fn forget_if_saved(client: &Client) -> Result<()> {
    match load() {
        Ok(saved) if !saved.names(client) => Ok(()),
        _ => forget().map(drop),
    }
}

/// The server to talk to, from flags/env, then the config file, then a
/// server running here.
///
/// # Errors
///
/// If the config file can't be read, or nothing names the address or the
/// token.
pub fn resolve(
    addr: Option<String>,
    token: Option<String>,
    token_file: Option<PathBuf>,
) -> Result<Client> {
    let (addr, token) = lookup(addr, token, token_file)?;
    let Some(token) = token else {
        bail!("no admin token: pass --token, set COMMANDANT_TOKEN, or run `commandant login`")
    };
    let Some(addr) = addr else {
        bail!("no server address: pass --addr, set COMMANDANT_ADDR, or run `commandant login`")
    };
    Ok(Client { addr, token })
}

/// The server, as [`resolve`] finds it; none when nothing names it yet,
/// before the first log in.
///
/// # Errors
///
/// If the config file can't be read.
pub fn find(
    addr: Option<String>,
    token: Option<String>,
    token_file: Option<PathBuf>,
) -> Result<Option<Client>> {
    let (addr, token) = lookup(addr, token, token_file)?;
    Ok(addr.zip(token).map(|(addr, token)| Client { addr, token }))
}

/// The server's address and admin token, from flags/env, then the config
/// file, then a server running here; either may be missing.
fn lookup(
    addr: Option<String>,
    token: Option<String>,
    token_file: Option<PathBuf>,
) -> Result<(Option<String>, Option<String>)> {
    let file = load()?;
    let token = match (token, token_file) {
        (Some(t), _) => Some(t),
        (None, Some(path)) => Some(fs::read_trimmed(&path).context("reading the token file")?),
        (None, None) => file.token,
    };
    let addr = addr.or(file.addr).or_else(local_server);
    Ok((addr, token))
}

/// The addresses of a server running on this machine as this user, from
/// the link it wrote.
fn local_server() -> Option<String> {
    let link = dirs::server_data().ok()?.join(dirs::LINK_FILE);
    let link: Link = fs::read_trimmed(&link).ok()?.parse().ok()?;
    Some(link.addr())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::fixtures::link;

    #[test]
    fn a_pasted_link_names_the_server_and_its_token() {
        let pasted = format!("  {}\n", link());
        let client = Client::from_link(&pasted).unwrap();
        assert_eq!(client.addr, link().addr());
        assert_eq!(client.token, link().token);
    }

    #[test]
    fn a_pasted_url_is_no_link() {
        let Err(e) = Client::from_link("https://192.168.1.15:7400") else {
            panic!("a URL was taken for a link");
        };
        assert!(e.to_string().contains("commandant://"), "{e:#}");
    }

    #[test]
    fn a_config_names_the_server_it_saved_and_no_other() {
        let client = Client::from_link(&link().to_string()).unwrap();
        let saved = FileConfig {
            addr: Some(client.addr.clone()),
            token: Some(client.token.clone()),
        };
        assert!(saved.names(&client));
        let elsewhere = Client {
            addr: "https://10.0.0.2:7400".into(),
            ..client.clone()
        };
        assert!(!saved.names(&elsewhere));
        assert!(!FileConfig::default().names(&client), "nothing is saved");
    }

    #[test]
    fn a_client_gives_back_its_link() {
        let client = Client::from_link(&link().to_string()).unwrap();
        assert_eq!(client.link(), link().to_string());
    }
}
