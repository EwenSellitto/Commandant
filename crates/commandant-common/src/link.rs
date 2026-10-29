//! Connection links: `commandant://<opaque>`, one string that carries where
//! the orchestrator is, the secret to present to it, and the fingerprint of
//! its TLS certificate, so whoever uses the link talks to that orchestrator
//! and no other.
//!
//! The opaque part packs the address and the token as bytes (an IP as 4 or 16
//! bytes, the port only when it isn't the default, the token's hex as raw
//! bytes), XOR-scrambled and base64url encoded. That keeps the link short, and
//! the token and address aren't readable at a glance. This is obfuscation, not
//! encryption: anyone with the link can decode it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use crate::DEFAULT_PORT;

/// What every connection link starts with.
pub const SCHEME: &str = "commandant://";

/// First byte of an encoded link, so the format can evolve. Links in the
/// older, textual format still parse.
const FORMAT: u8 = 2;
const TEXT_FORMAT: u8 = 1;
/// Several addresses, tried in turn: a count byte, the addresses, the token.
const MULTI_FORMAT: u8 = 3;
/// `FORMAT` and `MULTI_FORMAT` with the server's [`Pin`] first.
const PINNED_FORMAT: u8 = 4;
const PINNED_MULTI_FORMAT: u8 = 5;
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

/// A connection link: where a server is, the token it takes and, if pinned,
/// the certificate it must show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The admin or join token it hands over.
    pub token: String,
    /// `host:port`s the orchestrator may be reached on, as advertised: a
    /// client tries each (a public name, say, then a LAN address).
    pub hosts: Vec<String>,
    /// The orchestrator's certificate, which it must show; links from before
    /// it spoke TLS have none, and no longer work.
    pub pin: Option<Pin>,
}

/// The SHA-256 of an orchestrator's TLS certificate. Whoever has its link
/// connects only to a server showing that certificate, so nobody in between
/// can pose as it, or read what is said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin(pub [u8; 32]);

impl Pin {
    /// The pin of a DER-encoded certificate.
    pub fn of(certificate: &[u8]) -> Self {
        Self(Sha256::digest(certificate).into())
    }
}

/// `sha256=<hex>`, as an address carries it.
impl std::fmt::Display for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sha256={}", hex::encode(self.0))
    }
}

impl std::str::FromStr for Pin {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let hex = s.strip_prefix("sha256=").context("a pin is sha256=<hex>")?;
        let bytes = hex::decode(hex).context("a pin is sha256=<hex>")?;
        let bytes = bytes.try_into().ok().context("a pin is 32 bytes")?;
        Ok(Self(bytes))
    }
}

/// One of an orchestrator's addresses, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `host:port`.
    pub host: String,
    /// The certificate it must show.
    pub pin: Pin,
}

/// Takes apart an orchestrator's URL, `https://host:port#sha256=<hex>`: it
/// is spoken to over TLS, and must show that certificate.
///
/// # Errors
///
/// If `url` isn't `https` or carries no pin.
pub fn target(url: &str) -> Result<Target> {
    match parse_url(url)? {
        ("https", host, Some(pin)) => Ok(Target { host, pin }),
        ("https", _, None) => bail!(
            "{url} has no fingerprint of the server's certificate (#sha256=…), \
             so anyone could pose as it: use the server's link (commandant://…)"
        ),
        ("http", ..) => bail!(
            "{url} is spoken to in plain text, which servers no longer answer: \
             use the server's current link (commandant://…)"
        ),
        _ => bail!("{url:?} is no address: it starts with https://"),
    }
}

/// An URL's scheme, `host:port` and pin.
fn parse_url(url: &str) -> Result<(&str, String, Option<Pin>)> {
    let (scheme, rest) = url
        .split_once("://")
        .with_context(|| format!("{url:?} is no address: it starts with https://"))?;
    let (host, pin) = match rest.split_once('#') {
        Some((host, pin)) => (host, Some(pin.parse::<Pin>()?)),
        None => (rest, None),
    };
    Ok((scheme, host.trim_end_matches('/').to_string(), pin))
}

/// The URL of `host`, showing the certificate `pin` names; with no pin,
/// what a link from before TLS stood for.
pub fn url(host: &str, pin: Option<&Pin>) -> String {
    match pin {
        Some(pin) => format!("https://{host}#{pin}"),
        None => format!("http://{host}"),
    }
}

impl Link {
    /// `hosts` is one `host[:port]`, or several separated by commas.
    pub fn new(token: &str, hosts: &str) -> Self {
        Self {
            token: token.to_string(),
            hosts: hosts
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(|h| with_port(h, DEFAULT_PORT))
                .collect(),
            pin: None,
        }
    }

    /// The same link, naming the orchestrator's certificate.
    pub fn with_pin(self, pin: Option<Pin>) -> Self {
        Self { pin, ..self }
    }

    /// Link to orchestrator URLs such as `https://10.0.0.1:7400#sha256=…`,
    /// comma-separated; those that aren't URLs are left out.
    pub fn for_addr(token: &str, addr: &str) -> Self {
        let parsed: Vec<_> = urls(addr).filter_map(|url| parse_url(url).ok()).collect();
        let hosts: Vec<&str> = parsed.iter().map(|(_, host, _)| host.as_str()).collect();
        let pin = parsed.iter().find_map(|(_, _, pin)| *pin);
        Self::new(token, &hosts.join(",")).with_pin(pin)
    }

    /// The orchestrator's URLs, comma-separated: `https://10.0.0.1:7400#sha256=…`,
    /// or `http://10.0.0.1:7400` for a link with no pin.
    pub fn addr(&self) -> String {
        let urls: Vec<String> = self
            .hosts
            .iter()
            .map(|host| url(host, self.pin.as_ref()))
            .collect();
        urls.join(",")
    }
}

/// The URLs in an orchestrator address, which may list several separated by
/// commas, to try in turn.
pub fn urls(addr: &str) -> impl Iterator<Item = &str> {
    addr.split(',').map(str::trim).filter(|u| !u.is_empty())
}

impl std::fmt::Display for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut payload = Vec::new();
        if let Some(pin) = &self.pin {
            payload.extend(pin.0);
        }
        // One address keeps the shorter format.
        let multi = match self.hosts.as_slice() {
            [host] => {
                pack_host(host, &mut payload);
                false
            }
            hosts => {
                let count = u8::try_from(hosts.len()).unwrap_or(u8::MAX);
                payload.push(count);
                for host in &hosts[..usize::from(count)] {
                    pack_host(host, &mut payload);
                }
                true
            }
        };
        let format = match (self.pin.is_some(), multi) {
            (false, false) => FORMAT,
            (false, true) => MULTI_FORMAT,
            (true, false) => PINNED_FORMAT,
            (true, true) => PINNED_MULTI_FORMAT,
        };
        pack_token(&self.token, &mut payload);
        write!(f, "{SCHEME}{}", encode(format, payload))
    }
}

fn scramble(bytes: &mut [u8]) {
    for ((i, b), round) in bytes.iter_mut().enumerate().zip((0..=u8::MAX).cycle()) {
        *b ^= KEY[i % KEY.len()] ^ round.wrapping_mul(31);
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

/// The token, `host:port`s (comma-separated) and pin in an encoded link.
fn decode(opaque: &str) -> Option<(String, String, Option<Pin>)> {
    let bytes = URL_SAFE_NO_PAD.decode(opaque).ok()?;
    let [format, payload @ .., sum] = bytes.as_slice() else {
        return None;
    };
    let mut payload = payload.to_vec();
    scramble(&mut payload);
    if checksum(*format, &payload) != *sum {
        return None;
    }
    let mut rest = payload.as_slice();
    let pin = match *format {
        PINNED_FORMAT | PINNED_MULTI_FORMAT => Some(Pin(take(&mut rest, 32)?.try_into().ok()?)),
        _ => None,
    };
    match *format {
        FORMAT | PINNED_FORMAT => {
            let host = unpack_host(&mut rest)?;
            Some((unpack_token(rest)?, host, pin))
        }
        MULTI_FORMAT | PINNED_MULTI_FORMAT => {
            let count = *take(&mut rest, 1)?.first()?;
            let hosts = (0..count)
                .map(|_| unpack_host(&mut rest))
                .collect::<Option<Vec<_>>>()?;
            Some((unpack_token(rest)?, hosts.join(","), pin))
        }
        TEXT_FORMAT => {
            let text = String::from_utf8(payload).ok()?;
            let (token, host) = text.rsplit_once('@')?;
            Some((token.to_string(), host.to_string(), None))
        }
        _ => None,
    }
}

fn pack_host(host: &str, out: &mut Vec<u8>) {
    let (name, port) = match host.parse::<SocketAddr>() {
        Ok(addr) => (addr.ip().to_string(), addr.port()),
        Err(_) => match host
            .rsplit_once(':')
            .and_then(|(name, port)| Some((name, port.parse().ok()?)))
        {
            Some((name, port)) => (name.to_string(), port),
            None => (host.to_string(), DEFAULT_PORT),
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
            let len = u8::try_from(name.len()).unwrap_or(u8::MAX);
            out.push(ADDR_NAME | port_flag);
            out.push(len);
            out.extend(&name.as_bytes()[..usize::from(len)]);
        }
    }
    if port_flag != 0 {
        out.extend(port.to_be_bytes());
    }
}

fn unpack_host(rest: &mut &[u8]) -> Option<String> {
    let tag = *take(rest, 1)?.first()?;
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
        let kind: u8 = TOKEN_PREFIXES
            .iter()
            .position(|p| *p == prefix)?
            .try_into()
            .ok()?;
        // Only lowercase hex survives the round trip unchanged.
        let lowercase = !secret.bytes().any(|b| b.is_ascii_uppercase());
        let bytes = hex::decode(secret).ok().filter(|_| lowercase)?;
        Some((kind + 1, bytes))
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
    let (taken, tail) = rest.split_at_checked(n)?;
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
        let (token, host, pin) = match payload.rsplit_once('@') {
            // `commandant://<token>@<host>` rather than the obfuscated form.
            Some((token, host)) => (token.to_string(), host.to_string(), None),
            None => decode(payload).context("invalid link (truncated or mistyped?)")?,
        };
        let link = Self::new(&token, &host).with_pin(pin);
        if token.is_empty() || link.hosts.is_empty() {
            anyhow::bail!("invalid link");
        }
        Ok(link)
    }
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

/// Whether this runs in WSL 2 with its default NAT networking, where the
/// machine's own address is only reachable from the Windows host.
pub fn behind_wsl_nat(ip: IpAddr) -> bool {
    let wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|release| release.to_lowercase().contains("microsoft"));
    // Mirrored networking gives WSL the LAN address itself.
    let nat =
        matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 172 && (16..32).contains(&v4.octets()[1]));
    wsl && nat
}

/// Whether `ip` belongs to this machine (only local addresses can be bound).
fn is_local(ip: IpAddr) -> bool {
    ip.is_loopback() || UdpSocket::bind(SocketAddr::new(ip, 0)).is_ok()
}

/// The loopback address in `ip`'s family, IPv4 or IPv6.
pub fn loopback_of_same_family(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
    }
}

/// Rewrites a `host:port` to loopback when `host` is one of this machine's
/// own addresses, so local clients don't depend on hairpin routing or
/// firewall rules for the public interface.
pub async fn prefer_loopback(host: &str) -> String {
    let Ok(mut addrs) = tokio::net::lookup_host(host).await else {
        return host.to_string();
    };
    match addrs.find(|addr| is_local(addr.ip())) {
        Some(local) => {
            tracing::debug!(%host, "orchestrator is on this machine; using loopback");
            let loopback = SocketAddr::new(loopback_of_same_family(local.ip()), local.port());
            loopback.to_string()
        }
        None => host.to_string(),
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
        let mut typo = text.into_bytes();
        let last = typo.len() - 3;
        typo[last] = if typo[last] == b'A' { b'B' } else { b'A' };
        assert!(String::from_utf8(typo).unwrap().parse::<Link>().is_err());

        assert_eq!(
            Link::for_addr("t", "http://10.0.0.1:7400/"),
            Link::new("t", "10.0.0.1:7400")
        );

        // Several addresses, tried in turn; each with its own port.
        let link = Link::new("cmda_abc", "home.example.org:9000, 192.168.1.15");
        assert_eq!(link.hosts, ["home.example.org:9000", "192.168.1.15:7400"]);
        assert_eq!(
            link.addr(),
            "http://home.example.org:9000,http://192.168.1.15:7400"
        );
        assert_eq!(link.to_string().parse::<Link>().unwrap(), link);
        assert_eq!(Link::for_addr("cmda_abc", &link.addr()), link);
        let typed: Link = "commandant://t@a.lan,b.lan:81".parse().unwrap();
        assert_eq!(typed.hosts, ["a.lan:7400", "b.lan:81"]);

        // Plain links still work.
        let link: Link = "commandant://cmda_abc@10.0.0.1:7400".parse().unwrap();
        assert_eq!(link.token, "cmda_abc");
        assert_eq!(link.addr(), "http://10.0.0.1:7400");

        let link: Link = "commandant://t@box.lan".parse().unwrap();
        assert_eq!(link.hosts, ["box.lan:7400"]);
        let link: Link = "commandant://t@[fd00::1]:9000/".parse().unwrap();
        assert_eq!(link.addr(), "http://[fd00::1]:9000");
        assert_eq!(Link::new("t", "fd00::1").hosts, ["[fd00::1]:7400"]);

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
            let host = SocketAddr::new(ip, 7400).to_string();
            let expected = if ip.is_ipv4() {
                "127.0.0.1:7400"
            } else {
                "[::1]:7400"
            };
            assert_eq!(prefer_loopback(&host).await, expected);
        }
        // TEST-NET-3 is never assigned to a local interface.
        assert_eq!(
            prefer_loopback("203.0.113.7:7400").await,
            "203.0.113.7:7400"
        );
    }

    fn pin() -> Pin {
        Pin::of(b"a certificate")
    }

    #[test]
    fn a_pinned_link_names_the_servers_certificate() {
        let admin = format!("cmda_{}", "ab".repeat(16));
        for hosts in ["192.168.1.15:7400", "home.example.org:9000,192.168.1.15"] {
            let link = Link::new(&admin, hosts).with_pin(Some(pin()));
            let text = link.to_string();
            assert!(!text.contains(&pin().to_string()), "{text}");
            assert_eq!(text.parse::<Link>().unwrap(), link, "{hosts}");
            // Its addresses speak TLS, and carry the pin.
            assert_eq!(Link::for_addr(&admin, &link.addr()), link);
        }
        let link = Link::new("t", "10.0.0.1:7400").with_pin(Some(pin()));
        assert_eq!(link.addr(), format!("https://10.0.0.1:7400#{}", pin()));
        assert!(pin().to_string().starts_with("sha256="));
        assert_eq!(pin().to_string().parse::<Pin>().unwrap(), pin());

        // 32 bytes more than an unpinned link.
        let plain = Link::new(&admin, "192.168.1.15:7400").to_string();
        let pinned = Link::new(&admin, "192.168.1.15:7400").with_pin(Some(pin()));
        let pinned = pinned.to_string();
        assert_eq!(pinned.len() - plain.len(), 43, "{pinned}");
        // Links from before have no pin.
        assert_eq!(plain.parse::<Link>().unwrap().pin, None);
    }

    #[test]
    fn an_address_says_how_to_reach_the_server() {
        let url = format!("https://[fd00::1]:9000/#{}", pin());
        let pinned = target(&url).unwrap();
        assert_eq!(
            (pinned.host.as_str(), pinned.pin),
            ("[fd00::1]:9000", pin())
        );
        // TLS with no pin would trust whoever answers.
        let refused = target("https://10.0.0.1:7400").unwrap_err();
        assert!(refused.to_string().contains("fingerprint"), "{refused}");
        assert!(target("https://10.0.0.1:7400#sha256=00ff").is_err());
        // Plain text, as before TLS, says what to do instead.
        let plain = target("http://10.0.0.1:7400").unwrap_err();
        assert!(plain.to_string().contains("current link"), "{plain}");
        assert!(target("10.0.0.1:7400").is_err());
        // A link from before still names its addresses.
        let old = Link::for_addr("t", "http://10.0.0.1:7400/");
        assert_eq!(
            (old.hosts.as_slice(), old.pin),
            (&["10.0.0.1:7400".to_string()][..], None)
        );
    }
}
