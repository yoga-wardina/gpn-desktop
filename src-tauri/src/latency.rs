//! Connection-path quality: latency, jitter and packet loss for
//!   leg A  this PC  -> the GPN server (measured here: ICMP echo, falling back to TCP connect), and
//!   leg B  the GPN server -> the game server(s) (measured BY the server, fetched via /api/probe).
//! Together they show where the time goes on the route PC -> VPS -> game server.

use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;

use crate::net::cidr_contains;

/// Samples kept for the leg-A sparkline / statistics.
const WINDOW: usize = 60;
/// ICMP attempts without a single reply before falling back to TCP connect timing.
const ICMP_GIVE_UP: u32 = 6;
const PROBE_TIMEOUT_MS: u32 = 1000;

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Statistics of one leg; `samples` are recent round trips in ms (None = lost probe).
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Leg {
    pub method: String,
    pub last: Option<f64>,
    pub avg: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// mean absolute difference between consecutive round trips
    pub jitter: Option<f64>,
    pub loss_pct: f64,
    pub samples: Vec<Option<f64>>,
}

pub fn leg_stats(method: &str, samples: &VecDeque<Option<f64>>) -> Leg {
    let ok: Vec<f64> = samples.iter().flatten().copied().collect();
    let jitter = (ok.len() >= 2).then(|| round1(ok.windows(2).map(|w| (w[1] - w[0]).abs()).sum::<f64>() / (ok.len() - 1) as f64));
    Leg {
        method: method.into(),
        last: samples.back().copied().flatten().map(round1),
        avg: (!ok.is_empty()).then(|| round1(ok.iter().sum::<f64>() / ok.len() as f64)),
        min: ok.iter().copied().reduce(f64::min).map(round1),
        max: ok.iter().copied().reduce(f64::max).map(round1),
        jitter,
        loss_pct: if samples.is_empty() { 0.0 } else { round1(samples.iter().filter(|s| s.is_none()).count() as f64 / samples.len() as f64 * 100.0) },
        samples: samples.iter().map(|s| s.map(round1)).collect(),
    }
}

/// The server's measurement of one game server (shape of `GET /api/probe` results).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerLeg {
    pub ip: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub last: Option<f64>,
    #[serde(default)]
    pub avg: Option<f64>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub jitter: Option<f64>,
    #[serde(default)]
    pub loss_pct: f64,
    #[serde(default)]
    pub samples: Vec<Option<f64>>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PathView {
    pub tunnel_up: bool,
    /// the GPN server's public endpoint (host:port) once registered
    pub endpoint: Option<String>,
    /// this PC -> GPN server
    pub a: Option<Leg>,
    /// GPN server -> game server(s), best traffic first
    pub servers: Vec<ServerLeg>,
    /// false when the server doesn't have the probe endpoint yet (needs updating)
    pub servers_supported: bool,
    /// watched game these servers belong to
    pub game: Option<String>,
}

#[derive(Default)]
struct Inner {
    a: VecDeque<Option<f64>>,
    a_method: String,
    icmp_misses: u32,
    icmp_ever_ok: bool,
    servers: Vec<ServerLeg>,
    unsupported: bool,
    game: Option<String>,
}

#[derive(Default)]
pub struct PathMonitor {
    inner: Mutex<Inner>,
}

impl PathMonitor {
    /// Method to use for the next leg-A probe.
    pub fn next_method(&self) -> &'static str {
        let i = self.inner.lock().unwrap();
        if cfg!(windows) && (i.icmp_ever_ok || i.icmp_misses < ICMP_GIVE_UP) {
            "icmp"
        } else {
            "tcp"
        }
    }

    pub fn push_a(&self, method: &str, rtt: Option<f64>) {
        let mut i = self.inner.lock().unwrap();
        if method == "icmp" {
            match rtt {
                Some(_) => {
                    i.icmp_ever_ok = true;
                    i.icmp_misses = 0;
                }
                None => i.icmp_misses += 1,
            }
        }
        if i.a_method != method {
            // different method => different baseline; don't mix the two in one window
            i.a.clear();
            i.a_method = method.into();
        }
        i.a.push_back(rtt);
        while i.a.len() > WINDOW {
            i.a.pop_front();
        }
    }

    pub fn clear_a(&self) {
        let mut i = self.inner.lock().unwrap();
        i.a.clear();
        i.icmp_misses = 0;
        i.icmp_ever_ok = false;
    }

    pub fn set_servers(&self, game: Option<String>, servers: Vec<ServerLeg>, unsupported: bool) {
        let mut i = self.inner.lock().unwrap();
        i.game = game;
        i.servers = servers;
        i.unsupported = unsupported;
    }

    pub fn view(&self, tunnel_up: bool, endpoint: Option<String>) -> PathView {
        let i = self.inner.lock().unwrap();
        PathView {
            tunnel_up,
            endpoint,
            a: (!i.a.is_empty()).then(|| leg_stats(&i.a_method, &i.a)),
            servers: i.servers.clone(),
            servers_supported: !i.unsupported,
            game: i.game.clone(),
        }
    }
}

/// Pick up to `limit` game servers worth probing: highest-traffic first, only those whose traffic goes
/// through the tunnel (inside `allowed` CIDRs), one entry per IP.
pub fn pick_targets(candidates: &[(String, Option<u16>)], allowed: &[String], limit: usize) -> Vec<(String, Option<u16>)> {
    let mut out: Vec<(String, Option<u16>)> = Vec::new();
    for (ip, port) in candidates {
        let Ok(addr) = ip.parse::<Ipv4Addr>() else { continue };
        if !allowed.iter().any(|c| cidr_contains(c, addr)) || out.iter().any(|(o, _)| o == ip) {
            continue;
        }
        out.push((ip.clone(), *port));
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// One ICMP echo (blocking, up to `timeout_ms`). Windows only; works without administrator rights.
#[cfg(windows)]
pub fn icmp_echo(ip: Ipv4Addr, timeout_ms: u32) -> Option<f64> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::NetworkManagement::IpHelper::{IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY};

    let payload = [0x47u8; 32]; // "GGGG…"
    let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + payload.len() + 8];
    // SAFETY: plain iphlpapi calls; the reply buffer is sized as the API requires and outlives the call
    unsafe {
        let h = IcmpCreateFile();
        if h == INVALID_HANDLE_VALUE || h.is_null() {
            return None;
        }
        let started = Instant::now();
        let n = IcmpSendEcho(
            h,
            u32::from_ne_bytes(ip.octets()),
            payload.as_ptr().cast(),
            payload.len() as u16,
            std::ptr::null(),
            reply.as_mut_ptr().cast(),
            reply.len() as u32,
            timeout_ms,
        );
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        IcmpCloseHandle(h);
        if n == 0 {
            return None;
        }
        let r: ICMP_ECHO_REPLY = std::ptr::read_unaligned(reply.as_ptr().cast());
        (r.Status == 0).then_some(elapsed)
    }
}

#[cfg(not(windows))]
pub fn icmp_echo(_ip: Ipv4Addr, _timeout_ms: u32) -> Option<f64> {
    None
}

/// TCP connect time in ms. A refused connection still proves the host answered.
pub async fn tcp_connect_ms(ip: Ipv4Addr, port: u16, timeout: Duration) -> Option<f64> {
    let started = Instant::now();
    match tokio::time::timeout(timeout, TcpStream::connect((ip, port))).await {
        Ok(Ok(_)) => Some(started.elapsed().as_secs_f64() * 1000.0),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => Some(started.elapsed().as_secs_f64() * 1000.0),
        _ => None,
    }
}

/// One leg-A probe with the given method ("icmp" | "tcp").
pub async fn probe_a(method: &str, ip: Ipv4Addr) -> Option<f64> {
    if method == "icmp" {
        tokio::task::spawn_blocking(move || icmp_echo(ip, PROBE_TIMEOUT_MS)).await.ok().flatten()
    } else {
        tcp_connect_ms(ip, 443, Duration::from_millis(PROBE_TIMEOUT_MS as u64)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dq(v: &[Option<f64>]) -> VecDeque<Option<f64>> {
        v.iter().copied().collect()
    }

    #[test]
    fn stats_loss_jitter_and_nulls() {
        let l = leg_stats("icmp", &dq(&[Some(20.0), Some(30.0), None, Some(25.0)]));
        assert_eq!((l.avg, l.min, l.max, l.last), (Some(25.0), Some(20.0), Some(30.0), Some(25.0)));
        assert_eq!(l.loss_pct, 25.0);
        assert_eq!(l.jitter, Some(7.5));
        assert_eq!(l.samples[2], None);
        let empty = leg_stats("icmp", &dq(&[]));
        assert_eq!((empty.avg, empty.loss_pct), (None, 0.0));
        // last probe lost => no "last" value
        assert_eq!(leg_stats("tcp", &dq(&[Some(10.0), None])).last, None);
    }

    #[test]
    fn window_is_bounded_and_method_switch_resets() {
        let m = PathMonitor::default();
        for i in 0..100 {
            m.push_a("tcp", Some(i as f64));
        }
        assert_eq!(m.view(true, None).a.unwrap().samples.len(), WINDOW);
        m.push_a("icmp", Some(5.0));
        let a = m.view(true, None).a.unwrap();
        assert_eq!((a.method.as_str(), a.samples.len()), ("icmp", 1));
    }

    #[test]
    fn icmp_falls_back_to_tcp_when_never_answered() {
        let m = PathMonitor::default();
        if cfg!(windows) {
            assert_eq!(m.next_method(), "icmp");
            for _ in 0..ICMP_GIVE_UP {
                m.push_a("icmp", None);
            }
            assert_eq!(m.next_method(), "tcp");
        }
        // an ICMP reply once seen keeps ICMP preferred through later losses
        let m = PathMonitor::default();
        m.push_a("icmp", Some(3.0));
        for _ in 0..20 {
            m.push_a("icmp", None);
        }
        if cfg!(windows) {
            assert_eq!(m.next_method(), "icmp");
        }
    }

    #[test]
    fn picks_routed_servers_by_priority() {
        let cands = vec![
            ("203.0.113.50".to_string(), Some(13328)),
            ("198.51.100.9".to_string(), Some(443)), // not routed
            ("203.0.113.50".to_string(), Some(13700)), // duplicate IP
            ("203.0.113.77".to_string(), None),
            ("203.0.113.88".to_string(), Some(1)),
        ];
        let allowed = vec!["203.0.113.50/32".to_string(), "203.0.113.64/26".to_string()];
        let got = pick_targets(&cands, &allowed, 2);
        assert_eq!(got, vec![("203.0.113.50".to_string(), Some(13328)), ("203.0.113.77".to_string(), None)]);
    }

    #[test]
    fn parses_the_servers_probe_json() {
        let raw = r#"{"ip":"203.0.113.50","port":13328,"method":"tcp","last":41.2,"avg":40.1,"min":38.0,"max":47.9,"jitter":2.5,"lossPct":3.3,"samples":[40.1,null,41.2]}"#;
        let l: ServerLeg = serde_json::from_str(raw).unwrap();
        assert_eq!((l.port, l.loss_pct, l.samples.len()), (Some(13328), 3.3, 3));
        assert_eq!(l.samples[1], None);
        // a minimal object (older/other server) still parses
        let l: ServerLeg = serde_json::from_str(r#"{"ip":"1.2.3.4"}"#).unwrap();
        assert_eq!(l.loss_pct, 0.0);
    }

    #[tokio::test]
    async fn tcp_probe_against_a_local_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let rtt = tcp_connect_ms(Ipv4Addr::LOCALHOST, port, Duration::from_millis(500)).await;
        assert!(rtt.is_some_and(|r| r < 500.0));
        drop(listener);
        // closed port: refused = host reachable (Windows delays the loopback RST by a second or two)
        assert!(tcp_connect_ms(Ipv4Addr::LOCALHOST, port, Duration::from_secs(5)).await.is_some());
    }

    #[cfg(windows)]
    #[test]
    fn icmp_echo_loopback() {
        let rtt = icmp_echo(Ipv4Addr::LOCALHOST, 1000);
        assert!(rtt.is_some_and(|r| r < 1000.0), "loopback ICMP echo should answer: {rtt:?}");
    }
}
