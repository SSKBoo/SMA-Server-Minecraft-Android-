use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, SocketAddr};

#[derive(Debug, Clone, Copy)]
pub struct LowLatencyConfig {
    pub local_fast_path: bool,
    pub tcp_no_delay: bool,
    pub ip_tos_low_delay: bool,
    pub local_jitter_ms: u32,
    pub wan_mtu_bytes: u16,
}

pub fn is_local_or_lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

pub fn effective_jitter_ms(peer: SocketAddr, cfg: &LowLatencyConfig) -> u32 {
    if cfg.local_fast_path && is_local_or_lan(peer.ip()) {
        0
    } else {
        cfg.local_jitter_ms.max(8)
    }
}

pub fn configure_low_latency_tcp_socket(addr: SocketAddr, cfg: &LowLatencyConfig) -> std::io::Result<Socket> {
    let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_nodelay(cfg.tcp_no_delay)?;
    if cfg.ip_tos_low_delay && addr.is_ipv4() {
        let _ = socket.set_tos(0x10);
    }
    socket.bind(&addr.into())?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddrV4};

    #[test]
    fn loopback_bypasses_jitter() {
        let cfg = LowLatencyConfig {
            local_fast_path: true,
            tcp_no_delay: true,
            ip_tos_low_delay: true,
            local_jitter_ms: 0,
            wan_mtu_bytes: 1400,
        };
        let local_peer = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 19132));
        let wan_peer = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 19132));
        assert_eq!(effective_jitter_ms(local_peer, &cfg), 0);
        assert_eq!(effective_jitter_ms(wan_peer, &cfg), 8);
    }
}
