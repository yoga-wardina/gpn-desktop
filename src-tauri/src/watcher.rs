//! Process watcher: finds watched game processes and lists their remote
//! connections. Windows: tasklist + netstat -nao. Linux: ps + ss.

use std::collections::HashSet;
use std::process::Command;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct GameProc {
    pub pid: u32,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub proto: String,
}

fn run_cmd(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Find PIDs+names of processes whose image name contains any watched entry.
pub fn find_processes(watched: &[String]) -> Vec<GameProc> {
    let watched_lc: Vec<String> = watched.iter().map(|w| w.to_lowercase()).collect();
    if watched_lc.is_empty() {
        return Vec::new();
    }

    if cfg!(windows) {
        let out = run_cmd("tasklist", &["/FO", "CSV", "/NH"]);
        parse_csv_rows(&out)
            .into_iter()
            .filter_map(|row| {
                let name = row.first()?.to_string();
                let pid: u32 = row.get(1)?.trim().parse().ok()?;
                Some((name, pid))
            })
            .filter(|(name, _)| {
                let n = name.to_lowercase();
                watched_lc.iter().any(|w| n.contains(w.as_str()))
            })
            .map(|(name, pid)| GameProc { pid, name })
            .collect()
    } else {
        let out = run_cmd("ps", &["-eo", "pid,comm"]);
        out.lines()
            .skip(1)
            .filter_map(|line| {
                let mut it = line.trim().split_whitespace();
                let pid: u32 = it.next()?.parse().ok()?;
                let name = it.collect::<Vec<_>>().join(" ");
                Some((pid, name))
            })
            .filter(|(_, name)| {
                let n = name.to_lowercase();
                watched_lc.iter().any(|w| n.contains(w.as_str()))
            })
            .map(|(pid, name)| GameProc { pid, name })
            .collect()
    }
}

/// List remote (ip, port, proto) for the given PIDs.
pub fn connections_for(pids: &[u32]) -> Vec<Target> {
    if pids.is_empty() {
        return Vec::new();
    }
    let wanted: HashSet<u32> = pids.iter().copied().collect();
    let mut out = Vec::new();

    if cfg!(windows) {
        let out_s = run_cmd("netstat", &["-nao"]);
        for line in out_s.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 5 {
                continue;
            }
            let (proto_s, remote, pid_s) = (parts[0], parts[2], parts[4]);
            let Ok(pid) = pid_s.parse::<u32>() else { continue };
            if !wanted.contains(&pid) {
                continue;
            }
            let proto = if proto_s.to_lowercase().starts_with("udp") {
                "udp"
            } else if proto_s.to_lowercase().starts_with("tcp") {
                "tcp"
            } else {
                continue;
            };
            if let Some(t) = parse_remote(remote, proto) {
                out.push(t);
            }
        }
    } else {
        let out_s = run_cmd("ss", &["-tunap"]);
        for line in out_s.lines().skip(1) {
            let pid = match line.find("pid=") {
                Some(i) => {
                    let rest = &line[i + 4..];
                    let digits: String =
                        rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    match digits.parse::<u32>() {
                        Ok(p) => p,
                        Err(_) => continue,
                    }
                }
                None => continue,
            };
            if !wanted.contains(&pid) {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 5 {
                continue;
            }
            let proto = if parts[0] == "udp" { "udp" } else { "tcp" };
            if let Some(t) = parse_remote(parts[4], proto) {
                out.push(t);
            }
        }
    }
    out
}

/// "1.2.3.4:5678" or "[::1]:80" -> Target
fn parse_remote(remote: &str, proto: &str) -> Option<Target> {
    let (ip, port_str) = if let Some(stripped) = remote.strip_prefix('[') {
        let close = stripped.find(']')?;
        (&stripped[..close], &stripped[close + 2..])
    } else {
        let idx = remote.rfind(':')?;
        (&remote[..idx], &remote[idx + 1..])
    };
    if ip.is_empty() || ip == "0.0.0.0" || ip == "::" || ip == "*" {
        return None;
    }
    let port = port_str.parse().ok();
    Some(Target {
        ip: ip.to_string(),
        port,
        proto: proto.into(),
    })
}

/// Windows CSV rows: "a","b",...
fn parse_csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut row = Vec::new();
        let mut cur = String::new();
        let mut in_q = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    if in_q && chars.peek() == Some(&'"') {
                        cur.push('"');
                        chars.next();
                    } else {
                        in_q = !in_q;
                    }
                }
                ',' if !in_q => {
                    row.push(cur.clone());
                    cur.clear();
                }
                _ => cur.push(c),
            }
        }
        row.push(cur);
        rows.push(row);
    }
    rows
}
