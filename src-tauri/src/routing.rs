//! Decides what the tunnel should route and when.
//!
//! Desired routes = saved profile CIDRs of every *running* watched game (so the routing already
//! exists when the game opens its sockets — re-routing an open socket breaks the session) plus
//! the live /32 hosts seen on the wire. Changes are coalesced and applied strictly one at a
//! time; with no game traffic at all the tunnel is torn down after an idle grace period.

use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::net::{is_public_ipv4, parse_cidr};
use crate::profiles::Profile;
use crate::tunnel::TunnelManager;

/// How long to wait with no game traffic before dropping the tunnel (loading screens etc.).
const IDLE_GRACE: Duration = Duration::from_secs(60);
/// Coalesce bursts of new live connections into one update.
const DEBOUNCE: Duration = Duration::from_millis(1500);
/// The server rejects more targets than this (default 64).
const MAX_SERVER_IPS: usize = 60;
/// Live-detected hosts kept routed once seen (leaves headroom under the server cap for profile hosts).
const STICKY_CAP: usize = 40;
/// With no game running for this long the remembered hosts are forgotten.
const STICKY_FORGET: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Desired {
    /// tunnel AllowedIPs (CIDRs, sorted, /32s covered by a wider entry dropped)
    pub allowed: Vec<String>,
    /// /32 hosts for the server (POST /api/targets only takes public IPv4 hosts)
    pub server_ips: Vec<String>,
    /// routes that come from profiles only (the pre-route set)
    pub profile_cidrs: Vec<String>,
}

fn contains(cidr: &(Ipv4Addr, u8), ip: Ipv4Addr) -> bool {
    let (net, len) = *cidr;
    let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
    u32::from(ip) & mask == u32::from(net)
}

/// Build the desired route set from profiles of running games + live hosts.
pub fn desired_routes(profiles: &[Profile], running: &HashSet<String>, live: &[Ipv4Addr]) -> Desired {
    let mut cidrs: Vec<(Ipv4Addr, u8)> = Vec::new();
    // reviewed /32 hosts (not from a bundled range pack) — the only profile hosts worth telling the server about
    let mut reviewed_hosts: Vec<Ipv4Addr> = Vec::new();
    for p in profiles.iter().filter(|p| running.contains(&p.game)) {
        for e in &p.entries {
            if let Some(c) = parse_cidr(&e.cidr) {
                if !is_public_ipv4(c.0) {
                    continue;
                }
                if !cidrs.contains(&c) {
                    cidrs.push(c);
                }
                if c.1 == 32 && e.source != "pack" {
                    reviewed_hosts.push(c.0);
                }
            }
        }
    }
    let mut hosts: Vec<Ipv4Addr> = live.iter().copied().filter(|ip| is_public_ipv4(*ip)).collect();
    hosts.sort();
    hosts.dedup();

    let fmt = |(n, l): &(Ipv4Addr, u8)| format!("{n}/{l}");
    let mut profile_cidrs: Vec<String> = cidrs.iter().map(fmt).collect();
    profile_cidrs.sort();

    // server targets: /32 profile hosts first (they are what the user reviewed), then live hosts
    let mut server: Vec<Ipv4Addr> = reviewed_hosts;
    server.extend(hosts.iter().copied());
    server.sort();
    server.dedup();
    server.truncate(MAX_SERVER_IPS);

    // allowed: every CIDR + each live host not already covered by a profile CIDR
    let mut allowed: Vec<String> = cidrs.iter().map(fmt).collect();
    for h in &hosts {
        if !cidrs.iter().any(|c| contains(c, *h)) {
            allowed.push(format!("{h}/32"));
        }
    }
    allowed.sort();
    allowed.dedup();

    Desired { allowed, server_ips: server.iter().map(|i| i.to_string()).collect(), profile_cidrs }
}

/// Live hosts that stay routed after their connection ends.
///
/// MMOs hop between servers (a fresh instance server per dungeon, back to the hub afterwards). If a route
/// disappeared the moment its connection closed, every hop would reconfigure the tunnel just as the next
/// connection is opening — and that glitch is what kicks you. Routes therefore only ever get ADDED while
/// a game runs (oldest evicted beyond `STICKY_CAP`) and are dropped once no game has run for a while.
#[derive(Default)]
pub struct Sticky {
    seen: std::collections::HashMap<Ipv4Addr, Instant>,
    idle_since: Option<Instant>,
}

impl Sticky {
    pub fn update(&mut self, live_now: &[Ipv4Addr], game_running: bool) -> Vec<Ipv4Addr> {
        self.update_at(live_now, game_running, Instant::now())
    }

    fn update_at(&mut self, live_now: &[Ipv4Addr], game_running: bool, now: Instant) -> Vec<Ipv4Addr> {
        if !game_running {
            let since = *self.idle_since.get_or_insert(now);
            if now.duration_since(since) >= STICKY_FORGET {
                self.seen.clear();
            }
            return live_now.to_vec();
        }
        self.idle_since = None;
        for ip in live_now {
            self.seen.insert(*ip, now);
        }
        while self.seen.len() > STICKY_CAP {
            if let Some(oldest) = self.seen.iter().min_by_key(|(_, t)| **t).map(|(ip, _)| *ip) {
                self.seen.remove(&oldest);
            }
        }
        self.seen.keys().copied().collect()
    }
}

#[derive(Default)]
struct Sched {
    /// last desired value seen (to detect changes)
    last: Desired,
    pending: Option<Desired>,
    apply_at: Option<Instant>,
    idle_at: Option<Instant>,
    /// idle teardown already ran for the current quiet period
    gone_idle: bool,
}

pub struct Router {
    tunnel: Arc<TunnelManager>,
    sched: Mutex<Sched>,
}

impl Router {
    pub fn new(tunnel: Arc<TunnelManager>) -> Arc<Self> {
        Arc::new(Self { tunnel, sched: Mutex::new(Sched::default()) })
    }

    /// Called every watcher tick with the current desired set.
    pub fn update(&self, d: Desired) {
        let mut s = self.sched.lock().unwrap();
        if d.allowed.is_empty() {
            // connections vanish briefly on loading screens / server transfers: wait before tearing down
            s.pending = None;
            s.apply_at = None;
            s.last = d;
            if s.idle_at.is_none() && !s.gone_idle {
                s.idle_at = Some(Instant::now() + IDLE_GRACE);
            }
            return;
        }
        s.idle_at = None;
        s.gone_idle = false;
        if d == s.last {
            return;
        }
        // newly appearing profile routes (a game just started) are applied at once so they exist
        // before the game connects; live-host changes are debounced
        let new_profile_routes = d.profile_cidrs.iter().any(|c| !s.last.profile_cidrs.contains(c));
        s.last = d.clone();
        s.pending = Some(d);
        if new_profile_routes {
            s.apply_at = Some(Instant::now());
        } else if s.apply_at.is_none() {
            s.apply_at = Some(Instant::now() + DEBOUNCE);
        }
    }

    /// Worker: applies pending changes one at a time and handles idle teardown.
    pub async fn run(self: Arc<Self>, teardown_when_idle: impl Fn() -> bool + Send + 'static) {
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            enum Job {
                Apply(Desired),
                Idle,
            }
            let job = {
                let mut s = self.sched.lock().unwrap();
                let now = Instant::now();
                if s.apply_at.is_some_and(|t| now >= t) && s.pending.is_some() {
                    s.apply_at = None;
                    s.pending.take().map(Job::Apply)
                } else if s.idle_at.is_some_and(|t| now >= t) {
                    s.idle_at = None;
                    s.gone_idle = true;
                    Some(Job::Idle)
                } else {
                    None
                }
            };
            match job {
                Some(Job::Apply(d)) => {
                    log::info!("applying routes: {} allowed, {} server hosts", d.allowed.len(), d.server_ips.len());
                    if let Err(e) = self.tunnel.apply(d.allowed, d.server_ips).await {
                        log::error!("route apply failed: {e}");
                    }
                }
                Some(Job::Idle) => {
                    log::info!("no game traffic");
                    if teardown_when_idle() && self.tunnel.is_up() {
                        log::info!("tearing down tunnel");
                        self.tunnel.teardown(true).await;
                    }
                }
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::ProfileEntry;

    fn prof(game: &str, cidrs: &[&str]) -> Profile {
        Profile {
            version: 1,
            game: game.into(),
            name: game.into(),
            updated: 0,
            entries: cidrs
                .iter()
                .map(|c| ProfileEntry { cidr: c.to_string(), proto: "udp".into(), ports: vec![], source: "observed".into(), asn: None, as_name: None, note: String::new() })
                .collect(),
        }
    }
    fn running(g: &[&str]) -> HashSet<String> {
        g.iter().map(|s| s.to_string()).collect()
    }
    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn only_running_games_are_preroutes() {
        let ps = [prof("a.exe", &["52.1.0.0/20", "8.8.8.8/32"]), prof("b.exe", &["9.9.9.9/32"])];
        let d = desired_routes(&ps, &running(&["a.exe"]), &[]);
        assert_eq!(d.allowed, vec!["52.1.0.0/20", "8.8.8.8/32"]);
        assert_eq!(d.server_ips, vec!["8.8.8.8"]); // the /20 cannot go to the server
        assert!(desired_routes(&ps, &running(&[]), &[]).allowed.is_empty());
    }

    #[test]
    fn pack_hosts_are_routed_but_not_sent_to_the_server() {
        let mut p = prof("a.exe", &["8.8.8.8/32", "20.1.2.3/32", "20.0.0.0/16"]);
        p.entries[1].source = "pack".into();
        p.entries[2].source = "pack".into();
        let d = desired_routes(&[p], &running(&["a.exe"]), &[]);
        assert!(d.allowed.contains(&"20.1.2.3/32".to_string()) && d.allowed.contains(&"20.0.0.0/16".to_string()));
        assert_eq!(d.server_ips, vec!["8.8.8.8"]);
    }

    #[test]
    fn live_hosts_covered_by_profile_are_not_duplicated() {
        let ps = [prof("a.exe", &["52.1.0.0/20"])];
        let d = desired_routes(&ps, &running(&["a.exe"]), &[ip("52.1.2.3"), ip("1.1.1.1"), ip("10.0.0.1")]);
        assert_eq!(d.allowed, vec!["1.1.1.1/32", "52.1.0.0/20"]);
        assert_eq!(d.server_ips, vec!["1.1.1.1", "52.1.2.3"]); // server still learns the covered live host
    }

    #[test]
    fn sticky_hosts_survive_their_connection_and_expire_after_the_game() {
        let t0 = Instant::now();
        let mut s = Sticky::default();
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        let sorted = |mut v: Vec<Ipv4Addr>| {
            v.sort();
            v
        };
        // hub, then an instance server appears and the hub connection closes ...
        assert_eq!(sorted(s.update_at(&[ip("4.189.34.143")], true, at(0))), vec![ip("4.189.34.143")]);
        assert_eq!(s.update_at(&[ip("4.215.152.154")], true, at(10)).len(), 2);
        // ... the instance ends and nothing is live for a moment: both stay routed
        assert_eq!(s.update_at(&[], true, at(700)).len(), 2);
        // the next hop only adds
        assert_eq!(s.update_at(&[ip("40.81.221.196")], true, at(710)).len(), 3);
        // game closed: remembered for a minute (a relaunch keeps its routes), then forgotten
        assert!(s.update_at(&[], false, at(720)).is_empty());
        assert_eq!(s.update_at(&[], true, at(740)).len(), 3);
        assert!(s.update_at(&[], false, at(750)).is_empty());
        s.update_at(&[], false, at(750 + 61));
        assert!(s.update_at(&[], true, at(900)).is_empty());
    }

    #[test]
    fn sticky_set_is_capped_and_evicts_the_oldest() {
        let t0 = Instant::now();
        let mut s = Sticky::default();
        for n in 0..(STICKY_CAP as u64 + 5) {
            s.update_at(&[ip(&format!("8.8.{n}.1"))], true, t0 + Duration::from_secs(n));
        }
        let kept = s.update_at(&[], true, t0 + Duration::from_secs(1000));
        assert_eq!(kept.len(), STICKY_CAP);
        assert!(!kept.contains(&ip("8.8.0.1")) && kept.contains(&ip(&format!("8.8.{}.1", STICKY_CAP + 4))));
    }

    #[test]
    fn server_hosts_are_capped() {
        let live: Vec<Ipv4Addr> = (1..=100).map(|n| ip(&format!("8.8.{n}.1"))).collect();
        let d = desired_routes(&[], &running(&[]), &live);
        assert_eq!(d.server_ips.len(), MAX_SERVER_IPS);
    }
}
