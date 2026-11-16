use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

pub fn is_cgnat_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000
}

pub fn probe_local_and_wan(
    bedrock_port: u16,
    java_port: u16,
    enable_upnp: bool,
    dual_stack_ipv6: bool,
) -> String {
    let ssdp_status = if enable_upnp {
        send_ssdp_msearch()
    } else {
        "UPnP desactivado".to_string()
    };

    format!(
        r#"{{"bedrock_port":{},"java_port":{},"dual_stack_v6":{},"upnp":"{}"}}"#,
        bedrock_port, java_port, dual_stack_ipv6, ssdp_status
    )
}

fn send_ssdp_msearch() -> String {
    let Ok(sock) = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)) else {
        return "socket_bind_error".to_string();
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(600)));
    let payload = b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n";
    let target = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
    if sock.send_to(payload, target).is_err() {
        return "ssdp_send_skipped".to_string();
    }
    let mut buf = [0u8; 1024];
    match sock.recv_from(&mut buf) {
        Ok((n, _)) if n > 0 => "igd_gateway_found".to_string(),
        _ => "no_igd_or_cgnat".to_string(),
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
