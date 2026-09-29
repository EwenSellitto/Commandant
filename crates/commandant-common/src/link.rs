//! Connection links: `commandant://<opaque>`, one string that carries both
//! where the orchestrator is and the secret to present to it.
//!
//! The opaque part packs the address and the token as bytes (an IP as 4 or 16
//! bytes, the port only when it isn't the default, the token's hex as raw
//! bytes), XOR-scrambled and base64url encoded. That keeps the link short, and
//! the token and address aren't readable at a glance. This is obfuscation, not
//! encryption: anyone with the link can decode it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::DEFAULT_PORT;

pub const SCHEME: &str = "commandant://";

/// First byte of an encoded link, so the format can evolve. Links in the
/// older, textual format still parse.
const FORMAT: u8 = 2;
const TEXT_FORMAT: u8 = 1;
const KEY: &[u8] = b"commandant-link";

/// How the address is packed; `CUSTOM_PORT` is added when a port follows.
const ADDR_V4: u8 = 0;
const ADDR_V6: u8 = 1;
const ADDR_NAME: u8 = 2;
const CUSTOM_PORT: u8 = 0x80;

/// Token prefixes packed as one byte, their hex as raw bytes. Anything else
/// is kept as text (`TOKEN_TEXT`).
const TOKEN_TEXT: u8 = 0;
const TOKEN_PREFIXES: [&str; 3] = ["cmda", "cmdj", "cmdn"];

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
        let mut payload = Vec::new();
        pack_host(&self.host, &mut payload);
        pack_token(&self.token, &mut payload);
        write!(f, "{SCHEME}{}", encode(FORMAT, payload))
    }
}

fn scramble(bytes: &mut [u8]) {
    for (i, b) in bytes.iter_mut().enumerate() {
        *b ^= KEY[i % KEY.len()] ^ (i as u8).wrapping_mul(31);
    }
}

fn checksum(format: u8, bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .fold(format, |acc, b| acc.rotate_left(3) ^ b.wrapping_add(0x5b))
}

fn encode(format: u8, mut payload: Vec<u8>) -> String {
    let sum = checksum(format, &payload);
    scramble(&mut payload);
    let mut bytes = Vec::with_capacity(payload.len() + 2);
    bytes.push(format);
    bytes.extend(payload);
    bytes.push(sum);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The token and `host:port` in an encoded link.
fn decode(opaque: &str) -> Option<(String, String)> {
    let bytes = URL_SAFE_NO_PAD.decode(opaque).ok()?;
    let [format, payload @ .., sum] = bytes.as_slice() else {
        return None;
    };
    let mut payload = payload.to_vec();
    scramble(&mut payload);
    if checksum(*format, &payload) != *sum {
        return None;
    }
    match *format {
        FORMAT => {
            let mut rest = payload.as_slice();
            let host = unpack_host(&mut rest)?;
            Some((unpack_token(rest)?, host))
        }
        TEXT_FORMAT => {
            let text = String::from_utf8(payload).ok()?;
            let (token, host) = text.rsplit_once('@')?;
            Some((token.to_string(), host.to_string()))
        }
        _ => None,
    }
}

fn pack_host(host: &str, out: &mut Vec<u8>) {
    let (name, port) = match host.parse::<SocketAddr>() {
        Ok(addr) => (addr.ip().to_string(), addr.port()),
        Err(_) => match host.rsplit_once(':') {
            Some((name, port)) if port.parse::<u16>().is_ok() => {
                (name.to_string(), port.parse().expect("checked"))
            }
            _ => (host.to_string(), DEFAULT_PORT),
        },
    };
    let port_flag = if port == DEFAULT_PORT { 0 } else { CUSTOM_PORT };
    match name.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            out.push(ADDR_V4 | port_flag);
            out.extend(ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            out.push(ADDR_V6 | port_flag);
            out.extend(ip.octets());
        }
        Err(_) => {
            // Longer names are cut; no DNS name is anywhere near that long.
            let name = &name.as_bytes()[..name.len().min(255)];
            out.push(ADDR_NAME | port_flag);
            out.push(name.len() as u8);
            out.extend(name);
        }
    }
    if port_flag != 0 {
        out.extend(port.to_be_bytes());
    }
}

fn unpack_host(rest: &mut &[u8]) -> Option<String> {
    let (&tag, tail) = rest.split_first()?;
    *rest = tail;
    let name = match tag & !CUSTOM_PORT {
        ADDR_V4 => IpAddr::from(<[u8; 4]>::try_from(take(rest, 4)?).ok()?).to_string(),
        ADDR_V6 => format!(
            "[{}]",
            IpAddr::from(<[u8; 16]>::try_from(take(rest, 16)?).ok()?)
        ),
        ADDR_NAME => {
            let len = *take(rest, 1)?.first()?;
            String::from_utf8(take(rest, len.into())?.to_vec()).ok()?
        }
        _ => return None,
    };
    let port = if tag & CUSTOM_PORT != 0 {
        u16::from_be_bytes(take(rest, 2)?.try_into().ok()?)
    } else {
        DEFAULT_PORT
    };
    Some(format!("{name}:{port}"))
}

/// The token goes last, so its length is whatever remains.
fn pack_token(token: &str, out: &mut Vec<u8>) {
    let packed = token.split_once('_').and_then(|(prefix, secret)| {
        let kind = TOKEN_PREFIXES.iter().position(|p| *p == prefix)?;
        // Only lowercase hex survives the round trip unchanged.
        let lowercase = !secret.bytes().any(|b| b.is_ascii_uppercase());
        let bytes = hex::decode(secret).ok().filter(|_| lowercase)?;
        Some((kind as u8 + 1, bytes))
    });
    match packed {
        Some((kind, bytes)) => {
            out.push(kind);
            out.extend(bytes);
        }
        None => {
            out.push(TOKEN_TEXT);
            out.extend(token.as_bytes());
        }
    }
}

fn unpack_token(rest: &[u8]) -> Option<String> {
    let (&kind, bytes) = rest.split_first()?;
    match kind {
        TOKEN_TEXT => String::from_utf8(bytes.to_vec()).ok(),
        _ => {
            let prefix = TOKEN_PREFIXES.get(usize::from(kind) - 1)?;
            Some(format!("{prefix}_{}", hex::encode(bytes)))
        }
    }
    .filter(|token| !token.is_empty())
}

fn take<'a>(rest: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    if rest.len() < n {
        return None;
    }
    let (taken, tail) = rest.split_at(n);
    *rest = tail;
    Some(taken)
}

impl std::str::FromStr for Link {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let payload = s
            .trim()
            .strip_prefix(SCHEME)
            .with_context(|| format!("a link starts with {SCHEME}"))?
            .trim_end_matches('/');
        let (token, host) = if is_hand_written(payload) {
            let (token, host) = payload.rsplit_once('@').expect("has an @");
            (token.to_string(), host.to_string())
        } else {
            decode(payload).context("invalid link (truncated or mistyped?)")?
        };
        if token.is_empty() || host.is_empty() {
            anyhow::bail!("invalid link");
        }
        Ok(Self::new(&token, &host))
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

    #[test]
    fn packs_addresses_and_tokens_short() {
        let admin = format!("cmda_{}", "ab".repeat(32));
        for (token, host) in [
            (admin.as_str(), "192.168.1.15:7400"),
            (admin.as_str(), "[fd00::1]:9000"),
            ("cmdj_00ff", "box.lan:7400"),
            ("cmdn_00ff", "box.lan:81"),
            // Not a token Commandant makes: kept as text.
            ("cmda_ABCD", "10.0.0.1:7400"),
            ("secret", "10.0.0.1:7400"),
        ] {
            let link = Link::new(token, host);
            assert_eq!(
                link.to_string().parse::<Link>().unwrap(),
                link,
                "{token}@{host}"
            );
        }
        // 1 format + 5 address + 33 token + 1 checksum bytes.
        let text = Link::new(&admin, "192.168.1.15:7400").to_string();
        assert_eq!(text.len(), SCHEME.len() + 54, "{text}");
    }

    #[test]
    fn parses_links_in_the_older_format() {
        let payload = b"cmda_abc@10.0.0.1:7400".to_vec();
        let text = format!("{SCHEME}{}", encode(TEXT_FORMAT, payload));
        assert_eq!(
            text.parse::<Link>().unwrap(),
            Link::new("cmda_abc", "10.0.0.1:7400")
        );
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
