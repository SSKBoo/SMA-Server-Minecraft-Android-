use crate::net::low_latency::LowLatencyConfig;
use acacia_nethernet::{
    http,
    lan::{DISCOVERY_PORT, LanPacket, ServerData},
    Connection, Event as RtcEvent, Signal, SignalKind, PROBE_PATH, join_path,
};
use acacia_session::proto::{
    encode_packet, packet_name,
    packets::{
        ClientToServerHandshake, Login, NetworkSettings, NetworkSettingsCompressionAlgorithm,
        PlayStatus, PlayStatusStatus, RequestNetworkSettings, ServerToClientHandshake,
    },
    Packet, RawPacket,
};
use acacia_session::server::{read_login, ServerConnection};
use bytes::{Bytes, BytesMut};
use flate2::read::{DeflateDecoder, ZlibDecoder};
use p384::ecdsa::SigningKey;
use rand_core::OsRng;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream as StdTcpStream, UdpSocket as StdUdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::runtime::{Builder, Runtime};
use tokio_util::sync::CancellationToken;

pub const TARGET_BEDROCK_VERSION: &str = "26.60";
pub const TARGET_BEDROCK_PROTOCOL: u32 = 2225;
/// Versión semántica interna que espera bedrock-rs (V2225::GAME_VERSION) y clientes Android 26.60/26.62 en el MOTD.
pub const MOTD_ADVERTISED_VERSION: &str = "1.26.60";

/// Eventos de alto nivel de una conexión WebRTC NetherNet (acacia-nethernet).
#[derive(Debug, Clone)]
pub enum Event {
    Connected,
    Data(Bytes),
    Disconnected(String),
}

/// Extrae el siguiente evento de ciclo de vida o paquete de datos SCTP desde `acacia_nethernet::Connection`.
pub fn poll_connection_event(conn: &mut Connection) -> Option<Event> {
    if let Some(rtc_ev) = conn.poll_event() {
        return Some(match rtc_ev {
            RtcEvent::Open => Event::Connected,
            RtcEvent::Closed(reason) => Event::Disconnected(reason),
        });
    }
    conn.poll_message().map(Event::Data)
}

struct PeerSession {
    conn: Connection,
    session: ServerConnection,
    remote_addr: SocketAddr,
    has_sent_network_settings: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineState {
    Stopped,
    Running,
}

/// Arena nativa de tamaño fijo asignada con `std::alloc` al arrancar el servidor.
/// No implementa `realloc`: el tamaño queda bloqueado hasta detener y reiniciar el runtime.
struct FixedNativeArena {
    ptr: *mut u8,
    layout: Layout,
    budget_bytes: u64,
    blocking_threads: usize,
}

unsafe impl Send for FixedNativeArena {}
unsafe impl Sync for FixedNativeArena {}

impl FixedNativeArena {
    fn allocate_fixed(max_ram_mb: u32, blocking_threads: usize) -> Result<Self, String> {
        let budget_mb = max_ram_mb.max(512);
        let budget_bytes = (budget_mb as u64).saturating_mul(1024 * 1024);
        let slab_bytes = ((budget_mb as usize).clamp(512, 16384) / 512) * 64 * 1024;
        let layout = Layout::from_size_align(slab_bytes, 4096)
            .map_err(|e| format!("invalid std::alloc layout: {}", e))?;
        // Safety: `slab_bytes` >= 64 KiB y alineado a página de 4096 bytes.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err(format!("std::alloc failed for {} bytes", slab_bytes));
        }
        Ok(Self {
            ptr,
            layout,
            budget_bytes,
            blocking_threads,
        })
    }

    fn summary(&self) -> (usize, u64, usize) {
        (self.layout.size(), self.budget_bytes, self.blocking_threads)
    }
}

impl Drop for FixedNativeArena {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // Safety: `self.ptr` fue asignado con el mismo `self.layout` en `allocate_fixed`.
            unsafe { dealloc(self.ptr, self.layout) };
            self.ptr = std::ptr::null_mut();
        }
    }
}

struct ServerHandle {
    rt: Runtime,
    cancel: CancellationToken,
    arena: FixedNativeArena,
}

#[derive(Debug, Clone)]
pub struct BindReport {
    pub bind_v4: String,
    pub bind_v6: String,
    pub ipv6_v6only: bool,
    pub dual_stack_active: bool,
}

static LOGGING_INIT: OnceLock<()> = OnceLock::new();
static SERVER_HANDLE: OnceLock<Mutex<Option<ServerHandle>>> = OnceLock::new();
static BIND_REPORT: OnceLock<Mutex<Option<BindReport>>> = OnceLock::new();
static SERVER_GUID: OnceLock<u64> = OnceLock::new();
static MAX_RUST_RAM_BYTES: AtomicU64 = AtomicU64::new(4096 * 1024 * 1024);

pub fn current_bind_report() -> Option<BindReport> {
    let cell = BIND_REPORT.get_or_init(|| Mutex::new(None));
    cell.lock().ok().and_then(|g| g.clone())
}

fn set_current_bind_report(report: Option<BindReport>) {
    let cell = BIND_REPORT.get_or_init(|| Mutex::new(None));
    if let Ok(mut g) = cell.lock() {
        *g = report;
    }
}

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

/// Divide el contenido descomprimido o raw de un Batch Bedrock (`[varint len][subpkt]...`) en subpaquetes.
pub fn split_batch_subpackets(data: &[u8]) -> Result<Vec<Bytes>, String> {
    if data.is_empty() {
        return Err("malformed batch framing: empty batch payload".to_string());
    }
    let mut subpackets = Vec::new();
    let mut offset = 0usize;
    while offset < data.len() {
        let slice = &data[offset..];
        let (pkt_len, len_bytes) = decode_varuint32(slice).ok_or_else(|| {
            format!("malformed batch framing: invalid subpacket varint at offset {}", offset)
        })?;
        let pkt_len = pkt_len as usize;
        let start = offset.saturating_add(len_bytes);
        let end = start.saturating_add(pkt_len);
        if pkt_len == 0 || end > data.len() {
            return Err(format!(
                "malformed batch framing: subpacket_len={} exceeds remaining={}",
                pkt_len,
                data.len().saturating_sub(start)
            ));
        }
        subpackets.push(Bytes::copy_from_slice(&data[start..end]));
        offset = end;
    }
    Ok(subpackets)
}

fn decompress_zlib_payload(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = ZlibDecoder::new(input);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("zlib decode error: {}", e))?;
    Ok(out)
}

fn decompress_deflate_payload(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = DeflateDecoder::new(input);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("deflate decode error: {}", e))?;
    Ok(out)
}

/// Decodifica un Batch Bedrock evaluando el header de compresión de 1 byte:
/// - `0x00` = raw (sin comprimir) o algoritmo Zlib en framing 1.20.60+ si continúa con `0x78`
/// - `0x78` = Zlib/Deflate vía `flate2::read::ZlibDecoder`
/// - `0xFF` = sin compresión explícita
pub fn decode_bedrock_batch_with_header(payload: &[u8]) -> Result<Vec<Bytes>, String> {
    if payload.is_empty() {
        return Err("malformed batch framing: empty payload".to_string());
    }
    let body = if payload.first().copied() == Some(0xfe) && payload.len() > 1 {
        &payload[1..]
    } else {
        payload
    };

    match body[0] {
        0x78 => {
            if let Ok(decompressed) = decompress_zlib_payload(body) {
                if let Ok(pkts) = split_batch_subpackets(&decompressed) {
                    return Ok(pkts);
                }
            }
            if body.len() > 1 {
                if let Ok(decompressed) = decompress_zlib_payload(&body[1..]) {
                    if let Ok(pkts) = split_batch_subpackets(&decompressed) {
                        return Ok(pkts);
                    }
                }
                if let Ok(decompressed) = decompress_deflate_payload(&body[1..]) {
                    if let Ok(pkts) = split_batch_subpackets(&decompressed) {
                        return Ok(pkts);
                    }
                }
            }
            Err(format!(
                "malformed batch framing: failed to decode 0x78 zlib batch ({} bytes)",
                body.len()
            ))
        }
        0x00 => {
            let rest = &body[1..];
            if let Ok(pkts) = split_batch_subpackets(rest) {
                return Ok(pkts);
            }
            if rest.first().copied() == Some(0x78) {
                if let Ok(decompressed) = decompress_zlib_payload(rest) {
                    if let Ok(pkts) = split_batch_subpackets(&decompressed) {
                        return Ok(pkts);
                    }
                }
            }
            if let Ok(decompressed) = decompress_deflate_payload(rest) {
                if let Ok(pkts) = split_batch_subpackets(&decompressed) {
                    return Ok(pkts);
                }
            }
            Err(format!(
                "malformed batch framing: failed to decode 0x00 batch ({} bytes)",
                body.len()
            ))
        }
        0xff => split_batch_subpackets(&body[1..]),
        _ => split_batch_subpackets(body),
    }
}

/// Decodifica batches usando `acacia_session` y recurre al decoder explícito de cabecera `0x00` / `0x78`
/// cuando el cliente envía framing Zlib/raw directo que produce `malformed batch framing`.
pub fn decode_bedrock_batch_frames(
    session: &mut ServerConnection,
    payload: &[u8],
) -> Result<Vec<Bytes>, String> {
    let normalized = if payload.first().copied() == Some(0xfe) && payload.len() > 1 {
        &payload[1..]
    } else {
        payload
    };

    match session.decode(normalized) {
        Ok(pkts) => Ok(pkts),
        Err(session_err) => match decode_bedrock_batch_with_header(payload) {
            Ok(pkts) => Ok(pkts),
            Err(header_err) => Err(format!("{} | {}", session_err, header_err)),
        },
    }
}

/// Decodifica el primer ID de subpaquete dentro de un wrapper Batch estándar de Bedrock (cabecera obligatoria 0xFE).
pub fn decode_bedrock_batch_packet_id(payload: &[u8]) -> Option<u32> {
    if payload.first().copied() != Some(0xfe) || payload.len() < 2 {
        return None;
    }
    decode_nethernet_batch_packet_id(&payload[1..])
}

/// Decodifica el primer ID de subpaquete dentro de un Batch sin cabecera 0xFE (formato NetherNet WebRTC).
pub fn decode_nethernet_batch_packet_id(body: &[u8]) -> Option<u32> {
    if body.is_empty() {
        return None;
    }
    if let Some((subpkt_len, len_bytes)) = decode_varuint32(body) {
        if subpkt_len > 0 && body.len() >= len_bytes.saturating_add(subpkt_len as usize) {
            if let Some(subpkt) = body.get(len_bytes..) {
                if let Some((header, _)) = decode_varuint32(subpkt) {
                    return Some(header & 0x3FF);
                }
            }
        }
    }
    None
}

/// Construye el paquete NetworkSettings (ID 0x8F) con compresión Zlib/Deflate (0x0000) y umbral 256,
/// envuelto en un Batch de Bedrock compatible con 0xFE.
pub fn build_network_settings_batch(compression_threshold: u16) -> Vec<u8> {
    let mut subpkt = Vec::with_capacity(14);
    encode_varuint32(0x8F, &mut subpkt);
    subpkt.extend_from_slice(&compression_threshold.to_le_bytes());
    subpkt.extend_from_slice(&0x0000u16.to_le_bytes());
    subpkt.push(0x00);
    subpkt.push(0x00);
    subpkt.extend_from_slice(&0.0f32.to_le_bytes());

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

/// Construye el string MOTD estándar de Minecraft Bedrock, sin comillas extra ni caracteres nulos.
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

/// Resuelve una IP local concreta (no 0.0.0.0) para que `str0m::Candidate::host` dentro de
/// `Connection::answer` pueda incluir el candidato ICE UDP válido en el SDP answer.
fn resolve_local_candidate_addr(preferred_ip: Option<IpAddr>, udp_port: u16) -> SocketAddr {
    if let Some(ip) = preferred_ip {
        if !ip.is_unspecified() {
            return SocketAddr::new(ip, udp_port);
        }
    }
    if let Ok(probe) = StdUdpSocket::bind("0.0.0.0:0") {
        if probe.connect("8.8.8.8:80").is_ok() {
            if let Ok(local) = probe.local_addr() {
                if !local.ip().is_unspecified() {
                    return SocketAddr::new(local.ip(), udp_port);
                }
            }
        }
    }
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), udp_port)
}

/// Abre sockets acompañantes IPv6 (`[::]:port`) para TCP (/v1/join) y UDP (WebRTC / Pong ::1)
/// devolviendo también el valor real de `IPV6_V6ONLY` leído directamente del descriptor `udp_sock`.
fn bind_optional_ipv6_sockets(port: u16) -> Option<(TcpListener, Arc<UdpSocket>, bool)> {
    let v6_addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port);

    let tcp_sock = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )
    .ok()?;
    let _ = tcp_sock.set_only_v6(true);
    let _ = tcp_sock.set_reuse_address(true);
    tcp_sock.set_nonblocking(true).ok()?;
    tcp_sock.bind(&v6_addr.into()).ok()?;
    tcp_sock.listen(128).ok()?;
    let std_tcp: std::net::TcpListener = tcp_sock.into();
    let tcp_v6 = TcpListener::from_std(std_tcp).ok()?;

    let udp_sock = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .ok()?;
    let _ = udp_sock.set_only_v6(true);
    let _ = udp_sock.set_reuse_address(true);
    let real_v6_only = udp_sock.only_v6().unwrap_or(true);
    udp_sock.set_nonblocking(true).ok()?;
    udp_sock.bind(&v6_addr.into()).ok()?;
    let std_udp: std::net::UdpSocket = udp_sock.into();
    let udp_v6 = UdpSocket::from_std(std_udp).ok()?;

    Some((tcp_v6, Arc::new(udp_v6), real_v6_only))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JoinQueryParams {
    pub version: Option<String>,
    pub protocol: Option<u32>,
    pub platform: Option<String>,
    pub id: Option<u64>,
}

pub fn parse_join_query(query: &str) -> JoinQueryParams {
    let mut parsed = JoinQueryParams::default();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "version" => {
                if !v.is_empty() {
                    parsed.version = Some(v.to_string());
                }
            }
            "protocol" => {
                parsed.protocol = v.parse::<u32>().ok();
            }
            "platform" => {
                if !v.is_empty() {
                    parsed.platform = Some(v.to_string());
                }
            }
            "id" => {
                parsed.id = v.parse::<u64>().ok();
            }
            _ => {}
        }
    }
    parsed
}

/// Política oficial Bedrock: si el protocolo numérico coincide (`protocol == 2225`),
/// se acepta el cliente aunque su semver difiera (ej. cliente `1.26.62` vs servidor `1.26.60`).
pub fn is_protocol_compatible(client_protocol: Option<u32>, client_version: Option<&str>) -> bool {
    if let Some(proto) = client_protocol {
        return proto == TARGET_BEDROCK_PROTOCOL;
    }
    match client_version {
        None => true,
        Some(ver) => {
            ver == MOTD_ADVERTISED_VERSION
                || ver == TARGET_BEDROCK_VERSION
                || ver.starts_with("1.26.6")
                || ver.starts_with("26.6")
        }
    }
}

/// Procesa un lote de datos de sesión de Minecraft Bedrock recibido por el canal SCTP de WebRTC
/// utilizando `acacia_session::server::ServerConnection` y `acacia_session::proto`.
pub fn handle_bedrock_session_data(
    conn: &mut Connection,
    session: &mut ServerConnection,
    has_sent_network_settings: &mut bool,
    peer_addr: SocketAddr,
    payload: &[u8],
    now: Instant,
) {
    // 1. Decodificar el batch de Bedrock con soporte para framing acacia-session y header 0x00 (raw) / 0x78 (ZlibDecoder)
    let packets = match decode_bedrock_batch_frames(session, payload) {
        Ok(pkts) => pkts,
        Err(err) => {
            log::warn!(
                "[Bedrock Session] Error decodificando batch de {} ({} bytes): {}",
                peer_addr,
                payload.len(),
                err
            );
            return;
        }
    };

    for raw_bytes in packets {
        let pkt = match RawPacket::parse(raw_bytes) {
            Ok(p) => p,
            Err(err) => {
                log::warn!(
                    "[Bedrock Session] Error parseando cabecera de subpaquete de {}: {}",
                    peer_addr,
                    err
                );
                continue;
            }
        };

        let name = packet_name(pkt.id).unwrap_or("UnknownPacket");
        log::info!(
            "Paquete de Bedrock recibido (ID: 0x{:02x} / {}) desde {} ({} bytes)",
            pkt.id,
            name,
            peer_addr,
            pkt.body.len()
        );
        log::info!("[Bedrock Protocol] Recibido paquete de Minecraft: 0x{:02x}", pkt.id);

        match pkt.id {
            // TAREA 3.1: RequestNetworkSettings (0xC1 / 193 o 0x0B) -> NetworkSettings (0x8F)
            id if id == RequestNetworkSettings::ID || id == 0x0b => {
                if let Ok(req) = pkt.decode::<RequestNetworkSettings>() {
                    log::info!(
                        "[Bedrock Protocol] RequestNetworkSettings recibido de {} (client_protocol={})",
                        peer_addr,
                        req.client_protocol
                    );
                }

                let settings = NetworkSettings {
                    compression_threshold: 256,
                    compression_algorithm: NetworkSettingsCompressionAlgorithm::Deflate,
                    client_throttle: false,
                    client_throttle_threshold: 0u8,
                    client_throttle_scalar: 0.0,
                };

                let mut encoded = BytesMut::new();
                encode_packet(&settings, &mut encoded);
                // El paquete NetworkSettings se envía sin comprimir antes de activar Deflate
                let batch = session.encode(&[encoded.freeze()]);
                conn.send(batch, now);

                if let Err(err) = session.start_compression(&settings) {
                    log::warn!(
                        "[Bedrock Protocol] Error activando compresión Deflate para {}: {}",
                        peer_addr,
                        err
                    );
                } else {
                    *has_sent_network_settings = true;
                    log::info!(
                        "[Bedrock Protocol] NetworkSettings enviado a {} (Deflate, threshold=256)",
                        peer_addr
                    );
                }
            }

            // TAREA 3.2: Login (0x01) -> ServerToClientHandshake (0x03) + PlayStatus::LoginSuccess (0x02)
            id if id == Login::ID => {
                if !*has_sent_network_settings {
                    log::warn!(
                        "[Bedrock Login] Login (0x01) recibido antes de NetworkSettings desde {}",
                        peer_addr
                    );
                }

                match read_login(&pkt.body) {
                    Ok((protocol_version, _skin_jwt, client_key)) => {
                        log::info!(
                            "[Bedrock Login] Paquete Login (0x01) validado desde {} (proto={})",
                            peer_addr,
                            protocol_version
                        );
                        let _ = client_key;
                        // En NetherNet el transporte WebRTC/DTLS ya cifra el canal; usamos plaintext_handshake()
                        // (o start_encryption(&client_key) si se requiere cifrado de capa de sesión).
                        let handshake_pkt = session.plaintext_handshake();
                        let hs_batch = session.encode(&[handshake_pkt]);
                        conn.send(hs_batch, now);
                        log::info!(
                            "[Bedrock Login] ServerToClientHandshake (0x03) enviado a {}",
                            peer_addr
                        );
                    }
                    Err(err) => {
                        log::warn!(
                            "[Bedrock Login] Login JWT/clave en modo offline o formato alternativo ({}): enviando ServerToClientHandshake y LoginSuccess",
                            err
                        );
                        let handshake_pkt = session.plaintext_handshake();
                        let hs_batch = session.encode(&[handshake_pkt]);
                        conn.send(hs_batch, now);
                    }
                }

                let play_status = PlayStatus {
                    status: PlayStatusStatus::LoginSuccess,
                };
                let mut status_buf = BytesMut::new();
                encode_packet(&play_status, &mut status_buf);
                let status_batch = session.encode(&[status_buf.freeze()]);
                conn.send(status_batch, now);
                log::info!(
                    "[Bedrock Login] PlayStatus (0x02 LoginSuccess) enviado a {}",
                    peer_addr
                );
            }

            // Respuesta del cliente al ServerToClientHandshake (0x04 ClientToServerHandshake)
            id if id == ClientToServerHandshake::ID => {
                let _ = pkt.decode::<ClientToServerHandshake>();
                let _ = ServerToClientHandshake::ID;
                let play_status = PlayStatus {
                    status: PlayStatusStatus::LoginSuccess,
                };
                let mut status_buf = BytesMut::new();
                encode_packet(&play_status, &mut status_buf);
                let status_batch = session.encode(&[status_buf.freeze()]);
                conn.send(status_batch, now);
                log::info!(
                    "[Bedrock Login] ClientToServerHandshake (0x04) completado con {} -> PlayStatus::LoginSuccess (0x02) confirmado",
                    peer_addr
                );
            }

            other => {
                log::info!(
                    "[Bedrock Session] Paquete 0x{:02x} ({}) procesado en sesión activa con {}",
                    other,
                    name,
                    peer_addr
                );
            }
        }
    }
}

/// Atiende una conexión TCP entrante del servidor de señalización HTTP NetherNet (`/v1/join`).
async fn handle_http_signaling_stream(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    udp_port: u16,
    network_id: u64,
    motd_string: String,
    host_key: SigningKey,
    peers: Arc<Mutex<HashMap<u64, PeerSession>>>,
    udp_socket: Arc<UdpSocket>,
) {
    let local_tcp_ip = stream.local_addr().ok().map(|a| a.ip());
    let mut buf = Vec::with_capacity(4096);
    let mut temp = [0u8; 4096];

    loop {
        match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut temp)).await {
            Ok(Ok(0)) => return,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&temp[..n]);
                // Los clientes Bedrock 26.60+ intentan primero un ClientHello TLS (0x16 0x03 ...).
                // Respondemos con http::TLS_REFUSAL para que reintenten de inmediato en HTTP plano.
                if http::is_tls(&buf) {
                    log::info!(
                        "Señalización HTTP recibida: ClientHello TLS desde {} -> enviando TLS_REFUSAL para downgrade a HTTP",
                        peer_addr
                    );
                    let _ = stream.write_all(&http::TLS_REFUSAL).await;
                    return;
                }
                if http::request_complete(&buf) {
                    break;
                }
                if buf.len() > http::MAX_MESSAGE {
                    log::warn!("Señalización HTTP excedió MAX_MESSAGE desde {}", peer_addr);
                    return;
                }
            }
            Ok(Err(err)) => {
                log::warn!("Error leyendo señalización HTTP desde {}: {}", peer_addr, err);
                return;
            }
            Err(_) => {
                log::warn!("Timeout leyendo señalización HTTP desde {}", peer_addr);
                return;
            }
        }
    }

    let req = match http::parse_request(&buf) {
        Ok(r) => r,
        Err(err) => {
            log::warn!("Error parseando petición HTTP NetherNet de {}: {}", peer_addr, err);
            let resp = http::response(400, "text/plain", "Bad Request");
            let _ = stream.write_all(&resp).await;
            return;
        }
    };

    log::info!(
        "Señalización HTTP recibida: {} {} desde {} ({} bytes body)",
        req.method,
        req.path,
        peer_addr,
        req.body.len()
    );

    let (path_only, query_str) = req.path.split_once('?').unwrap_or((req.path, ""));
    let query_params = parse_join_query(query_str);
    let proto_ok = is_protocol_compatible(query_params.protocol, query_params.version.as_deref());

    let expected_join_prefix = format!("{}/", PROBE_PATH);
    if req.method.eq_ignore_ascii_case("GET")
        && (path_only == PROBE_PATH || path_only == join_path(network_id) || path_only.starts_with(&expected_join_prefix))
    {
        if !proto_ok {
            let err_json = format!(
                r#"{{"error":"protocol_mismatch","expected_protocol":{},"got_protocol":{}}}"#,
                TARGET_BEDROCK_PROTOCOL,
                query_params.protocol.unwrap_or(0)
            );
            let resp = http::response(426, "application/json", &err_json);
            let _ = stream.write_all(&resp).await;
            return;
        }
        if let Some(ref client_ver) = query_params.version {
            log::info!(
                "Cliente NetherNet compatible (client_ver={} client_proto={:?} -> protocol_match=true)",
                client_ver,
                query_params.protocol
            );
        }
        let probe_json = format!(
            r#"{{"network_id":{},"protocol":{},"version":"{}","motd":"{}","client_compatible":true}}"#,
            network_id, TARGET_BEDROCK_PROTOCOL, MOTD_ADVERTISED_VERSION, motd_string
        );
        let resp = http::response(200, "application/json", &probe_json);
        let _ = stream.write_all(&resp).await;
    } else if req.method.eq_ignore_ascii_case("POST")
        && (path_only == PROBE_PATH
            || path_only == join_path(network_id)
            || path_only.starts_with(&expected_join_prefix))
    {
        if !proto_ok {
            let resp = http::response(426, "text/plain", "Protocol version mismatch");
            let _ = stream.write_all(&resp).await;
            return;
        }
        let offer_sdp = match std::str::from_utf8(req.body) {
            Ok(s) => s,
            Err(_) => {
                let resp = http::response(400, "text/plain", "Invalid UTF-8 SDP offer");
                let _ = stream.write_all(&resp).await;
                return;
            }
        };

        let local_candidate = resolve_local_candidate_addr(local_tcp_ip, udp_port);
        let now = Instant::now();
        match Connection::answer(offer_sdp, local_candidate, &host_key, now) {
            Ok((mut conn, answer_sdp)) => {
                log::info!(
                    "Señalización HTTP recibida: SDP offer procesado para {} (candidato ICE local={} client_ver={:?})",
                    peer_addr,
                    local_candidate,
                    query_params.version
                );

                while let Some(tx) = conn.poll_transmit() {
                    let _ = udp_socket.send_to(&tx.contents, tx.destination).await;
                }

                let session_id = query_params.id.unwrap_or_else(|| {
                    path_only
                        .rsplit('/')
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or_else(|| (peer_addr.port() as u64) ^ network_id)
                });

                if let Ok(mut guard) = peers.lock() {
                    guard.insert(
                        session_id,
                        PeerSession {
                            conn,
                            session: ServerConnection::nethernet(host_key.clone()),
                            remote_addr: peer_addr,
                            has_sent_network_settings: false,
                        },
                    );
                }

                let resp = http::response(200, "application/sdp", &answer_sdp);
                let _ = stream.write_all(&resp).await;
            }
            Err(err) => {
                log::warn!(
                    "Error en Connection::answer para oferta WebRTC de {}: {}",
                    peer_addr,
                    err
                );
                let resp = http::response(500, "text/plain", &err.to_string());
                let _ = stream.write_all(&resp).await;
            }
        }
    } else {
        let resp = http::response(404, "text/plain", "Not Found");
        let _ = stream.write_all(&resp).await;
    }
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

pub fn current_engine_state() -> EngineState {
    if is_engine_running() {
        EngineState::Running
    } else {
        EngineState::Stopped
    }
}

pub fn start_runtime(max_ram_mb: u32, blocking_threads: usize) -> Result<String, String> {
    if current_engine_state() == EngineState::Running {
        return Err("Server is already running. Stop it first.".to_string());
    }
    let net_cfg = LowLatencyConfig {
        local_fast_path: true,
        tcp_no_delay: true,
        ip_tos_low_delay: true,
        local_jitter_ms: 0,
        wan_mtu_bytes: 1400,
    };
    start_async_engine(
        "/data/local/tmp",
        "BedrockDroid RS",
        "BEDROCK",
        19132,
        max_ram_mb,
        blocking_threads,
        net_cfg,
    )
}

pub fn start_async_engine(
    data_dir: &str,
    server_name: &str,
    lane_name: &str,
    bedrock_port: u16,
    max_ram_mb: u32,
    blocking_threads: usize,
    net_cfg: LowLatencyConfig,
) -> Result<String, String> {
    init_android_logging();

    let mut guard = handle_slot()
        .lock()
        .map_err(|_| "runtime mutex poisoned".to_string())?;

    let engine_state = if guard.is_some() {
        EngineState::Running
    } else {
        EngineState::Stopped
    };
    if engine_state == EngineState::Running {
        return Err("Server is already running. Stop it first.".to_string());
    }

    let budget_mb = max_ram_mb.max(512);
    let max_blocking = if blocking_threads == 0 {
        ((budget_mb / 1024).clamp(4, 32)) as usize
    } else {
        blocking_threads.clamp(1, 64)
    };
    let arena = FixedNativeArena::allocate_fixed(budget_mb, max_blocking)?;
    let (arena_slab_bytes, budget_bytes, locked_threads) = arena.summary();
    MAX_RUST_RAM_BYTES.store(budget_bytes, Ordering::Relaxed);
    log::info!(
        "[MemoryArena] Slab fijo std::alloc={} bytes (budget={} MB, blocking_threads={}) bloqueado hasta detener servidor",
        arena_slab_bytes,
        budget_mb,
        locked_threads
    );

    let rt = Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(max_blocking)
        .thread_stack_size(8 * 1024 * 1024)
        .thread_name("bdr-tokio-worker")
        .enable_all()
        .build()
        .map_err(|e| format!("tokio init error: {}", e))?;

    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let dir_owned = data_dir.to_string();
    let server_name_owned = server_name.to_string();
    let lane_owned = lane_name.to_string();
    let is_bedrock_profile = lane_name.trim().eq_ignore_ascii_case("bedrock");
    let _ = is_bedrock_profile;

    let bind_addr: SocketAddr = format!("0.0.0.0:{}", bedrock_port)
        .parse()
        .map_err(|e| format!("invalid bind addr: {}", e))?;

    // Bind coordinado de TCP (señalización HTTP /v1/join) y UDP (transporte WebRTC ICE/DTLS/SCTP) + acompañante IPv6 real
    let (tcp_listener, udp_socket, actual_addr, v6_bound) = rt.block_on(async move {
        let tcp = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| format!("nethernet tcp bind {} error: {}", bind_addr, e))?;
        let local_tcp_addr = tcp
            .local_addr()
            .map_err(|e| format!("nethernet tcp local_addr error: {}", e))?;
        let udp_bind = SocketAddr::new(bind_addr.ip(), local_tcp_addr.port());
        let udp = UdpSocket::bind(udp_bind)
            .await
            .map_err(|e| format!("nethernet udp bind {} error: {}", udp_bind, e))?;
        let v6_opt = bind_optional_ipv6_sockets(local_tcp_addr.port());
        Ok::<(TcpListener, UdpSocket, SocketAddr, Option<(TcpListener, Arc<UdpSocket>, bool)>), String>((
            tcp,
            udp,
            local_tcp_addr,
            v6_opt,
        ))
    })?;

    let effective_port = actual_addr.port();
    let bind_report = match &v6_bound {
        Some((_, _, v6only)) => BindReport {
            bind_v4: actual_addr.to_string(),
            bind_v6: format!("[::]:{}", effective_port),
            ipv6_v6only: *v6only,
            dual_stack_active: true,
        },
        None => BindReport {
            bind_v4: actual_addr.to_string(),
            bind_v6: "none".to_string(),
            ipv6_v6only: false,
            dual_stack_active: false,
        },
    };
    set_current_bind_report(Some(bind_report.clone()));
    let bind_report_summary = if bind_report.dual_stack_active {
        format!(
            "[::]:{} (bind_v4={} ipv6_v6only={})",
            effective_port, bind_report.bind_v4, bind_report.ipv6_v6only
        )
    } else {
        format!("{} (bind_v6=none ipv6_v6only=false)", bind_report.bind_v4)
    };
    let bind_report_for_log = bind_report.clone();

    let motd_string = build_bedrock_advertisement(&server_name_owned, effective_port);
    let adv_len = motd_string.len();
    let net_id = server_id();
    let host_key = SigningKey::random(&mut OsRng);
    let peers: Arc<Mutex<HashMap<u64, PeerSession>>> = Arc::new(Mutex::new(HashMap::new()));
    let udp_shared = Arc::new(udp_socket);

    rt.spawn(async move {
        log::info!(
            "bind_v4 = {} | bind_v6 = {} | ipv6_v6only = {}",
            bind_report_for_log.bind_v4,
            bind_report_for_log.bind_v6,
            bind_report_for_log.ipv6_v6only
        );
        log::info!(
            "BedrockDroid RS Fase 3 NetherNet listo: dir={} lane={} target={} motd_ver={} proto={} max_ram={}MB (blocking_threads={}) fast_path={}",
            dir_owned,
            lane_owned,
            TARGET_BEDROCK_VERSION,
            MOTD_ADVERTISED_VERSION,
            TARGET_BEDROCK_PROTOCOL,
            budget_mb,
            max_blocking,
            net_cfg.local_fast_path
        );

        // Watchdog de memoria nativa (OOM Guard)
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
            "Servidor NetherNet (acacia-nethernet) escuchando señalización HTTP en TCP {} y WebRTC en UDP {}",
            actual_addr,
            actual_addr
        );
        log::info!(
            "Advertisement configurado ({} bytes, ServerID={}): {}",
            adv_len,
            net_id,
            motd_string
        );

        // Listener opcional de descubrimiento LAN NetherNet en UDP 7551 (acacia_nethernet::lan)
        let lan_bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DISCOVERY_PORT);
        if let Ok(lan_sock) = UdpSocket::bind(lan_bind).await {
            let lan_cancel = task_cancel.child_token();
            let lan_peers = Arc::clone(&peers);
            let lan_udp = Arc::clone(&udp_shared);
            let lan_key = host_key.clone();
            let lan_server_name = server_name_owned.clone();
            tokio::spawn(async move {
                log::info!(
                    "[NetherNet LAN] Discovery y señalización LAN activos en UDP {}",
                    lan_bind
                );
                let mut lan_buf = [0u8; 4096];
                loop {
                    tokio::select! {
                        _ = lan_cancel.cancelled() => break,
                        res = lan_sock.recv_from(&mut lan_buf) => {
                            if let Ok((len, from)) = res {
                                if let Ok((pkt, sender_id)) = LanPacket::decode(&lan_buf[..len]) {
                                    match pkt {
                                        LanPacket::Request => {
                                            let server_data = ServerData {
                                                server_name: lan_server_name.clone(),
                                                level_name: "Bedrock World".to_string(),
                                                protocol: TARGET_BEDROCK_PROTOCOL as i32,
                                                version: MOTD_ADVERTISED_VERSION.to_string(),
                                                player_count: 0,
                                                max_player_count: 10,
                                                game_type: 0,
                                                editor_world: false,
                                                hardcore: false,
                                                accepts_online_auth: true,
                                                accepts_self_signed_auth: true,
                                                nonce: format!("{:016X}", net_id),
                                                connection_type: 4,
                                            };
                                            let resp = LanPacket::Response(server_data.encode()).encode(net_id);
                                            let _ = lan_sock.send_to(&resp, from).await;
                                        }
                                        LanPacket::Message { recipient, data } if recipient == net_id => {
                                            if let Ok(sig) = Signal::parse(&data) {
                                                match sig.kind {
                                                    SignalKind::ConnectRequest => {
                                                        let local_cand = resolve_local_candidate_addr(None, effective_port);
                                                        let now = Instant::now();
                                                        if let Ok((mut conn, answer_sdp)) = Connection::answer(&sig.data, local_cand, &lan_key, now) {
                                                            log::info!("Señalización LAN recibida: ConnectRequest desde {} (session={})", from, sig.connection_id);
                                                            while let Some(tx) = conn.poll_transmit() {
                                                                let _ = lan_udp.send_to(&tx.contents, tx.destination).await;
                                                            }
                                                            if let Ok(mut guard) = lan_peers.lock() {
                                                                guard.insert(
                                                                    sig.connection_id,
                                                                    PeerSession {
                                                                        conn,
                                                                        session: ServerConnection::nethernet(lan_key.clone()),
                                                                        remote_addr: from,
                                                                        has_sent_network_settings: false,
                                                                    },
                                                                );
                                                            }
                                                            let answer_sig = Signal {
                                                                kind: SignalKind::ConnectResponse,
                                                                connection_id: sig.connection_id,
                                                                data: answer_sdp,
                                                            };
                                                            let msg = LanPacket::Message {
                                                                recipient: sender_id,
                                                                data: answer_sig.to_string(),
                                                            }.encode(net_id);
                                                            let _ = lan_sock.send_to(&msg, from).await;
                                                        }
                                                    }
                                                    SignalKind::CandidateAdd => {
                                                        let now = Instant::now();
                                                        if let Ok(mut guard) = lan_peers.lock() {
                                                            if let Some(peer) = guard.get_mut(&sig.connection_id) {
                                                                let _ = peer.conn.add_remote_candidate(&sig.data, now);
                                                            }
                                                        }
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
            });
        }

        // Listener acompañante Dual-Stack IPv6 ([::]:port) para TCP /v1/join y UDP (::1 ping / WebRTC IPv6)
        let udp_v6_shared: Option<Arc<UdpSocket>> =
            if let Some((tcp_v6, udp_v6, real_v6_only)) = v6_bound {
                let udp_v6_for_main = Arc::clone(&udp_v6);
                let v6_cancel = task_cancel.child_token();
                let v6_peers = Arc::clone(&peers);
                let v6_key = host_key.clone();
                let v6_motd = motd_string.clone();
                tokio::spawn(async move {
                log::info!(
                    "[NetherNet IPv6] Listener Dual-Stack activo en [::]:{} (TCP /v1/join + UDP WebRTC, ipv6_v6only={})",
                    effective_port,
                    real_v6_only
                );
                let mut v6_buf = [0u8; 65535];
                const RAKNET_MAGIC_V6: [u8; 16] = [
                    0x00, 0xFF, 0xFF, 0x00, 0xFE, 0xFE, 0xFE, 0xFE, 0xFD, 0xFD, 0xFD, 0xFD, 0x12,
                    0x34, 0x56, 0x78,
                ];
                loop {
                    tokio::select! {
                        _ = v6_cancel.cancelled() => break,
                        accept_res = tcp_v6.accept() => {
                            if let Ok((stream, peer_addr)) = accept_res {
                                let peers_clone = Arc::clone(&v6_peers);
                                let udp_clone = Arc::clone(&udp_v6);
                                let key_clone = v6_key.clone();
                                let motd_clone = v6_motd.clone();
                                tokio::spawn(async move {
                                    handle_http_signaling_stream(
                                        stream,
                                        peer_addr,
                                        effective_port,
                                        net_id,
                                        motd_clone,
                                        key_clone,
                                        peers_clone,
                                        udp_clone,
                                    ).await;
                                });
                            }
                        }
                        recv_res = udp_v6.recv_from(&mut v6_buf) => {
                            if let Ok((len, src_addr)) = recv_res {
                                let pkt = &v6_buf[..len];
                                if len >= 25 && (pkt[0] == 0x01 || pkt[0] == 0x02) && pkt[9..25] == RAKNET_MAGIC_V6 {
                                    let motd_bytes = v6_motd.as_bytes();
                                    let mut pong = Vec::with_capacity(35 + motd_bytes.len());
                                    pong.push(0x1c);
                                    pong.extend_from_slice(&pkt[1..9]);
                                    pong.extend_from_slice(&net_id.to_be_bytes());
                                    pong.extend_from_slice(&RAKNET_MAGIC_V6);
                                    pong.extend_from_slice(&(motd_bytes.len() as u16).to_be_bytes());
                                    pong.extend_from_slice(motd_bytes);
                                    let _ = udp_v6.send_to(&pong, src_addr).await;
                                } else {
                                    let now = Instant::now();
                                    let mut outbound: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
                                    if let Ok(mut guard) = v6_peers.lock() {
                                        let mut closed_ids = Vec::new();
                                        for (&sid, peer) in guard.iter_mut() {
                                            peer.conn.handle_datagram(now, src_addr, pkt);
                                            while let Some(ev) = poll_connection_event(&mut peer.conn) {
                                                match ev {
                                                    Event::Connected => {
                                                        peer.remote_addr = src_addr;
                                                        log::info!(
                                                            "Conexión WebRTC IPv6 establecida con {} (session={}). Esperando RequestNetworkSettings...",
                                                            src_addr,
                                                            sid
                                                        );
                                                    }
                                                    Event::Data(payload) => {
                                                        handle_bedrock_session_data(
                                                            &mut peer.conn,
                                                            &mut peer.session,
                                                            &mut peer.has_sent_network_settings,
                                                            peer.remote_addr,
                                                            &payload,
                                                            now,
                                                        );
                                                    }
                                                    Event::Disconnected(reason) => {
                                                        log::info!(
                                                            "Conexión WebRTC IPv6 cerrada con {} (session={}): {}",
                                                            peer.remote_addr,
                                                            sid,
                                                            reason
                                                        );
                                                        closed_ids.push(sid);
                                                    }
                                                }
                                            }
                                            while let Some(tx) = peer.conn.poll_transmit() {
                                                outbound.push((tx.destination, tx.contents));
                                            }
                                        }
                                        for sid in closed_ids {
                                            guard.remove(&sid);
                                        }
                                    }
                                    for (dest, data) in outbound {
                                        if dest.is_ipv6() {
                                            let _ = udp_v6.send_to(&data, dest).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            });
            Some(udp_v6_for_main)
        } else {
            None
        };

        // Bucle principal: acepta señalización HTTP (/v1/join), recibe datagramas UDP WebRTC y despacha Event
        let mut udp_buf = [0u8; 65535];
        let mut rtc_tick = tokio::time::interval(Duration::from_millis(20));

        const RAKNET_MAGIC: [u8; 16] = [
            0x00, 0xFF, 0xFF, 0x00, 0xFE, 0xFE, 0xFE, 0xFE, 0xFD, 0xFD, 0xFD, 0xFD, 0x12, 0x34,
            0x56, 0x78,
        ];

        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => {
                    log::info!("Servidor NetherNet cancelado por CancellationToken");
                    break;
                }
                accept_res = tcp_listener.accept() => {
                    match accept_res {
                        Ok((stream, peer_addr)) => {
                            let peers_clone = Arc::clone(&peers);
                            let udp_clone = Arc::clone(&udp_shared);
                            let key_clone = host_key.clone();
                            let motd_clone = motd_string.clone();
                            tokio::spawn(async move {
                                handle_http_signaling_stream(
                                    stream,
                                    peer_addr,
                                    effective_port,
                                    net_id,
                                    motd_clone,
                                    key_clone,
                                    peers_clone,
                                    udp_clone,
                                ).await;
                            });
                        }
                        Err(err) => {
                            log::warn!("Error aceptando conexión TCP de señalización HTTP: {}", err);
                        }
                    }
                }
                recv_res = udp_shared.recv_from(&mut udp_buf) => {
                    if let Ok((len, src_addr)) = recv_res {
                        let pkt = &udp_buf[..len];
                        // Compatibilidad diagnóstica con Unconnected Ping UDP (0x01) para el escáner pinglan
                        if len >= 25 && (pkt[0] == 0x01 || pkt[0] == 0x02) && pkt[9..25] == RAKNET_MAGIC {
                            let motd_bytes = motd_string.as_bytes();
                            let mut pong = Vec::with_capacity(35 + motd_bytes.len());
                            pong.push(0x1c);
                            pong.extend_from_slice(&pkt[1..9]);
                            pong.extend_from_slice(&net_id.to_be_bytes());
                            pong.extend_from_slice(&RAKNET_MAGIC);
                            pong.extend_from_slice(&(motd_bytes.len() as u16).to_be_bytes());
                            pong.extend_from_slice(motd_bytes);
                            let _ = udp_shared.send_to(&pong, src_addr).await;
                            continue;
                        }

                        // Encaminar datagrama WebRTC (STUN / ICE / DTLS / SCTP) a las conexiones activas
                        let now = Instant::now();
                        let mut outbound: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
                        if let Ok(mut guard) = peers.lock() {
                            let mut closed_ids = Vec::new();
                            for (&sid, peer) in guard.iter_mut() {
                                peer.conn.handle_datagram(now, src_addr, pkt);
                                while let Some(ev) = poll_connection_event(&mut peer.conn) {
                                    match ev {
                                        Event::Connected => {
                                            peer.remote_addr = src_addr;
                                            log::info!(
                                                "Conexión WebRTC establecida con {} (session={}). Esperando RequestNetworkSettings...",
                                                src_addr,
                                                sid
                                            );
                                        }
                                        Event::Data(payload) => {
                                            handle_bedrock_session_data(
                                                &mut peer.conn,
                                                &mut peer.session,
                                                &mut peer.has_sent_network_settings,
                                                peer.remote_addr,
                                                &payload,
                                                now,
                                            );
                                        }
                                        Event::Disconnected(reason) => {
                                            log::info!(
                                                "Conexión WebRTC cerrada con {} (session={}): {}",
                                                peer.remote_addr,
                                                sid,
                                                reason
                                            );
                                            closed_ids.push(sid);
                                        }
                                    }
                                }
                                while let Some(tx) = peer.conn.poll_transmit() {
                                    outbound.push((tx.destination, tx.contents));
                                }
                            }
                            for sid in closed_ids {
                                guard.remove(&sid);
                            }
                        }
                        for (dest, data) in outbound {
                            let _ = udp_shared.send_to(&data, dest).await;
                        }
                    }
                }
                _ = rtc_tick.tick() => {
                    let now = Instant::now();
                    let mut outbound: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
                    if let Ok(mut guard) = peers.lock() {
                        let mut closed_ids = Vec::new();
                        for (&sid, peer) in guard.iter_mut() {
                            peer.conn.handle_timeout(now);
                            while let Some(ev) = poll_connection_event(&mut peer.conn) {
                                match ev {
                                    Event::Connected => {
                                        log::info!(
                                            "Conexión WebRTC establecida con {} (session={})",
                                            peer.remote_addr,
                                            sid
                                        );
                                    }
                                    Event::Data(payload) => {
                                        handle_bedrock_session_data(
                                            &mut peer.conn,
                                            &mut peer.session,
                                            &mut peer.has_sent_network_settings,
                                            peer.remote_addr,
                                            &payload,
                                            now,
                                        );
                                    }
                                    Event::Disconnected(reason) => {
                                        log::info!(
                                            "Conexión WebRTC cerrada con {} (session={}): {}",
                                            peer.remote_addr,
                                            sid,
                                            reason
                                        );
                                        closed_ids.push(sid);
                                    }
                                }
                            }
                            while let Some(tx) = peer.conn.poll_transmit() {
                                outbound.push((tx.destination, tx.contents));
                            }
                        }
                        for sid in closed_ids {
                            guard.remove(&sid);
                        }
                    }
                    for (dest, data) in outbound {
                        if dest.is_ipv6() {
                            if let Some(ref v6) = udp_v6_shared {
                                let _ = v6.send_to(&data, dest).await;
                            }
                        } else {
                            let _ = udp_shared.send_to(&data, dest).await;
                        }
                    }
                }
            }
        }
    });

    *guard = Some(ServerHandle { rt, cancel, arena });
    Ok(format!("engine=running nethernet={}", bind_report_summary))
}

pub fn stop_async_engine() {
    set_current_bind_report(None);
    let taken = handle_slot()
        .lock()
        .ok()
        .and_then(|mut guard| guard.take());

    if let Some(handle) = taken {
        handle.cancel.cancel();
        // Límite de 2 segundos para asegurar liberación completa de los sockets TCP/UDP entre reinicios rápidos.
        handle.rt.shutdown_timeout(Duration::from_secs(2));
        log::info!("bedrock_android_core runtime detenido y sockets NetherNet liberados");
    }
}

pub fn is_engine_running() -> bool {
    handle_slot()
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

/// Verifica que el servidor de señalización HTTP NetherNet responde en `127.0.0.1:<port>` (TCP `GET /v1/join`).
pub fn send_ping_command(bedrock_port: u16) -> String {
    let target_port = if bedrock_port == 0 { 19132 } else { bedrock_port };
    let target_addr: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), target_port);
    let host_header = format!("127.0.0.1:{}", target_port);

    let started = Instant::now();
    let mut stream = match StdTcpStream::connect_timeout(&target_addr, Duration::from_secs(2)) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!(
                "[PING] Sin respuesta del servidor (timeout) en TCP {}: {}",
                target_addr, e
            );
            log::warn!("{}", msg);
            return msg;
        }
    };

    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let req_bytes = http::probe_request(&host_header);
    if let Err(e) = stream.write_all(&req_bytes) {
        let msg = format!(
            "[PING] Error enviando petición HTTP GET {} a {}: {}",
            PROBE_PATH, target_addr, e
        );
        log::warn!("{}", msg);
        return msg;
    }

    let mut resp_buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                resp_buf.extend_from_slice(&chunk[..n]);
                if http::response_complete(&resp_buf) {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    match http::parse_response(&resp_buf) {
        Ok((200, body)) => {
            let rtt_ms = started.elapsed().as_secs_f64() * 1000.0;
            let body_str = String::from_utf8_lossy(body).to_string();
            log::info!(
                "[PING] Servidor responde ({:.2} ms, NetherNet HTTP /v1/join): {}",
                rtt_ms,
                body_str
            );
            format!("[PING] Servidor responde ({:.2} ms): {}", rtt_ms, body_str)
        }
        Ok((status, _)) => {
            let msg = format!(
                "[PING] Respuesta HTTP inesperada (status={}) en {}",
                status, target_addr
            );
            log::warn!("{}", msg);
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
        let sample = [0xfe, 0x05, 0xc1, 0x01, 0x00, 0x00, 0x08, 0xb1];
        assert_eq!(decode_bedrock_batch_packet_id(&sample), Some(0xc1));
        assert_eq!(decode_bedrock_batch_packet_id(&[0x01, 0x02]), None);
        // Batch sin cabecera 0xFE (formato nativo NetherNet WebRTC)
        assert_eq!(decode_nethernet_batch_packet_id(&sample[1..]), Some(0xc1));
        // Longitud declarada mayor que los bytes disponibles -> inválido (None)
        assert_eq!(decode_nethernet_batch_packet_id(&[0x05, 0x01, 0x02]), None);
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
    fn acacia_session_encodes_network_settings_and_login_handshake() {
        let key = SigningKey::random(&mut OsRng);
        let mut session = ServerConnection::nethernet(key);

        let settings = NetworkSettings {
            compression_threshold: 256,
            compression_algorithm: NetworkSettingsCompressionAlgorithm::Deflate,
            client_throttle: false,
            client_throttle_threshold: 0u8,
            client_throttle_scalar: 0.0,
        };
        let mut out = BytesMut::new();
        encode_packet(&settings, &mut out);
        let batch = session.encode(&[out.freeze()]);
        assert!(!batch.is_empty());

        session
            .start_compression(&settings)
            .expect("start_compression should succeed");

        let hs = session.plaintext_handshake();
        let hs_batch = session.encode(&[hs]);
        assert!(!hs_batch.is_empty());
    }

    #[test]
    fn accepts_http_join_query_from_client_1_26_62_protocol_2225() {
        let query = parse_join_query("version=1.26.62&protocol=2225&platform=1&id=1477");
        assert_eq!(query.version.as_deref(), Some("1.26.62"));
        assert_eq!(query.protocol, Some(2225));
        assert_eq!(query.platform.as_deref(), Some("1"));
        assert_eq!(query.id, Some(1477));
        assert!(is_protocol_compatible(query.protocol, query.version.as_deref()));
        assert!(!is_protocol_compatible(Some(9999), Some("1.26.62")));
    }

    #[test]
    fn decodes_bedrock_client_1_26_62_request_network_settings_fixture() {
        // Fixture capturada de cliente Android 1.26.62 (protocol=2225 -> 0x000008b1) en frame NetherNet sin 0xFE y con 0xFE
        let client_1_26_62_payload = [0x05, 0xc1, 0x01, 0x00, 0x00, 0x08, 0xb1];
        assert_eq!(decode_nethernet_batch_packet_id(&client_1_26_62_payload), Some(0xc1));
        let mut raknet_wrapped = vec![0xfe];
        raknet_wrapped.extend_from_slice(&client_1_26_62_payload);
        assert_eq!(decode_bedrock_batch_packet_id(&raknet_wrapped), Some(0xc1));
    }

    #[test]
    fn decodes_bedrock_batch_header_0x00_raw_and_0x78_zlib_26038_bytes() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;

        // 1. Header 0x00 = raw sin comprimir
        let raw_subpkt = [0xc1u8, 0x01, 0x00, 0x00, 0x08, 0xb1];
        let mut raw_batch = vec![0x00u8];
        encode_varuint32(raw_subpkt.len() as u32, &mut raw_batch);
        raw_batch.extend_from_slice(&raw_subpkt);
        let decoded_raw = decode_bedrock_batch_with_header(&raw_batch)
            .expect("0x00 raw batch should decode");
        assert_eq!(decoded_raw.len(), 1);
        assert_eq!(decoded_raw[0].as_ref(), &raw_subpkt);

        // 2. Payload realista Login de 26038 bytes comprimido con Zlib (header 0x78)
        let mut login_subpkt = Vec::with_capacity(26038);
        encode_varuint32(0x01, &mut login_subpkt); // Login packet ID
        while login_subpkt.len() < 26038 {
            login_subpkt.push(((login_subpkt.len() * 31) & 0xff) as u8);
        }
        assert_eq!(login_subpkt.len(), 26038);

        let mut uncompressed_frame = Vec::with_capacity(26045);
        encode_varuint32(login_subpkt.len() as u32, &mut uncompressed_frame);
        uncompressed_frame.extend_from_slice(&login_subpkt);

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&uncompressed_frame).expect("zlib write");
        let zlib_payload = encoder.finish().expect("zlib finish");
        assert_eq!(zlib_payload[0], 0x78);

        let decoded_zlib = decode_bedrock_batch_with_header(&zlib_payload)
            .expect("0x78 zlib 26038-byte payload should decode");
        assert_eq!(decoded_zlib.len(), 1);
        assert_eq!(decoded_zlib[0].len(), 26038);
        assert_eq!(decoded_zlib[0].as_ref(), login_subpkt.as_slice());

        // También probar con cabecera explícita 0x78 precediendo al stream y wrapper 0xFE
        let mut prefixed = vec![0xfeu8, 0x78];
        prefixed.extend_from_slice(&zlib_payload);
        let key = SigningKey::random(&mut OsRng);
        let mut session = ServerConnection::nethernet(key);
        let decoded_session = decode_bedrock_batch_frames(&mut session, &prefixed)
            .expect("decode_bedrock_batch_frames should handle 0xFE + 0x78 zlib");
        assert_eq!(decoded_session.len(), 1);
        assert_eq!(decoded_session[0].len(), 26038);
    }

    #[test]
    fn ipv6_dual_stack_socket_reports_real_fd_only_v6_and_responds_to_loopback_ping() {
        let rt = Builder::new_current_thread().enable_all().build().expect("rt");
        let bound = rt.block_on(async { bind_optional_ipv6_sockets(0) });
        if let Some((_tcp_v6, udp_v6, real_v6_only)) = bound {
            let local_v6 = udp_v6.local_addr().expect("local v6 addr");
            assert!(local_v6.is_ipv6());
            assert!(real_v6_only);
        }
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
        let status = start_async_engine("/tmp/bedrock_test", "BedrockDroid RS", "BEDROCK", 0, 8192, 8, cfg)
            .expect("start failed");
        assert!(status.starts_with("engine=running nethernet="));
        assert!(is_engine_running());
        assert_eq!(current_engine_state(), EngineState::Running);

        // P2: Bloqueo en Rust mientras el servidor está RUNNING (sin redimensionar en caliente)
        let second_err = start_async_engine("/tmp/bedrock_test", "BedrockDroid RS", "BEDROCK", 0, 4096, 4, cfg)
            .expect_err("re-starting while running must fail");
        assert_eq!(second_err, "Server is already running. Stop it first.");
        assert_eq!(
            start_runtime(4096, 4).expect_err("start_runtime while running must fail"),
            "Server is already running. Stop it first."
        );

        let report = current_bind_report().expect("bind report should exist while running");
        assert!(report.bind_v4.starts_with("0.0.0.0:"));

        let port: u16 = report
            .bind_v4
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .expect("valid bound port");

        // Verificar que el servidor de señalización HTTP NetherNet responde en TCP /v1/join
        let ping_res = send_ping_command(port);
        assert!(ping_res.contains("[PING] Servidor responde"));
        assert!(ping_res.contains("MCPE;BedrockDroid RS;"));

        // Verificar que GET /v1/join?version=1.26.62&protocol=2225&platform=1&id=1477 devuelve 200 OK
        let join_addr = format!("127.0.0.1:{}", port);
        let mut tcp = StdTcpStream::connect(&join_addr).expect("connect /v1/join");
        let _ = tcp.set_read_timeout(Some(Duration::from_secs(2)));
        let raw_req = format!(
            "GET /v1/join?version=1.26.62&protocol=2225&platform=1&id=1477 HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            join_addr
        );
        tcp.write_all(raw_req.as_bytes()).expect("write GET /v1/join");
        let mut raw_resp = String::new();
        let _ = tcp.read_to_string(&mut raw_resp);
        assert!(raw_resp.contains("200 OK"));
        assert!(raw_resp.contains("\"client_compatible\":true"));

        stop_async_engine();
        assert!(!is_engine_running());
        assert_eq!(current_engine_state(), EngineState::Stopped);
    }
}
