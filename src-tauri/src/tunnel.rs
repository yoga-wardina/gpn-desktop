//! Local WireGuard tunnel management (Windows, WireGuard for Windows in C:\Program Files\WireGuard).
//!
//! Flow: persistent client keypair -> register the public key with the GPN server -> write a
//! split-tunnel wg-quick config (AllowedIPs = only the game CIDRs/IPs) -> `wireguard.exe
//! /installtunnelservice` -> on target changes tell the server, then reinstall with the new
//! AllowedIPs (Windows only reloads routes on service start: a short blip).
//!
//! Needs WireGuard for Windows installed and this app elevated (service install). All mutating
//! operations are serialised through `inner`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::api::GpnApi;
use crate::config::ConfigStore;
use crate::proc::{restrict_file, run};

const WG_DIR: &str = r"C:\Program Files\WireGuard";
const TUNNEL_NAME: &str = "gpn";
const SERVICE: &str = "WireGuardTunnel$gpn";
const STALE_HANDSHAKE_SECS: u64 = 180;

fn wg_exe(exe: &str) -> String {
    format!(r"{WG_DIR}\{exe}")
}

fn conf_path() -> PathBuf {
    PathBuf::from(WG_DIR).join("Tunnel Configs").join(format!("{TUNNEL_NAME}.conf"))
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelView {
    pub registered: bool,
    pub vpn_ip: Option<String>,
    pub tunnel_up: bool,
    pub allowed_ips: Vec<String>,
    /// seconds since the last WireGuard handshake (None = never / tunnel down)
    pub handshake_age_secs: Option<u64>,
    /// the GPN server's public WireGuard endpoint (host:port) once registered
    pub endpoint: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Default)]
struct Inner {
    priv_key: Option<String>,
    pub_key: Option<String>,
    vpn_ip: Option<String>,
    server_pub: Option<String>,
    endpoint: Option<String>,
    /// AllowedIPs of the running tunnel (CIDRs, sorted)
    allowed: Vec<String>,
    /// /32 hosts last synced to the server (sorted)
    server_ips: Vec<String>,
    up: bool,
    up_since: Option<Instant>,
}

pub struct TunnelManager {
    api: Arc<GpnApi>,
    cfg: Arc<ConfigStore>,
    inner: tokio::sync::Mutex<Inner>,
    view: Mutex<TunnelView>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

pub fn build_conf(private_key: &str, address: &str, server_pub: &str, allowed: &[String], endpoint: &str) -> String {
    // No DNS line: with a split tunnel the DNS server isn't in AllowedIPs and would break name resolution.
    format!(
        "[Interface]\nPrivateKey = {private_key}\nAddress = {address}/32\n\n[Peer]\nPublicKey = {server_pub}\nAllowedIPs = {}\nEndpoint = {endpoint}\nPersistentKeepalive = 15\n",
        allowed.join(", ")
    )
}

/// `wg show <if> latest-handshakes` -> "<pubkey>\t<unix timestamp>" (0 = never). It is a
/// TIMESTAMP, not an age.
pub fn parse_handshake_ts(out: &str) -> u64 {
    out.lines().next().and_then(|l| l.split('\t').nth(1)).and_then(|t| t.trim().parse().ok()).unwrap_or(0)
}

/// `wg show <if> allowed-ips` -> "<pubkey>\t<cidr> <cidr> ..."
pub fn parse_allowed_ips(out: &str) -> Vec<String> {
    let line = out.lines().next().unwrap_or("");
    sorted(
        line.split('\t')
            .nth(1)
            .unwrap_or("")
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

impl TunnelManager {
    pub fn new(api: Arc<GpnApi>, cfg: Arc<ConfigStore>) -> Self {
        Self { api, cfg, inner: tokio::sync::Mutex::new(Inner::default()), view: Mutex::new(TunnelView::default()) }
    }

    pub fn view(&self) -> TunnelView {
        self.view.lock().unwrap().clone()
    }

    fn publish(&self, i: &Inner, err: Option<String>) {
        let mut v = self.view.lock().unwrap();
        v.registered = i.vpn_ip.is_some();
        v.vpn_ip = i.vpn_ip.clone();
        v.tunnel_up = i.up;
        v.allowed_ips = i.allowed.clone();
        v.endpoint = i.endpoint.clone();
        if !i.up {
            v.handshake_age_secs = None;
        }
        v.last_error = err;
    }

    /// Adopt a tunnel service left running by a previous run (crash / restart).
    pub async fn init(&self) {
        let mut i = self.inner.lock().await;
        if probe(&mut i).await {
            i.up_since = Some(Instant::now());
            i.allowed = running_allowed_ips().await.unwrap_or_default();
            log::info!("adopted running tunnel ({} allowed entr(ies))", i.allowed.len());
        }
        self.publish(&i, None);
    }

    /// Bring the tunnel to exactly `allowed` (CIDRs) and tell the server about `server_ips` (/32 hosts).
    pub async fn apply(&self, allowed: Vec<String>, server_ips: Vec<String>) -> Result<(), String> {
        let mut i = self.inner.lock().await;
        let mut warn = None;
        let res = self.apply_inner(&mut i, sorted(allowed), sorted(server_ips), &mut warn).await;
        let err = res.clone().err().or(warn);
        if let Some(e) = &err {
            log::error!("tunnel apply failed: {e}");
        }
        self.publish(&i, err);
        res
    }

    async fn apply_inner(&self, i: &mut Inner, allowed: Vec<String>, server_ips: Vec<String>, warn: &mut Option<String>) -> Result<(), String> {
        if allowed.is_empty() {
            return Ok(());
        }
        let tunnel_change = !(i.up && i.allowed == allowed);
        let sync_needed = i.server_ips != server_ips;
        if !tunnel_change && !sync_needed {
            return Ok(());
        }

        if i.priv_key.is_none() || i.vpn_ip.is_none() || i.server_pub.is_none() || i.endpoint.is_none() {
            self.register_inner(i).await?;
        }
        if sync_needed {
            match self.sync_server(i, &server_ips).await {
                Ok(()) => i.server_ips = server_ips,
                Err(e) => *warn = Some(format!("server-side route sync failed: {e}")),
            }
        }
        if !tunnel_change {
            return Ok(());
        }

        let conf = build_conf(
            i.priv_key.as_deref().unwrap(),
            i.vpn_ip.as_deref().unwrap(),
            i.server_pub.as_deref().unwrap(),
            &allowed,
            i.endpoint.as_deref().unwrap(),
        );
        let path = conf_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot write tunnel config ({e}) — is WireGuard installed and the app running as Administrator?"))?;
        }
        std::fs::write(&path, conf).map_err(|e| format!("cannot write tunnel config ({e}) — run the app as Administrator"))?;
        restrict_file(&path, &["*S-1-5-18", "*S-1-5-32-544"]); // SYSTEM + Administrators only

        if !i.up {
            install(i).await?;
        } else if running_allowed_ips().await.is_some_and(|r| r == allowed) {
            // already in sync
        } else {
            reinstall(i).await?;
        }
        i.allowed = allowed;
        Ok(())
    }

    async fn ensure_keys(&self, i: &mut Inner) -> Result<(), String> {
        let saved = self.cfg.config.lock().unwrap().wg_private_key.clone().filter(|k| !k.is_empty());
        let priv_key = match saved {
            Some(k) => k,
            None => {
                let k = run(&wg_exe("wg.exe"), &["genkey"], None, true).await?.trim().to_string();
                self.cfg.update(|c| c.wg_private_key = Some(k.clone()));
                k
            }
        };
        let pub_key = run(&wg_exe("wg.exe"), &["pubkey"], Some(priv_key.clone()), true).await?.trim().to_string();
        self.api.set_peer_key(Some(pub_key.clone()));
        i.priv_key = Some(priv_key);
        i.pub_key = Some(pub_key);
        Ok(())
    }

    /// Register with the GPN server (idempotent) and make sure we have a keypair.
    async fn register_inner(&self, i: &mut Inner) -> Result<(), String> {
        self.ensure_keys(i).await?;
        let name: String = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "client".into()).chars().take(64).collect();
        let data = self.api.register_peer(i.pub_key.as_deref().unwrap(), &name).await.map_err(|e| format!("register failed: {e}"))?;
        let field = |k: &str| data[k].as_str().map(str::to_string);
        i.vpn_ip = Some(field("ip").ok_or("register: server returned no ip")?);
        i.server_pub = Some(field("serverPublicKey").ok_or("register: server returned no serverPublicKey")?);
        i.endpoint = Some(field("endpoint").ok_or("register: server returned no endpoint")?);
        Ok(())
    }

    /// Tell the server our current targets. Re-registers once if the server forgot this peer.
    async fn sync_server(&self, i: &mut Inner, ips: &[String]) -> Result<(), String> {
        match self.api.push_targets(ips).await {
            Err(e) if e.starts_with("HTTP 404") => {
                self.register_inner(i).await?;
                self.api.push_targets(ips).await.map(|_| ())
            }
            r => r.map(|_| ()),
        }
    }

    /// Stop the tunnel. `clear_server`: also empty the peer's server-side targets (best effort).
    pub async fn teardown(&self, clear_server: bool) {
        let mut i = self.inner.lock().await;
        if clear_server && self.api.peer_key().is_some() && !i.server_ips.is_empty() {
            if let Err(e) = self.api.push_targets(&[]).await {
                log::warn!("clearing server targets failed: {e}");
            }
        }
        uninstall().await;
        i.up = false;
        i.allowed.clear();
        i.server_ips.clear();
        self.publish(&i, None);
    }

    pub fn is_up(&self) -> bool {
        self.view.lock().unwrap().tunnel_up
    }

    /// Every 30s: if the tunnel runs but the latest handshake is older than 3 minutes (keepalives
    /// renew it every ~2 min) — or there never was one since it came up — reinstall the service.
    pub async fn watchdog(self: Arc<Self>) {
        let mut last_reload: Option<Instant> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let mut i = self.inner.lock().await;
            if !probe(&mut i).await {
                self.publish(&i, self.view().last_error);
                continue;
            }
            let out = run(&wg_exe("wg.exe"), &["show", TUNNEL_NAME, "latest-handshakes"], None, false).await.unwrap_or_default();
            let ts = parse_handshake_ts(&out);
            let age = if ts > 0 { now_secs().saturating_sub(ts) } else { i.up_since.map_or(0, |t| t.elapsed().as_secs()) };
            self.view.lock().unwrap().handshake_age_secs = (ts > 0).then_some(age);
            if age > STALE_HANDSHAKE_SECS && last_reload.is_none_or(|t| t.elapsed() > Duration::from_secs(120)) {
                last_reload = Some(Instant::now());
                log::warn!("watchdog: handshake stale ({age}s) — reloading tunnel");
                let err = reinstall(&mut i).await.err();
                self.publish(&i, err);
            } else {
                self.publish(&i, self.view().last_error);
            }
        }
    }
}

/// Is the tunnel service actually RUNNING? Updates `up`.
async fn probe(i: &mut Inner) -> bool {
    i.up = run("sc", &["query", SERVICE], None, false).await.is_ok_and(|o| o.contains("RUNNING"));
    i.up
}

async fn running_allowed_ips() -> Option<Vec<String>> {
    let out = run(&wg_exe("wg.exe"), &["show", TUNNEL_NAME, "allowed-ips"], None, true).await.ok()?;
    Some(parse_allowed_ips(&out))
}

async fn uninstall() {
    let _ = run(&wg_exe("wireguard.exe"), &["/uninstalltunnelservice", TUNNEL_NAME], None, true).await; // not installed is fine
}

async fn install(i: &mut Inner) -> Result<(), String> {
    let conf = conf_path().to_string_lossy().into_owned();
    if run(&wg_exe("wireguard.exe"), &["/installtunnelservice", &conf], None, true).await.is_err() {
        // service may already exist (stale from a crash): remove and retry once
        uninstall().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        run(&wg_exe("wireguard.exe"), &["/installtunnelservice", &conf], None, true)
            .await
            .map_err(|e| format!("could not install the tunnel service (is the app running as Administrator?): {e}"))?;
    }
    for _ in 0..10 {
        if probe(i).await {
            i.up_since = Some(Instant::now());
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err("tunnel service installed but is not running".into())
}

/// Windows only reloads AllowedIPs/routes on service (re)start: costs a short blip.
async fn reinstall(i: &mut Inner) -> Result<(), String> {
    uninstall().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    install(i).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_is_a_timestamp() {
        assert_eq!(parse_handshake_ts("abcPUB=\t1700000000\n"), 1_700_000_000);
        assert_eq!(parse_handshake_ts("abcPUB=\t0\n"), 0);
        assert_eq!(parse_handshake_ts(""), 0);
    }

    #[test]
    fn allowed_ips_parse() {
        assert_eq!(parse_allowed_ips("PUB=\t10.0.0.2/32 52.1.0.0/20\n"), vec!["10.0.0.2/32", "52.1.0.0/20"]);
        assert_eq!(parse_allowed_ips("PUB=\t(none)\n"), vec!["(none)"]);
        assert!(parse_allowed_ips("").is_empty());
    }

    #[test]
    fn conf_layout() {
        let c = build_conf("PRIV", "10.8.0.5", "SPUB", &["1.2.3.4/32".into(), "52.1.0.0/20".into()], "vpn.example.com:51820");
        assert!(c.contains("Address = 10.8.0.5/32"));
        assert!(c.contains("AllowedIPs = 1.2.3.4/32, 52.1.0.0/20"));
        assert!(c.contains("Endpoint = vpn.example.com:51820"));
        assert!(!c.contains("DNS"));
    }
}
