use crate::net::low_latency::LowLatencyConfig;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::runtime::{Builder, Runtime};
use tokio_util::sync::CancellationToken;

#[cfg(feature = "transport-raknet")]
use tokio_raknet::transport::{RaknetListener, RaknetListenerConfig};

pub const TARGET_BEDROCK_VERSION: &str = "26.60";
pub const TARGET_BEDROCK_PROTOCOL: u32 = 2225;
/// Versión semántica interna que espera bedrock-rs (V2225::GAME_VERSION) y clientes Android 26.60/26.62 en el MOTD.
pub const MOTD_ADVERTISED_VERSION: &str = "1.26.60";

struct ServerHandle {
    rt: Runtime,
    cancel: CancellationToken,
}

static LOGGING_INIT: OnceLock<()> = OnceLock::new();
static SERVER_HANDLE: OnceLock<Mutex<Option<ServerHandle>>> = OnceLock::new();
static SERVER_GUID: OnceLock<u64> = OnceLock::new();

fn handle_slot() -> &'static Mutex<Option<ServerHandle>> {
    SERVER_HANDLE.get_or_init(|| Mutex::new(None))
}

/// Genera un ServerID positivo de 64 bits (bit de signo en 0: 1..=i64::MAX) estático durante la sesión.
pub fn server_id() -> u64 {
    *SERVER_GUID.get_or_init(|| {
        use std::hash::BuildHasher;
        let raw: u64 = std::collections::hash_map::RandomState::new()
            .hash_one(std::process::id()) as u64;
        let positive = raw & (i64::MAX as u64);
        if positive == 0 {
            0x5241_4B4E_4554_0001u64
        } else {
            positive
        }
    })
}

/// Construye el string MOTD estándar de Minecraft Bedrock para Unconnected Pong (0x1c),
/// sin comillas extra ni caracteres nulos.
pub fn build_bedrock_advertisement(motd_name: &str, ipv4_port: u16) -> String {
    let clean_name: String = motd_name
        .chars()
        .filter(|&c| c != ';' && c != '"' && c != '\0' && c != '\r' && c != '\n')
        .collect();
    let name = if clean_name.trim().is_empty() {
        "BedrockDroid RS"
    } else {
        clean_name.trim()
    };
    let effective_port = if ipv4_port == 0 { 19132 } else { ipv4_port };
    let ipv6_port = effective_port.saturating_add(1);
    let sid = format!("{}", server_id());
    format!(
        "MCPE;{};{};{};0;10;{};Bedrock World;Survival;1;{};{};",
        name,
        TARGET_BEDROCK_PROTOCOL,
        MOTD_ADVERTISED_VERSION,
        sid,
        effective_port,
        ipv6_port
    )
}

/// Inicializa logcat nativo y registra panic_hook exactamente una sola vez.
pub fn init_android_logging() {
    LOGGING_INIT.get_or_init(|| {
        #[cfg(target_os = "android")]
        {
            android_logger::init_once(
                android_logger::Config::default()
                    .with_max_level(log::LevelFilter::Info)
                    .with_tag("BedrockDroidRS"),
            );
        }

        std::panic::set_hook(Box::new(|info| {
            log::error!("PANIC nativo en bedrock_android_core: {}", info);
        }));
    });
}

fn bind_reusable_std_udp(addr: SocketAddr) -> Result<std::net::UdpSocket, String> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| format!("udp socket create error: {}", e))?;
    socket
        .set_reuse_address(true)
        .map_err(|e| format!("udp SO_REUSEADDR error: {}", e))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| format!("udp set_nonblocking error: {}", e))?;
    socket
        .bind(&SockAddr::from(addr))
        .map_err(|e| format!("raknet bind {} error: {}", addr, e))?;
    Ok(socket.into())
}

pub fn start_async_engine(
    data_dir: &str,
    server_name: &str,
    lane_name: &str,
    bedrock_port: u16,
    net_cfg: LowLatencyConfig,
) -> Result<String, String> {
    init_android_logging();

    let mut guard = handle_slot()
        .lock()
        .map_err(|_| "runtime mutex poisoned".to_string())?;

    if guard.is_some() {
        return Ok("engine=already_running".to_string());
    }

    let rt = Builder::new_multi_thread()
        .worker_threads(4)
        .thread_name("bdr-tokio-worker")
        .enable_all()
        .build()
        .map_err(|e| format!("tokio init error: {}", e))?;

    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let dir_owned = data_dir.to_string();
    let motd_string = build_bedrock_advertisement(server_name, bedrock_port);
    let is_bedrock_profile = lane_name.trim().eq_ignore_ascii_case("bedrock");

    #[cfg(feature = "transport-raknet")]
    let bound_summary: String = {
        let bind_ip = if is_bedrock_profile && net_cfg.local_fast_path {
            "127.0.0.1"
        } else {
            "0.0.0.0"
        };
        let bind_addr: SocketAddr = format!("{}:{}", bind_ip, bedrock_port)
            .parse()
            .map_err(|e| format!("invalid bind addr: {}", e))?;

        let adv_bytes = motd_string.as_bytes().to_vec();
        let adv_len = adv_bytes.len();

        let raknet_cfg = RaknetListenerConfig {
            max_mtu: net_cfg.wan_mtu_bytes.clamp(576, 1492),
            advertisement: adv_bytes,
            ..RaknetListenerConfig::default()
        };

        let std_public_udp = bind_reusable_std_udp(bind_addr)?;
        let (public_sock, mut listener, internal_addr) = rt.block_on(async move {
            let public_sock = Arc::new(
                UdpSocket::from_std(std_public_udp)
                    .map_err(|e| format!("tokio UdpSocket::from_std error: {}", e))?,
            );
            let internal_bind: SocketAddr = "127.0.0.1:0"
                .parse()
                .map_err(|e| format!("invalid internal bind addr: {}", e))?;
            let listener = RaknetListener::bind_with_config(internal_bind, raknet_cfg)
                .await
                .map_err(|e| format!("raknet listener bind error: {}", e))?;
            let internal_addr = listener.local_addr();
            Ok::<_, String>((public_sock, listener, internal_addr))
        })?;

        let actual_addr = public_sock
            .local_addr()
            .map_err(|e| format!("public_sock local_addr error: {}", e))?;
        let advertised_motd = motd_string.clone();
        let lane_owned = lane_name.to_string();

        rt.spawn(async move {
            log::info!(
                "BedrockDroid RS Fase 2 listo: dir={} lane={} target={} motd_ver={} proto={} fast_path={}",
                dir_owned,
                lane_owned,
                TARGET_BEDROCK_VERSION,
                MOTD_ADVERTISED_VERSION,
                TARGET_BEDROCK_PROTOCOL,
                net_cfg.local_fast_path
            );

            #[cfg(feature = "protocol-core")]
            {
                use bedrock::protocol::{ProtoVersion, V2225};
                log::info!(
                    "Feature activa: protocol-core (bedrock-rs V2225 proto={} game={})",
                    V2225::PROTOCOL_VERSION,
                    V2225::GAME_VERSION
                );
            }

            log::info!(
                "Servidor RakNet escuchando en {} (muxer interno={})",
                actual_addr,
                internal_addr
            );
            log::info!(
                "Advertisement configurado ({} bytes, ServerID={}): {}",
                adv_len,
                server_id(),
                advertised_motd
            );

            let mut buf = vec![0u8; 2048];
            let mut peer_relays: HashMap<SocketAddr, (Arc<UdpSocket>, CancellationToken)> =
                HashMap::new();
            let mut proxy_to_real: HashMap<SocketAddr, SocketAddr> = HashMap::new();

            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => {
                        log::info!("Listener RakNet cancelado por CancellationToken");
                        for (_, (_, peer_cancel)) in peer_relays.drain() {
                            peer_cancel.cancel();
                        }
                        break;
                    }
                    recv_res = public_sock.recv_from(&mut buf) => {
                        match recv_res {
                            Ok((len, peer)) if len > 0 => {
                                let pkt_id = buf[0];
                                if matches!(pkt_id, 0x01 | 0x02 | 0x05 | 0x07) {
                                    log::info!(
                                        "[RakNet RX] Paquete no conectado id=0x{:02x} ({} bytes) desde {}",
                                        pkt_id,
                                        len,
                                        peer
                                    );
                                }

                                if !peer_relays.contains_key(&peer) {
                                    if peer_relays.len() >= 128 {
                                        for (_, (_, old_cancel)) in peer_relays.drain() {
                                            old_cancel.cancel();
                                        }
                                        proxy_to_real.clear();
                                    }

                                    match UdpSocket::bind("127.0.0.1:0").await {
                                        Ok(relay) => {
                                            if let (Ok(()), Ok(proxy_addr)) =
                                                (relay.connect(internal_addr).await, relay.local_addr())
                                            {
                                                let relay_arc = Arc::new(relay);
                                                let relay_rx = Arc::clone(&relay_arc);
                                                let pub_tx = Arc::clone(&public_sock);
                                                let peer_cancel = task_cancel.child_token();
                                                let peer_cancel_task = peer_cancel.clone();

                                                peer_relays.insert(peer, (relay_arc, peer_cancel));
                                                proxy_to_real.insert(proxy_addr, peer);

                                                tokio::spawn(async move {
                                                    let mut rbuf = [0u8; 2048];
                                                    loop {
                                                        tokio::select! {
                                                            _ = peer_cancel_task.cancelled() => break,
                                                            res = relay_rx.recv(&mut rbuf) => {
                                                                match res {
                                                                    Ok(n) if n > 0 => {
                                                                        if rbuf[0] == 0x1c {
                                                                            log::info!(
                                                                                "[RakNet TX] UnconnectedPong (0x1c, {} bytes) enviado a {}",
                                                                                n,
                                                                                peer
                                                                            );
                                                                        } else if matches!(rbuf[0], 0x06 | 0x08 | 0x19) {
                                                                            log::info!(
                                                                                "[RakNet TX] Respuesta offline id=0x{:02x} ({} bytes) -> {}",
                                                                                rbuf[0],
                                                                                n,
                                                                                peer
                                                                            );
                                                                        }
                                                                        if let Err(e) = pub_tx.send_to(&rbuf[..n], peer).await {
                                                                            log::warn!("[RakNet TX] Error enviando UDP a {}: {}", peer, e);
                                                                        }
                                                                    }
                                                                    Ok(_) => {}
                                                                    Err(_) => break,
                                                                }
                                                            }
                                                        }
                                                    }
                                                });
                                            }
                                        }
                                        Err(e) => {
                                            log::warn!("[RakNet Relay] No se pudo crear socket relay para {}: {}", peer, e);
                                        }
                                    }
                                }

                                if let Some((relay_sock, _)) = peer_relays.get(&peer) {
                                    if let Err(e) = relay_sock.send(&buf[..len]).await {
                                        log::warn!("[RakNet Relay] Error enviando paquete a muxer para {}: {}", peer, e);
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                log::warn!("[RakNet RX] Error en recv_from UDP: {}", e);
                            }
                        }
                    }
                    maybe_stream = listener.accept() => {
                        match maybe_stream {
                            Some(stream) => {
                                let proxy_peer = stream.peer_addr();
                                let real_peer = proxy_to_real
                                    .get(&proxy_peer)
                                    .copied()
                                    .unwrap_or(proxy_peer);
                                // Fase 2: Solo Ping/Pong y conexión RakNet base; aún no se procesa Login Handshake de Bedrock.
                                log::info!(
                                    "Cliente RakNet conectado (handshake UDP completado) desde {} (proxy={})",
                                    real_peer,
                                    proxy_peer
                                );
                            }
                            None => {
                                log::warn!("Listener RakNet cerrado inesperadamente");
                                break;
                            }
                        }
                    }
                }
            }
        });

        format!("engine=running raknet={}", actual_addr)
    };

    #[cfg(not(feature = "transport-raknet"))]
    let bound_summary: String = {
        let _ = is_bedrock_profile;
        rt.spawn(async move {
            log::info!(
                "BedrockDroid RS listo (sin feature transport-raknet): dir={} proto={}",
                dir_owned,
                TARGET_BEDROCK_PROTOCOL
            );
            task_cancel.cancelled().await;
            log::info!("Runtime Tokio cancelado ordenadamente");
        });
        "engine=running raknet=disabled".to_string()
    };

    *guard = Some(ServerHandle { rt, cancel });
    Ok(bound_summary)
}

pub fn stop_async_engine() {
    let taken = handle_slot()
        .lock()
        .ok()
        .and_then(|mut guard| guard.take());

    if let Some(handle) = taken {
        handle.cancel.cancel();
        // Límite de 2 segundos para asegurar liberación completa del socket UDP entre reinicios rápidos.
        handle.rt.shutdown_timeout(Duration::from_secs(2));
        log::info!("bedrock_android_core runtime detenido y socket RakNet liberado");
    }
}

pub fn is_engine_running() -> bool {
    handle_slot()
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

/// Envía un paquete UDP Unconnected Ping (0x01) a 127.0.0.1:<port> con el Magic Number de RakNet
/// y un Client GUID aleatorio, esperando Unconnected Pong (0x1c) con un timeout de 2 segundos.
pub fn send_ping_command(bedrock_port: u16) -> String {
    use std::hash::BuildHasher;
    use std::net::UdpSocket as StdUdpSocket;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    const RAKNET_MAGIC: [u8; 16] = [
        0x00, 0xFF, 0xFF, 0x00, 0xFE, 0xFE, 0xFE, 0xFE, 0xFD, 0xFD, 0xFD, 0xFD, 0x12, 0x34, 0x56,
        0x78,
    ];

    let target_port = if bedrock_port == 0 { 19132 } else { bedrock_port };
    let target_addr = format!("127.0.0.1:{}", target_port);

    let socket = match StdUdpSocket::bind("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("[PING] Error creando socket UDP local: {}", e);
            log::warn!("{}", msg);
            return msg;
        }
    };

    let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = socket.set_write_timeout(Some(Duration::from_secs(2)));

    let ping_time: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1);

    let raw_guid = std::collections::hash_map::RandomState::new().hash_one(ping_time) as u64;
    let client_guid: u64 = (raw_guid & (i64::MAX as u64)).max(1);

    let mut packet = Vec::with_capacity(33);
    packet.push(0x01u8); // Unconnected Ping
    packet.extend_from_slice(&ping_time.to_be_bytes());
    packet.extend_from_slice(&RAKNET_MAGIC);
    packet.extend_from_slice(&client_guid.to_be_bytes());

    let started = Instant::now();
    if let Err(e) = socket.send_to(&packet, &target_addr) {
        let msg = format!("[PING] Error enviando Unconnected Ping a {}: {}", target_addr, e);
        log::warn!("{}", msg);
        return msg;
    }

    let mut resp = [0u8; 2048];
    match socket.recv_from(&mut resp) {
        Ok((n, _from)) if n > 35 && resp[0] == 0x1c && resp[17..33] == RAKNET_MAGIC => {
            let rtt_ms = started.elapsed().as_secs_f64() * 1000.0;
            let motd_len = u16::from_be_bytes([resp[33], resp[34]]) as usize;
            let end = 35usize.saturating_add(motd_len).min(n);
            let motd = String::from_utf8_lossy(&resp[35..end]).to_string();
            log::info!("[PING] Servidor responde ({:.2} ms): {}", rtt_ms, motd);
            format!("[PING] Servidor responde ({:.2} ms): {}", rtt_ms, motd)
        }
        Ok((n, from)) => {
            log::warn!("[PING] Respuesta inesperada desde {} ({} bytes, id=0x{:02x})", from, n, resp[0]);
            "[PING] Sin respuesta del servidor (timeout).".to_string()
        }
        Err(_) => {
            log::warn!("[PING] Sin respuesta del servidor (timeout).");
            "[PING] Sin respuesta del servidor (timeout).".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket as StdUdpSocket;

    #[test]
    fn motd_advertisement_matches_minecraft_bedrock_format() {
        let motd = build_bedrock_advertisement("BedrockDroid RS\0\"Test\"", 19132);
        let sid = server_id();
        assert!(sid > 0 && sid <= i64::MAX as u64);
        assert!(!motd.contains('"'));
        assert!(!motd.contains('\0'));
        let expected = format!(
            "MCPE;BedrockDroid RSTest;{};{};0;10;{};Bedrock World;Survival;1;19132;19133;",
            TARGET_BEDROCK_PROTOCOL,
            MOTD_ADVERTISED_VERSION,
            sid
        );
        assert_eq!(motd, expected);
    }

    #[test]
    fn runtime_engine_starts_and_stops_cleanly() {
        let cfg = LowLatencyConfig {
            local_fast_path: true,
            tcp_no_delay: true,
            ip_tos_low_delay: true,
            local_jitter_ms: 0,
            wan_mtu_bytes: 1400,
        };
        let status = start_async_engine("/tmp/bedrock_test", "BedrockDroid RS", "BEDROCK", 0, cfg)
            .expect("start failed");
        assert!(status.starts_with("engine=running raknet=127.0.0.1:"));
        assert!(is_engine_running());

        if let Some(addr_str) = status.split("raknet=").nth(1) {
            let port: u16 = addr_str
                .rsplit(':')
                .next()
                .and_then(|p| p.parse().ok())
                .expect("valid bound port");
            let ping_res = send_ping_command(port);
            assert!(ping_res.contains("[PING] Servidor responde"));
            assert!(ping_res.contains("MCPE;BedrockDroid RS;"));

            let client = StdUdpSocket::bind("127.0.0.1:0").expect("client bind");
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("set_read_timeout");
            let mut ping = Vec::with_capacity(33);
            ping.push(0x01u8);
            ping.extend_from_slice(&12345678u64.to_be_bytes());
            ping.extend_from_slice(&[
                0x00, 0xFF, 0xFF, 0x00, 0xFE, 0xFE, 0xFE, 0xFE, 0xFD, 0xFD, 0xFD, 0xFD, 0x12, 0x34,
                0x56, 0x78,
            ]);
            ping.extend_from_slice(&999u64.to_be_bytes());
            client.send_to(&ping, addr_str).expect("send ping");

            let mut resp = [0u8; 2048];
            let (n, _) = client.recv_from(&mut resp).expect("recv pong");
            assert_eq!(resp[0], 0x1c);
            assert!(n > 35);
            let motd_len = u16::from_be_bytes([resp[33], resp[34]]) as usize;
            let motd = std::str::from_utf8(&resp[35..35 + motd_len]).expect("utf8 motd");
            assert!(motd.starts_with("MCPE;BedrockDroid RS;"));
        }

        stop_async_engine();
        assert!(!is_engine_running());
    }
}
