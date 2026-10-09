use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub gpn_server_url: String,
    #[serde(default)]
    pub gpn_token: String,
    #[serde(default = "default_poll")]
    pub poll_interval_ms: u64,
    #[serde(default)]
    pub games: Vec<String>,
    #[serde(default)]
    pub game_ports: Vec<u16>,
}

fn default_poll() -> u64 {
    2000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gpn_server_url: "http://localhost:9090".into(),
            gpn_token: String::new(),
            poll_interval_ms: default_poll(),
            games: Vec::new(),
            game_ports: Vec::new(),
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
