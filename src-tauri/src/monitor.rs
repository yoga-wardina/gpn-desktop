//! Per-game traffic monitor: bytes in/out per second, session totals, peak rates and a per-server
//! breakdown. Fed by the same ETW flow events as the live targets, but independent of learn mode.
//!
//! Counts TCP/UDP payload bytes to public IPv4 hosts (LAN and IPv6 traffic is not reported by the
//! flow capture). Directions are from the game's point of view: `rx` = downloaded, `tx` = uploaded.

use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::sync::Mutex;

use serde::Serialize;

use crate::etw::{FlowEvent, Proto};

/// Per-second history kept per game.
const MAX_SECONDS: usize = 600;
const MAX_HOSTS: usize = 200;
const TOP_HOSTS: usize = 30;
/// Rates are averaged over this many completed seconds.
const RATE_WINDOW_SECS: u64 = 3;
/// A game counts as "active" if it moved bytes within this many seconds.
const ACTIVE_SECS: u64 = 5;

#[derive(Clone, Copy)]
struct Bucket {
    sec: u64,
    rx: u64,
    tx: u64,
}

struct HostStat {
    port: u16,
    rx: u64,
    tx: u64,
    last_ms: u64,
}

#[derive(Default)]
struct GameTraffic {
    started_ms: u64,
    total_rx: u64,
    total_tx: u64,
    peak_rx: u64,
    peak_tx: u64,
    buckets: VecDeque<Bucket>,
    hosts: HashMap<(Proto, Ipv4Addr), HostStat>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostView {
    pub ip: String,
    pub proto: String,
    pub port: u16,
    pub rx: u64,
    pub tx: u64,
    pub last_age_ms: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TrafficView {
    /// moved bytes in the last few seconds
    pub active: bool,
    pub started_ms: u64,
    pub total_rx: u64,
    pub total_tx: u64,
    /// highest one-second totals seen this session (bytes/s)
    pub peak_rx: u64,
    pub peak_tx: u64,
    /// recent average (bytes/s)
    pub rx_per_sec: f64,
    pub tx_per_sec: f64,
    /// bytes per second, oldest first, one entry per second of the requested window
    pub series_rx: Vec<u64>,
    pub series_tx: Vec<u64>,
    pub hosts: Vec<HostView>,
}

#[derive(Default)]
pub struct Monitor {
    games: Mutex<HashMap<String, GameTraffic>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Monitor {
    pub fn record(&self, game: &str, ev: &FlowEvent) {
        self.record_at(game, ev, now_ms());
    }

    fn record_at(&self, game: &str, ev: &FlowEvent, now: u64) {
        let sec = now / 1000;
        let (rx, tx) = if ev.outbound { (0, ev.bytes as u64) } else { (ev.bytes as u64, 0) };
        let mut games = self.games.lock().unwrap();
        let g = games.entry(game.to_string()).or_default();
        if g.started_ms == 0 {
            g.started_ms = now;
        }
        g.total_rx += rx;
        g.total_tx += tx;

        match g.buckets.back_mut() {
            Some(b) if sec <= b.sec => {
                // same second (or a slightly late event): add to the newest bucket
                b.rx += rx;
                b.tx += tx;
            }
            _ => {
                g.buckets.push_back(Bucket { sec, rx, tx });
                while g.buckets.len() > MAX_SECONDS {
                    g.buckets.pop_front();
                }
            }
        }
        if let Some(b) = g.buckets.back() {
            g.peak_rx = g.peak_rx.max(b.rx);
            g.peak_tx = g.peak_tx.max(b.tx);
        }

        let key = (ev.proto, ev.remote_ip);
        if !g.hosts.contains_key(&key) && g.hosts.len() >= MAX_HOSTS {
            // drop the quietest host to stay bounded
            if let Some(k) = g.hosts.iter().min_by_key(|(_, h)| h.rx + h.tx).map(|(k, _)| *k) {
                g.hosts.remove(&k);
            }
        }
        let h = g.hosts.entry(key).or_insert(HostStat { port: ev.remote_port, rx: 0, tx: 0, last_ms: now });
        h.port = ev.remote_port;
        h.rx += rx;
        h.tx += tx;
        h.last_ms = now;
    }

    /// Start fresh counters (game launched, or the user pressed reset).
    pub fn begin_session(&self, game: &str) {
        self.games
            .lock()
            .unwrap()
            .insert(game.to_string(), GameTraffic { started_ms: now_ms(), ..Default::default() });
    }

    pub fn snapshot(&self, game: &str, window_secs: usize) -> TrafficView {
        self.snapshot_at(game, window_secs, now_ms())
    }

    fn snapshot_at(&self, game: &str, window_secs: usize, now: u64) -> TrafficView {
        let window = window_secs.clamp(10, MAX_SECONDS);
        let mut view = TrafficView {
            series_rx: vec![0; window],
            series_tx: vec![0; window],
            ..Default::default()
        };
        let games = self.games.lock().unwrap();
        let Some(g) = games.get(game) else { return view };
        let now_sec = now / 1000;

        view.started_ms = g.started_ms;
        view.total_rx = g.total_rx;
        view.total_tx = g.total_tx;
        view.peak_rx = g.peak_rx;
        view.peak_tx = g.peak_tx;

        let first = now_sec.saturating_sub(window as u64 - 1);
        let (mut rate_rx, mut rate_tx) = (0u64, 0u64);
        for b in &g.buckets {
            if b.sec >= first && b.sec <= now_sec {
                let i = (b.sec - first) as usize;
                view.series_rx[i] = b.rx;
                view.series_tx[i] = b.tx;
            }
            // completed seconds only, so a half-finished second doesn't make the rate flicker
            if b.sec < now_sec && b.sec + RATE_WINDOW_SECS >= now_sec {
                rate_rx += b.rx;
                rate_tx += b.tx;
            }
            if b.sec + ACTIVE_SECS >= now_sec && b.sec <= now_sec {
                view.active = true;
            }
        }
        view.rx_per_sec = rate_rx as f64 / RATE_WINDOW_SECS as f64;
        view.tx_per_sec = rate_tx as f64 / RATE_WINDOW_SECS as f64;

        let mut hosts: Vec<HostView> = g
            .hosts
            .iter()
            .map(|((proto, ip), h)| HostView {
                ip: ip.to_string(),
                proto: proto.as_str().into(),
                port: h.port,
                rx: h.rx,
                tx: h.tx,
                last_age_ms: now.saturating_sub(h.last_ms),
            })
            .collect();
        hosts.sort_by(|a, b| (b.rx + b.tx).cmp(&(a.rx + a.tx)));
        hosts.truncate(TOP_HOSTS);
        view.hosts = hosts;
        view
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ip: &str, port: u16, bytes: u32, outbound: bool) -> FlowEvent {
        FlowEvent { pid: 1, proto: Proto::Tcp, remote_ip: ip.parse().unwrap(), remote_port: port, bytes, outbound }
    }

    #[test]
    fn totals_series_rates_and_hosts() {
        let m = Monitor::default();
        let t0 = 10_000_000u64; // a whole-second boundary
        // 3 seconds of 1000 B down + 200 B up to the game server, plus a little to a CDN
        for s in 0..3u64 {
            m.record_at("g.exe", &ev("203.0.113.9", 13328, 1000, false), t0 + s * 1000 + 10);
            m.record_at("g.exe", &ev("203.0.113.9", 13328, 200, true), t0 + s * 1000 + 20);
        }
        m.record_at("g.exe", &ev("198.51.100.1", 443, 50, false), t0 + 2500);

        let v = m.snapshot_at("g.exe", 10, t0 + 3000); // "now" is the start of the 4th second
        assert_eq!((v.total_rx, v.total_tx), (3050, 600));
        assert_eq!(v.series_rx.len(), 10);
        // the last 3 completed seconds are t0..t0+2 -> average ~1016 B/s down, 200 B/s up
        assert!((v.rx_per_sec - 3050.0 / 3.0).abs() < 0.01, "{}", v.rx_per_sec);
        assert!((v.tx_per_sec - 200.0).abs() < 0.01);
        // series is oldest-first and ends at "now": seconds t0..t0+2 sit at indices 6..8
        assert_eq!(&v.series_rx[6..9], &[1000, 1000, 1050]);
        assert_eq!(v.series_rx[9], 0);
        assert!(v.active);
        assert_eq!(v.peak_rx, 1050);
        // hosts ranked by total bytes
        assert_eq!(v.hosts[0].ip, "203.0.113.9");
        assert_eq!(v.hosts[0].port, 13328);
        assert_eq!(v.hosts[1].ip, "198.51.100.1");
    }

    #[test]
    fn inactive_after_silence_and_reset() {
        let m = Monitor::default();
        let t0 = 20_000_000u64;
        m.record_at("g.exe", &ev("203.0.113.9", 7777, 500, false), t0);
        assert!(!m.snapshot_at("g.exe", 30, t0 + 60_000).active);
        assert_eq!(m.snapshot_at("g.exe", 30, t0 + 60_000).rx_per_sec, 0.0);
        assert!(m.snapshot_at("other.exe", 30, t0).hosts.is_empty());

        m.begin_session("g.exe");
        let v = m.snapshot_at("g.exe", 30, t0);
        assert_eq!((v.total_rx, v.total_tx, v.hosts.len()), (0, 0, 0));
    }
}
