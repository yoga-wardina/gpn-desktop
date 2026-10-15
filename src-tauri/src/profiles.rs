//! Saved game profiles: the reviewed set of server CIDRs (+ protocol/ports) for a game.
//! A profile is plain JSON so other clients/the server can consume the same file.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::net::{is_public_ipv4, parse_cidr};

const MAX_ENTRIES: usize = 500;
/// Widest prefix a profile may contain (a /16 is 65k addresses).
const MIN_PREFIX_LEN: u8 = 16;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEntry {
    pub cidr: String,
    /// "udp" | "tcp" | "any"
    pub proto: String,
    /// empty = all ports
    #[serde(default)]
    pub ports: Vec<u16>,
    /// "observed" (a single learned host) | "prefix" (its whole announced prefix) |
    /// "pack" (a bundled range list, e.g. a cloud region; survives re-saving the reviewed servers)
    pub source: String,
    #[serde(default)]
    pub asn: Option<u32>,
    #[serde(default)]
    pub as_name: Option<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub version: u32,
    /// game key = lowercase exe file name
    pub game: String,
    pub name: String,
    pub updated: u64,
    pub entries: Vec<ProfileEntry>,
}

/// Validate and normalise entries; rejects anything that is not public, too wide or malformed.
pub fn sanitize(entries: Vec<ProfileEntry>) -> Result<Vec<ProfileEntry>, String> {
    if entries.len() > MAX_ENTRIES {
        return Err(format!("too many entries (max {MAX_ENTRIES})"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for mut e in entries {
        let (net, len) = parse_cidr(&e.cidr).ok_or_else(|| format!("invalid CIDR: {}", e.cidr))?;
        if len < MIN_PREFIX_LEN {
            return Err(format!("{} is too wide (minimum prefix /{MIN_PREFIX_LEN})", e.cidr));
        }
        if !is_public_ipv4(net) {
            return Err(format!("{} is not a public address", e.cidr));
        }
        if !matches!(e.proto.as_str(), "udp" | "tcp" | "any") {
            return Err(format!("invalid protocol for {}", e.cidr));
        }
        if e.ports.iter().any(|p| *p == 0) {
            return Err(format!("invalid port for {}", e.cidr));
        }
        e.ports.sort_unstable();
        e.ports.dedup();
        e.cidr = format!("{net}/{len}");
        if seen.insert((e.cidr.clone(), e.proto.clone())) {
            out.push(e);
        }
    }
    Ok(out)
}

pub struct ProfileStore {
    dir: PathBuf,
    path: PathBuf,
    map: Mutex<HashMap<String, Profile>>,
}

impl ProfileStore {
    pub fn new(app_data: &std::path::Path) -> Self {
        let path = app_data.join("profiles.json");
        let map = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self { dir: app_data.join("profiles"), path, map: Mutex::new(map) }
    }

    fn persist(&self, m: &HashMap<String, Profile>) {
        if let Ok(json) = serde_json::to_string_pretty(m) {
            let _ = std::fs::write(&self.path, json);
        }
    }

    pub fn list(&self) -> Vec<Profile> {
        let mut v: Vec<Profile> = self.map.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.game.cmp(&b.game));
        v
    }

    pub fn save(&self, game: &str, name: &str, entries: Vec<ProfileEntry>) -> Result<Profile, String> {
        // a pack is not part of the learn-mode selection: drop any from the input, carry the old ones over
        let entries = sanitize(entries.into_iter().filter(|e| e.source != "pack").collect())?;
        if entries.is_empty() {
            return Err("select at least one server".into());
        }
        let mut m = self.map.lock().unwrap();
        let mut entries = entries;
        if let Some(old) = m.get(game) {
            entries.extend(old.entries.iter().filter(|e| e.source == "pack").cloned());
        }
        if entries.len() > MAX_ENTRIES {
            return Err(format!("too many entries (max {MAX_ENTRIES})"));
        }
        let profile = Profile {
            version: 1,
            game: game.to_string(),
            name: name.to_string(),
            updated: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            entries,
        };
        m.insert(game.to_string(), profile.clone());
        self.persist(&m);
        Ok(profile)
    }

    /// Remove the range pack(s) from a game's profile; returns how many entries went away.
    pub fn remove_pack(&self, game: &str) -> usize {
        let mut m = self.map.lock().unwrap();
        let Some(p) = m.get_mut(game) else { return 0 };
        let before = p.entries.len();
        p.entries.retain(|e| e.source != "pack");
        let removed = before - p.entries.len();
        if p.entries.is_empty() {
            m.remove(game);
        }
        self.persist(&m);
        removed
    }

    pub fn delete(&self, game: &str) {
        let mut m = self.map.lock().unwrap();
        m.remove(game);
        self.persist(&m);
    }

    /// Write `<app data>/profiles/<game>.json` and return its path.
    pub fn export(&self, game: &str) -> Result<String, String> {
        let p = self.map.lock().unwrap().get(game).cloned().ok_or("no saved profile for this game")?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let safe: String = game.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        let file = self.dir.join(format!("{safe}.json"));
        let json = serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?;
        std::fs::write(&file, json).map_err(|e| e.to_string())?;
        Ok(file.to_string_lossy().into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(cidr: &str) -> ProfileEntry {
        ProfileEntry { cidr: cidr.into(), proto: "udp".into(), ports: vec![7777, 7777, 7000], source: "observed".into(), asn: None, as_name: None, note: String::new() }
    }

    #[test]
    fn sanitizes() {
        let ok = sanitize(vec![entry("203.0.113.9/32"), entry("203.0.113.9/32")]);
        // 203.0.113.0/24 is TEST-NET-3 (not public) -> rejected
        assert!(ok.is_err());
        let ok = sanitize(vec![entry("8.8.8.8/32"), entry("8.8.8.8/32"), entry("52.1.2.3/20")]).unwrap();
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].ports, vec![7000, 7777]);
        assert_eq!(ok[1].cidr, "52.1.0.0/20");
    }

    #[test]
    fn resaving_keeps_the_pack_and_remove_pack_drops_it() {
        let dir = std::env::temp_dir().join(format!("gpn-profiles-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = ProfileStore::new(&dir);
        let mut pack = entry("52.1.0.0/16");
        pack.source = "pack".into();
        // seed a profile that has a pack (as a data file would)
        {
            let mut m = store.map.lock().unwrap();
            m.insert("g.exe".into(), Profile { version: 1, game: "g.exe".into(), name: "G".into(), updated: 0, entries: vec![entry("8.8.8.8/32"), pack.clone()] });
        }
        // saving a new reviewed selection replaces the servers but keeps the pack; input "pack" entries are ignored
        let saved = store.save("g.exe", "G", vec![entry("9.9.9.9/32"), pack.clone()]).unwrap();
        let cidrs: Vec<&str> = saved.entries.iter().map(|e| e.cidr.as_str()).collect();
        assert_eq!(cidrs, vec!["9.9.9.9/32", "52.1.0.0/16"]);
        assert_eq!(store.remove_pack("g.exe"), 1);
        assert_eq!(store.list()[0].entries.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_bad_entries() {
        assert!(sanitize(vec![entry("10.0.0.0/24")]).is_err());
        assert!(sanitize(vec![entry("52.0.0.0/8")]).is_err());
        assert!(sanitize(vec![entry("nonsense")]).is_err());
        let mut e = entry("8.8.8.8/32");
        e.proto = "icmp".into();
        assert!(sanitize(vec![e]).is_err());
    }
}
