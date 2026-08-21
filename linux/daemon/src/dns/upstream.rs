//! Forwarding to upstream resolvers, in plaintext or over TLS.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

use super::message::MAX_MESSAGE;

/// How a single upstream resolver is reached.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "protocol", rename_all = "kebab-case")]
pub enum Upstream {
    /// Whatever the machine is already configured to use.
    ///
    /// RatBlocker sits *in front of* the system resolver rather than replacing
    /// it, so on a network that only permits its own DNS server — a corporate
    /// or campus network, a captive portal, a split-horizon VPN — inheriting
    /// the existing servers is the only thing that works. It is also the only
    /// way internal names keep resolving.
    System,
    /// Ordinary DNS over UDP, falling back to TCP on truncation.
    Plain { address: SocketAddr },
    /// DNS over TLS (RFC 7858).
    Tls {
        address: SocketAddr,
        /// Name to validate the certificate against.
        server_name: String,
    },
}

impl Upstream {
    pub fn describe(&self) -> String {
        match self {
            Upstream::System => "system".to_string(),
            Upstream::Plain { address } => format!("udp://{address}"),
            Upstream::Tls { address, server_name } => format!("tls://{server_name}@{address}"),
        }
    }
}

/// Files that name the machine's real upstream resolvers, best first.
///
/// `/run/systemd/resolve/resolv.conf` lists the actual servers; the similarly
/// named `stub-resolv.conf` lists systemd-resolved's own stub, which is the
/// thing RatBlocker gets put in front of. Reading the wrong one would build a
/// resolution loop, so only the former is consulted.
const RESOLV_CONF_CANDIDATES: &[&str] = &[
    "/run/systemd/resolve/resolv.conf",
    "/etc/resolv.conf",
];

/// Addresses that must never be used as an upstream, because they are either
/// RatBlocker itself or the resolver stub pointed at RatBlocker.
fn is_loop_risk(ip: IpAddr, own: &[SocketAddr]) -> bool {
    if own.iter().any(|a| a.ip() == ip) {
        return true;
    }
    // systemd-resolved's stub listeners.
    matches!(ip, IpAddr::V4(v4) if v4.octets() == [127, 0, 0, 53] || v4.octets() == [127, 0, 0, 54])
}

/// Read the machine's configured resolvers, excluding anything that would loop.
pub fn system_upstreams(own: &[SocketAddr]) -> Vec<Upstream> {
    for path in RESOLV_CONF_CANDIDATES {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut found = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("nameserver") else {
                continue;
            };
            let Ok(ip) = rest.trim().split('%').next().unwrap_or("").parse::<IpAddr>() else {
                continue;
            };
            if is_loop_risk(ip, own) {
                continue;
            }
            found.push(Upstream::Plain { address: SocketAddr::new(ip, 53) });
        }
        if !found.is_empty() {
            tracing::info!(source = path, count = found.len(), "using the system's resolvers");
            return found;
        }
    }
    tracing::warn!("no usable system resolver found; falling back to the configured upstreams");
    Vec::new()
}

/// A pool of upstreams, tried in order.
pub struct Resolver {
    upstreams: Vec<Upstream>,
    timeout: Duration,
    tls: Arc<ClientConfig>,
}

impl Resolver {
    /// `own` lists the addresses RatBlocker itself listens on, so a `system`
    /// upstream cannot expand into a query loop back into the proxy.
    pub fn new(upstreams: Vec<Upstream>, timeout: Duration, own: &[SocketAddr]) -> Result<Self> {
        if upstreams.is_empty() {
            bail!("at least one upstream resolver is required");
        }
        // Expand `system` entries in place, keeping the configured order so a
        // static entry after `system` still acts as a fallback.
        let mut expanded = Vec::with_capacity(upstreams.len());
        for upstream in upstreams {
            match upstream {
                Upstream::System => expanded.extend(system_upstreams(own)),
                other => expanded.push(other),
            }
        }
        let upstreams = expanded;
        if upstreams.is_empty() {
            bail!("no usable upstream resolver: `system` found none and no fallback is configured");
        }
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            upstreams,
            timeout,
            tls: Arc::new(tls),
        })
    }

    pub fn describe(&self) -> Vec<String> {
        self.upstreams.iter().map(Upstream::describe).collect()
    }

    /// Forward a query, trying each upstream until one answers.
    pub async fn resolve(&self, request: &[u8]) -> Result<Vec<u8>> {
        let mut last: Option<anyhow::Error> = None;
        for upstream in &self.upstreams {
            let attempt = match upstream {
                // Expanded away in `new`; nothing to do at query time.
                Upstream::System => continue,
                Upstream::Plain { address } => self.plain(*address, request).await,
                Upstream::Tls { address, server_name } => {
                    self.tls(*address, server_name, request).await
                }
            };
            match attempt {
                Ok(response) => return Ok(response),
                Err(error) => {
                    tracing::debug!(upstream = %upstream.describe(), %error, "upstream failed");
                    last = Some(error);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no upstream answered")))
    }

    async fn plain(&self, address: SocketAddr, request: &[u8]) -> Result<Vec<u8>> {
        let bind: SocketAddr = if address.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let socket = UdpSocket::bind(bind).await.context("binding upstream socket")?;
        socket.connect(address).await.context("connecting upstream")?;
        socket.send(request).await.context("sending query")?;

        let mut buf = vec![0u8; MAX_MESSAGE];
        let n = tokio::time::timeout(self.timeout, socket.recv(&mut buf))
            .await
            .context("upstream timed out")?
            .context("receiving reply")?;
        buf.truncate(n);

        // TC bit set: retry over TCP so the full answer is not lost.
        if n >= 3 && buf[2] & 0x02 != 0 {
            return self.plain_tcp(address, request).await;
        }
        Ok(buf)
    }

    async fn plain_tcp(&self, address: SocketAddr, request: &[u8]) -> Result<Vec<u8>> {
        let stream = tokio::time::timeout(self.timeout, TcpStream::connect(address))
            .await
            .context("upstream TCP connect timed out")??;
        exchange(stream, request, self.timeout).await
    }

    async fn tls(&self, address: SocketAddr, server_name: &str, request: &[u8]) -> Result<Vec<u8>> {
        let name = ServerName::try_from(server_name.to_string())
            .with_context(|| format!("invalid upstream server name {server_name:?}"))?;
        let stream = tokio::time::timeout(self.timeout, TcpStream::connect(address))
            .await
            .context("upstream TLS connect timed out")??;
        let connector = TlsConnector::from(self.tls.clone());
        let stream = tokio::time::timeout(self.timeout, connector.connect(name, stream))
            .await
            .context("TLS handshake timed out")??;
        exchange(stream, request, self.timeout).await
    }
}

/// One length-prefixed DNS exchange over a stream (RFC 1035 §4.2.2).
async fn exchange<S>(mut stream: S, request: &[u8], timeout: Duration) -> Result<Vec<u8>>
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let len = u16::try_from(request.len()).context("query too large for TCP framing")?;
    let mut framed = Vec::with_capacity(2 + request.len());
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(request);

    tokio::time::timeout(timeout, stream.write_all(&framed))
        .await
        .context("upstream write timed out")??;

    let mut header = [0u8; 2];
    tokio::time::timeout(timeout, stream.read_exact(&mut header))
        .await
        .context("upstream read timed out")??;
    let length = u16::from_be_bytes(header) as usize;
    if length > MAX_MESSAGE {
        bail!("upstream response of {length} bytes exceeds the {MAX_MESSAGE} byte limit");
    }
    let mut response = vec![0u8; length];
    tokio::time::timeout(timeout, stream.read_exact(&mut response))
        .await
        .context("upstream read timed out")??;
    Ok(response)
}
