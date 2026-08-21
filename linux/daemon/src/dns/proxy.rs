//! The DNS proxy: UDP and TCP listeners in front of the filtering engine.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::Semaphore;

use ratblocker_core::FilterDecision;

use super::cache::Key;
use super::message::{self, MAX_MESSAGE};
use crate::state::DaemonState;

/// Concurrent upstream queries. Bounds memory and file descriptors, and stops
/// a query flood from being amplified into an upstream flood.
const MAX_INFLIGHT: usize = 256;

/// Longest a single TCP client may hold a connection open while idle.
const TCP_IDLE: Duration = Duration::from_secs(10);

/// Is this client allowed to use the proxy?
///
/// Refusing anything that is not loopback or a private address is what keeps
/// RatBlocker from becoming an open resolver if it is ever bound to a routable
/// interface by mistake (§7).
fn source_allowed(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                // Unique local (fc00::/7) and link-local (fe80::/10).
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Handle one query and produce the bytes to send back.
async fn handle(state: &Arc<DaemonState>, request: &[u8], source: SocketAddr) -> Option<Vec<u8>> {
    state.counters.queries.fetch_add(1, Ordering::Relaxed);

    if !source_allowed(&source) {
        state.counters.refused_source.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(%source, "refusing DNS query from a non-local source");
        return None;
    }

    let query = match message::parse_query(request) {
        Ok(q) => q,
        Err(error) => {
            // Malformed input is dropped, not answered: replying would make the
            // daemon useful as a reflection amplifier.
            state.counters.errors.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(%source, %error, "dropping malformed DNS query");
            return None;
        }
    };

    // 1. Filtering decision.
    if state.filtering_active() {
        let engine = state.engine();
        let result = engine.evaluate_host(&query.name, None);
        if result.decision == FilterDecision::Block {
            state.counters.blocked.fetch_add(1, Ordering::Relaxed);
            state
                .statistics
                .record(FilterDecision::Block, Some(&query.name));
            tracing::debug!(name = %query.name, rule = ?result.matched_rule_id, "blocked");
            return Some(message::build_blocked(
                &query,
                request,
                state.block_response,
                state.block_ttl,
            ));
        }
    }

    // 2. Cache.
    let key = Key {
        name: query.name.clone(),
        qtype: query.qtype,
        qclass: query.qclass,
    };
    if let Ok(mut cache) = state.cache.lock() {
        if let Some(hit) = cache.get(&key, query.id) {
            return Some(hit);
        }
    }

    // 3. Forward.
    match state.resolver.resolve(request).await {
        Ok(response) => {
            state.counters.forwarded.fetch_add(1, Ordering::Relaxed);
            let ttl = message::minimum_ttl(
                &response,
                query.question_end,
                state.cache_floor.as_secs() as u32,
            );
            if let Ok(mut cache) = state.cache.lock() {
                cache.insert(key, response.clone(), Duration::from_secs(ttl as u64));
            }
            Some(response)
        }
        Err(error) => {
            state.counters.errors.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(name = %query.name, %error, "upstream lookup failed");
            Some(message::build_error(
                &query,
                request,
                message::rcode::SERVFAIL,
            ))
        }
    }
}

/// Serve DNS over UDP.
pub async fn run_udp(state: Arc<DaemonState>, addr: SocketAddr) -> Result<()> {
    let socket = Arc::new(
        UdpSocket::bind(addr)
            .await
            .with_context(|| format!("binding UDP {addr}"))?,
    );
    tracing::info!(%addr, "DNS proxy listening on UDP");
    let permits = Arc::new(Semaphore::new(MAX_INFLIGHT));
    let mut buf = vec![0u8; MAX_MESSAGE];

    loop {
        let (n, source) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(%error, "UDP receive failed");
                continue;
            }
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            // At capacity: drop rather than queue without bound.
            state.counters.errors.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let request = buf[..n].to_vec();
        let state = state.clone();
        let socket = socket.clone();
        tokio::spawn(async move {
            if let Some(response) = handle(&state, &request, source).await {
                if let Err(error) = socket.send_to(&response, source).await {
                    tracing::debug!(%error, "UDP reply failed");
                }
            }
            drop(permit);
        });
    }
}

/// Serve DNS over TCP.
pub async fn run_tcp(state: Arc<DaemonState>, addr: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding TCP {addr}"))?;
    tracing::info!(%addr, "DNS proxy listening on TCP");
    let permits = Arc::new(Semaphore::new(MAX_INFLIGHT));

    loop {
        let (mut stream, source) = match listener.accept().await {
            Ok(v) => v,
            Err(error) => {
                tracing::warn!(%error, "TCP accept failed");
                continue;
            }
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            // A TCP client may send several queries on one connection.
            loop {
                let mut header = [0u8; 2];
                match tokio::time::timeout(TCP_IDLE, stream.read_exact(&mut header)).await {
                    Ok(Ok(_)) => {}
                    _ => break,
                }
                let length = u16::from_be_bytes(header) as usize;
                if length == 0 || length > MAX_MESSAGE {
                    break;
                }
                let mut request = vec![0u8; length];
                if tokio::time::timeout(TCP_IDLE, stream.read_exact(&mut request))
                    .await
                    .is_err()
                {
                    break;
                }
                let Some(response) = handle(&state, &request, source).await else {
                    break;
                };
                let Ok(len) = u16::try_from(response.len()) else {
                    break;
                };
                let mut framed = Vec::with_capacity(2 + response.len());
                framed.extend_from_slice(&len.to_be_bytes());
                framed.extend_from_slice(&response);
                if stream.write_all(&framed).await.is_err() {
                    break;
                }
            }
            drop(permit);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_sources_are_served() {
        let allow = |s: &str| source_allowed(&s.parse().unwrap());
        assert!(allow("127.0.0.1:1234"));
        assert!(allow("192.168.1.5:1234"));
        assert!(allow("10.0.0.9:1234"));
        assert!(allow("[::1]:1234"));
        assert!(allow("[fd00::1]:1234"));
        // A routable address must never be served.
        assert!(!allow("8.8.8.8:1234"));
        assert!(!allow("203.0.113.7:1234"));
        assert!(!allow("[2001:db8::1]:1234"));
    }
}
