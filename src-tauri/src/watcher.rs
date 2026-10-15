//! Process watcher: finds watched game processes and lists their TCP connections.
//! Windows: tasklist + netstat -nao. Linux: ps + ss. UDP comes from ETW (see etw.rs);
//! netstat cannot report a UDP remote address.

use std::collections::HashSet;
use std::net::Ipv4Addr;

use serde::Serialize;

use crate::net::is_public_ipv4;
use crate::proc::command;

#[derive(Debug, Clone, Serialize)]
pub struct GameProc {
    pub pid: u32,
    pub name: String,
    /// watched-entry key this process belongs to (lowercase exe file name)
    pub game: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Target {
    pub ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub proto: String,
}

/// Key of a watched entry: its lowercase file name ("D:\\x\\AION2.exe" -> "aion2.exe").
pub fn game_key(entry: &str) -> String {
    entry.rsplit(['\\', '/']).next().unwrap_or(entry).trim().to_lowercase()
}

/// Image names are truncated by tasklist (25 chars) / ps (15): accept a truncated prefix.
fn name_matches(proc_name_lc: &str, key: &str) -> bool {
    proc_name_lc == key || (proc_name_lc.len() >= 15 && key.starts_with(proc_name_lc))
}

fn run_cmd(cmd: &str, args: &[&str]) -> String {
    command(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Find running processes matching any watched entry (by file name).
pub fn find_processes(watched: &[String]) -> Vec<GameProc> {
    let keys: Vec<String> = watched.iter().map(|w| game_key(w)).filter(|k| !k.is_empty()).collect();
    if keys.is_empty() {
        return Vec::new();
    }

    let rows: Vec<(String, u32)> = if cfg!(windows) {
        let out = run_cmd("tasklist", &["/FO", "CSV", "/NH"]);
        parse_csv_rows(&out)
            .into_iter()
            .filter_map(|row| Some((row.first()?.to_string(), row.get(1)?.trim().parse().ok()?)))
            .collect()
    } else {
        let out = run_cmd("ps", &["-eo", "pid,comm"]);
        out.lines()
            .skip(1)
            .filter_map(|line| {
                let mut it = line.trim().split_whitespace();
                let pid: u32 = it.next()?.parse().ok()?;
                Some((it.collect::<Vec<_>>().join(" "), pid))
            })
            .collect()
    };

    rows.into_iter()
        .filter_map(|(name, pid)| {
            let lc = name.to_lowercase();
            let game = keys.iter().find(|k| name_matches(&lc, k))?.clone();
            Some(GameProc { pid, name, game })
        })
        .collect()
}

/// Established TCP connections of the given PIDs to public IPv4 hosts.
pub fn connections_for(pids: &[u32]) -> Vec<Target> {
    if pids.is_empty() {
        return Vec::new();
    }
    let wanted: HashSet<u32> = pids.iter().copied().collect();
    let mut out = Vec::new();

    if cfg!(windows) {
        for line in run_cmd("netstat", &["-nao"]).lines() {
            if let Some(t) = parse_netstat_line(line, &wanted) {
                out.push(t);
            }
        }
    } else {
        for line in run_cmd("ss", &["-tunap"]).lines().skip(1) {
            let Some(i) = line.find("pid=") else { continue };
            let digits: String = line[i + 4..].chars().take_while(|c| c.is_ascii_digit()).collect();
            let Ok(pid) = digits.parse::<u32>() else { continue };
            if !wanted.contains(&pid) {
                continue;
            }
            // Netid State Recv-Q Send-Q Local Peer Process
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 6 || parts[1] != "ESTAB" {
                continue;
            }
            if let Some(t) = parse_remote(parts[5], if parts[0] == "udp" { "udp" } else { "tcp" }) {
                out.push(t);
            }
        }
    }
    out
}

// States that mean "not an active connection". Windows localizes state names, so on other
// locales nothing is excluded here and only the remote-port check in parse_remote applies.
const DEAD_STATES: [&str; 8] = ["TIME_WAIT", "CLOSE_WAIT", "FIN_WAIT_1", "FIN_WAIT_2", "LAST_ACK", "CLOSING", "SYN_SENT", "SYN_RECEIVED"];

/// `  TCP    192.168.1.5:50000    203.0.113.9:443    ESTABLISHED    1234`
/// (UDP rows have no state column => 4 fields, skipped.)
fn parse_netstat_line(line: &str, wanted: &HashSet<u32>) -> Option<Target> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 5 || !parts[0].eq_ignore_ascii_case("tcp") {
        return None;
    }
    let pid: u32 = parts[4].parse().ok()?;
    if !wanted.contains(&pid) || DEAD_STATES.contains(&parts[3].to_uppercase().as_str()) {
        return None;
    }
    parse_remote(parts[2], "tcp")
}

/// "1.2.3.4:5678" -> Target (public IPv4 only; IPv6 and LISTENING rows are dropped).
fn parse_remote(remote: &str, proto: &str) -> Option<Target> {
    let idx = remote.rfind(':')?;
    let ip: Ipv4Addr = remote[..idx].parse().ok()?;
    let port: u16 = remote[idx + 1..].parse().ok()?;
    if port == 0 || !is_public_ipv4(ip) {
        return None;
    }
    Some(Target { ip: ip.to_string(), port: Some(port), proto: proto.into() })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_matching() {
        assert_eq!(game_key(r"D:\SteamLibrary\AION2.exe"), "aion2.exe");
        assert_eq!(game_key("AION2.exe"), "aion2.exe");
        assert!(name_matches("aion2.exe", "aion2.exe"));
        assert!(!name_matches("aion2.exe.mui", "aion2.exe"));
        // truncated image name
        assert!(name_matches("verylongexecutablename", "verylongexecutablename.exe"));
        assert!(!name_matches("short", "shortlonger.exe"));
    }

    #[test]
    fn netstat_rows() {
        let wanted: HashSet<u32> = [1234].into();
        let ok = parse_netstat_line("  TCP    192.168.1.5:50000    8.8.4.4:443    ESTABLISHED    1234", &wanted).unwrap();
        assert_eq!((ok.ip.as_str(), ok.port), ("8.8.4.4", Some(443)));
        // listening, private, dead state, other pid, udp
        assert!(parse_netstat_line("  TCP    0.0.0.0:135    0.0.0.0:0    LISTENING    1234", &wanted).is_none());
        assert!(parse_netstat_line("  TCP    192.168.1.5:50000    192.168.1.9:445    ESTABLISHED    1234", &wanted).is_none());
        assert!(parse_netstat_line("  TCP    192.168.1.5:50000    8.8.4.4:443    TIME_WAIT    1234", &wanted).is_none());
        assert!(parse_netstat_line("  TCP    192.168.1.5:50000    8.8.4.4:443    ESTABLISHED    99", &wanted).is_none());
        assert!(parse_netstat_line("  UDP    0.0.0.0:5353    *:*    1234", &wanted).is_none());
        assert!(parse_netstat_line("  TCP    [::1]:50000    [2001:db8::1]:443    ESTABLISHED    1234", &wanted).is_none());
    }
}
