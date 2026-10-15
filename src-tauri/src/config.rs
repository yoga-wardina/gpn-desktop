use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::AuthSession;

/// Factory defaults, compiled into the app from `src-tauri/default-config.json`.
/// Never put secrets (tokens, keys, sessions) in that file — it is committed to git.
const DEFAULTS_JSON: &str = include_str!("../default-config.json");

/// Settings the user's own `gpn.config.json` may override. Any field missing from the user's file
/// falls back to the embedded defaults (struct-level `serde(default)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub gpn_server_url: String,
    pub gpn_token: String,
    pub poll_interval_ms: u64,
    pub games: Vec<String>,
    pub game_ports: Vec<u16>,
    /// Tear the tunnel down after ~60s without game traffic.
    pub tunnel_teardown_when_idle: bool,
    /// Persistent WireGuard private key (keeps the same peer identity across restarts).
    pub wg_private_key: Option<String>,
    /// Logged-in user session.
    pub auth: Option<AuthSession>,
}

impl Default for Config {
    fn default() -> Self {
        /// Used only if the embedded JSON were ever invalid (a unit test guards against that).
        fn fallback() -> Config {
            Config {
                gpn_server_url: "http://localhost:9090".into(),
                gpn_token: String::new(),
                poll_interval_ms: 2000,
                games: Vec::new(),
                game_ports: Vec::new(),
                tunnel_teardown_when_idle: true,
                wg_private_key: None,
                auth: None,
            }
        }
        // parse into a plain mirror struct so this doesn't recurse into Config::default()
        #[derive(Deserialize)]
        struct Embedded {
            gpn_server_url: String,
            #[serde(default)]
            poll_interval_ms: Option<u64>,
            #[serde(default)]
            games: Vec<String>,
            #[serde(default)]
            game_ports: Vec<u16>,
            #[serde(default)]
            tunnel_teardown_when_idle: Option<bool>,
        }
        match serde_json::from_str::<Embedded>(DEFAULTS_JSON) {
            Ok(e) => {
                let base = fallback();
                Config {
                    gpn_server_url: e.gpn_server_url,
                    poll_interval_ms: e.poll_interval_ms.unwrap_or(base.poll_interval_ms),
                    games: e.games,
                    game_ports: e.game_ports,
                    tunnel_teardown_when_idle: e.tunnel_teardown_when_idle.unwrap_or(true),
                    ..base
                }
            }
            Err(_) => fallback(),
        }
    }
}

pub struct ConfigStore {
    path: PathBuf,
    pub config: std::sync::Mutex<Config>,
}

impl ConfigStore {
    pub fn new(app_data: &std::path::Path) -> Self {
        let path = app_data.join("gpn.config.json");
        let config = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let store = Self {
            path,
            config: std::sync::Mutex::new(config),
        };
        store.persist();
        // holds tokens and the WG private key: current user + SYSTEM only (best effort)
        crate::proc::restrict_file(&store.path, &[&crate::proc::current_user(), "*S-1-5-18"]);
        store
    }

    pub fn persist(&self) {
        let cfg = self.config.lock().unwrap().clone();
        if let Ok(json) = serde_json::to_string_pretty(&cfg) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    pub fn update<F: FnOnce(&mut Config)>(&self, f: F) {
        let mut cfg = self.config.lock().unwrap();
        f(&mut cfg);
        drop(cfg);
        self.persist();
    }
}

/// Metadata cache: exe key -> { name, company, icon (png base64), source }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameMeta {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub source: String,
    /// metadata format version; entries older than the app's current one are re-resolved
    #[serde(default)]
    pub v: u32,
}

pub struct MetaCache {
    path: PathBuf,
    map: std::sync::Mutex<HashMap<String, GameMeta>>,
}

impl MetaCache {
    pub fn new(app_data: &std::path::Path) -> Self {
        let path = app_data.join("meta.json");
        let map = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            map: std::sync::Mutex::new(map),
        }
    }

    pub fn get(&self, key: &str) -> Option<GameMeta> {
        self.map.lock().unwrap().get(key).cloned()
    }

    pub fn insert(&self, key: String, meta: GameMeta) {
        let mut m = self.map.lock().unwrap();
        m.insert(key, meta);
        if let Ok(json) = serde_json::to_string_pretty(&*m) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_defaults_are_valid_and_secret_free() {
        let raw: serde_json::Value = serde_json::from_str(DEFAULTS_JSON).expect("default-config.json must be valid JSON");
        for forbidden in ["gpn_token", "wg_private_key", "auth", "accessToken", "refreshToken"] {
            assert!(raw.get(forbidden).is_none(), "default-config.json must not contain `{forbidden}`");
        }
        let c = Config::default();
        assert!(c.gpn_server_url.starts_with("https://"));
        assert!(c.gpn_token.is_empty() && c.wg_private_key.is_none() && c.auth.is_none());
        assert_eq!(c.poll_interval_ms, 2000);
        assert!(!c.games.is_empty());
    }

    #[test]
    fn missing_fields_in_a_user_file_inherit_the_defaults() {
        let c: Config = serde_json::from_str(r#"{ "gpn_token": "abc" }"#).unwrap();
        assert_eq!(c.gpn_token, "abc");
        assert_eq!(c.gpn_server_url, Config::default().gpn_server_url);
        assert_eq!(c.games, Config::default().games);
        // an explicitly emptied list stays empty
        let c: Config = serde_json::from_str(r#"{ "games": [] }"#).unwrap();
        assert!(c.games.is_empty());
    }
}
