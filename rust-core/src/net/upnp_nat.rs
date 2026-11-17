use socket2::{Domain, Protocol, Socket, Type};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static LAST_SSDP_PROBE_MS: AtomicU64 = AtomicU64::new(0);
const SSDP_COOLDOWN_MS: u64 = 30_000;

pub fn is_cgnat_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000
}

fn probe_kernel_ipv6_v6only() -> (bool, bool) {
    let Ok(sock) = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)) else {
        return (false, false);
    };
    let v6_only = sock.only_v6().unwrap_or(true);
    (true, v6_only)
}

pub fn probe_local_and_wan(
    bedrock_port: u16,
    java_port: u16,
    enable_upnp: bool,
    dual_stack_ipv6: bool,
) -> String {
    probe_local_and_wan_with_bind(bedrock_port, java_port, enable_upnp, dual_stack_ipv6, None, true)
}

pub fn probe_local_and_wan_with_bind(
    bedrock_port: u16,
    java_port: u16,
    enable_upnp: bool,
    requested_v6: bool,
    active_bind: Option<(String, String, bool)>,
    force_ssdp: bool,
) -> String {
    let (ssdp_status, igd_wan_ip) = if enable_upnp {
        send_ssdp_msearch(force_ssdp)
    } else {
        ("UPnP desactivado".to_string(), None)
    };

    let (bind_v4, bind_v6, real_dual_stack, real_v6_only) = match active_bind {
        Some((v4, v6, v6only)) => {
            let v6_active = v6 != "none";
            (v4, v6, v6_active, v6only)
        }
        None => {
            let (v6_supported, v6only) = probe_kernel_ipv6_v6only();
            (
                format!("0.0.0.0:{}", bedrock_port),
                if requested_v6 && v6_supported {
                    format!("[::]:{}", bedrock_port)
                } else {
                    "none".to_string()
                },
                requested_v6 && v6_supported,
                v6only,
            )
        }
    };

    let igd_wan_json = match igd_wan_ip {
        Some(ref ip) => format!(r#""{}""#, ip),
        None => "null".to_string(),
    };

    format!(
        r#"{{"bedrock_port":{},"java_port":{},"dual_stack_v6":{},"ipv6_v6only":{},"bind_v4":"{}","bind_v6":"{}","upnp":"{}","igd_wan_ip":{}}}"#,
        bedrock_port,
        java_port,
        real_dual_stack,
        real_v6_only,
        bind_v4,
        bind_v6,
        ssdp_status,
        igd_wan_json
    )
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn send_ssdp_msearch(force: bool) -> (String, Option<String>) {
    let now = now_epoch_ms();
    let prev = LAST_SSDP_PROBE_MS.load(Ordering::Relaxed);
    if !force && prev > 0 && now.saturating_sub(prev) < SSDP_COOLDOWN_MS {
        return ("ssdp_cooldown_skipped".to_string(), None);
    }
    LAST_SSDP_PROBE_MS.store(now, Ordering::Relaxed);

    let Ok(sock) = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)) else {
        return ("socket_bind_error".to_string(), None);
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(600)));
    let payload = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
    let target = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
    if sock.send_to(payload, target).is_err() {
        return ("ssdp_send_skipped".to_string(), None);
    }
    let mut buf = [0u8; 2048];
    match sock.recv_from(&mut buf) {
        Ok((n, _)) if n > 0 => {
            let text = String::from_utf8_lossy(&buf[..n]);
            let wan_ip = parse_ssdp_location(&text).and_then(|loc| query_igd_external_ip(&loc));
            ("igd_gateway_found".to_string(), wan_ip)
        }
        _ => ("no_igd_or_cgnat".to_string(), None),
    }
}

fn parse_ssdp_location(response: &str) -> Option<String> {
    for line in response.lines() {
        let trimmed = line.trim();
        if trimmed.to_ascii_lowercase().starts_with("location:") {
            return Some(trimmed["location:".len()..].trim().to_string());
        }
    }
    None
}

fn query_igd_external_ip(location_url: &str) -> Option<String> {
    let without_scheme = location_url.strip_prefix("http://")?;
    let (host_port, _) = without_scheme.split_once('/').unwrap_or((without_scheme, ""));
    let addr: SocketAddr = host_port.parse().ok()?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let soap_body = "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:GetExternalIPAddress xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\"/></s:Body></s:Envelope>";
    let req = format!(
        "POST /ctl/IPConn HTTP/1.0\r\nHost: {}\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nSOAPACTION: \"urn:schemas-upnp-org:service:WANIPConnection:1#GetExternalIPAddress\"\r\nContent-Length: {}\r\n\r\n{}",
        host_port,
        soap_body.len(),
        soap_body
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut resp = String::new();
    let _ = stream.read_to_string(&mut resp);
    let start_tag = "<NewExternalIPAddress>";
    let end_tag = "</NewExternalIPAddress>";
    let start = resp.find(start_tag)? + start_tag.len();
    let end = resp[start..].find(end_tag)?;
    let candidate = resp[start..start + end].trim();
    if candidate.parse::<Ipv4Addr>().is_ok() {
        Some(candidate.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_carrier_grade_nat_range() {
        assert!(is_cgnat_ipv4(Ipv4Addr::new(100, 64, 12, 5)));
        assert!(is_cgnat_ipv4(Ipv4Addr::new(100, 127, 255, 1)));
        assert!(!is_cgnat_ipv4(Ipv4Addr::new(192, 168, 1, 10)));
    }
}
