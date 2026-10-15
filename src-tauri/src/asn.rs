//! IP -> origin AS / announced BGP prefix, via Team Cymru's bulk whois (TCP 43).
//! Used by learn mode to widen an observed server IP to the publisher's netblock.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AsnInfo {
    pub asn: u32,
    /// announced BGP prefix containing the IP, e.g. "52.0.0.0/11"
    pub prefix: String,
    pub cc: String,
    pub name: String,
}

/// Bulk lookup. Result has one entry per IP Cymru answered (None = not announced).
pub async fn lookup(ips: &[String]) -> Result<Vec<(String, Option<AsnInfo>)>, String> {
    if ips.is_empty() {
        return Ok(Vec::new());
    }
    let work = async {
        let mut s = TcpStream::connect("whois.cymru.com:43").await.map_err(|e| e.to_string())?;
        let mut q = String::from("begin\nverbose\n");
        for ip in ips {
            q.push_str(ip);
            q.push('\n');
        }
        q.push_str("end\n");
        s.write_all(q.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
        Ok::<_, String>(String::from_utf8_lossy(&buf).into_owned())
    };
    let text = tokio::time::timeout(Duration::from_secs(15), work)
        .await
        .map_err(|_| "whois.cymru.com timed out".to_string())??;
    Ok(parse_response(&text).into_iter().collect())
}

/// Rows: `AS | IP | BGP Prefix | CC | Registry | Allocated | AS Name`
pub fn parse_response(text: &str) -> HashMap<String, Option<AsnInfo>> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('|').map(str::trim).collect();
        if f.len() < 7 || f[0].eq_ignore_ascii_case("AS") || f[1].parse::<std::net::Ipv4Addr>().is_err() {
            continue; // header / banner / junk
        }
        let info = f[0].parse::<u32>().ok().map(|asn| AsnInfo {
            asn,
            prefix: f[2].to_string(),
            cc: f[3].to_string(),
            name: f[6].to_string(),
        });
        out.insert(f[1].to_string(), info);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bulk_response() {
        let text = "Bulk mode; whois.cymru.com [2026-10-15 10:00:00 +0000]\n\
AS      | IP               | BGP Prefix          | CC | Registry | Allocated  | AS Name\n\
15169   | 8.8.8.8          | 8.8.8.0/24          | US | arin     | 2023-12-28 | GOOGLE, US\n\
NA      | 10.0.0.1         | NA                  | NA | other    | NA         | NA\n";
        let m = parse_response(text);
        assert_eq!(m.len(), 2);
        let g = m["8.8.8.8"].as_ref().unwrap();
        assert_eq!((g.asn, g.prefix.as_str(), g.name.as_str()), (15169, "8.8.8.0/24", "GOOGLE, US"));
        assert!(m["10.0.0.1"].is_none());
    }
}
