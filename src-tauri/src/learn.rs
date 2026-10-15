//! Learn mode: records which public servers a watched game talks to (from ETW flow events),
//! enriches them with AS / BGP-prefix info and ranks them so a profile can be reviewed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::asn::AsnInfo;
use crate::etw::FlowEvent;

const MAX_PORTS_PER_HOST: usize = 64;
/// Silence longer than this ends a host's "episode" (one continuous connection period).
const EPISODE_GAP_MS: u64 = 60_000;
const MAX_EPISODES: usize = 100;
/// Hosts below this much traffic are not pre-selected for the profile.
const SUGGEST_MIN_BYTES: u64 = 20_000;
/// Chatter that is never the game server on its own.
const NOISE_PORTS: &[u16] = &[53, 67, 68, 123, 137, 138, 1900, 5353, 5355];

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// One continuous period of traffic with a host inside a play session (epoch ms).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub session: u32,
    pub first: u64,
    pub last: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observed {
    pub ip: String,
    pub proto: String,
    /// remote port -> event count
    pub ports: BTreeMap<u16, u64>,
    pub bytes_out: u64,
    pub bytes_in: u64,
    pub packets: u64,
    pub first_seen: u64,
    pub last_seen: u64,
    /// number of distinct play sessions this host showed up in
    pub sessions: u32,
    /// when this host was active, per play session (older recordings have none)
    #[serde(default)]
    pub episodes: Vec<Episode>,
    #[serde(default)]
    last_session: u32,
}

#[derive(Default, Serialize, Deserialize)]
struct GameLearn {
    #[serde(default)]
    observed: HashMap<String, Observed>,
    #[serde(default)]
    sessions: u32,
    /// play session number -> start time (epoch ms)
    #[serde(default)]
    session_started: BTreeMap<u32, u64>,
}

#[derive(Default, Serialize, Deserialize)]
struct Data {
    #[serde(default)]
    learning: bool,
    #[serde(default)]
    games: HashMap<String, GameLearn>,
    /// ip -> AS info (None = looked up, not announced)
    #[serde(default)]
    asn: HashMap<String, Option<AsnInfo>>,
    #[serde(skip)]
    pending: HashSet<String>,
}

/// One row of the review table.
#[derive(Debug, Clone, Serialize)]
pub struct EntryView {
    #[serde(flatten)]
    pub obs: Observed,
    pub asn: Option<AsnInfo>,
    pub total_bytes: u64,
    pub score: f64,
    /// pre-selected for the profile
    pub suggested: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStart {
    pub session: u32,
    pub started: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearnView {
    pub learning: bool,
    pub sessions: u32,
    pub session_starts: Vec<SessionStart>,
    pub entries: Vec<EntryView>,
}

pub struct Learn {
    path: PathBuf,
    data: Mutex<Data>,
    dirty: AtomicBool,
    asn_tx: OnceLock<UnboundedSender<String>>,
}

impl Learn {
    pub fn new(app_data: &std::path::Path) -> Self {
        let path = app_data.join("learned.json");
        let data = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { path, data: Mutex::new(data), dirty: AtomicBool::new(false), asn_tx: OnceLock::new() }
    }

    pub fn set_asn_sender(&self, tx: UnboundedSender<String>) {
        let _ = self.asn_tx.set(tx);
    }

    pub fn enabled(&self) -> bool {
        self.data.lock().unwrap().learning
    }

    pub fn set_enabled(&self, on: bool) {
        self.data.lock().unwrap().learning = on;
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// A watched game just started: later hosts count towards a new play session.
    pub fn begin_session(&self, game: &str) {
        let mut d = self.data.lock().unwrap();
        let g = d.games.entry(game.to_string()).or_default();
        g.sessions += 1;
        let n = g.sessions;
        g.session_started.insert(n, now_ms());
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn clear(&self, game: &str) {
        self.data.lock().unwrap().games.remove(game);
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn record(&self, game: &str, ev: &FlowEvent) {
        self.record_at(game, ev, now_ms());
    }

    fn record_at(&self, game: &str, ev: &FlowEvent, now: u64) {
        let ip = ev.remote_ip.to_string();
        let lookup = {
            let mut d = self.data.lock().unwrap();
            if !d.learning {
                return;
            }
            let g = d.games.entry(game.to_string()).or_default();
            if g.sessions == 0 {
                g.sessions = 1; // game was already running when the app started
            }
            let session = g.sessions;
            g.session_started.entry(session).or_insert(now);
            let o = g.observed.entry(format!("{}:{}", ev.proto.as_str(), ip)).or_insert_with(|| Observed {
                ip: ip.clone(),
                proto: ev.proto.as_str().into(),
                ports: BTreeMap::new(),
                bytes_out: 0,
                bytes_in: 0,
                packets: 0,
                first_seen: now,
                last_seen: now,
                sessions: 0,
                episodes: Vec::new(),
                last_session: 0,
            });
            if o.ports.contains_key(&ev.remote_port) || o.ports.len() < MAX_PORTS_PER_HOST {
                *o.ports.entry(ev.remote_port).or_insert(0) += 1;
            }
            if ev.outbound {
                o.bytes_out += ev.bytes as u64;
            } else {
                o.bytes_in += ev.bytes as u64;
            }
            o.packets += 1;
            o.last_seen = now;
            match o.episodes.last_mut() {
                Some(e) if e.session == session && now.saturating_sub(e.last) <= EPISODE_GAP_MS => e.last = now,
                _ => {
                    if o.episodes.len() >= MAX_EPISODES {
                        o.episodes.remove(0);
                    }
                    o.episodes.push(Episode { session, first: now, last: now });
                }
            }
            if o.last_session != session {
                o.last_session = session;
                o.sessions += 1;
            }
            !d.asn.contains_key(&ip) && d.pending.insert(ip.clone())
        };
        self.dirty.store(true, Ordering::Relaxed);
        if lookup {
            self.queue_asn(ip);
        }
    }

    fn queue_asn(&self, ip: String) {
        if let Some(tx) = self.asn_tx.get() {
            let _ = tx.send(ip);
        }
    }

    pub fn apply_asn(&self, requested: &[String], results: Vec<(String, Option<AsnInfo>)>) {
        let mut d = self.data.lock().unwrap();
        for ip in requested {
            d.pending.remove(ip); // unanswered ones become eligible for retry
        }
        for (ip, info) in results {
            d.asn.insert(ip, info);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Re-queue lookups that failed earlier (network down, timeout).
    pub fn retry_missing_asn(&self) {
        let todo: Vec<String> = {
            let mut d = self.data.lock().unwrap();
            let ips: Vec<String> = d
                .games
                .values()
                .flat_map(|g| g.observed.values().map(|o| o.ip.clone()))
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            ips.into_iter().filter(|ip| !d.asn.contains_key(ip) && d.pending.insert(ip.clone())).collect()
        };
        for ip in todo {
            self.queue_asn(ip);
        }
    }

    pub fn snapshot(&self, game: &str) -> LearnView {
        let d = self.data.lock().unwrap();
        let Some(g) = d.games.get(game) else {
            return LearnView { learning: d.learning, sessions: 0, session_starts: Vec::new(), entries: Vec::new() };
        };
        let mut entries: Vec<EntryView> = g
            .observed
            .values()
            .map(|o| {
                let total = o.bytes_in + o.bytes_out;
                let noise = o.ports.keys().all(|p| NOISE_PORTS.contains(p));
                let score = if noise {
                    -10.0
                } else {
                    (1.0 + total as f64).log10() + 0.5 * o.sessions.min(5) as f64 + if o.proto == "udp" { 1.0 } else { 0.0 }
                };
                EntryView {
                    obs: o.clone(),
                    asn: d.asn.get(&o.ip).cloned().flatten(),
                    total_bytes: total,
                    score,
                    suggested: !noise && total >= SUGGEST_MIN_BYTES,
                }
            })
            .collect();
        entries.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        let session_starts = g.session_started.iter().map(|(s, t)| SessionStart { session: *s, started: *t }).collect();
        LearnView { learning: d.learning, sessions: g.sessions, session_starts, entries }
    }

    /// Write to disk if anything changed since the last flush.
    pub fn flush(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let json = {
            let d = self.data.lock().unwrap();
            serde_json::to_vec(&*d)
        };
        let Ok(json) = json else { return };
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::etw::Proto;

    fn ev(ip: &str, port: u16, bytes: u32, outbound: bool) -> FlowEvent {
        FlowEvent { pid: 1, proto: Proto::Udp, remote_ip: ip.parse().unwrap(), remote_port: port, bytes, outbound }
    }

    #[test]
    fn records_ranks_and_counts_sessions() {
        let dir = std::env::temp_dir().join(format!("gpn-learn-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let l = Learn::new(&dir);
        l.set_enabled(true);
        l.begin_session("game.exe");
        for _ in 0..100 {
            l.record("game.exe", &ev("203.0.113.9", 7777, 400, true)); // game server
        }
        l.record("game.exe", &ev("1.1.1.1", 53, 80, true)); // DNS noise
        l.begin_session("game.exe");
        l.record("game.exe", &ev("203.0.113.9", 7778, 400, false));

        let v = l.snapshot("game.exe");
        assert_eq!(v.sessions, 2);
        assert_eq!(v.entries[0].obs.ip, "203.0.113.9");
        assert_eq!(v.entries[0].obs.sessions, 2);
        assert_eq!(v.entries[0].obs.ports.len(), 2);
        assert!(v.entries[0].suggested);
        let dns = v.entries.iter().find(|e| e.obs.ip == "1.1.1.1").unwrap();
        assert!(!dns.suggested);

        l.flush();
        let reloaded = Learn::new(&dir);
        assert_eq!(reloaded.snapshot("game.exe").entries.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn episodes_track_when_a_host_is_active() {
        let dir = std::env::temp_dir().join(format!("gpn-learn-test3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let l = Learn::new(&dir);
        l.set_enabled(true);
        l.begin_session("game.exe");
        let t0 = 1_000_000u64;
        let e = ev("203.0.113.9", 7777, 100, true);
        l.record_at("game.exe", &e, t0);
        l.record_at("game.exe", &e, t0 + 10_000); // same episode
        l.record_at("game.exe", &e, t0 + 200_000); // after a long silence: a new episode
        l.begin_session("game.exe");
        l.record_at("game.exe", &e, t0 + 300_000); // next play session: a new episode

        let v = l.snapshot("game.exe");
        let eps = &v.entries[0].obs.episodes;
        assert_eq!(eps.len(), 3);
        assert_eq!((eps[0].session, eps[0].first, eps[0].last), (1, t0, t0 + 10_000));
        assert_eq!((eps[1].first, eps[1].last), (t0 + 200_000, t0 + 200_000));
        assert_eq!(eps[2].session, 2);
        assert_eq!(v.session_starts.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_events_when_not_learning() {
        let dir = std::env::temp_dir().join(format!("gpn-learn-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let l = Learn::new(&dir);
        l.record("game.exe", &ev("203.0.113.9", 7777, 400, true));
        assert!(l.snapshot("game.exe").entries.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
