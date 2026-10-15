//! Per-process network flow capture via ETW (Microsoft-Windows-Kernel-Network).
//!
//! Unlike netstat, this sees UDP: every send/receive event carries the owning PID and both
//! endpoints, so the remote server of a UDP game can be learned without a driver or packet
//! capture. Needs an elevated process (or membership of "Performance Log Users").

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};

use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;

use crate::net::is_public_ipv4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    pub fn as_str(self) -> &'static str {
        match self {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        }
    }
}

/// One observed packet/connection event belonging to a watched game process.
#[derive(Debug, Clone)]
pub struct FlowEvent {
    pub pid: u32,
    pub proto: Proto,
    pub remote_ip: Ipv4Addr,
    pub remote_port: u16,
    pub bytes: u32,
    pub outbound: bool,
}

/// pid -> game key (lowercase exe file name). Only these PIDs are reported.
pub type PidMap = Arc<RwLock<HashMap<u32, String>>>;

/// Pick the remote endpoint of a packet from its two endpoints. The Kernel-Network manifest names
/// the fields daddr/saddr, but which one is the local side differs between send and receive events
/// in practice, so prefer the single public address; only fall back to direction if both are public.
pub fn pick_remote(daddr: Ipv4Addr, saddr: Ipv4Addr, dport: u16, sport: u16, outbound: bool) -> Option<(Ipv4Addr, u16)> {
    match (is_public_ipv4(daddr), is_public_ipv4(saddr)) {
        (true, false) => Some((daddr, dport)),
        (false, true) => Some((saddr, sport)),
        (true, true) => Some(if outbound { (daddr, dport) } else { (saddr, sport) }),
        (false, false) => None,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EtwStatus {
    /// "off" | "running" | "denied" | "error" | "unsupported"
    pub state: String,
    pub message: String,
}

impl EtwStatus {
    fn new(state: &str, message: impl Into<String>) -> Self {
        Self { state: state.into(), message: message.into() }
    }
    pub fn off() -> Self {
        Self::new("off", "capture not started")
    }
    pub fn running() -> Self {
        Self::new("running", "capturing per-process network flows")
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use ferrisetw::parser::Parser;
    use ferrisetw::provider::Provider;
    use ferrisetw::schema_locator::SchemaLocator;
    use ferrisetw::trace::{stop_trace_by_name, UserTrace};
    use ferrisetw::EventRecord;

    const TRACE_NAME: &str = "GPN-Kernel-Network";
    /// Microsoft-Windows-Kernel-Network
    const KERNEL_NETWORK: &str = "7dd42a49-5329-4832-8dfd-43d979153a88";

    /// Keeps the ETW session alive; dropping it stops the trace.
    pub struct EtwHandle {
        _trace: UserTrace,
    }

    pub fn start(tx: UnboundedSender<FlowEvent>, pids: PidMap) -> Result<EtwHandle, EtwStatus> {
        // a previous crashed run can leave the named session behind
        let _ = stop_trace_by_name(TRACE_NAME);

        let provider = Provider::by_guid(KERNEL_NETWORK)
            .add_callback(move |record: &EventRecord, locator: &SchemaLocator| {
                on_event(record, locator, &tx, &pids);
            })
            .build();

        match UserTrace::new().named(TRACE_NAME.to_string()).enable(provider).start_and_process() {
            Ok(trace) => Ok(EtwHandle { _trace: trace }),
            Err(e) => {
                let msg = format!("{e:?}");
                if msg.contains("PermissionDenied") || msg.contains("code: 5,") {
                    Err(EtwStatus::new("denied", "ETW needs an elevated process (run as administrator)"))
                } else {
                    Err(EtwStatus::new("error", format!("could not start ETW session: {msg}")))
                }
            }
        }
    }

    fn on_event(record: &EventRecord, locator: &SchemaLocator, tx: &UnboundedSender<FlowEvent>, pids: &PidMap) {
        // Kernel-Network IPv4 events: TCP send/recv/connect = 10/11/12, UDP send/recv = 42/43
        let (proto, outbound) = match record.event_id() {
            10 | 12 => (Proto::Tcp, true),
            11 => (Proto::Tcp, false),
            42 => (Proto::Udp, true),
            43 => (Proto::Udp, false),
            _ => return,
        };
        let Ok(schema) = locator.event_schema(record) else { return };
        let parser = Parser::create(record, &schema);

        // the header PID is the logger's; the owning process is in the payload
        let Ok(pid) = parser.try_parse::<u32>("PID") else { return };
        if !pids.read().map(|m| m.contains_key(&pid)).unwrap_or(false) {
            return;
        }

        let (Ok(daddr), Ok(saddr)) = (
            parser.try_parse::<std::net::IpAddr>("daddr"),
            parser.try_parse::<std::net::IpAddr>("saddr"),
        ) else {
            return;
        };
        let (std::net::IpAddr::V4(daddr), std::net::IpAddr::V4(saddr)) = (daddr, saddr) else { return };
        // ports are logged in network byte order
        let dport = parser.try_parse::<u16>("dport").map(u16::swap_bytes).unwrap_or(0);
        let sport = parser.try_parse::<u16>("sport").map(u16::swap_bytes).unwrap_or(0);
        let bytes = parser.try_parse::<u32>("size").unwrap_or(0);

        if std::env::var_os("GPN_ETW_DEBUG").is_some() {
            log::info!("etw id={} pid={pid} {saddr}:{sport} -> {daddr}:{dport} size={bytes}", record.event_id());
        }

        if let Some((remote_ip, remote_port)) = pick_remote(daddr, saddr, dport, sport, outbound) {
            let _ = tx.send(FlowEvent { pid, proto, remote_ip, remote_port, bytes, outbound });
        }
    }
}

#[cfg(windows)]
pub use imp::{start, EtwHandle};

#[cfg(not(windows))]
pub struct EtwHandle;

#[cfg(not(windows))]
pub fn start(_tx: UnboundedSender<FlowEvent>, _pids: PidMap) -> Result<EtwHandle, EtwStatus> {
    Err(EtwStatus::new("unsupported", "ETW capture is Windows-only"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    #[test]
    fn remote_is_the_public_side_regardless_of_field_naming() {
        // send: local 192.168.1.5 -> server
        assert_eq!(pick_remote(ip("203.1.2.3"), ip("192.168.1.5"), 7777, 50000, true), Some((ip("203.1.2.3"), 7777)));
        // receive with swapped naming: daddr=local, saddr=server
        assert_eq!(pick_remote(ip("192.168.1.5"), ip("203.1.2.3"), 50000, 7777, false), Some((ip("203.1.2.3"), 7777)));
        // LAN-only traffic is ignored
        assert_eq!(pick_remote(ip("192.168.1.5"), ip("192.168.1.9"), 1, 2, true), None);
    }

    #[test]
    fn both_public_falls_back_to_direction() {
        assert_eq!(pick_remote(ip("8.8.8.8"), ip("1.1.1.1"), 53, 40000, true), Some((ip("8.8.8.8"), 53)));
        assert_eq!(pick_remote(ip("8.8.8.8"), ip("1.1.1.1"), 40000, 53, false), Some((ip("1.1.1.1"), 53)));
    }
}
