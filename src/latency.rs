//! Pluggable latency probe interface and concrete implementations.
//!
//! The `LatencyProbe` trait allows different measurement strategies to be
//! swapped in without changing the surrounding infrastructure.  The initial
//! concrete implementation (`TcpPing`) measures the TCP SYN+ACK round-trip
//! time to `discord.com:443` — no root or raw sockets required.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

/// A pluggable strategy for measuring network latency.
///
/// Implementations must be `Send + 'static` so they can be moved into a
/// background thread.  `measure` is called periodically; the result (ms) is
/// forwarded to the UI via `DiscordEvent::PingResult`.
pub trait LatencyProbe: Send + 'static {
    /// Perform a single latency measurement.
    ///
    /// Returns the observed latency in milliseconds, or `None` if the
    /// measurement failed (network unreachable, timeout, DNS error, …).
    fn measure(&self) -> Option<u32>;

    /// Short human-readable label describing the method (e.g. `"TCP ping"`).
    /// Reserved for future use in the UI or logs.
    #[allow(dead_code)]
    fn label(&self) -> &str;
}

// ─── TcpPing ────────────────────────────────────────────────────────────────

/// Measures latency by timing a TCP SYN+ACK handshake to a remote host.
///
/// DNS resolution is performed *outside* the timed section so the result
/// reflects pure network RTT, not resolver latency.
pub struct TcpPing {
    host: String,
}

impl TcpPing {
    /// Create a new probe targeting `host` (e.g. `"discord.com:443"`).
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }
}

impl LatencyProbe for TcpPing {
    fn measure(&self) -> Option<u32> {
        // Resolve DNS outside the timed section
        let addr = self.host.to_socket_addrs().ok()?.next()?;
        let start = Instant::now();
        TcpStream::connect_timeout(&addr, Duration::from_secs(3)).ok()?;
        Some(start.elapsed().as_millis() as u32)
    }

    fn label(&self) -> &str {
        "TCP ping"
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_ping_label() {
        let p = TcpPing::new("discord.com:443");
        assert_eq!(p.label(), "TCP ping");
    }

    #[test]
    fn tcp_ping_new_stores_host() {
        let p = TcpPing::new("example.com:80");
        assert_eq!(p.host, "example.com:80");
    }

    /// Verify that an unreachable / invalid host returns None rather than panicking.
    #[test]
    fn tcp_ping_invalid_host_returns_none() {
        let p = TcpPing::new("this.host.does.not.exist.invalid:443");
        assert!(p.measure().is_none());
    }

    /// Verify that a valid port that refuses connections returns None.
    #[test]
    fn tcp_ping_refused_connection_returns_none() {
        // Port 1 is almost certainly not listening
        let p = TcpPing::new("127.0.0.1:1");
        assert!(p.measure().is_none());
    }

    // ── Trait-object tests ───────────────────────────────────────────────────

    struct MockProbe {
        result: Option<u32>,
    }

    impl LatencyProbe for MockProbe {
        fn measure(&self) -> Option<u32> {
            self.result
        }
        fn label(&self) -> &str {
            "mock"
        }
    }

    #[test]
    fn trait_object_returns_value() {
        let probe: Box<dyn LatencyProbe> = Box::new(MockProbe { result: Some(99) });
        assert_eq!(probe.measure(), Some(99));
        assert_eq!(probe.label(), "mock");
    }

    #[test]
    fn trait_object_returns_none() {
        let probe: Box<dyn LatencyProbe> = Box::new(MockProbe { result: None });
        assert!(probe.measure().is_none());
    }

    #[test]
    fn different_probes_satisfy_same_trait() {
        let probes: Vec<Box<dyn LatencyProbe>> = vec![
            Box::new(MockProbe { result: Some(10) }),
            Box::new(MockProbe { result: Some(20) }),
            Box::new(MockProbe { result: None }),
        ];
        assert_eq!(probes[0].measure(), Some(10));
        assert_eq!(probes[1].measure(), Some(20));
        assert!(probes[2].measure().is_none());
    }
}
