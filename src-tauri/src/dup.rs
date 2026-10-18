//! Packet duplication for lossy links.
//!
//! WireGuard is pointed at a local UDP proxy instead of the server. The proxy sends every datagram to
//! the server's duplication relay twice (a few ms apart); WireGuard's replay protection on the far end
//! drops the second copy, so a packet is only lost if both copies are. Replies come back the same way
//! (the relay duplicates them), and the second copy is dropped by the local WireGuard.
//!
//! Also here: a UDP loss probe against the relay (`probe`), to see whether duplication is worth it.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::net::{lookup_host, UdpSocket};
use tokio::task::JoinHandle;

const PROBE_MAGIC: &[u8; 4] = b"GPNP";
const PROBE_LEN: usize = 13; // magic + seq(u32) + copy(u8) + sent-ms(u32)

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupStats {
    /// datagrams WireGuard handed to the proxy (each is sent twice)
    pub sent: u64,
    /// datagrams received from the server (before WireGuard drops the duplicates)
    pub received: u64,
    /// downstream packets that arrived with both copies
    pub down_pairs: u64,
    /// downstream packets that arrived with only one copy (the other was lost on the way)
    pub down_singles: u64,
}

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    received: AtomicU64,
    down_pairs: AtomicU64,
    down_singles: AtomicU64,
}

/// How long to wait for a packet's second copy before counting it as a single.
const PAIR_WINDOW: Duration = Duration::from_millis(300);

/// Recognises the two copies of a downstream packet: seen twice = both arrived, once = one was lost.
#[derive(Default)]
pub struct PairTracker {
    seen: HashMap<u64, (Instant, u8)>,
}

impl PairTracker {
    pub fn note(&mut self, payload: &[u8], now: Instant) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        payload.hash(&mut h);
        self.seen.entry(h.finish()).and_modify(|e| e.1 = e.1.saturating_add(1)).or_insert((now, 1));
    }

    /// Retire entries older than the window; returns (pairs, singles) among them.
    pub fn settle(&mut self, now: Instant) -> (u64, u64) {
        let (mut pairs, mut singles) = (0, 0);
        self.seen.retain(|_, (t, n)| {
            if now.duration_since(*t) < PAIR_WINDOW {
                return true;
            }
            if *n >= 2 {
                pairs += 1;
            } else {
                singles += 1;
            }
            false
        });
        (pairs, singles)
    }
}

/// Cheap handle to read a running proxy's counters.
#[derive(Clone)]
pub struct StatsHandle(Arc<Counters>);

impl StatsHandle {
    pub fn get(&self) -> DupStats {
        DupStats {
            sent: self.0.sent.load(Ordering::Relaxed),
            received: self.0.received.load(Ordering::Relaxed),
            down_pairs: self.0.down_pairs.load(Ordering::Relaxed),
            down_singles: self.0.down_singles.load(Ordering::Relaxed),
        }
    }
}

/// A running proxy. Dropping it stops it.
pub struct DupProxy {
    /// loopback port WireGuard's `Endpoint` should point at
    pub local_port: u16,
    counters: Arc<Counters>,
    tasks: Vec<JoinHandle<()>>,
}

impl DupProxy {
    pub async fn start(server: &str, delay_ms: u64) -> Result<Self, String> {
        let server_addr = resolve_v4(server).await?;
        let local = Arc::new(UdpSocket::bind("127.0.0.1:0").await.map_err(|e| format!("duplication proxy: {e}"))?);
        let local_port = local.local_addr().map_err(|e| e.to_string())?.port();
        let up = Arc::new(UdpSocket::bind("0.0.0.0:0").await.map_err(|e| format!("duplication proxy: {e}"))?);
        up.connect(server_addr).await.map_err(|e| format!("duplication proxy: {e}"))?;

        let counters = Arc::new(Counters::default());
        let wg_addr: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
        let delay = Duration::from_millis(delay_ms.min(50));
        let tracker = Arc::new(Mutex::new(PairTracker::default()));

        // WireGuard -> server, every datagram twice
        let t1 = {
            let (local, up, wg_addr, counters) = (local.clone(), up.clone(), wg_addr.clone(), counters.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    // errors here are Windows' "ICMP port unreachable" echoes on UDP sockets — keep going
                    let Ok((n, from)) = local.recv_from(&mut buf).await else { continue };
                    *wg_addr.lock().unwrap() = Some(from);
                    counters.sent.fetch_add(1, Ordering::Relaxed);
                    let _ = up.send(&buf[..n]).await;
                    if delay.is_zero() {
                        let _ = up.send(&buf[..n]).await;
                    } else {
                        let (up, copy) = (up.clone(), buf[..n].to_vec());
                        tokio::spawn(async move {
                            tokio::time::sleep(delay).await;
                            let _ = up.send(&copy).await;
                        });
                    }
                }
            })
        };
        // server -> WireGuard (the relay already sends two copies; WireGuard drops the second)
        let t2 = {
            let (local, up, counters, tracker) = (local.clone(), up.clone(), counters.clone(), tracker.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    let Ok(n) = up.recv(&mut buf).await else { continue };
                    counters.received.fetch_add(1, Ordering::Relaxed);
                    tracker.lock().unwrap().note(&buf[..n], Instant::now());
                    let to = *wg_addr.lock().unwrap();
                    if let Some(to) = to {
                        let _ = local.send_to(&buf[..n], to).await;
                    }
                }
            })
        };
        // retire packets whose second copy never came
        let t3 = {
            let counters = counters.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let (p, s) = tracker.lock().unwrap().settle(Instant::now());
                    counters.down_pairs.fetch_add(p, Ordering::Relaxed);
                    counters.down_singles.fetch_add(s, Ordering::Relaxed);
                }
            })
        };
        Ok(Self { local_port, counters, tasks: vec![t1, t2, t3] })
    }

    pub fn handle(&self) -> StatsHandle {
        StatsHandle(self.counters.clone())
    }
}

impl Drop for DupProxy {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

async fn resolve_v4(server: &str) -> Result<SocketAddr, String> {
    lookup_host(server)
        .await
        .map_err(|e| format!("cannot resolve {server}: {e}"))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| format!("no IPv4 address for {server}"))
}

// ---------------------------------------------------------------------------------------------
// UDP loss probe
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub sent: u32,
    /// round-trip loss with one copy per probe (what you get today)
    pub single_loss_pct: f64,
    /// loss when each probe is sent twice and either echo counts (what duplication would give)
    pub dup_loss_pct: f64,
    pub avg_rtt_ms: f64,
    pub jitter_ms: f64,
    pub delay_ms: u64,
}

fn probe_packet(seq: u32, copy: u8, sent_ms: u32) -> [u8; PROBE_LEN] {
    let mut p = [0u8; PROBE_LEN];
    p[..4].copy_from_slice(PROBE_MAGIC);
    p[4..8].copy_from_slice(&seq.to_be_bytes());
    p[8] = copy;
    p[9..13].copy_from_slice(&sent_ms.to_be_bytes());
    p
}

fn parse_probe(b: &[u8]) -> Option<(u32, u8, u32)> {
    if b.len() != PROBE_LEN || &b[..4] != PROBE_MAGIC {
        return None;
    }
    Some((u32::from_be_bytes(b[4..8].try_into().ok()?), b[8], u32::from_be_bytes(b[9..13].try_into().ok()?)))
}

/// Pure maths of a probe run. `echoes` = (seq, copy, rtt in ms) for every echo that came back.
pub fn summarize(sent: u32, echoes: &[(u32, u8, f64)], delay_ms: u64) -> ProbeResult {
    let mut got0 = vec![false; sent as usize];
    let mut got_any = vec![false; sent as usize];
    let mut rtts: Vec<(u32, f64)> = Vec::new();
    for &(seq, copy, rtt) in echoes {
        let Some(slot) = got_any.get_mut(seq as usize) else { continue };
        *slot = true;
        if copy == 0 && !got0[seq as usize] {
            got0[seq as usize] = true;
            rtts.push((seq, rtt));
        }
    }
    rtts.sort_by_key(|r| r.0);
    let pct = |v: &[bool]| if sent == 0 { 0.0 } else { 100.0 * v.iter().filter(|x| !**x).count() as f64 / sent as f64 };
    let avg = if rtts.is_empty() { 0.0 } else { rtts.iter().map(|r| r.1).sum::<f64>() / rtts.len() as f64 };
    let jitter = if rtts.len() < 2 { 0.0 } else { rtts.windows(2).map(|w| (w[1].1 - w[0].1).abs()).sum::<f64>() / (rtts.len() - 1) as f64 };
    let r1 = |x: f64| (x * 10.0).round() / 10.0;
    ProbeResult { sent, single_loss_pct: r1(pct(&got0)), dup_loss_pct: r1(pct(&got_any)), avg_rtt_ms: r1(avg), jitter_ms: r1(jitter), delay_ms }
}

/// Send `count` probes (each twice, `delay_ms` apart) to the relay, one every `interval_ms`.
pub async fn probe(server: &str, count: u32, interval_ms: u64, delay_ms: u64) -> Result<ProbeResult, String> {
    let addr = resolve_v4(server).await?;
    let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await.map_err(|e| e.to_string())?);
    sock.connect(addr).await.map_err(|e| e.to_string())?;
    let t0 = Instant::now();
    let echoes: Arc<Mutex<Vec<(u32, u8, f64)>>> = Arc::new(Mutex::new(Vec::new()));

    let rx = {
        let (sock, echoes) = (sock.clone(), echoes.clone());
        tokio::spawn(async move {
            let mut buf = [0u8; 256];
            loop {
                let Ok(n) = sock.recv(&mut buf).await else { continue };
                if let Some((seq, copy, sent_ms)) = parse_probe(&buf[..n]) {
                    let now = t0.elapsed().as_millis() as u32;
                    echoes.lock().unwrap().push((seq, copy, now.saturating_sub(sent_ms) as f64));
                }
            }
        })
    };

    for seq in 0..count {
        let now = t0.elapsed().as_millis() as u32;
        let _ = sock.send(&probe_packet(seq, 0, now)).await;
        let (s2, d) = (sock.clone(), Duration::from_millis(delay_ms));
        tokio::spawn(async move {
            tokio::time::sleep(d).await;
            let sent_ms = (Instant::now().duration_since(t0)).as_millis() as u32;
            let _ = s2.send(&probe_packet(seq, 1, sent_ms)).await;
        });
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }
    tokio::time::sleep(Duration::from_millis(1200)).await; // stragglers
    rx.abort();
    let echoes = echoes.lock().unwrap().clone();
    if echoes.is_empty() {
        return Err("no reply from the duplication relay — is its UDP port open on the server (dupPort)?".into());
    }
    Ok(summarize(count, &echoes, delay_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_tracker_tells_pairs_from_singles() {
        let t0 = Instant::now();
        let mut t = PairTracker::default();
        t.note(b"a", t0);
        t.note(b"a", t0 + Duration::from_millis(3)); // both copies
        t.note(b"b", t0); // one copy only
        assert_eq!(t.settle(t0 + Duration::from_millis(100)), (0, 0)); // still inside the window
        assert_eq!(t.settle(t0 + Duration::from_millis(400)), (1, 1));
        assert_eq!(t.settle(t0 + Duration::from_secs(5)), (0, 0));
    }

    #[test]
    fn probe_packets_round_trip() {
        let p = probe_packet(7, 1, 1234);
        assert_eq!(parse_probe(&p), Some((7, 1, 1234)));
        assert_eq!(parse_probe(&p[..12]), None);
        assert_eq!(parse_probe(b"XXXX000000000"), None);
    }

    #[test]
    fn duplication_recovers_single_copy_losses() {
        // 10 probes: copy 0 lost for seq 2 and 5, copy 1 also lost for seq 5, seq 8 lost entirely
        let mut e: Vec<(u32, u8, f64)> = Vec::new();
        for seq in 0..10u32 {
            if ![2, 5, 8].contains(&seq) {
                e.push((seq, 0, 20.0 + seq as f64));
            }
            if ![5, 8].contains(&seq) && seq != 0 {
                e.push((seq, 1, 25.0));
            }
        }
        let r = summarize(10, &e, 3);
        assert_eq!(r.single_loss_pct, 30.0); // 2, 5, 8
        assert_eq!(r.dup_loss_pct, 20.0); // 5, 8 (seq 2 saved by copy 1)
        assert!(r.avg_rtt_ms > 20.0 && r.jitter_ms == 1.5);
    }

    #[tokio::test]
    async fn proxy_sends_each_datagram_twice_and_relays_replies() {
        // fake relay: counts datagrams and echoes each one
        let relay = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let count = Arc::new(AtomicU64::new(0));
        let c2 = count.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let Ok((n, from)) = relay.recv_from(&mut buf).await else { continue };
                c2.fetch_add(1, Ordering::Relaxed);
                let _ = relay.send_to(&buf[..n], from).await;
            }
        });

        let proxy = DupProxy::start(&relay_addr.to_string(), 2).await.unwrap();
        let wg = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        wg.send_to(&[4, 0, 0, 0, 1, 2, 3], ("127.0.0.1", proxy.local_port)).await.unwrap();
        let mut buf = [0u8; 64];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), wg.recv_from(&mut buf)).await.expect("no reply").unwrap();
        assert_eq!(&buf[..n], &[4, 0, 0, 0, 1, 2, 3]);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(count.load(Ordering::Relaxed), 2, "relay should see both copies");
        assert_eq!(proxy.handle().get().sent, 1);
    }
}
