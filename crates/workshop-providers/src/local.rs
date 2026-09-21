//! Loopback reachability for Local providers. Presence only, short timeouts, never a LAN sweep.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::manifest::{ProviderManifest, builtin_manifests};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalHealth {
    /// Something accepted a TCP connection on the loopback port.
    Reachable,
    Unreachable,
    /// The manifest's host is not loopback; Workshop does not probe it.
    NotLoopback,
}

fn loopback_addr(base_url: &str) -> Option<SocketAddr> {
    let url = Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs().ok()?.collect();
    addrs.into_iter().find(|a| a.ip().is_loopback())
}

/// TCP connect to the manifest's loopback endpoint with `timeout`.
pub fn probe_local(manifest: &ProviderManifest, timeout: Duration) -> LocalHealth {
    if !manifest.is_local() {
        return LocalHealth::NotLoopback;
    }
    match loopback_addr(manifest.base_url) {
        None => LocalHealth::NotLoopback,
        Some(addr) => match TcpStream::connect_timeout(&addr, timeout) {
            Ok(_) => LocalHealth::Reachable,
            Err(_) => LocalHealth::Unreachable,
        },
    }
}

/// Probe every Local manifest. Sequential, bounded by `timeout` each.
pub fn probe_all_local(timeout: Duration) -> Vec<(ProviderManifest, LocalHealth)> {
    builtin_manifests()
        .into_iter()
        .filter(|m| m.is_local())
        .map(|m| {
            let health = probe_local(&m, timeout);
            (m, health)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn detects_a_listening_loopback_port_and_nothing_else() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url: &'static str =
            Box::leak(format!("http://127.0.0.1:{port}/v1").into_boxed_str());
        let mut m = crate::manifest::manifest("ollama").unwrap();
        m.base_url = base_url;
        assert_eq!(
            probe_local(&m, Duration::from_millis(300)),
            LocalHealth::Reachable
        );
        drop(listener);
        // A closed port on loopback is Unreachable, quickly.
        let started = std::time::Instant::now();
        assert_eq!(
            probe_local(&m, Duration::from_millis(300)),
            LocalHealth::Unreachable
        );
        assert!(started.elapsed() < Duration::from_secs(2));

        let direct = crate::manifest::manifest("openai").unwrap();
        assert_eq!(
            probe_local(&direct, Duration::from_millis(300)),
            LocalHealth::NotLoopback
        );
    }

    #[test]
    fn non_loopback_hosts_are_never_probed() {
        let mut m = crate::manifest::manifest("ollama").unwrap();
        m.base_url = "http://192.168.1.50:11434/v1";
        assert_eq!(
            probe_local(&m, Duration::from_millis(100)),
            LocalHealth::NotLoopback
        );
    }
}
