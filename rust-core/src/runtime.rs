use crate::net::low_latency::LowLatencyConfig;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::runtime::{Builder, Runtime};
use tokio_util::sync::CancellationToken;

#[cfg(feature = "transport-raknet")]
use tokio_raknet::protocol::reliability::Reliability;
#[cfg(feature = "transport-raknet")]
use tokio_raknet::transport::{Message, RaknetListener, RaknetListenerConfig};

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
static MAX_RUST_RAM_BYTES: AtomicU64 = AtomicU64::new(4096 * 1024 * 1024);

pub fn configured_max_ram_bytes() -> u64 {
    MAX_RUST_RAM_BYTES.load(Ordering::Relaxed)
}

fn current_process_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    #[cfg(unix)]
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE).max(4096) as u64 };
    #[cfg(not(unix))]
    let page_size = 4096u64;
    Some(rss_pages.saturating_mul(page_size))
}

fn decode_varuint32(input: &[u8]) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    let mut shift: u32 = 0;
    for (idx, &byte) in input.iter().enumerate() {
        value |= ((byte & 0x7F) as u32) << shift;
        if (byte & 0x80) == 0 {
            return Some((value, idx + 1));
        }
        shift += 7;
        if shift >= 35 {
            return None;
        }
    }
    None
}

fn encode_varuint32(mut value: u32, out: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Decodifica el primer ID de subpaquete dentro de un wrapper Batch de Bedrock (0xFE).
pub fn decode_bedrock_batch_packet_id(payload: &[u8]) -> Option<u32> {
    if payload.first().copied() != Some(0xfe) || payload.len() < 2 {
        return None;
    }
    let body = &payload[1..];
    if let Some((_subpkt_len, len_bytes)) = decode_varuint32(body) {
        if let Some(subpkt) = body.get(len_bytes..) {
            if let Some((header, _)) = decode_varuint32(subpkt) {
                return Some(header & 0x3FF);
            }
        }
    }
    Some(body[0] as u32)
}

/// Construye el paquete NetworkSettings (ID 0x8F) con compresión Zlib (0x0000) y umbral 256,
/// envuelto en un Batch de Bedrock sin compresión previa (0xFE + longitud varuint32).
pub fn build_network_settings_batch(compression_threshold: u16) -> Vec<u8> {
    let mut subpkt = Vec::with_capacity(14);
    // Packet ID 0x8F (143) en VarUInt32 ([0x8F, 0x01])
    encode_varuint32(0x8F, &mut subpkt);
    // Compression Threshold (u16 LE según el wire format de Bedrock, p.ej. 256)
    subpkt.extend_from_slice(&compression_threshold.to_le_bytes());
    // Compression Algorithm = Zlib (0x0000)
    subpkt.extend_from_slice(&0x0000u16.to_le_bytes());
    // Campos finales obligatorios en Bedrock >= 1.19.30 / 26.60 (client_throttle)
    subpkt.push(0x00); // client_throttle_enabled = false
    subpkt.push(0x00); // client_throttle_threshold = 0
    subpkt.extend_from_slice(&0.0f32.to_le_bytes()); // client_throttle_scalar = 0.0

    let mut batch = Vec::with_capacity(2 + subpkt.len());
    batch.push(0xFE);
    encode_varuint32(subpkt.len() as u32, &mut batch);
    batch.extend_from_slice(&subpkt);
    batch
}

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

fn bind_reusable_std_udp(addr: SocketAddr, only_v6: bool) -> Result<std::net::UdpSocket, String> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| format!("udp socket create error: {}", e))?;
    if addr.is_ipv6() && only_v6 {
        let _ = socket.set_only_v6(true);
    }
    socket
        .set_reuse_address(true)
        .map_err(|e| format!("udp SO_REUSEADDR error: {}", e))?;
    let _ = socket.set_broadcast(true);
    socket
        .set_nonblocking(true)
        .map_err(|e| format!("udp set_nonblocking error: {}", e))?;
    socket
        .bind(&SockAddr::from(addr))
        .map_err(|e| format!("raknet bind {} error: {}", addr, e))?;
    Ok(socket.into())
}

#[cfg(feature = "transport-raknet")]
async fn recv_optional_v6(
    sock: &Option<Arc<UdpSocket>>,
    buf: &mut [u8],
) -> std::io::Result<(usize, SocketAddr)> {
    match sock {
        Some(s) => s.recv_from(buf).await,
        None => std::future::pending().await,
    }
}

pub fn start_async_engine(
    data_dir: &str,
    server_name: &str,
    lane_name: &str,
    bedrock_port: u16,
    max_ram_mb: u32,
    net_cfg: LowLatencyConfig,
) -> Result<String, String> {
    init_android_logging();

    let mut guard = handle_slot()
        .lock()
        .map_err(|_| "runtime mutex poisoned".to_string())?;

    if guard.is_some() {
        return Ok("engine=already_running".to_string());
    }

    let budget_mb = max_ram_mb.max(512);
    let budget_bytes = (budget_mb as u64).saturating_mul(1024 * 1024);
    MAX_RUST_RAM_BYTES.store(budget_bytes, Ordering::Relaxed);

    let max_blocking = ((budget_mb / 1024).clamp(4, 32)) as usize;
    let max_peer_relays = ((budget_mb / 64) as usize).clamp(32, 512);

    let rt = Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(max_blocking)
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
        let _ = is_bedrock_profile;
        // En Android siempre escuchamos en 0.0.0.0 para aceptar tanto 127.0.0.1 como la IP LAN Wi-Fi (ej. 192.168.1.84)
        let bind_ip = "0.0.0.0";
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

        let std_public_udp = bind_reusable_std_udp(bind_addr, false)?;
        let (public_sock, public_sock_v6, mut listener, internal_addr) = rt.block_on(async move {
            let public_sock = Arc::new(
                UdpSocket::from_std(std_public_udp)
                    .map_err(|e| format!("tokio UdpSocket::from_std error: {}", e))?,
            );
            let actual_port = public_sock
                .local_addr()
                .map(|a| a.port())
                .unwrap_or(bedrock_port);
            let v6_addr = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, actual_port, 0, 0));
            let public_sock_v6 = bind_reusable_std_udp(v6_addr, true)
                .ok()
                .and_then(|std_v6| UdpSocket::from_std(std_v6).ok())
                .map(Arc::new);

            let internal_bind: SocketAddr = "127.0.0.1:0"
                .parse()
                .map_err(|e| format!("invalid internal bind addr: {}", e))?;
            let listener = RaknetListener::bind_with_config(internal_bind, raknet_cfg)
                .await
                .map_err(|e| format!("raknet listener bind error: {}", e))?;
            let internal_addr = listener.local_addr();
            Ok::<_, String>((public_sock, public_sock_v6, listener, internal_addr))
        })?;

        let actual_addr = public_sock
            .local_addr()
            .map_err(|e| format!("public_sock local_addr error: {}", e))?;
        let v6_bound_label = public_sock_v6
            .as_ref()
            .and_then(|s| s.local_addr().ok())
            .map(|a| a.to_string())
            .unwrap_or_else(|| "disabled".to_string());
        let advertised_motd = motd_string.clone();
        let lane_owned = lane_name.to_string();

        rt.spawn(async move {
            log::info!(
                "BedrockDroid RS Fase 2 listo: dir={} lane={} target={} motd_ver={} proto={} max_ram={}MB (blocking_threads={}, max_peers={}) fast_path={}",
                dir_owned,
                lane_owned,
                TARGET_BEDROCK_VERSION,
                MOTD_ADVERTISED_VERSION,
                TARGET_BEDROCK_PROTOCOL,
                budget_mb,
                max_blocking,
                max_peer_relays,
                net_cfg.local_fast_path
            );

            // Watchdog de memoria nativa (OOM Guard): evita que el motor supere el límite configurado en Android
            let mem_guard_cancel = task_cancel.child_token();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(15));
                loop {
                    tokio::select! {
                        _ = mem_guard_cancel.cancelled() => break,
                        _ = ticker.tick() => {
                            if let Some(rss) = current_process_rss_bytes() {
                                let limit = configured_max_ram_bytes();
                                if limit > 0 && rss >= (limit * 85) / 100 {
                                    log::warn!(
                                        "[MemoryGuard] RSS={} MB acerca al límite nativo {} MB (>=85%), restringiendo nuevos buffers",
                                        rss / (1024 * 1024),
                                        limit / (1024 * 1024)
                                    );
                                }
                            }
                        }
                    }
                }
            });

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
                "Servidor RakNet escuchando en {} (IPv6={}, muxer interno={})",
                actual_addr,
                v6_bound_label,
                internal_addr
            );
            log::info!(
                "Advertisement configurado ({} bytes, ServerID={}): {}",
                adv_len,
                server_id(),
                advertised_motd
            );

            let mut buf = vec![0u8; 4096];
            let mut buf_v6 = vec![0u8; 4096];
            let mut peer_relays: HashMap<SocketAddr, (Arc<UdpSocket>, CancellationToken)> =
                HashMap::new();
            let mut proxy_to_real: HashMap<SocketAddr, SocketAddr> = HashMap::new();

            log::info!("[Relay] Esperando paquetes en {}...", actual_addr);

            loop {
                let incoming: Option<(usize, SocketAddr, Arc<UdpSocket>, bool)> = tokio::select! {
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
                                Some((len, peer, Arc::clone(&public_sock), false))
                            }
                            Ok(_) => None,
                            Err(e) => {
                                log::warn!("[RakNet RX] Error en recv_from UDP IPv4: {}", e);
                                None
                            }
                        }
                    }
                    recv_v6_res = recv_optional_v6(&public_sock_v6, &mut buf_v6) => {
                        match recv_v6_res {
                            Ok((len, peer)) if len > 0 => {
                                if let Some(v6_sock) = &public_sock_v6 {
                                    Some((len, peer, Arc::clone(v6_sock), true))
                                } else {
                                    None
                                }
                            }
                            Ok(_) => None,
                            Err(e) => {
                                log::warn!("[RakNet RX] Error en recv_from UDP IPv6: {}", e);
                                None
                            }
                        }
                    }
                    maybe_stream = listener.accept() => {
                        match maybe_stream {
                            Some(mut stream) => {
                                let proxy_peer = stream.peer_addr();
                                let real_peer = proxy_to_real
                                    .get(&proxy_peer)
                                    .copied()
                                    .unwrap_or(proxy_peer);
                                log::info!(
                                    "Cliente RakNet conectado (handshake UDP completado) desde {} (proxy={})",
                                    real_peer,
                                    proxy_peer
                                );
                                // Preparación Fase 3: registrar conexión sin procesar aún el handshake de Login Bedrock.
                                log::info!("[Bedrock RX] Nuevo cliente conectado. Esperando RequestNetworkSettings...");

                                let stream_cancel = task_cancel.child_token();
                                tokio::spawn(async move {
                                    let mut has_sent_network_settings = false;
                                    loop {
                                        tokio::select! {
                                            _ = stream_cancel.cancelled() => {
                                                log::info!("[Bedrock RX] Sesión con {} cerrada por apagado del motor", real_peer);
                                                break;
                                            }
                                            maybe_pkt = stream.recv() => {
                                                match maybe_pkt {
                                                    Some(Ok(payload)) => {
                                                        if payload.first().copied() == Some(0xfe) {
                                                            if !has_sent_network_settings {
                                                                let pkt_id = decode_bedrock_batch_packet_id(&payload).unwrap_or(0);
                                                                log::info!("[Bedrock Protocol] Recibido paquete de Minecraft: 0x{:02x}", pkt_id);
                                                                if pkt_id == 0xc1 || pkt_id == 0x0b {
                                                                    let batch = build_network_settings_batch(256);
                                                                    let msg = Message::new(batch)
                                                                        .reliability(Reliability::ReliableOrdered)
                                                                        .channel(0);
                                                                    match stream.send(msg).await {
                                                                        Ok(()) => {
                                                                            has_sent_network_settings = true;
                                                                            log::info!("[Bedrock Protocol] NetworkSettings enviado a {}", real_peer);
                                                                        }
                                                                        Err(err) => {
                                                                            log::warn!("[Bedrock Protocol] Error enviando NetworkSettings a {}: {:?}", real_peer, err);
                                                                            break;
                                                                        }
                                                                    }
                                                                } else {
                                                                    log::warn!(
                                                                        "[Bedrock Protocol] Paquete 0x{:02x} ignorado de {}: NetworkSettings aún no enviado",
                                                                        pkt_id,
                                                                        real_peer
                                                                    );
                                                                }
                                                            } else {
                                                                let comp_hdr = payload.get(1).copied().unwrap_or(0);
                                                                log::info!(
                                                                    "[Bedrock Protocol] Paquete post-NetworkSettings recibido de {} ({} bytes, comp_hdr=0x{:02x})",
                                                                    real_peer,
                                                                    payload.len(),
                                                                    comp_hdr
                                                                );
                                                            }
                                                        } else {
                                                            let raw_id = payload.first().copied().unwrap_or(0);
                                                            if !has_sent_network_settings {
                                                                log::warn!(
                                                                    "[Bedrock Protocol] Paquete 0x{:02x} ignorado de {}: NetworkSettings aún no enviado",
                                                                    raw_id,
                                                                    real_peer
                                                                );
                                                            } else {
                                                                log::info!("[Bedrock Protocol] Recibido paquete de Minecraft: 0x{:02x}", raw_id);
                                                            }
                                                        }
                                                    }
                                                    Some(Err(err)) => {
                                                        log::warn!("[Bedrock RX] Error leyendo stream RakNet de {}: {:?}", real_peer, err);
                                                        break;
                                                    }
                                                    None => {
                                                        log::info!("[Bedrock RX] Cliente {} desconectado", real_peer);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                });
                            }
                            None => {
                                log::warn!("Listener RakNet cerrado inesperadamente");
                                break;
                            }
                        }
                        None
                    }
                };

                if let Some((len, peer, pub_tx_sock, is_v6)) = incoming {
                    let packet_slice = if is_v6 { &buf_v6[..len] } else { &buf[..len] };
                    let pkt_id = packet_slice[0];
                    log::info!(
                        "[Relay RX] Paquete recibido de {} ({} bytes, primer byte=0x{:02x})",
                        peer,
                        len,
                        pkt_id
                    );
                    if matches!(pkt_id, 0x01 | 0x02 | 0x05 | 0x07) {
                        log::info!(
                            "[RakNet RX] Paquete no conectado id=0x{:02x} ({} bytes) desde {}",
                            pkt_id,
                            len,
                            peer
                        );
                    }

                    if !peer_relays.contains_key(&peer) {
                        if let Some(rss) = current_process_rss_bytes() {
                            if rss >= (budget_bytes * 95) / 100 {
                                log::warn!(
                                    "[MemoryGuard] RSS={} MB >= 95% del límite ({} MB); rechazando nuevo relay para {}",
                                    rss / (1024 * 1024),
                                    budget_mb,
                                    peer
                                );
                                continue;
                            }
                        }
                        if peer_relays.len() >= max_peer_relays {
                            for (_, (_, old_cancel)) in peer_relays.drain() {
                                old_cancel.cancel();
                            }
                            proxy_to_real.clear();
                        }

                        match UdpSocket::bind("127.0.0.1:0").await {
                            Ok(relay) => {
                                match (relay.connect(internal_addr).await, relay.local_addr()) {
                                    (Ok(()), Ok(proxy_addr)) => {
                                        let relay_arc = Arc::new(relay);
                                        let relay_rx = Arc::clone(&relay_arc);
                                        let pub_tx = pub_tx_sock;
                                        let peer_cancel = task_cancel.child_token();
                                        let peer_cancel_task = peer_cancel.clone();

                                        peer_relays.insert(peer, (relay_arc, peer_cancel));
                                        proxy_to_real.insert(proxy_addr, peer);

                                        tokio::spawn(async move {
                                            let mut rbuf = [0u8; 4096];
                                            loop {
                                                tokio::select! {
                                                    _ = peer_cancel_task.cancelled() => break,
                                                    res = relay_rx.recv(&mut rbuf) => {
                                                        match res {
                                                            Ok(n) if n > 0 => {
                                                                log::info!(
                                                                    "[Relay TX] Respuesta recibida del muxer ({} bytes, primer byte=0x{:02x}) -> enviando a {}",
                                                                    n,
                                                                    rbuf[0],
                                                                    peer
                                                                );
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
                                                                    log::warn!("[Relay TX] Error enviando UDP a {}: {}", peer, e);
                                                                }
                                                            }
                                                            Ok(_) => {}
                                                            Err(e) => {
                                                                log::warn!("[Relay TX] Error leyendo relay_rx para {}: {}", peer, e);
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        });
                                    }
                                    (Err(e), _) => {
                                        log::warn!("[Relay RX] Error en relay.connect({}) para {}: {}", internal_addr, peer, e);
                                    }
                                    (_, Err(e)) => {
                                        log::warn!("[Relay RX] Error obteniendo local_addr del relay para {}: {}", peer, e);
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!("[RakNet Relay] No se pudo crear socket relay para {}: {}", peer, e);
                            }
                        }
                    }

                    if let Some((relay_sock, _)) = peer_relays.get(&peer) {
                        if let Err(e) = relay_sock.send(packet_slice).await {
                            log::warn!("[RakNet Relay] Error enviando paquete a muxer para {}: {}", peer, e);
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
                "BedrockDroid RS listo (sin feature transport-raknet): dir={} max_ram={}MB proto={}",
                dir_owned,
                max_ram_mb,
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
    fn decodes_bedrock_batch_request_network_settings() {
        // 0xFE (Bedrock Game Packet Batch) + varint len (5) + varint pkt_id (0xc1 = RequestNetworkSettings) + proto 2225
        let sample = [0xfe, 0x05, 0xc1, 0x01, 0x00, 0x00, 0x08, 0xb1];
        assert_eq!(decode_bedrock_batch_packet_id(&sample), Some(0xc1));
        assert_eq!(decode_bedrock_batch_packet_id(&[0x01, 0x02]), None);
    }

    #[test]
    fn encodes_bedrock_network_settings_batch() {
        let batch = build_network_settings_batch(256);
        assert_eq!(batch[0], 0xFE);
        assert_eq!(batch[1], 12);
        assert_eq!(decode_bedrock_batch_packet_id(&batch), Some(0x8F));
        assert_eq!(&batch[2..4], &[0x8F, 0x01]);
        assert_eq!(&batch[4..6], &256u16.to_le_bytes());
        assert_eq!(&batch[6..8], &0x0000u16.to_le_bytes());
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
        let status = start_async_engine("/tmp/bedrock_test", "BedrockDroid RS", "BEDROCK", 0, 8192, cfg)
            .expect("start failed");
        assert!(status.starts_with("engine=running raknet=0.0.0.0:"));
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
            let target_local = format!("127.0.0.1:{}", port);
            client.send_to(&ping, &target_local).expect("send ping");

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
