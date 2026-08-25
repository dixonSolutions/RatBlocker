//! Forwarding to upstream resolvers, in plaintext or over TLS.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
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

fn resolv_conf_candidates() -> Vec<PathBuf> {
    RESOLV_CONF_CANDIDATES.iter().map(PathBuf::from).collect()
}

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
    system_upstreams_from(&resolv_conf_candidates(), own)
}

/// The body of `system_upstreams`, with the files to consult passed in so the
/// parsing and loop-avoidance rules can be tested without touching `/run`.
fn system_upstreams_from(paths: &[PathBuf], own: &[SocketAddr]) -> Vec<Upstream> {
    for path in paths {
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
            // Debug, not info: this runs on every refresh, and the change
            // itself is what is worth a line in the log, not the polling.
            tracing::debug!(
                source = %path.display(),
                count = found.len(),
                "read the system's resolvers"
            );
            return found;
        }
    }
    tracing::debug!("no usable system resolver found; falling back to the configured upstreams");
    Vec::new()
}

/// Expand `system` entries into the machine's resolvers, keeping the configured
/// order so a static entry after `system` still acts as a fallback. They are
/// read by the caller: whether the machine has any is a different question from
/// whether the expansion is empty, and `refresh` turns on the first.
fn expand(configured: &[Upstream], system: &[Upstream]) -> Vec<Upstream> {
    let mut expanded = Vec::with_capacity(configured.len());
    for upstream in configured {
        match upstream {
            Upstream::System => expanded.extend(system.iter().cloned()),
            other => expanded.push(other.clone()),
        }
    }
    expanded
}

/// How often the machine's resolvers are re-read while a `system` upstream is
/// configured.
///
/// Short, because the window it covers is the one where the machine has no
/// working DNS at all: between a VPN or a Wi-Fi network replacing the system
/// resolvers and RatBlocker noticing. Re-reading costs one read of a file that
/// is well under a kilobyte and is not on the query path.
pub const SYSTEM_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A pool of upstreams, tried in order.
pub struct Resolver {
    /// Upstreams exactly as configured, with `system` left unexpanded so it can
    /// be resolved again whenever the machine's own resolvers change.
    configured: Vec<Upstream>,
    /// The expansion currently in use. Swapped wholesale rather than mutated,
    /// like the engine in `DaemonState`, so a query in flight always sees one
    /// consistent list.
    active: RwLock<Arc<Vec<Upstream>>>,
    /// Addresses RatBlocker itself listens on, excluded from every expansion.
    own: Vec<SocketAddr>,
    /// Whether anything would be gained by re-reading. False when no `system`
    /// entry is configured, which makes `refresh` free.
    follows_system: bool,
    /// Files consulted for a `system` upstream. A field rather than the
    /// constant so the refresh path can be tested without writing to `/run`.
    resolv_paths: Vec<PathBuf>,
    timeout: Duration,
    tls: Arc<ClientConfig>,
}

impl Resolver {
    /// `own` lists the addresses RatBlocker itself listens on, so a `system`
    /// upstream cannot expand into a query loop back into the proxy.
    pub fn new(upstreams: Vec<Upstream>, timeout: Duration, own: &[SocketAddr]) -> Result<Self> {
        Self::build(upstreams, timeout, own, resolv_conf_candidates())
    }

    fn build(
        upstreams: Vec<Upstream>,
        timeout: Duration,
        own: &[SocketAddr],
        resolv_paths: Vec<PathBuf>,
    ) -> Result<Self> {
        if upstreams.is_empty() {
            bail!("at least one upstream resolver is required");
        }
        let active = expand(&upstreams, &system_upstreams_from(&resolv_paths, own));
        if active.is_empty() {
            bail!("no usable upstream resolver: `system` found none and no fallback is configured");
        }
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            follows_system: upstreams.contains(&Upstream::System),
            configured: upstreams,
            active: RwLock::new(Arc::new(active)),
            own: own.to_vec(),
            resolv_paths,
            timeout,
            tls: Arc::new(tls),
        })
    }

    /// True when the resolver tracks the machine's own DNS configuration and so
    /// has something to re-read when the network changes.
    pub fn follows_system(&self) -> bool {
        self.follows_system
    }

    /// The set of upstreams in use right now.
    ///
    /// Kept by the caller across a query and compared with `Arc::ptr_eq`
    /// against a later reading, it answers the only question a finished query
    /// has: is the set it went to still the set in use, or did the network move
    /// underneath it? A new set is always a new allocation, so the comparison
    /// holds however the swap happened — this query's own refresh, the poll, or
    /// another query that failed alongside it.
    pub fn active(&self) -> Arc<Vec<Upstream>> {
        self.active
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Re-read the machine's resolvers and swap them in if they have changed.
    ///
    /// Returns true when the active set changed, which means answers learned
    /// from the previous network must not be served any more.
    ///
    /// Without this the list expanded at startup would outlive the network it
    /// describes. Connecting a VPN replaces the machine's resolvers and, with a
    /// kill switch, firewalls off the old ones; a resolver still pointed at
    /// them resolves nothing at all, and any query that does escape leaks past
    /// the tunnel to the previous network's DNS server.
    pub fn refresh(&self) -> bool {
        if !self.follows_system {
            return false;
        }
        // What decides this is whether the machine has resolvers of its own,
        // not whether the expansion is empty: with fallbacks configured behind
        // `system` — as the shipped defaults have — it never is, and an
        // unreadable resolv.conf would read as a move to the public resolvers.
        let system = system_upstreams_from(&self.resolv_paths, &self.own);
        if system.is_empty() {
            // Better to keep querying resolvers that may have gone away than to
            // hold none, or to drop to the fallbacks: the machine may simply be
            // between networks, the old set may yet come back, and a public
            // resolver would take names outside a tunnel that is still up.
            tracing::warn!("no usable resolver found while refreshing; keeping the current set");
            return false;
        }
        let candidate = expand(&self.configured, &system);

        let mut guard = self.active.write().unwrap_or_else(|e| e.into_inner());
        if **guard == candidate {
            return false;
        }
        tracing::info!(
            upstreams = ?candidate.iter().map(Upstream::describe).collect::<Vec<_>>(),
            "the machine's resolvers changed; following them"
        );
        *guard = Arc::new(candidate);
        true
    }

    pub fn describe(&self) -> Vec<String> {
        self.active().iter().map(Upstream::describe).collect()
    }

    /// Forward a query, trying each of `upstreams` until one answers.
    ///
    /// One pass, and no refresh: recovering from a network change also has to
    /// drop the cache, so that decision belongs to the caller that owns both.
    /// The set is passed in for the same reason — the caller holds the handle
    /// `active` gave it, and so can still tell, once the query is over, which
    /// set the answer or the failure came from.
    pub async fn resolve(&self, upstreams: &[Upstream], request: &[u8]) -> Result<Vec<u8>> {
        let mut last: Option<anyhow::Error> = None;
        for upstream in upstreams {
            let attempt = match upstream {
                // `expand` removes these; nothing to do at query time.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Once;

    /// A directory under the system temporary directory, removed on drop, so a
    /// test can rewrite a `resolv.conf` the way a network change does.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "ratblocker-upstream-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).expect("creating the temporary directory");
            Self(path)
        }

        fn resolv_conf(&self, contents: &str) -> PathBuf {
            let path = self.0.join("resolv.conf");
            std::fs::write(&path, contents).expect("writing resolv.conf");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `ClientConfig::builder` needs a process-wide provider, which the daemon
    /// installs in `main`. Tests have no `main`, so install it here too.
    fn crypto_provider() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    fn listen() -> Vec<SocketAddr> {
        vec!["127.0.0.2:53".parse().unwrap()]
    }

    fn plain(address: &str) -> Upstream {
        Upstream::Plain { address: address.parse().unwrap() }
    }

    fn resolver(configured: Vec<Upstream>, resolv: &Path) -> Resolver {
        crypto_provider();
        Resolver::build(
            configured,
            Duration::from_secs(1),
            &listen(),
            vec![resolv.to_path_buf()],
        )
        .expect("building the resolver")
    }

    #[test]
    fn nameservers_are_parsed_and_loop_risks_excluded() {
        let dir = TempDir::new();
        let path = dir.resolv_conf(concat!(
            "# a comment\n",
            "search example.test\n",
            "nameserver 127.0.0.2\n",   // RatBlocker itself
            "nameserver 127.0.0.53\n",  // the systemd-resolved stub
            "nameserver 127.0.0.54\n",  // and its delegate
            "nameserver 192.168.1.1\n",
            "nameserver not-an-address\n",
        ));
        let found = system_upstreams_from(&[path], &listen());
        assert_eq!(found, vec![plain("192.168.1.1:53")]);
    }

    #[test]
    fn an_interface_scope_suffix_is_ignored() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver fe80::1%wlan0\n");
        let found = system_upstreams_from(&[path], &listen());
        assert_eq!(found, vec![plain("[fe80::1]:53")]);
    }

    /// The regression this whole mechanism exists for: a VPN coming up replaces
    /// the machine's resolvers, and the previous ones stop answering. Expanding
    /// `system` once at startup left the proxy talking to a resolver that was no
    /// longer reachable, which took down DNS for the whole machine — including
    /// the names the VPN itself needed.
    #[test]
    fn resolvers_replaced_by_a_tunnel_are_followed() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver 192.168.1.1\n");
        let resolver = resolver(vec![Upstream::System], &path);
        assert_eq!(resolver.describe(), vec!["udp://192.168.1.1:53"]);

        dir.resolv_conf("nameserver 10.2.0.1\n");
        assert!(resolver.refresh(), "the change should have been noticed");
        assert_eq!(resolver.describe(), vec!["udp://10.2.0.1:53"]);
    }

    #[test]
    fn an_unchanged_resolv_conf_reports_no_change() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver 192.168.1.1\n");
        let resolver = resolver(vec![Upstream::System], &path);

        // Rewritten with the same servers: the file is new, the resolvers are
        // not, and reporting a change here would flush the cache on every poll.
        dir.resolv_conf("# regenerated\nsearch example.test\nnameserver 192.168.1.1\n");
        assert!(!resolver.refresh());
        assert_eq!(resolver.describe(), vec!["udp://192.168.1.1:53"]);
    }

    #[test]
    fn the_last_known_resolvers_are_kept_when_none_can_be_read() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver 192.168.1.1\n");
        let resolver = resolver(vec![Upstream::System], &path);

        // Between networks: holding no upstream at all would be worse than
        // holding one that may yet come back.
        std::fs::remove_file(&path).unwrap();
        assert!(!resolver.refresh());
        assert_eq!(resolver.describe(), vec!["udp://192.168.1.1:53"]);
    }

    /// The same rule under the shipped defaults, where public resolvers sit
    /// behind `system`. Those are there for a machine whose resolvers cannot be
    /// read at all, not for the gap between two networks: dropping to them
    /// there discards the resolvers the machine is about to have back, flushes
    /// the cache, and sends names to a public resolver outside whatever tunnel
    /// is still up.
    #[test]
    fn configured_fallbacks_do_not_displace_the_last_known_resolvers() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver 192.168.1.1\n");
        let resolver = resolver(vec![Upstream::System, plain("9.9.9.9:53")], &path);

        std::fs::remove_file(&path).unwrap();
        assert!(!resolver.refresh());
        assert_eq!(
            resolver.describe(),
            vec!["udp://192.168.1.1:53", "udp://9.9.9.9:53"]
        );
    }

    #[test]
    fn configured_fallbacks_keep_their_place_across_a_refresh() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver 192.168.1.1\n");
        let resolver = resolver(vec![Upstream::System, plain("9.9.9.9:53")], &path);
        assert_eq!(
            resolver.describe(),
            vec!["udp://192.168.1.1:53", "udp://9.9.9.9:53"]
        );

        dir.resolv_conf("nameserver 10.2.0.1\n");
        assert!(resolver.refresh());
        assert_eq!(
            resolver.describe(),
            vec!["udp://10.2.0.1:53", "udp://9.9.9.9:53"]
        );
    }

    /// The proxy decides whether to retry a failed query, and whether an answer
    /// may still be cached, by comparing the handle it queried against the
    /// current one — which is the only way a query that lost the race to swap
    /// the resolvers can still tell that they moved. Both depend on a swap
    /// producing a distinct handle, and on an unchanged set keeping its own.
    #[test]
    fn the_active_handle_changes_only_when_the_resolvers_do() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver 192.168.1.1\n");
        let resolver = resolver(vec![Upstream::System], &path);
        let queried = resolver.active();

        assert!(!resolver.refresh());
        assert!(Arc::ptr_eq(&queried, &resolver.active()));

        dir.resolv_conf("nameserver 10.2.0.1\n");
        assert!(resolver.refresh());
        assert!(!Arc::ptr_eq(&queried, &resolver.active()));
    }

    #[test]
    fn a_static_resolver_does_not_follow_the_machine() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver 192.168.1.1\n");
        let resolver = resolver(vec![plain("9.9.9.9:53")], &path);

        assert!(!resolver.follows_system());
        dir.resolv_conf("nameserver 10.2.0.1\n");
        assert!(!resolver.refresh(), "an explicit upstream is the user's choice");
        assert_eq!(resolver.describe(), vec!["udp://9.9.9.9:53"]);
    }

    #[test]
    fn a_system_entry_makes_the_resolver_follow_the_machine() {
        let dir = TempDir::new();
        let path = dir.resolv_conf("nameserver 192.168.1.1\n");
        assert!(resolver(vec![Upstream::System], &path).follows_system());
        assert!(resolver(vec![Upstream::System, plain("9.9.9.9:53")], &path).follows_system());
    }
}
