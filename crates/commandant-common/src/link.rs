//! Connection links: `commandant://<opaque>`, one string that carries both
//! where the orchestrator is and the secret to present to it.
//!
//! The opaque part is `<token>@<host>:<port>`, XOR-scrambled and base64url
//! encoded, so the token and address aren't readable at a glance. This is
//! obfuscation, not encryption: anyone with the link can decode it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::DEFAULT_PORT;

pub const SCHEME: &str = "commandant://";

/// First byte of an encoded link, so the format can evolve.
const FORMAT: u8 = 1;
const KEY: &[u8] = b"commandant-link";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub token: String,
    /// `host:port`, as advertised by the orchestrator.
    pub host: String,
}

impl Link {
    pub fn new(token: &str, host: &str) -> Self {
        Self {
            token: token.to_string(),
            host: with_port(host, DEFAULT_PORT),
        }
    }

    /// Link to an orchestrator URL such as `http://10.0.0.1:7400`.
    pub fn for_addr(token: &str, addr: &str) -> Self {
        let host = addr.trim_start_matches("http://").trim_end_matches('/');
        Self::new(token, host)
    }

    /// Orchestrator URL, e.g. `http://10.0.0.1:7400`.
    pub fn addr(&self) -> String {
        format!("http://{}", self.host)
    }
}

impl std::fmt::Display for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{SCHEME}{}",
            encode(&format!("{}@{}", self.token, self.host))
        )
    }
}

fn scramble(bytes: &mut [u8]) {
    for (i, b) in bytes.iter_mut().enumerate() {
        *b ^= KEY[i % KEY.len()] ^ (i as u8).wrapping_mul(31);
    }
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .fold(FORMAT, |acc, b| acc.rotate_left(3) ^ b.wrapping_add(0x5b))
}

fn encode(plain: &str) -> String {
    let mut payload = plain.as_bytes().to_vec();
    let sum = checksum(&payload);
    scramble(&mut payload);
    let mut bytes = Vec::with_capacity(payload.len() + 2);
    bytes.push(FORMAT);
    bytes.extend(payload);
    bytes.push(sum);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode(opaque: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD.decode(opaque).ok()?;
    let [FORMAT, payload @ .., sum] = bytes.as_slice() else {
        return None;
    };
    let mut payload = payload.to_vec();
    scramble(&mut payload);
    if checksum(&payload) != *sum {
        return None;
    }
    String::from_utf8(payload).ok()
}

impl std::str::FromStr for Link {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let payload = s
            .trim()
            .strip_prefix(SCHEME)
            .with_context(|| format!("a link starts with {SCHEME}"))?
            .trim_end_matches('/');
        let token_at_host = if is_hand_written(payload) {
            payload.to_string()
        } else {
            decode(payload).context("invalid link (truncated or mistyped?)")?
        };
        let (token, host) = token_at_host
            .rsplit_once('@')
            .filter(|(token, host)| !token.is_empty() && !host.is_empty())
            .context("invalid link")?;
        Ok(Self::new(token, host))
    }
}

/// `commandant://<token>@<host>` rather than the obfuscated form.
fn is_hand_written(payload: &str) -> bool {
    payload.contains('@')
}

/// Appends `default_port` unless `host` already has a port.
pub fn with_port(host: &str, default_port: u16) -> String {
    let is_bare_ipv6 = host.parse::<Ipv6Addr>().is_ok();
    let ends_with_port = host
        .rsplit_once(':')
        .is_some_and(|(_, port)| port.parse::<u16>().is_ok());
    if is_bare_ipv6 {
        format!("[{host}]:{default_port}")
    } else if ends_with_port {
        host.to_string()
    } else {
        format!("{host}:{default_port}")
    }
}

/// The address other machines most likely reach this one on: the source
/// address of the default route. Connecting a UDP socket sends nothing.
pub fn primary_ip() -> Option<IpAddr> {
    const NEVER_ROUTED: &str = "192.0.2.1:9"; // TEST-NET-1
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect(NEVER_ROUTED).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// Whether `ip` belongs to this machine (only local addresses can be bound).
fn is_local(ip: IpAddr) -> bool {
    ip.is_loopback() || UdpSocket::bind(SocketAddr::new(ip, 0)).is_ok()
}

pub fn loopback_of_same_family(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
    }
}

/// Rewrites an `http://host:port` URL to loopback when `host` is one of this
/// machine's own addresses, so local clients don't depend on hairpin routing or
/// firewall rules for the public interface.
pub async fn prefer_loopback(url: &str) -> String {
    let Some(authority) = url.strip_prefix("http://") else {
        return url.to_string();
    };
    let Ok(mut addrs) = tokio::net::lookup_host(authority.trim_end_matches('/')).await else {
        return url.to_string();
    };
    match addrs.find(|addr| is_local(addr.ip())) {
        Some(local) => {
            tracing::debug!(%url, "orchestrator is on this machine; using loopback");
            let loopback = SocketAddr::new(loopback_of_same_family(local.ip()), local.port());
            format!("http://{loopback}")
        }
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints() {
        let link = Link::new("cmda_abc", "10.0.0.1:7400");
        let text = link.to_string();
        assert!(text.starts_with(SCHEME));
        assert!(
            !text.contains("cmda") && !text.contains("10.0.0.1"),
            "{text}"
        );
        assert_eq!(text.parse::<Link>().unwrap(), link);

        // A typo is caught by the checksum.
        let mut typo = text.clone().into_bytes();
        let last = typo.len() - 3;
        typo[last] = if typo[last] == b'A' { b'B' } else { b'A' };
        assert!(String::from_utf8(typo).unwrap().parse::<Link>().is_err());

        assert_eq!(
            Link::for_addr("t", "http://10.0.0.1:7400/"),
            Link::new("t", "10.0.0.1:7400")
        );

        // Plain links still work.
        let link: Link = "commandant://cmda_abc@10.0.0.1:7400".parse().unwrap();
        assert_eq!(link.token, "cmda_abc");
        assert_eq!(link.addr(), "http://10.0.0.1:7400");

        let link: Link = "commandant://t@box.lan".parse().unwrap();
        assert_eq!(link.host, "box.lan:7400");
        let link: Link = "commandant://t@[fd00::1]:9000/".parse().unwrap();
        assert_eq!(link.addr(), "http://[fd00::1]:9000");
        assert_eq!(Link::new("t", "fd00::1").host, "[fd00::1]:7400");

        assert!("http://10.0.0.1:7400".parse::<Link>().is_err());
        assert!("commandant://10.0.0.1:7400".parse::<Link>().is_err());
    }

    #[tokio::test]
    async fn rewrites_own_addresses_to_loopback() {
        if let Some(ip) = primary_ip() {
            let url = format!("http://{}", SocketAddr::new(ip, 7400));
            let expected = if ip.is_ipv4() {
                "http://127.0.0.1:7400"
            } else {
                "http://[::1]:7400"
            };
            assert_eq!(prefer_loopback(&url).await, expected);
        }
        // TEST-NET-3 is never assigned to a local interface.
        assert_eq!(
            prefer_loopback("http://203.0.113.7:7400").await,
            "http://203.0.113.7:7400"
        );
    }
}
