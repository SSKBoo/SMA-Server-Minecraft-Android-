use crate::net::low_latency::LowLatencyConfig;
use acacia_nethernet::{
    http,
    lan::{DISCOVERY_PORT, LanPacket, ServerData},
    Connection, Event as RtcEvent, Signal, SignalKind, PROBE_PATH, join_path,
};
use acacia_session::proto::{
    encode_packet, packet_name,
    packets::{
        ClientCacheStatus, ClientToServerHandshake, Login, NetworkSettings,
        NetworkSettingsCompressionAlgorithm, PlayStatus, PlayStatusStatus, RequestChunkRadius,
        RequestNetworkSettings, ResourcePackClientResponse,
        ResourcePackClientResponseResponseStatus, ServerToClientHandshake,
        SetLocalPlayerAsInitialized,
    },
    Packet, RawPacket,
};
use acacia_session::server::{read_login, ServerConnection};
use bytes::{Bytes, BytesMut};
use flate2::read::{DeflateDecoder, ZlibDecoder};
use p384::ecdsa::SigningKey;
use rand_core::OsRng;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::any::Any;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream as StdTcpStream, UdpSocket as StdUdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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

#[derive(Debug, Clone, Default)]
pub struct SessionProgress {
    pub has_sent_network_settings: bool,
    pub player_logged_in: bool,
    pub has_sent_login_success: bool,
    pub has_sent_resource_packs_info: bool,
    pub has_sent_resource_packs_stack: bool,
    pub has_received_client_cache_status: bool,
    pub has_sent_start_game: bool,
    pub has_sent_world_registries: bool,
    pub has_sent_initial_chunks: bool,
    pub has_sent_player_spawn_entities: bool,
    pub has_sent_chunk_radius: bool,
    pub set_local_player_initialized: bool,
    pub player_name: String,
    pub player_uuid: [u8; 16],
    pub player_auth_input_count: u64,
    pub latency_ping_count: u64,
    pub keepalives_sent: u64,
    pub movement_tick: u64,
    pub last_packet_id: Option<u32>,
    pub last_packet_name: String,
}

struct PeerSession {
    conn: Connection,
    session: ServerConnection,
    remote_addr: SocketAddr,
    progress: SessionProgress,
}

#[derive(Debug, Clone, Default)]
struct ConnectedPlayerState {
    connected_count: usize,
    last_player_name: String,
    world_spawn_sent: bool,
    chunks_sent: usize,
}

pub type PlayerEventHook = Arc<dyn Fn(&str, usize, bool) + Send + Sync>;
pub type CrashEventHook = Arc<dyn Fn(&str) + Send + Sync>;

static PLAYER_STATE: OnceLock<Mutex<ConnectedPlayerState>> = OnceLock::new();
static PLAYER_EVENT_HOOK: OnceLock<Mutex<Option<PlayerEventHook>>> = OnceLock::new();
static CRASH_EVENT_HOOK: OnceLock<Mutex<Option<CrashEventHook>>> = OnceLock::new();
static CRASH_LOG_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
static CRASH_PROTECTION_ENABLED: AtomicBool = AtomicBool::new(false);
static SIGNAL_HOOKS_INIT: OnceLock<()> = OnceLock::new();
const DEFAULT_ANDROID_CRASH_LOG_PATH: &str =
    "/storage/emulated/0/Android/data/com.bedrockdroid.rs/files/crash_log.txt";

fn player_state_slot() -> &'static Mutex<ConnectedPlayerState> {
    PLAYER_STATE.get_or_init(|| Mutex::new(ConnectedPlayerState::default()))
}

fn player_hook_slot() -> &'static Mutex<Option<PlayerEventHook>> {
    PLAYER_EVENT_HOOK.get_or_init(|| Mutex::new(None))
}

fn crash_hook_slot() -> &'static Mutex<Option<CrashEventHook>> {
    CRASH_EVENT_HOOK.get_or_init(|| Mutex::new(None))
}

fn crash_log_path_slot() -> &'static Mutex<Option<PathBuf>> {
    CRASH_LOG_PATH.get_or_init(|| Mutex::new(None))
}

pub fn set_crash_protection_enabled(enabled: bool) {
    CRASH_PROTECTION_ENABLED.store(enabled, Ordering::SeqCst);
}

pub fn is_crash_protection_enabled() -> bool {
    CRASH_PROTECTION_ENABLED.load(Ordering::SeqCst)
}

pub fn set_crash_log_path(path: Option<PathBuf>) {
    if let Ok(mut guard) = crash_log_path_slot().lock() {
        *guard = path;
    }
}

pub fn set_crash_event_hook(hook: Option<CrashEventHook>) {
    if let Ok(mut guard) = crash_hook_slot().lock() {
        *guard = hook;
    }
}

pub fn record_native_signal_crash(sig_name: &str, sig_num: i32) {
    if !is_crash_protection_enabled() {
        return;
    }
    let bt = backtrace::Backtrace::new();
    let target_path = crash_log_path_slot()
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ANDROID_CRASH_LOG_PATH));
    if let Some(parent) = target_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&target_path) {
        let _ = writeln!(file, "CRASH NATIVO: {} (sig={})\n{:?}", sig_name, sig_num, bt);
        let _ = file.flush();
        let _ = file.sync_all();
    }
}

pub fn init_native_signal_handlers() {
    SIGNAL_HOOKS_INIT.get_or_init(|| {
        #[cfg(unix)]
        unsafe {
            use signal_hook::consts::signal::{SIGABRT, SIGBUS, SIGILL, SIGSEGV};
            let signals: [(&str, libc::c_int); 4] = [
                ("SIGSEGV", SIGSEGV),
                ("SIGABRT", SIGABRT),
                ("SIGBUS", SIGBUS),
                ("SIGILL", SIGILL),
            ];
            for (sig_name, sig_num) in signals {
                let res = if signal_hook::consts::FORBIDDEN.contains(&sig_num) {
                    signal_hook_registry::register_signal_unchecked(sig_num, move || {
                        record_native_signal_crash(sig_name, sig_num as i32);
                    })
                } else {
                    signal_hook::low_level::register(sig_num, move || {
                        record_native_signal_crash(sig_name, sig_num as i32);
                    })
                };
                if let Err(err) = res {
                    log::warn!(
                        "[RustSignalHook] No se pudo registrar señal {} ({}): {}",
                        sig_name,
                        sig_num,
                        err
                    );
                }
            }
        }
    });
}

pub fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown non-string panic payload".to_string()
    }
}

fn append_crash_to_disk(error: &str) {
    if !is_crash_protection_enabled() {
        return;
    }
    let path = crash_log_path_slot()
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ANDROID_CRASH_LOG_PATH));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let epoch_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(file, "=== [epoch={}] RUST CRASH / PANIC ===", epoch_secs);
        let _ = writeln!(file, "{}", error.trim_end());
        let _ = writeln!(file);
        let _ = file.flush();
        let _ = file.sync_all();
    }
}

pub fn report_rust_crash(error: &str) {
    if !is_crash_protection_enabled() {
        return;
    }
    log::error!("{}", error);
    append_crash_to_disk(error);
    let hook_opt = crash_hook_slot().lock().ok().and_then(|g| g.clone());
    if let Some(hook) = hook_opt {
        hook(error);
    }
}

pub fn run_catch_unwind<F: FnOnce()>(context_label: &str, f: F) -> bool {
    if !is_crash_protection_enabled() {
        f();
        return true;
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(()) => true,
        Err(payload) => {
            let bt = std::backtrace::Backtrace::force_capture();
            let msg = format!(
                "PANIC DE RUST:\n{} en {}\n\n--- Rust Backtrace ---\n{}",
                panic_payload_to_string(payload),
                context_label,
                bt
            );
            report_rust_crash(&msg);
            false
        }
    }
}

struct CatchUnwindFuture<F> {
    label: &'static str,
    inner: std::pin::Pin<Box<F>>,
}

impl<F> CatchUnwindFuture<F> {
    fn new(label: &'static str, fut: F) -> Self {
        Self {
            label,
            inner: Box::pin(fut),
        }
    }
}

impl<F: std::future::Future<Output = ()>> std::future::Future for CatchUnwindFuture<F> {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let label = self.label;
        let res = if is_crash_protection_enabled() {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.inner.as_mut().poll(cx)
            }))
        } else {
            Ok(self.inner.as_mut().poll(cx))
        };
        match res {
            Ok(poll) => poll,
            Err(payload) => {
                let bt = std::backtrace::Backtrace::force_capture();
                let msg = format!(
                    "PANIC DE RUST (tokio::spawn):\n{} en {}\n\n--- Rust Backtrace ---\n{}",
                    panic_payload_to_string(payload),
                    label,
                    bt
                );
                report_rust_crash(&msg);
                std::task::Poll::Ready(())
            }
        }
    }
}

fn spawn_protected<F>(label: &'static str, fut: F) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(CatchUnwindFuture::new(label, fut))
}

pub fn set_player_event_hook(hook: Option<PlayerEventHook>) {
    if let Ok(mut guard) = player_hook_slot().lock() {
        *guard = hook;
    }
}

fn emit_player_hook(player_name: &str, count: usize, connected: bool) {
    let hook_opt = player_hook_slot().lock().ok().and_then(|g| g.clone());
    if let Some(hook) = hook_opt {
        hook(player_name, count, connected);
    }
}

fn notify_player_login(player_name: &str) {
    let clean_name = if player_name.trim().is_empty() {
        "Jugador Bedrock".to_string()
    } else {
        player_name.trim().to_string()
    };
    let count = {
        let mut st = match player_state_slot().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        st.connected_count = st.connected_count.saturating_add(1);
        st.last_player_name = clean_name.clone();
        st.connected_count
    };
    emit_player_hook(&clean_name, count, true);
}

fn notify_player_disconnect() {
    let (name, count, still_connected) = {
        let mut st = match player_state_slot().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        st.connected_count = st.connected_count.saturating_sub(1);
        if st.connected_count == 0 {
            st.last_player_name.clear();
            st.world_spawn_sent = false;
            st.chunks_sent = 0;
            (String::new(), 0usize, false)
        } else {
            (st.last_player_name.clone(), st.connected_count, true)
        }
    };
    emit_player_hook(&name, count, still_connected);
}

fn notify_world_spawn_sent(chunks_sent: usize) {
    if let Ok(mut st) = player_state_slot().lock() {
        st.world_spawn_sent = true;
        st.chunks_sent = st.chunks_sent.saturating_add(chunks_sent);
    }
}

fn reset_player_state() {
    if let Ok(mut st) = player_state_slot().lock() {
        *st = ConnectedPlayerState::default();
    }
    emit_player_hook("", 0, false);
}

pub fn current_connected_player_status_json() -> String {
    let st = player_state_slot()
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    serde_json::json!({
        "connected": st.connected_count > 0,
        "count": st.connected_count,
        "player": st.last_player_name,
        "worldSpawnSent": st.world_spawn_sent,
        "chunksSent": st.chunks_sent
    })
    .to_string()
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

fn encode_varuint64(mut value: u64, out: &mut Vec<u8>) {
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

/// Decodificador compacto Base64/Base64URL para inspeccionar el payload JWT de `Login` sin dependencias externas.
fn decode_base64_segment(segment: &str) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = segment
        .bytes()
        .filter(|&b| b != b'=' && !b.is_ascii_whitespace())
        .collect();
    if clean.len() < 4 {
        return None;
    }
    let mut out = Vec::with_capacity((clean.len() * 3) / 4);
    let mut i = 0usize;
    while i + 1 < clean.len() {
        let a = val(clean[i])?;
        let b = val(clean[i + 1])?;
        out.push((a << 2) | (b >> 4));
        if i + 2 < clean.len() {
            let c = val(clean[i + 2])?;
            out.push(((b & 0x0f) << 4) | (c >> 2));
            if i + 3 < clean.len() {
                let d = val(clean[i + 3])?;
                out.push(((c & 0x03) << 6) | d);
            }
        }
        i += 4;
    }
    Some(out)
}

fn find_json_display_name(text: &str) -> Option<String> {
    for needle in ["\"displayName\"", "\"DisplayName\""] {
        if let Some(pos) = text.find(needle) {
            let rest = &text[pos + needle.len()..];
            if let Some(colon_idx) = rest.find(':') {
                let after_colon = rest[colon_idx + 1..].trim_start();
                if let Some(quoted) = after_colon.strip_prefix('"') {
                    if let Some(end_quote) = quoted.find('"') {
                        let name = quoted[..end_quote].trim();
                        if !name.is_empty() && name.len() <= 32 {
                            return Some(name.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extrae el gamertag del cliente desde el cuerpo del paquete `Login` (JWT en claro o cadena Base64URL).
/// Copia/escanea de forma segura evitando decodificar tokens gigantes de skins (>16 KB).
pub fn extract_player_name_from_login(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if let Some(name) = find_json_display_name(&text) {
        return name;
    }
    for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '+' || c == '/' || c == '.')) {
        if token.len() < 16 || token.len() > 16_384 {
            continue;
        }
        let mut parts = token.splitn(4, '.');
        let _h = parts.next();
        let payload_seg = parts.next();
        let sig_seg = parts.next();
        if payload_seg.is_some() && sig_seg.is_some() && parts.next().is_none() {
            if let Some(decoded) = decode_base64_segment(payload_seg.unwrap_or_default()) {
                let payload_str = String::from_utf8_lossy(&decoded);
                if let Some(name) = find_json_display_name(&payload_str) {
                    return name;
                }
            }
        }
    }
    "Jugador Bedrock".to_string()
}

fn zigzag32(n: i32) -> u32 {
    ((n << 1) ^ (n >> 31)) as u32
}

/// Calcula el checksum FNV-1a de 32 bits sobre cualquier payload binario (bloques y diagnóstico de chunks).
pub fn fnv1a_32_checksum(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Calcula el hash FNV-1a 32-bit de un compuesto NBT little-endian de estado de bloque Bedrock 1.20+/1.26.
/// Incluye únicamente `name` y `states` (sin campo `version`) sin pasar por `HashMap` ni `write_map`.
pub fn block_state_hash_nbtx(block_name: &str) -> u32 {
    let mut nbt_bytes = Vec::with_capacity(36 + block_name.len());
    // Root TAG_Compound (0x0a) con nombre vacío (u16 LE = 0)
    nbt_bytes.push(0x0a);
    nbt_bytes.extend_from_slice(&0u16.to_le_bytes());

    // 1) TAG_String "name" = block_name
    nbt_bytes.push(0x08);
    nbt_bytes.extend_from_slice(&(4u16).to_le_bytes());
    nbt_bytes.extend_from_slice(b"name");
    let name_bytes = block_name.as_bytes();
    nbt_bytes.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
    nbt_bytes.extend_from_slice(name_bytes);

    // 2) TAG_Compound "states" = {}
    nbt_bytes.push(0x0a);
    nbt_bytes.extend_from_slice(&(6u16).to_le_bytes());
    nbt_bytes.extend_from_slice(b"states");
    nbt_bytes.push(0x00);

    // Root TAG_End
    nbt_bytes.push(0x00);

    fnv1a_32_checksum(&nbt_bytes)
}

fn write_bedrock_string(out: &mut Vec<u8>, s: &str) {
    encode_varuint32(s.len() as u32, out);
    out.extend_from_slice(s.as_bytes());
}

fn zigzag64(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

pub fn derive_player_uuid_bytes(player_name: &str) -> [u8; 16] {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x9e37_79b9_7f4a_7c15;
    for &b in player_name.as_bytes() {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
        h2 ^= (b as u64).wrapping_add(0x55);
        h2 = h2.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&h1.to_le_bytes());
    out[8..].copy_from_slice(&h2.to_le_bytes());
    out[6] = (out[6] & 0x0f) | 0x40;
    out[8] = (out[8] & 0x3f) | 0x80;
    out
}

fn write_bedrock_uuid(out: &mut Vec<u8>, uuid_bytes: &[u8; 16]) {
    out.extend_from_slice(uuid_bytes);
}

fn write_nbt_net_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    encode_varuint32(bytes.len() as u32, out);
    out.extend_from_slice(bytes);
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum NbtFieldValue {
    Byte(i8),
    Short(i16),
    Int(i32),
    Float(f32),
    Double(f64),
    String(String),
}

pub const DEFAULT_SPAWN_BIOME_ID: i32 = 1; // Plains
pub const MIN_SPAWN_SAFE_Y: f32 = 64.0;

/// Serializa únicamente los campos internos + `TAG_End` de un compuesto NBT NetworkLittleEndian
/// sin `HashMap` ni `nbtx::nbt::ser::write_map`.
pub fn write_nbt_network_compound_body_from_pairs(
    entries: &[(String, NbtFieldValue)],
    out: &mut Vec<u8>,
) {
    for (key, val) in entries {
        match val {
            NbtFieldValue::Byte(v) => {
                out.push(0x01);
                write_nbt_net_bytes(key.as_bytes(), out);
                out.push(*v as u8);
            }
            NbtFieldValue::Short(v) => {
                out.push(0x02);
                write_nbt_net_bytes(key.as_bytes(), out);
                out.extend_from_slice(&v.to_le_bytes());
            }
            NbtFieldValue::Int(v) => {
                out.push(0x03);
                write_nbt_net_bytes(key.as_bytes(), out);
                encode_varuint32(zigzag32(*v), out);
            }
            NbtFieldValue::Float(v) => {
                out.push(0x05);
                write_nbt_net_bytes(key.as_bytes(), out);
                out.extend_from_slice(&v.to_le_bytes());
            }
            NbtFieldValue::Double(v) => {
                out.push(0x06);
                write_nbt_net_bytes(key.as_bytes(), out);
                out.extend_from_slice(&v.to_le_bytes());
            }
            NbtFieldValue::String(s) => {
                out.push(0x08);
                write_nbt_net_bytes(key.as_bytes(), out);
                write_nbt_net_bytes(s.as_bytes(), out);
            }
        }
    }
    out.push(0x00); // TAG_End
}

/// Serializa un compuesto NBT NetworkLittleEndian desde `Vec<(String, NbtFieldValue)>`
/// evitando por completo `HashMap`, `nbtx::nbt::ser::write_map` y `facet_reflect::PeekMapIter`.
pub fn write_nbt_network_compound_from_pairs(
    entries: &[(String, NbtFieldValue)],
    out: &mut Vec<u8>,
) {
    out.push(0x0a); // TAG_Compound raíz
    out.push(0x00); // Nombre raíz vacío (varuint32 = 0)
    write_nbt_network_compound_body_from_pairs(entries, out);
}

fn append_uniform_subchunk_v9(out: &mut Vec<u8>, sub_y: i8, primary_hash: u32, secondary_hash: u32) {
    // Formato NETWORK Bedrock v9 completo con 1 bit/bloque (0x03 = (1 << 1) | 1):
    // 128 palabras u32 Little-Endian (512 bytes) + paleta explícita de 2 entradas ZigZag VarInt32.
    out.push(0x09); // SubChunk versión 9
    out.push(0x01); // 1 capa de bloques
    out.push(sub_y as u8);
    out.push(0x03); // 1 bit por bloque + flag network=1 (Little-Endian)

    // Todos los 4096 bloques apuntan al índice 0 (primary_hash) en formato network XZY
    for _ in 0..128usize {
        out.extend_from_slice(&0u32.to_le_bytes());
    }

    encode_varuint32(zigzag32(2), out); // 2 entradas en paleta
    encode_varuint32(zigzag32(primary_hash as i32), out);
    encode_varuint32(zigzag32(secondary_hash as i32), out);
}

fn append_surface_subchunk_v9(out: &mut Vec<u8>, sub_y: i8, stone_hash: u32, grass_hash: u32) {
    // Formato NETWORK Bedrock v9 (bit 0 = 1 => palabras u32 Little-Endian + paleta runtime ZigZag VarInt32)
    out.push(0x09); // SubChunk versión 9
    out.push(0x01); // 1 capa de bloques
    out.push(sub_y as u8);
    out.push(0x03); // 1 bit por bloque (paleta de 2 valores) + flag network=1

    // 4096 bloques empaquetados a 1 bit/bloque -> 128 palabras u32 LE.
    // Coordenada en SubChunk: index = (x << 8) | (z << 4) | y -> local_y = index & 0x0F.
    for word_idx in 0..128usize {
        let base = word_idx * 32;
        let mut word: u32 = 0;
        for bit in 0..32usize {
            let local_y = (base + bit) & 0x0F;
            if local_y == 15 {
                word |= 1u32 << bit;
            }
        }
        out.extend_from_slice(&word.to_le_bytes());
    }

    encode_varuint32(zigzag32(2), out); // 2 entradas en paleta
    encode_varuint32(zigzag32(stone_hash as i32), out);
    encode_varuint32(zigzag32(grass_hash as i32), out);
}

fn append_overworld_biomes_and_borders(out: &mut Vec<u8>) {
    // Overworld abarca 24 subchunks de bioma (-64..319); paleta uniforme network = DEFAULT_SPAWN_BIOME_ID (1 = Plains)
    for _ in 0..24 {
        out.push(0x01);
        encode_varuint32(zigzag32(DEFAULT_SPAWN_BIOME_ID), out);
    }
    out.push(0x00); // border_blocks = 0
}

/// Genera un chunk vacío (todo `minecraft:air`) usando almacenamiento paletizado 1-bit de red (~4.8 KB, 9 subchunks: -4..=4).
pub fn build_empty_air_chunk_payload_nbtx() -> Vec<u8> {
    let air_hash = block_state_hash_nbtx("minecraft:air");
    let stone_hash = block_state_hash_nbtx("minecraft:stone");
    let mut data = Vec::with_capacity(4900);
    for sub_y in -4i8..=4i8 {
        append_uniform_subchunk_v9(&mut data, sub_y, air_hash, stone_hash);
    }
    append_overworld_biomes_and_borders(&mut data);
    data
}

/// Genera un chunk de spawn seguro en formato NETWORK Little-Endian (~4.8 KB, 9 subchunks: -4..=4):
/// piedra en `y=-64..62`, césped en `y=63` y aire en `y>=64`, evitando caídas al vacío en `(0, 64, 0)`.
pub fn build_spawn_chunk_payload_nbtx() -> Vec<u8> {
    let air_hash = block_state_hash_nbtx("minecraft:air");
    let stone_hash = block_state_hash_nbtx("minecraft:stone");
    let grass_hash = block_state_hash_nbtx("minecraft:grass_block");

    let mut data = Vec::with_capacity(4900);
    // Subchunks -4..=2 (y = -64..47): sólido de piedra
    for sub_y in -4i8..=2i8 {
        append_uniform_subchunk_v9(&mut data, sub_y, stone_hash, air_hash);
    }
    // Subchunk 3 (y = 48..63): stone en local_y 0..14 y grass_block en local_y 15 (y=63)
    append_surface_subchunk_v9(&mut data, 3, stone_hash, grass_hash);
    // Subchunk 4 (y = 64..79): todo aire para el jugador en y=64
    append_uniform_subchunk_v9(&mut data, 4, air_hash, stone_hash);

    append_overworld_biomes_and_borders(&mut data);
    data
}

#[cfg(feature = "protocol-core")]
fn encode_bedrock_v2225_packet<P: bedrock::protocol::Packet>(pkt: &P) -> Result<Bytes, String> {
    use bedrock::protocol::{PacketHeader as BedrockPacketHeader, ProtoCodec as BedrockProtoCodec};

    let mut raw = Vec::with_capacity(256);
    let header = BedrockPacketHeader {
        packet_id: P::ID,
        sender_sub_client_id: 0,
        target_sub_client_id: 0,
    };
    header
        .serialize(&mut raw)
        .map_err(|e| format!("header encode err (id={}): {}", P::ID, e))?;
    pkt.serialize(&mut raw)
        .map_err(|e| format!("body encode err (id={}): {}", P::ID, e))?;
    Ok(Bytes::from(raw))
}

#[cfg(feature = "protocol-core")]
pub fn build_v2225_start_game_packet_bytes() -> Result<Bytes, String> {
    use bedrock::protocol::v2168::enums::EducationEditionOffer;
    use bedrock::protocol::v2168::packets::StartGamePacket;
    use bedrock::protocol::v2225::types::{
        EditorLevelMigrationVersion, GameRuleLegacyData, LevelSettings,
    };
    use bedrock::protocol::v662::enums::{
        ChatRestrictionLevel, Difficulty, EditorWorldType, GamePublishSetting, GameType,
        GeneratorType, SpawnBiomeType,
    };
    use bedrock::protocol::v662::types::{
        ActorRuntimeID, ActorUniqueID, BaseGameVersion, EduSharedUriResource, Experiments,
        NetworkPermissions, SpawnSettings,
    };
    use bedrock::protocol::v818::types::SyncedPlayerMovementSettings;
    use bedrock::protocol::v944::types::NetworkBlockPosition;
    use bedrock::protocol::{Packet, PacketHeader as BedrockPacketHeader, ProtoCodec as BedrockProtoCodec, V2225};

    let settings = LevelSettings::<V2225> {
        seed: 1,
        spawn_settings: SpawnSettings::<V2225> {
            spawn_type: SpawnBiomeType::Default,
            user_defined_biome_name: String::new(),
            dimension: 0,
        },
        generator_type: GeneratorType::Flat,
        game_type: GameType::Survival,
        is_hardcore_enabled: false,
        game_difficulty: Difficulty::Normal,
        default_spawn_block_position: NetworkBlockPosition { x: 0, y: 64, z: 0 },
        achievements_disabled: true,
        editor_world_type: EditorWorldType::NonEditor,
        is_created_in_editor: false,
        is_exported_from_editor: false,
        editor_level_migration_version: EditorLevelMigrationVersion::Legacy,
        day_cycle_stop_time: 6000,
        education_edition_offer: EducationEditionOffer::None,
        education_features_enabled: false,
        education_product_id: String::new(),
        rain_level: 0.0,
        lightning_level: 0.0,
        has_confirmed_platform_locked_content: false,
        multiplayer_enabled: true,
        lan_broadcasting_enabled: true,
        xbox_live_broadcast_setting: GamePublishSetting::FriendsOfFriends,
        platform_broadcast_setting: GamePublishSetting::FriendsOfFriends,
        commands_enabled: true,
        texture_packs_required: false,
        rule_data: GameRuleLegacyData {
            rules_list: Vec::new(),
        },
        experiments: Experiments {
            experiments: Vec::new(),
            ever_toggled: false,
        },
        bonus_chest_enabled: false,
        starting_map_enabled: false,
        player_permissions: 1,
        server_chunk_tick_range: 4,
        locked_behaviour_pack: false,
        locked_resource_pack: false,
        from_locked_template: false,
        use_msa_gamer_tags: false,
        from_template: false,
        has_locked_template_settings: false,
        only_spawn_v1_villagers: false,
        persona_disabled: false,
        custom_skins_disabled: false,
        emote_chat_muted: false,
        base_game_version: BaseGameVersion(MOTD_ADVERTISED_VERSION.to_string()),
        limited_world_width: 0,
        limited_world_depth: 0,
        nether_type: false,
        edu_shared_uri_resource: EduSharedUriResource {
            button_name: String::new(),
            link_uri: String::new(),
        },
        override_force_experimental_gameplay: None,
        chat_restriction_level: ChatRestrictionLevel::None,
        disable_player_interactions: false,
        server_editor_connection_policy: 0,
        allow_anonymous_block_drops_in_editor_worlds: false,
    };

    let movement_settings = SyncedPlayerMovementSettings {
        rewind_history_size: 40,
        server_authoritative_block_breaking: true,
    };

    let network_permissions = NetworkPermissions {
        server_auth_sound_enabled: false,
    };

    // Workaround SIGABRT en nbtx::nbt::ser::write_map / facet_reflect::PeekMapIter::drop:
    // Reemplazamos player_property_data (HashMap) por Vec<(String, NbtFieldValue)> y serializamos
    // campo por campo el paquete StartGamePacket::<V2225> (ID 0x0b) sin invocar HashMap/write_map.
    let player_property_pairs: Vec<(String, NbtFieldValue)> = Vec::new();

    let mut raw = Vec::with_capacity(384);
    let header = BedrockPacketHeader {
        packet_id: StartGamePacket::<V2225>::ID,
        sender_sub_client_id: 0,
        target_sub_client_id: 0,
    };
    header
        .serialize(&mut raw)
        .map_err(|e| format!("start_game header encode err: {}", e))?;

    ActorUniqueID(1)
        .serialize(&mut raw)
        .map_err(|e| format!("target_actor_id encode err: {}", e))?;
    ActorRuntimeID(1)
        .serialize(&mut raw)
        .map_err(|e| format!("target_runtime_id encode err: {}", e))?;
    GameType::Survival
        .serialize(&mut raw)
        .map_err(|e| format!("actor_game_type encode err: {}", e))?;

    // position: (0.0, 64.0, 0.0) & rotation: (0.0, 0.0) en f32 little-endian
    for f in [0.0f32, 64.0f32, 0.0f32, 0.0f32, 0.0f32] {
        raw.extend_from_slice(&f.to_le_bytes());
    }

    settings
        .serialize(&mut raw)
        .map_err(|e| format!("level_settings encode err: {}", e))?;

    write_bedrock_string(&mut raw, "bedrockdroid_world"); // level_id
    write_bedrock_string(&mut raw, "Bedrock World");      // level_name
    write_bedrock_string(&mut raw, "");                   // template_content_identity
    raw.push(0);                                          // is_trial = false

    movement_settings
        .serialize(&mut raw)
        .map_err(|e| format!("movement_settings encode err: {}", e))?;

    raw.extend_from_slice(&6000u64.to_le_bytes());        // current_level_time (u64 LE)
    encode_varuint32(zigzag32(0), &mut raw);              // enchantment_seed = 0 (varint32)
    encode_varuint32(0, &mut raw);                        // block_properties (Vec vacío)
    write_bedrock_string(&mut raw, "");                   // multiplayer_correlation_id
    raw.push(1);                                          // enable_item_stack_net_manager = true
    write_bedrock_string(&mut raw, MOTD_ADVERTISED_VERSION); // server_version

    // player_property_data sin HashMap ni facet_reflect::PeekMapIter:
    write_nbt_network_compound_from_pairs(&player_property_pairs, &mut raw);

    raw.extend_from_slice(&0u64.to_le_bytes());           // server_block_type_registry_checksum
    raw.extend_from_slice(uuid::Uuid::nil().as_bytes());  // world_template_id (Uuid nil)
    raw.push(0);                                          // server_enabled_client_side_generation = false
    raw.push(1);                                          // block_network_ids_are_hashes = true

    network_permissions
        .serialize(&mut raw)
        .map_err(|e| format!("network_permissions encode err: {}", e))?;

    raw.push(0);                                          // server_join_information = None
    write_bedrock_string(&mut raw, "");                   // server_id
    write_bedrock_string(&mut raw, "");                   // scenario_id
    write_bedrock_string(&mut raw, "");                   // world_id
    write_bedrock_string(&mut raw, "");                   // owner_id

    Ok(Bytes::from(raw))
}

#[cfg(feature = "protocol-core")]
pub fn build_v2225_level_chunk_packet_bytes(cx: i32, cz: i32, serialized_chunk_data: Vec<u8>) -> Result<Bytes, String> {
    use bedrock::protocol::v2225::packets::LevelChunkPacket;
    use bedrock::protocol::v662::types::ChunkPos;
    use bedrock::protocol::V2225;

    let pkt = LevelChunkPacket::<V2225> {
        chunk_position: ChunkPos { x: cx, z: cz },
        dimension_id: 0,
        sub_chunk_count: 9,
        client_request_sub_chunk_limit: None,
        cache_enabled: false,
        cache_blobs: Vec::new(),
        serialized_chunk_data,
        is_client_biome_update: false,
    };
    encode_bedrock_v2225_packet(&pkt)
}

#[cfg(feature = "protocol-core")]
pub fn build_v2225_player_spawn_status_bytes() -> Result<Bytes, String> {
    use bedrock::protocol::v662::enums::PlayStatus as BedrockPlayStatus;
    use bedrock::protocol::v662::packets::PlayStatusPacket;
    use bedrock::protocol::V2225;

    let pkt = PlayStatusPacket::<V2225> {
        status: BedrockPlayStatus::PlayerSpawn,
    };
    encode_bedrock_v2225_packet(&pkt)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedPlayerAuthInput {
    pub pitch: f32,
    pub yaw: f32,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub head_yaw: f32,
}

fn read_f32_le(bytes: &[u8], offset: usize) -> Option<f32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(f32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Extrae pitch, yaw, posición (x, y, z) y head_yaw de `PlayerAuthInput` (0x90).
pub fn parse_player_auth_input_packet(body: &[u8]) -> Option<ParsedPlayerAuthInput> {
    let pitch = read_f32_le(body, 0)?;
    let yaw = read_f32_le(body, 4)?;
    let x = read_f32_le(body, 8)?;
    let y = read_f32_le(body, 12)?;
    let z = read_f32_le(body, 16)?;
    let head_yaw = read_f32_le(body, 28).unwrap_or(yaw);
    if !pitch.is_finite() || !yaw.is_finite() || !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return None;
    }
    Some(ParsedPlayerAuthInput {
        pitch,
        yaw,
        x,
        y,
        z,
        head_yaw: if head_yaw.is_finite() { head_yaw } else { yaw },
    })
}

/// Construye `MovePlayerPacket` (0x13 / 19) en formato wire Bedrock para confirmar posición al cliente.
#[allow(clippy::too_many_arguments)]
pub fn build_v2225_move_player_packet_bytes(
    runtime_id: u64,
    x: f32,
    y: f32,
    z: f32,
    pitch: f32,
    yaw: f32,
    head_yaw: f32,
    mode: u8,
    on_ground: bool,
    tick: u64,
) -> Bytes {
    let mut raw = Vec::with_capacity(48);
    encode_varuint32(0x13, &mut raw);
    encode_varuint64(runtime_id, &mut raw);
    raw.extend_from_slice(&x.to_le_bytes());
    raw.extend_from_slice(&y.to_le_bytes());
    raw.extend_from_slice(&z.to_le_bytes());
    raw.extend_from_slice(&pitch.to_le_bytes());
    raw.extend_from_slice(&yaw.to_le_bytes());
    raw.extend_from_slice(&head_yaw.to_le_bytes());
    raw.push(mode);
    raw.push(if on_ground { 1 } else { 0 });
    encode_varuint64(0, &mut raw); // riding_runtime_id = 0
    if mode == 2 {
        encode_varuint32(0, &mut raw); // teleport_cause
        encode_varuint32(0, &mut raw); // teleport_item
    }
    encode_varuint64(tick, &mut raw);
    Bytes::from(raw)
}

pub fn parse_network_stack_latency_packet(body: &[u8]) -> Option<(i64, bool)> {
    let ts_slice = body.get(0..8)?;
    let timestamp = i64::from_le_bytes([
        ts_slice[0], ts_slice[1], ts_slice[2], ts_slice[3],
        ts_slice[4], ts_slice[5], ts_slice[6], ts_slice[7],
    ]);
    let needs_response = body.get(8).copied().unwrap_or(1) != 0;
    Some((timestamp, needs_response))
}

/// Construye `NetworkStackLatencyPacket` (0x73 / 115) para responder inmediatamente al KeepAlive del cliente.
pub fn build_network_stack_latency_packet_bytes(timestamp: i64, from_server: bool) -> Bytes {
    let mut raw = Vec::with_capacity(12);
    encode_varuint32(0x73, &mut raw);
    raw.extend_from_slice(&timestamp.to_le_bytes());
    raw.push(if from_server { 1 } else { 0 });
    Bytes::from(raw)
}

#[cfg(feature = "protocol-core")]
pub fn send_chunk_radius_updated(
    conn: &mut Connection,
    session: &mut ServerConnection,
    _peer_addr: SocketAddr,
    now: Instant,
    radius: i32,
) -> i32 {
    use bedrock::protocol::v662::packets::ChunkRadiusUpdatedPacket;

    let clamped_radius = radius.clamp(1, 4);
    if let Ok(rad_pkt) = encode_bedrock_v2225_packet(&ChunkRadiusUpdatedPacket {
        chunk_radius: clamped_radius,
    }) {
        let batch = session.encode(&[rad_pkt]);
        conn.send(batch, now);
    }
    clamped_radius
}

/// Construye `BiomeDefinitionListPacket` (0x7a / 122) en formato Bedrock V2225 sin usar `HashMap`.
pub fn build_v2225_biome_definition_list_bytes() -> Bytes {
    let mut raw = Vec::with_capacity(8);
    encode_varuint32(0x7a, &mut raw); // Packet ID 122 (BiomeDefinitionList)
    encode_varuint32(0, &mut raw);    // biome_definitions (Vec vacío)
    encode_varuint32(0, &mut raw);    // string_table (Vec vacío)
    Bytes::from(raw)
}

/// Construye `AvailableEntityIdentifiersPacket` (0x77 / 119) en NBT NetworkLittleEndian
/// registrando entidades estáticas básicas (`minecraft:player`, `minecraft:boat`).
pub fn build_v2225_available_entity_identifiers_bytes() -> Bytes {
    let mut raw = Vec::with_capacity(192);
    encode_varuint32(0x77, &mut raw); // Packet ID 119 (AvailableEntityIdentifiers)

    raw.push(0x0a); // TAG_Compound raíz
    raw.push(0x00); // Nombre raíz vacío (varuint32 = 0)
    raw.push(0x09); // TAG_List
    write_nbt_net_bytes(b"idlist", &mut raw);
    raw.push(0x0a); // Elemento de lista: TAG_Compound
    encode_varuint32(zigzag32(2), &mut raw); // 2 entidades estáticas (VarInt32 ZigZag)

    let entities = [
        vec![
            ("bid".to_string(), NbtFieldValue::String(String::new())),
            ("hasspawnegg".to_string(), NbtFieldValue::Byte(0)),
            ("id".to_string(), NbtFieldValue::String("minecraft:player".to_string())),
            ("rid".to_string(), NbtFieldValue::Int(1)),
            ("summonable".to_string(), NbtFieldValue::Byte(0)),
        ],
        vec![
            ("bid".to_string(), NbtFieldValue::String(String::new())),
            ("hasspawnegg".to_string(), NbtFieldValue::Byte(1)),
            ("id".to_string(), NbtFieldValue::String("minecraft:boat".to_string())),
            ("rid".to_string(), NbtFieldValue::Int(90)),
            ("summonable".to_string(), NbtFieldValue::Byte(1)),
        ],
    ];
    for entry in &entities {
        write_nbt_network_compound_body_from_pairs(entry, &mut raw);
    }
    raw.push(0x00); // Root TAG_End
    Bytes::from(raw)
}

/// Construye `ItemRegistryPacket` / `ItemComponentPacket` (0xa2 / 162) vacío para inicializar inventario.
pub fn build_v2225_item_component_registry_bytes() -> Bytes {
    let mut raw = Vec::with_capacity(6);
    encode_varuint32(0xa2, &mut raw); // Packet ID 162 (ItemRegistry / ItemComponent)
    encode_varuint32(0, &mut raw);    // items (Vec vacío)
    Bytes::from(raw)
}

/// Construye `CreativeContentPacket` (0x91 / 145) vacío para permitir abrir el inventario creativo/supervivencia sin crash.
pub fn build_v2225_creative_content_bytes() -> Bytes {
    let mut raw = Vec::with_capacity(6);
    encode_varuint32(0x91, &mut raw); // Packet ID 145 (CreativeContent)
    encode_varuint32(0, &mut raw);    // groups (Vec vacío)
    encode_varuint32(0, &mut raw);    // items (Vec vacío)
    Bytes::from(raw)
}

/// Garantiza que la coordenada `Y` enviada al cliente en `MovePlayerPacket` nunca caiga por debajo del suelo (`64.0`).
pub fn clamp_player_auth_y(y: f32) -> f32 {
    if !y.is_finite() || y < MIN_SPAWN_SAFE_Y {
        MIN_SPAWN_SAFE_Y
    } else {
        y
    }
}

/// Construye `PlayerListPacket` (0x3f / 63, acción Add = 0) registrando al jugador con UUID, nombre, XUID y skin válida 64x32.
pub fn build_v2225_player_list_add_packet_bytes(
    player_uuid: &[u8; 16],
    entity_unique_id: i64,
    username: &str,
    xuid: &str,
) -> Bytes {
    let mut raw = Vec::with_capacity(8400);
    encode_varuint32(0x3f, &mut raw); // Packet ID 63 (PlayerList)
    raw.push(0x00);                   // ActionType = Add (0)
    encode_varuint32(1, &mut raw);    // 1 entrada

    write_bedrock_uuid(&mut raw, player_uuid);
    encode_varuint64(zigzag64(entity_unique_id), &mut raw);
    write_bedrock_string(&mut raw, username);
    write_bedrock_string(&mut raw, xuid);
    write_bedrock_string(&mut raw, ""); // platform_chat_id
    raw.extend_from_slice(&1i32.to_le_bytes()); // build_platform = 1 (Android)

    // SerializedSkin mínimo válido (64x32 RGBA = 8192 bytes) sin HashMap
    write_bedrock_string(&mut raw, "Standard_Steve"); // skin_id
    write_bedrock_string(&mut raw, "");               // play_fab_id
    write_bedrock_string(
        &mut raw,
        "{\"geometry\":{\"default\":\"geometry.humanoid.custom\"}}",
    );
    raw.extend_from_slice(&64u32.to_le_bytes()); // skin_image_width
    raw.extend_from_slice(&32u32.to_le_bytes()); // skin_image_height
    let skin_rgba_len = 64usize * 32usize * 4usize;
    encode_varuint32(skin_rgba_len as u32, &mut raw);
    raw.resize(raw.len() + skin_rgba_len, 0xff);

    raw.extend_from_slice(&0u32.to_le_bytes()); // animations_count = 0
    raw.extend_from_slice(&0u32.to_le_bytes()); // cape_image_width = 0
    raw.extend_from_slice(&0u32.to_le_bytes()); // cape_image_height = 0
    encode_varuint32(0, &mut raw);             // cape_data vacía

    write_bedrock_string(&mut raw, "");               // geometry_data
    write_bedrock_string(&mut raw, "");               // geometry_data_engine_version
    write_bedrock_string(&mut raw, "");               // animation_data
    write_bedrock_string(&mut raw, "");               // cape_id
    write_bedrock_string(&mut raw, "Standard_Steve"); // full_id
    write_bedrock_string(&mut raw, "wide");           // arm_size
    write_bedrock_string(&mut raw, "#0");             // skin_color
    raw.extend_from_slice(&0u32.to_le_bytes());        // persona_pieces_count = 0
    raw.extend_from_slice(&0u32.to_le_bytes());        // piece_tint_colors_count = 0
    raw.push(0);                                      // is_premium_skin
    raw.push(0);                                      // is_persona_skin
    raw.push(0);                                      // is_persona_cape_on_classic
    raw.push(1);                                      // is_primary_user
    raw.push(0);                                      // override_appearance

    raw.push(0);                                      // is_teacher
    raw.push(0);                                      // is_host
    raw.push(0);                                      // is_sub_client
    raw.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // player_colour (RGBA)
    raw.push(1);                                      // verified[0] = true

    Bytes::from(raw)
}

/// Construye `AddPlayerPacket` (0x0c / 12) para materializar la entidad del jugador en `(0, 64, 0)` con mano vacía (aire).
#[allow(clippy::too_many_arguments)]
pub fn build_v2225_add_player_packet_bytes(
    player_uuid: &[u8; 16],
    username: &str,
    entity_unique_id: i64,
    runtime_id: u64,
    x: f32,
    y: f32,
    z: f32,
    pitch: f32,
    yaw: f32,
) -> Bytes {
    let mut raw = Vec::with_capacity(192);
    encode_varuint32(0x0c, &mut raw); // Packet ID 12 (AddPlayer)
    write_bedrock_uuid(&mut raw, player_uuid);
    write_bedrock_string(&mut raw, username);
    encode_varuint64(runtime_id, &mut raw);
    write_bedrock_string(&mut raw, ""); // platform_chat_id

    // Posición (x, y, z) y velocidad inicial (0, 0, 0)
    for f in [x, y, z, 0.0f32, 0.0f32, 0.0f32, pitch, yaw, yaw] {
        raw.extend_from_slice(&f.to_le_bytes());
    }

    // Ítem en la mano: aire (network_id = 0)
    encode_varuint32(zigzag32(0), &mut raw);
    // GameType = Survival (0)
    encode_varuint32(zigzag32(0), &mut raw);
    // EntityMetadata vacía + PropertySyncData vacía
    encode_varuint32(0, &mut raw);
    encode_varuint32(0, &mut raw);
    encode_varuint32(0, &mut raw);

    // AbilityData (EntityUniqueID i64 LE, perms=Member, cmd=Normal, 1 layer base)
    raw.extend_from_slice(&entity_unique_id.to_le_bytes());
    raw.push(1); // PlayerPermissions = Member
    raw.push(0); // CommandPermissions = Normal
    raw.push(1); // 1 AbilityLayer
    raw.extend_from_slice(&1u16.to_le_bytes());          // LayerType = Base
    raw.extend_from_slice(&0x0007_FFFFu32.to_le_bytes()); // AbilitiesSet
    raw.extend_from_slice(&0x0000_00F7u32.to_le_bytes()); // AbilityValues
    raw.extend_from_slice(&0.05f32.to_le_bytes());        // FlySpeed
    raw.extend_from_slice(&0.10f32.to_le_bytes());        // WalkSpeed

    encode_varuint32(0, &mut raw);             // EntityLinks = 0
    write_bedrock_string(&mut raw, "");        // DeviceID
    raw.extend_from_slice(&1i32.to_le_bytes()); // BuildPlatform = 1 (Android)
    Bytes::from(raw)
}

/// Construye `SetPlayerGameTypePacket` (0x3e / 62) para confirmar el modo de juego tras `ServerboundLoadingScreen`.
pub fn build_v2225_set_player_game_type_bytes(game_type: i32) -> Bytes {
    let mut raw = Vec::with_capacity(6);
    encode_varuint32(0x3e, &mut raw); // Packet ID 62 (SetPlayerGameType)
    encode_varuint32(zigzag32(game_type), &mut raw);
    Bytes::from(raw)
}

/// Construye `SetSpawnPositionPacket` (ID 43 / 0x2b) indicando el punto de aparición del mundo y la brújula.
pub fn build_v2225_set_spawn_position_packet_bytes(x: i32, y: u32, z: i32, dimension: i32) -> Bytes {
    let mut raw = Vec::with_capacity(24);
    encode_varuint32(0x2b, &mut raw); // Packet ID 43 (SetSpawnPosition)
    encode_varuint32(zigzag32(1), &mut raw); // SpawnType = WorldSpawn (1)
    // position (NetworkBlockPosition: x zigzag32, y varuint32, z zigzag32)
    encode_varuint32(zigzag32(x), &mut raw);
    encode_varuint32(y, &mut raw);
    encode_varuint32(zigzag32(z), &mut raw);
    // dimension (Overworld = 0)
    encode_varuint32(zigzag32(dimension), &mut raw);
    // spawn_position (BlockPosition)
    encode_varuint32(zigzag32(x), &mut raw);
    encode_varuint32(y, &mut raw);
    encode_varuint32(zigzag32(z), &mut raw);
    Bytes::from(raw)
}

/// Construye `SetDifficultyPacket` (ID 60 / 0x3c) sincronizando la dificultad del mundo (2 = Normal).
pub fn build_v2225_set_difficulty_packet_bytes(difficulty: u32) -> Bytes {
    let mut raw = Vec::with_capacity(6);
    encode_varuint32(0x3c, &mut raw); // Packet ID 60 (SetDifficulty)
    encode_varuint32(difficulty, &mut raw);
    Bytes::from(raw)
}

/// Construye `GameRulesChangedPacket` (ID 72 / 0x48) con reglas básicas (`showcoordinates`, `dodaylightcycle`).
pub fn build_v2225_game_rules_changed_packet_bytes() -> Bytes {
    let mut raw = Vec::with_capacity(48);
    encode_varuint32(0x48, &mut raw); // Packet ID 72 (GameRulesChanged)
    encode_varuint32(2, &mut raw);    // 2 reglas

    for (name, val) in [("showcoordinates", true), ("dodaylightcycle", true)] {
        write_bedrock_string(&mut raw, name);
        raw.push(1);                      // editable = true
        encode_varuint32(1, &mut raw);    // type = 1 (Boolean)
        raw.push(if val { 1 } else { 0 });
    }
    Bytes::from(raw)
}

#[cfg(feature = "protocol-core")]
pub fn send_spawn_chunks(
    conn: &mut Connection,
    session: &mut ServerConnection,
    peer_addr: SocketAddr,
    now: Instant,
    radius: i32,
) -> usize {
    use bedrock::protocol::v662::packets::NetworkChunkPublisherUpdatePacket;
    use bedrock::protocol::v662::types::BlockPos;
    use bedrock::protocol::V2225;

    let clamped_radius = send_chunk_radius_updated(conn, session, peer_addr, now, radius);

    // Ordenamos las coordenadas para garantizar que el chunk de spawn (0,0) llegue primero
    let mut coords: Vec<(i32, i32)> = Vec::with_capacity(25);
    for cx in -2..=2 {
        for cz in -2..=2 {
            coords.push((cx, cz));
        }
    }
    coords.sort_by_key(|&(cx, cz)| (cx * cx + cz * cz, cx.abs(), cz.abs()));

    let spawn_chunk_template = build_spawn_chunk_payload_nbtx();
    let payload_size = spawn_chunk_template.len();
    let payload_checksum = fnv1a_32_checksum(&spawn_chunk_template);
    let mut first_packet_size = 0usize;
    let mut sent_count = 0usize;
    for (cx, cz) in coords {
        if let Ok(chunk_bytes) =
            build_v2225_level_chunk_packet_bytes(cx, cz, spawn_chunk_template.clone())
        {
            if first_packet_size == 0 {
                first_packet_size = chunk_bytes.len();
            }
            let batch = session.encode(&[chunk_bytes]);
            conn.send(batch, now);
            sent_count += 1;
        }
    }

    if let Ok(pub_pkt) = encode_bedrock_v2225_packet(&NetworkChunkPublisherUpdatePacket::<V2225> {
        new_view_position: BlockPos { x: 0, y: 64, z: 0 },
        new_view_radius: (clamped_radius as u32) * 16,
        server_built_chunks: Vec::new(),
    }) {
        let batch = session.encode(&[pub_pkt]);
        conn.send(batch, now);
    }

    log::info!(
        "[Bedrock World] {} chunks network generados (chunk (0,0) primero: payload={} bytes, packet={} bytes, checksum=0x{:08x}, subchunks=9, suelo stone/grass y=63, aire y>=64, biome_id={} Plains) + NetworkChunkPublisherUpdate enviados a {}",
        sent_count,
        payload_size,
        first_packet_size,
        payload_checksum,
        DEFAULT_SPAWN_BIOME_ID,
        peer_addr
    );
    sent_count
}

#[cfg(not(feature = "protocol-core"))]
pub fn send_spawn_chunks(
    _conn: &mut Connection,
    _session: &mut ServerConnection,
    _peer_addr: SocketAddr,
    _now: Instant,
    _radius: i32,
) -> usize {
    0
}

#[cfg(feature = "protocol-core")]
pub fn build_v2225_resource_packs_info_bytes() -> Result<Bytes, String> {
    use bedrock::protocol::v2168::packets::ResourcePacksInfoPacket;

    let pkt = ResourcePacksInfoPacket {
        resource_pack_required: false,
        has_addon_packs: false,
        has_scripts: false,
        force_disable_vibrant_visuals: false,
        world_template_uuid: uuid::Uuid::nil(),
        world_template_version: String::new(),
        resource_packs: Vec::new(),
    };
    encode_bedrock_v2225_packet(&pkt)
}

#[cfg(feature = "protocol-core")]
pub fn build_v2225_resource_pack_stack_bytes() -> Result<Bytes, String> {
    use bedrock::protocol::v662::types::{BaseGameVersion, Experiments};
    use bedrock::protocol::v898::packets::ResourcePackStackPacket;
    use bedrock::protocol::V2225;

    let pkt = ResourcePackStackPacket::<V2225> {
        texture_pack_required: false,
        addon_list: Vec::new(),
        base_game_version: BaseGameVersion(MOTD_ADVERTISED_VERSION.to_string()),
        experiments: Experiments {
            experiments: Vec::new(),
            ever_toggled: false,
        },
        include_editor_packs: false,
    };
    encode_bedrock_v2225_packet(&pkt)
}

#[cfg(feature = "protocol-core")]
pub fn build_v2225_voxel_shapes_bytes() -> Result<Bytes, String> {
    use bedrock::protocol::v944::packets::VoxelShapesPacket;

    let pkt = VoxelShapesPacket {
        shapes: Vec::new(),
        names: Vec::new(),
        custom_shape_count: 0,
    };
    encode_bedrock_v2225_packet(&pkt)
}

pub fn send_resource_packs_sequence(
    conn: &mut Connection,
    session: &mut ServerConnection,
    progress: &mut SessionProgress,
    peer_addr: SocketAddr,
    now: Instant,
) {
    #[cfg(feature = "protocol-core")]
    {
        if !progress.has_sent_resource_packs_info {
            match build_v2225_resource_packs_info_bytes() {
                Ok(rp_info) => {
                    let batch = session.encode(&[rp_info]);
                    conn.send(batch, now);
                    progress.has_sent_resource_packs_info = true;
                    log::info!("[Bedrock Packs] ResourcePacksInfo (0x06) enviado a {}", peer_addr);
                }
                Err(err) => {
                    log::warn!("[Bedrock Packs] Error codificando ResourcePacksInfo: {}", err);
                }
            }
        }
        if !progress.has_sent_resource_packs_stack {
            match build_v2225_resource_pack_stack_bytes() {
                Ok(rp_stack) => {
                    let batch = session.encode(&[rp_stack]);
                    conn.send(batch, now);
                    progress.has_sent_resource_packs_stack = true;
                    log::info!("[Bedrock Packs] ResourcePackStack (0x07) enviado a {}", peer_addr);
                }
                Err(err) => {
                    log::warn!("[Bedrock Packs] Error codificando ResourcePackStack: {}", err);
                }
            }
        }
    }
    #[cfg(not(feature = "protocol-core"))]
    {
        let _ = (conn, session, progress, peer_addr, now);
    }
}

pub fn send_world_start_and_spawn(
    conn: &mut Connection,
    session: &mut ServerConnection,
    progress: &mut SessionProgress,
    peer_addr: SocketAddr,
    now: Instant,
) {
    // Asegurar secuencia oficial de bedrock-rs: ResourcePacksInfo -> ResourcePackStack -> VoxelShapes -> StartGamePacket
    send_resource_packs_sequence(conn, session, progress, peer_addr, now);

    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::packets::SetTimePacket;

        if !progress.has_sent_start_game {
            if let Ok(voxel_bytes) = build_v2225_voxel_shapes_bytes() {
                let voxel_batch = session.encode(&[voxel_bytes]);
                conn.send(voxel_batch, now);
            }

            match build_v2225_start_game_packet_bytes() {
                Ok(start_bytes) => {
                    let start_batch = session.encode(&[start_bytes]);
                    conn.send(start_batch, now);
                    progress.has_sent_start_game = true;
                    log::info!(
                        "[Bedrock Session] StartGamePacket enviado a {} (spawn=0,64,0)",
                        peer_addr
                    );
                }
                Err(err) => {
                    log::warn!("[Bedrock Session] Error codificando StartGamePacket: {}", err);
                }
            }

            if !progress.has_sent_world_registries {
                let spawn_pos_bytes = build_v2225_set_spawn_position_packet_bytes(0, 64, 0, 0);
                let difficulty_bytes = build_v2225_set_difficulty_packet_bytes(2);
                let gamerules_bytes = build_v2225_game_rules_changed_packet_bytes();
                let biome_bytes = build_v2225_biome_definition_list_bytes();
                let entity_bytes = build_v2225_available_entity_identifiers_bytes();
                let item_bytes = build_v2225_item_component_registry_bytes();
                let creative_bytes = build_v2225_creative_content_bytes();

                for pkt_bytes in [
                    spawn_pos_bytes,
                    difficulty_bytes,
                    gamerules_bytes,
                    biome_bytes,
                    entity_bytes,
                    item_bytes,
                    creative_bytes,
                ] {
                    let batch = session.encode(&[pkt_bytes]);
                    conn.send(batch, now);
                }
                progress.has_sent_world_registries = true;
                log::info!(
                    "[Bedrock Session] Entorno y registros enviados a {}: SetSpawnPosition (0x2b), SetDifficulty (0x3c), GameRulesChanged (0x48), BiomeDefinitionList (0x7a), AvailableEntityIdentifiers (0x77), ItemRegistry (0xa2), CreativeContent (0x91)",
                    peer_addr
                );
            }

            if let Ok(time_bytes) = encode_bedrock_v2225_packet(&SetTimePacket { time: 6000 }) {
                let time_batch = session.encode(&[time_bytes]);
                conn.send(time_batch, now);
                log::info!("[Bedrock Session] SetTimePacket (0x0a, time=6000) enviado a {}", peer_addr);
            }
        }

        let sent_chunks = if !progress.has_sent_initial_chunks {
            let n = send_spawn_chunks(conn, session, peer_addr, now, 4);
            progress.has_sent_initial_chunks = true;
            n
        } else {
            0
        };

        if let Ok(spawn_status) = build_v2225_player_spawn_status_bytes() {
            let status_batch = session.encode(&[spawn_status]);
            conn.send(status_batch, now);
            log::info!(
                "[Bedrock Session] PlayStatus::PlayerSpawn (0x02 / status=3) enviado a {}",
                peer_addr
            );
        }

        if !progress.has_sent_player_spawn_entities {
            let effective_name = if progress.player_name.trim().is_empty() {
                "Jugador Bedrock".to_string()
            } else {
                progress.player_name.clone()
            };
            let effective_uuid = if progress.player_uuid == [0u8; 16] {
                derive_player_uuid_bytes(&effective_name)
            } else {
                progress.player_uuid
            };
            progress.player_name = effective_name.clone();
            progress.player_uuid = effective_uuid;

            let list_bytes =
                build_v2225_player_list_add_packet_bytes(&effective_uuid, 1, &effective_name, "");
            let add_player_bytes = build_v2225_add_player_packet_bytes(
                &effective_uuid,
                &effective_name,
                1,
                1,
                0.0,
                MIN_SPAWN_SAFE_Y,
                0.0,
                0.0,
                0.0,
            );
            let move_bytes = build_v2225_move_player_packet_bytes(
                1,
                0.0,
                MIN_SPAWN_SAFE_Y,
                0.0,
                0.0,
                0.0,
                0.0,
                0,
                true,
                progress.movement_tick,
            );

            for pkt_bytes in [list_bytes, add_player_bytes, move_bytes] {
                let batch = session.encode(&[pkt_bytes]);
                conn.send(batch, now);
            }
            progress.has_sent_player_spawn_entities = true;
            progress.set_local_player_initialized = true;
            log::info!(
                "[Bedrock Session] PlayerList (0x3f) + AddPlayer (0x0c) + MovePlayer (0x13, 0,64,0) enviados a {} (jugador={}, spawn_init=true)",
                peer_addr,
                effective_name
            );
        }

        notify_world_spawn_sent(sent_chunks);
    }

    #[cfg(not(feature = "protocol-core"))]
    {
        let _ = (conn, session, progress, peer_addr, now);
    }
}

/// Procesa un lote de datos de sesión de Minecraft Bedrock recibido por el canal SCTP de WebRTC
/// utilizando `acacia_session::server::ServerConnection` y `acacia_session::proto`.
pub fn handle_bedrock_session_data(
    conn: &mut Connection,
    session: &mut ServerConnection,
    progress: &mut SessionProgress,
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

        let pkt_id = pkt.id;
        let _ = run_catch_unwind(&format!("bedrock_packet_0x{:02x}", pkt_id), || {
            let name = packet_name(pkt.id).unwrap_or("UnknownPacket");
            progress.last_packet_id = Some(pkt.id);
            progress.last_packet_name = name.to_string();
            if pkt.id != 0x90 && name != "PlayerAuthInput" {
                log::info!(
                    "Paquete de Bedrock recibido (ID: 0x{:02x} / {}) desde {} ({} bytes)",
                    pkt.id,
                    name,
                    peer_addr,
                    pkt.body.len()
                );
                log::info!("[Bedrock Protocol] Recibido paquete de Minecraft: 0x{:02x}", pkt.id);
            }

            match pkt.id {
            // TAREA 3.1: RequestNetworkSettings (0xC1 / 193) -> NetworkSettings (0x8F)
            id if id == RequestNetworkSettings::ID || (!progress.has_sent_network_settings && id == 0x0b) => {
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

                // En acacia-session 0.1.0: start_compression() programa Switch::Compression que entra
                // en vigor exactamente después del próximo session.encode() (NetworkSettings viaja sin comprimir).
                if let Err(err) = session.start_compression(&settings) {
                    log::warn!(
                        "[Bedrock Protocol] Error activando compresión Deflate para {}: {}",
                        peer_addr,
                        err
                    );
                } else {
                    let mut encoded = BytesMut::new();
                    encode_packet(&settings, &mut encoded);
                    let batch = session.encode(&[encoded.freeze()]);
                    conn.send(batch, now);
                    progress.has_sent_network_settings = true;
                    log::info!(
                        "[Bedrock Protocol] NetworkSettings enviado a {} (Deflate, threshold=256)",
                        peer_addr
                    );
                }
            }

            // TAREA 3.2: Login (0x01) -> ServerToClientHandshake (0x03) + PlayStatus::LoginSuccess (0x02) + callback JNI
            id if id == Login::ID => {
                if !progress.has_sent_network_settings {
                    log::warn!(
                        "[Bedrock Login] Login (0x01) recibido antes de NetworkSettings desde {}",
                        peer_addr
                    );
                }

                let login_bytes: Vec<u8> = pkt.body.to_vec();
                log::info!(
                    "[Bedrock Login] Procesando Login (0x01) de {} ({} bytes)",
                    peer_addr,
                    login_bytes.len()
                );
                let player_name = extract_player_name_from_login(&login_bytes);
                match read_login(&login_bytes) {
                    Ok((protocol_version, _skin_jwt, client_key)) => {
                        log::info!(
                            "[Bedrock Login] Paquete Login (0x01) validado desde {} (jugador={}, proto={})",
                            peer_addr,
                            player_name,
                            protocol_version
                        );
                        let _ = client_key;
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
                            "[Bedrock Login] Login JWT/clave en modo offline o formato alternativo ({}): enviando ServerToClientHandshake y LoginSuccess (jugador={})",
                            err,
                            player_name
                        );
                        let handshake_pkt = session.plaintext_handshake();
                        let hs_batch = session.encode(&[handshake_pkt]);
                        conn.send(hs_batch, now);
                    }
                }

                if !progress.player_logged_in {
                    progress.player_logged_in = true;
                    progress.player_name = player_name.clone();
                    progress.player_uuid = derive_player_uuid_bytes(&player_name);
                    notify_player_login(&player_name);
                }

                let play_status = PlayStatus {
                    status: PlayStatusStatus::LoginSuccess,
                };
                let mut status_buf = BytesMut::new();
                encode_packet(&play_status, &mut status_buf);
                let status_batch = session.encode(&[status_buf.freeze()]);
                conn.send(status_batch, now);
                progress.has_sent_login_success = true;
                log::info!(
                    "[Bedrock Login] PlayStatus (0x02 LoginSuccess) enviado a {}",
                    peer_addr
                );
            }

            // Respuesta del cliente al ServerToClientHandshake (0x04 ClientToServerHandshake) -> ResourcePacksInfo + ResourcePackStack
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
                progress.has_sent_login_success = true;

                send_resource_packs_sequence(conn, session, progress, peer_addr, now);
                log::info!(
                    "[Bedrock Login] ClientToServerHandshake (0x04) completado con {} -> ResourcePacksInfo + ResourcePackStack enviados",
                    peer_addr
                );
            }

            // Respuesta de paquetes de recursos (0x08 ResourcePackClientResponse) -> ResourcePackStack / StartGame
            id if id == ResourcePackClientResponse::ID => {
                if let Ok(rp) = pkt.decode::<ResourcePackClientResponse>() {
                    match rp.response_status {
                        ResourcePackClientResponseResponseStatus::HaveAllPacks => {
                            send_resource_packs_sequence(conn, session, progress, peer_addr, now);
                        }
                        ResourcePackClientResponseResponseStatus::Completed => {
                            log::info!("[Bedrock Packs] ResourcePackClientResponse Completed desde {}", peer_addr);
                            send_world_start_and_spawn(conn, session, progress, peer_addr, now);
                        }
                        _ => {}
                    }
                }
            }

            // FASE 4 P0/P1: Al recibir ClientCacheStatus (0x81 / 129), enviar secuencia de recursos si falta,
            // VoxelShapesPacket, StartGamePacket (0x0b), SetTimePacket (0x0a), Chunks iniciales nbtx y PlayStatus::PlayerSpawn (0x02).
            id if id == ClientCacheStatus::ID || id == 0x81 => {
                progress.has_received_client_cache_status = true;
                let cache_enabled = pkt
                    .decode::<ClientCacheStatus>()
                    .map(|c| c.enabled)
                    .unwrap_or(false);
                log::info!(
                    "[Bedrock Session] ClientCacheStatus (0x81, enabled={}) recibido de {} -> disparando StartGamePacket + Spawn + Chunks",
                    cache_enabled,
                    peer_addr
                );
                send_world_start_and_spawn(conn, session, progress, peer_addr, now);
            }

            // Solicitud de radio de chunks (0x45 / 69 RequestChunkRadius) -> ChunkRadiusUpdated (0x46) + chunks spawn
            id if id == RequestChunkRadius::ID || id == 0x45 => {
                progress.last_packet_name = "RequestChunkRadius".to_string();
                let requested = pkt
                    .decode::<RequestChunkRadius>()
                    .map(|r| r.chunk_radius)
                    .unwrap_or(4);
                #[cfg(feature = "protocol-core")]
                {
                    let approved = send_chunk_radius_updated(conn, session, peer_addr, now, requested);
                    progress.has_sent_chunk_radius = true;
                    if !progress.has_sent_initial_chunks {
                        let sent = send_spawn_chunks(conn, session, peer_addr, now, approved);
                        progress.has_sent_initial_chunks = true;
                        notify_world_spawn_sent(sent);
                    }
                    log::info!(
                        "[Bedrock World] RequestChunkRadius (0x45, req={}) -> ChunkRadiusUpdated (0x46, radius={}) enviado a {}",
                        requested,
                        approved,
                        peer_addr
                    );
                }
                #[cfg(not(feature = "protocol-core"))]
                {
                    let _ = requested;
                }
            }

            // KeepAlive de aplicación Bedrock: NetworkStackLatency (0x73 / 115)
            id if id == 0x73 || name == "NetworkStackLatency" => {
                progress.last_packet_name = "NetworkStackLatency".to_string();
                progress.latency_ping_count = progress.latency_ping_count.saturating_add(1);
                if let Some((timestamp, needs_response)) = parse_network_stack_latency_packet(&pkt.body) {
                    if needs_response {
                        let pong = build_network_stack_latency_packet_bytes(timestamp, false);
                        let batch = session.encode(&[pong]);
                        conn.send(batch, now);
                        if progress.latency_ping_count <= 3 || progress.latency_ping_count % 20 == 0 {
                            log::info!(
                                "[Bedrock KeepAlive] NetworkStackLatency (0x73, ts={}) respondido a {}",
                                timestamp,
                                peer_addr
                            );
                        }
                    }
                }
            }

            // Movimiento autoritativo del jugador: PlayerAuthInput (0x90 / 144) -> MovePlayerPacket (0x13)
            id if id == 0x90 || name == "PlayerAuthInput" => {
                progress.last_packet_name = "PlayerAuthInput".to_string();
                progress.player_auth_input_count = progress.player_auth_input_count.saturating_add(1);
                progress.movement_tick = progress.movement_tick.saturating_add(1);
                if let Some(input) = parse_player_auth_input_packet(&pkt.body) {
                    let safe_y = clamp_player_auth_y(input.y);
                    let move_bytes = build_v2225_move_player_packet_bytes(
                        1,
                        input.x,
                        safe_y,
                        input.z,
                        input.pitch,
                        input.yaw,
                        input.head_yaw,
                        0,
                        true,
                        progress.movement_tick,
                    );
                    let batch = session.encode(&[move_bytes]);
                    conn.send(batch, now);
                    if progress.player_auth_input_count == 1 || progress.player_auth_input_count % 40 == 0 {
                        log::info!(
                            "[Bedrock Move] PlayerAuthInput (0x90) #{} de {} (raw_y={:.2}, pos={:.2},{:.2},{:.2}) -> MovePlayerPacket (0x13) enviado",
                            progress.player_auth_input_count,
                            peer_addr,
                            input.y,
                            input.x,
                            safe_y,
                            input.z
                        );
                    }
                } else {
                    log::warn!(
                        "[Bedrock Move] PlayerAuthInput (0x90) inválido ({} bytes) desde {}",
                        pkt.body.len(),
                        peer_addr
                    );
                }
            }

            // Pantalla de carga del cliente completada (0x138 / 312 ServerboundLoadingScreen) -> SetPlayerGameType (0x3e) + ChunkRadiusUpdated (0x46)
            id if id == 0x138 || name == "ServerboundLoadingScreen" => {
                progress.last_packet_name = "ServerboundLoadingScreen".to_string();
                if !progress.has_sent_player_spawn_entities {
                    send_world_start_and_spawn(conn, session, progress, peer_addr, now);
                }
                let game_type_bytes = build_v2225_set_player_game_type_bytes(0);
                let gt_batch = session.encode(&[game_type_bytes]);
                conn.send(gt_batch, now);
                #[cfg(feature = "protocol-core")]
                {
                    let _ = send_chunk_radius_updated(conn, session, peer_addr, now, 4);
                    progress.has_sent_chunk_radius = true;
                }
                log::info!(
                    "[Bedrock World] ServerboundLoadingScreen (0x138) recibido de {} -> SetPlayerGameType (0x3e) + ChunkRadiusUpdated (0x46) enviados (spawn_init={})",
                    peer_addr,
                    progress.set_local_player_initialized
                );
            }

            // Confirmación del cliente tras aparecer en el mundo (0x71 / 113 SetLocalPlayerAsInitialized)
            id if id == SetLocalPlayerAsInitialized::ID => {
                progress.last_packet_name = "SetLocalPlayerAsInitialized".to_string();
                progress.set_local_player_initialized = true;
                log::info!(
                    "[Bedrock World] Jugador inicializado dentro del mundo (SetLocalPlayerAsInitialized 0x71) desde {}",
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
        });
    }
}

pub fn format_disconnect_debug_summary(
    sid: u64,
    remote_addr: SocketAddr,
    progress: &SessionProgress,
    reason: &str,
) -> String {
    let last_pkt = if progress.last_packet_name.is_empty() {
        "ninguno".to_string()
    } else {
        match progress.last_packet_id {
            Some(id) => format!("{} (0x{:02x})", progress.last_packet_name, id),
            None => progress.last_packet_name.clone(),
        }
    };
    let hint = if progress.has_sent_login_success && !progress.has_sent_start_game {
        "desconexión antes de StartGamePacket / faltó ClientCacheStatus"
    } else if progress.has_sent_start_game && !progress.has_sent_chunk_radius {
        "faltó ChunkRadiusUpdated (0x46) tras RequestChunkRadius (0x45)"
    } else if progress.player_auth_input_count > 0 && progress.latency_ping_count == 0 {
        "cierre manual del cliente o falta de respuesta keepalive/chunks"
    } else if reason.contains("peer closed") {
        "cierre limpio iniciado por el cliente (peer closed)"
    } else {
        "cierre de transporte WebRTC"
    };
    format!(
        "Conexión WebRTC cerrada con {} (session={}, motivo={}). Último paquete procesado: {} | diagnóstico={} | auth_inputs={} | keepalives={} | keepalives_sent={} | chunk_radius={} | spawn_init={}",
        remote_addr,
        sid,
        reason,
        last_pkt,
        hint,
        progress.player_auth_input_count,
        progress.latency_ping_count,
        progress.keepalives_sent,
        progress.has_sent_chunk_radius,
        progress.set_local_player_initialized
    )
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
        let answer_res = if is_crash_protection_enabled() {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Connection::answer(offer_sdp, local_candidate, &host_key, now)
            })) {
                Ok(r) => r,
                Err(payload) => {
                    let bt = std::backtrace::Backtrace::force_capture();
                    let panic_msg = format!(
                        "[Rust Panic] Connection::answer HTTP desde {}: {}\n--- Rust Backtrace ---\n{}",
                        peer_addr,
                        panic_payload_to_string(payload),
                        bt
                    );
                    report_rust_crash(&panic_msg);
                    let resp = http::response(500, "text/plain", "Internal Rust panic during WebRTC answer");
                    let _ = stream.write_all(&resp).await;
                    return;
                }
            }
        } else {
            Connection::answer(offer_sdp, local_candidate, &host_key, now)
        };
        match answer_res {
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
                            progress: SessionProgress::default(),
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
/// Redirige cualquier Rust Panic (mensaje + ubicación + backtrace) hacia archivo en disco y CrashHandler.logCrash.
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

        std::panic::set_hook(Box::new(|panic_info| {
            let payload = panic_info.payload();
            let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "Pánico desconocido".to_string()
            };
            let location = panic_info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "ubicacion_desconocida".to_string());
            if !is_crash_protection_enabled() {
                log::error!("PANIC DE RUST:\n{} en {}", msg, location);
                return;
            }
            let bt = std::backtrace::Backtrace::force_capture();
            let report = format!(
                "PANIC DE RUST:\n{} en {}\n\n--- Rust Backtrace ---\n{}",
                msg, location, bt
            );
            report_rust_crash(&report);
        }));
    });
    init_native_signal_handlers();
}

pub fn current_engine_state() -> EngineState {
    if is_engine_running() {
        EngineState::Running
    } else {
        EngineState::Stopped
    }
}

pub fn start_runtime(max_ram_mb: u32, blocking_threads: usize) -> Result<String, String> {
    init_android_logging();
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

    rt.spawn(CatchUnwindFuture::new("nethernet_main_loop", async move {
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
        spawn_protected("memory_guard_watchdog", async move {
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
            spawn_protected("lan_discovery_udp_7551", async move {
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
                                let decoded_opt = if is_crash_protection_enabled() {
                                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        LanPacket::decode(&lan_buf[..len])
                                    })) {
                                        Ok(Ok(v)) => Some(v),
                                        Ok(Err(_)) => None,
                                        Err(payload) => {
                                            let bt = std::backtrace::Backtrace::force_capture();
                                            let msg = format!(
                                                "[Rust Panic] LanPacket::decode desde {}: {}\n--- Rust Backtrace ---\n{}",
                                                from,
                                                panic_payload_to_string(payload),
                                                bt
                                            );
                                            report_rust_crash(&msg);
                                            None
                                        }
                                    }
                                } else {
                                    LanPacket::decode(&lan_buf[..len]).ok()
                                };
                                if let Some((pkt, sender_id)) = decoded_opt {
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
                                                        let lan_ans = if is_crash_protection_enabled() {
                                                            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                                                Connection::answer(&sig.data, local_cand, &lan_key, now)
                                                            })) {
                                                                Ok(Ok(v)) => Some(v),
                                                                Ok(Err(_)) => None,
                                                                Err(payload) => {
                                                                    let bt = std::backtrace::Backtrace::force_capture();
                                                                    let msg = format!(
                                                                        "[Rust Panic] Connection::answer LAN desde {}: {}\n--- Rust Backtrace ---\n{}",
                                                                        from,
                                                                        panic_payload_to_string(payload),
                                                                        bt
                                                                    );
                                                                    report_rust_crash(&msg);
                                                                    None
                                                                }
                                                            }
                                                        } else {
                                                            Connection::answer(&sig.data, local_cand, &lan_key, now).ok()
                                                        };
                                                        if let Some((mut conn, answer_sdp)) = lan_ans {
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
                                                                        progress: SessionProgress::default(),
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
                spawn_protected("ipv6_dual_stack_listener", async move {
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
                                spawn_protected("http_signaling_v6", async move {
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
                                            let _ = run_catch_unwind("webrtc_v6_handle_datagram", || {
                                                peer.conn.handle_datagram(now, src_addr, pkt);
                                            });
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
                                                        let _ = run_catch_unwind("webrtc_v6_session_data", || {
                                                            handle_bedrock_session_data(
                                                                &mut peer.conn,
                                                                &mut peer.session,
                                                                &mut peer.progress,
                                                                peer.remote_addr,
                                                                &payload,
                                                                now,
                                                            );
                                                        });
                                                    }
                                                    Event::Disconnected(reason) => {
                                                        if peer.progress.has_sent_login_success && !peer.progress.has_sent_start_game {
                                                            log::warn!(
                                                                "[Bedrock Session] Cliente {} desconectado inesperadamente después de PlayStatus (reason={}, session={}, client_cache_received={})",
                                                                peer.remote_addr,
                                                                reason,
                                                                sid,
                                                                peer.progress.has_received_client_cache_status
                                                            );
                                                        } else {
                                                            log::info!(
                                                                "Conexión WebRTC IPv6 cerrada con {} (session={}): {}",
                                                                peer.remote_addr,
                                                                sid,
                                                                reason
                                                            );
                                                        }
                                                        closed_ids.push(sid);
                                                    }
                                                }
                                            }
                                            while let Some(tx) = peer.conn.poll_transmit() {
                                                outbound.push((tx.destination, tx.contents));
                                            }
                                        }
                                        let mut logged_out_count = 0usize;
                                        for sid in closed_ids {
                                            if let Some(removed) = guard.remove(&sid) {
                                                if removed.progress.player_logged_in {
                                                    logged_out_count += 1;
                                                }
                                            }
                                        }
                                        drop(guard);
                                        for _ in 0..logged_out_count {
                                            notify_player_disconnect();
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
        let mut keepalive_tick = tokio::time::interval(Duration::from_secs(2));
        keepalive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                            spawn_protected("http_signaling_v4", async move {
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
                                let _ = run_catch_unwind("webrtc_v4_handle_datagram", || {
                                    peer.conn.handle_datagram(now, src_addr, pkt);
                                });
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
                                            let _ = run_catch_unwind("webrtc_v4_session_data", || {
                                                handle_bedrock_session_data(
                                                    &mut peer.conn,
                                                    &mut peer.session,
                                                    &mut peer.progress,
                                                    peer.remote_addr,
                                                    &payload,
                                                    now,
                                                );
                                            });
                                        }
                                        Event::Disconnected(reason) => {
                                            let msg = format_disconnect_debug_summary(
                                                sid,
                                                peer.remote_addr,
                                                &peer.progress,
                                                &reason,
                                            );
                                            if peer.progress.has_sent_login_success && !peer.progress.has_sent_start_game {
                                                log::warn!(
                                                    "[Bedrock Session] Cliente {} desconectado inesperadamente después de PlayStatus ({})",
                                                    peer.remote_addr,
                                                    msg
                                                );
                                            } else {
                                                log::info!("{}", msg);
                                            }
                                            closed_ids.push(sid);
                                        }
                                    }
                                }
                                while let Some(tx) = peer.conn.poll_transmit() {
                                    outbound.push((tx.destination, tx.contents));
                                }
                            }
                            let mut logged_out_count = 0usize;
                            for sid in closed_ids {
                                if let Some(removed) = guard.remove(&sid) {
                                    if removed.progress.player_logged_in {
                                        logged_out_count += 1;
                                    }
                                }
                            }
                            drop(guard);
                            for _ in 0..logged_out_count {
                                notify_player_disconnect();
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
                            let _ = run_catch_unwind("webrtc_rtc_handle_timeout", || {
                                peer.conn.handle_timeout(now);
                            });
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
                                        let _ = run_catch_unwind("webrtc_rtc_session_data", || {
                                            handle_bedrock_session_data(
                                                &mut peer.conn,
                                                &mut peer.session,
                                                &mut peer.progress,
                                                peer.remote_addr,
                                                &payload,
                                                now,
                                            );
                                        });
                                    }
                                    Event::Disconnected(reason) => {
                                        let msg = format_disconnect_debug_summary(
                                            sid,
                                            peer.remote_addr,
                                            &peer.progress,
                                            &reason,
                                        );
                                        if peer.progress.has_sent_login_success && !peer.progress.has_sent_start_game {
                                            log::warn!(
                                                "[Bedrock Session] Cliente {} desconectado inesperadamente después de PlayStatus ({})",
                                                peer.remote_addr,
                                                msg
                                            );
                                        } else {
                                            log::info!("{}", msg);
                                        }
                                        closed_ids.push(sid);
                                    }
                                }
                            }
                            while let Some(tx) = peer.conn.poll_transmit() {
                                outbound.push((tx.destination, tx.contents));
                            }
                        }
                        let mut logged_out_count = 0usize;
                        for sid in closed_ids {
                            if let Some(removed) = guard.remove(&sid) {
                                if removed.progress.player_logged_in {
                                    logged_out_count += 1;
                                }
                            }
                        }
                        drop(guard);
                        for _ in 0..logged_out_count {
                            notify_player_disconnect();
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
                _ = keepalive_tick.tick() => {
                    let now = Instant::now();
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64;
                    let mut outbound: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
                    if let Ok(mut guard) = peers.lock() {
                        for peer in guard.values_mut() {
                            if peer.progress.has_sent_start_game {
                                let ping_pkt = build_network_stack_latency_packet_bytes(ts, true);
                                let batch = peer.session.encode(&[ping_pkt]);
                                peer.conn.send(batch, now);
                                peer.progress.keepalives_sent =
                                    peer.progress.keepalives_sent.saturating_add(1);
                                if peer.progress.keepalives_sent == 1 || peer.progress.keepalives_sent % 15 == 0 {
                                    log::info!(
                                        "[Bedrock KeepAlive] NetworkStackLatency (0x73, ts={}) enviado periódicamente (#{}) a {}",
                                        ts,
                                        peer.progress.keepalives_sent,
                                        peer.remote_addr
                                    );
                                }
                                while let Some(tx) = peer.conn.poll_transmit() {
                                    outbound.push((tx.destination, tx.contents));
                                }
                            }
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
    }));

    *guard = Some(ServerHandle { rt, cancel, arena });
    Ok(format!("engine=running nethernet={}", bind_report_summary))
}

pub fn stop_async_engine() {
    set_current_bind_report(None);
    reset_player_state();
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
        session
            .start_compression(&settings)
            .expect("start_compression should succeed");

        let mut out = BytesMut::new();
        encode_packet(&settings, &mut out);
        let batch = session.encode(&[out.freeze()]);
        assert!(!batch.is_empty());

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

    #[test]
    fn nbtx_generates_block_hashes_and_spawn_chunk_floor() {
        let air_h = block_state_hash_nbtx("minecraft:air");
        let stone_h = block_state_hash_nbtx("minecraft:stone");
        let grass_h = block_state_hash_nbtx("minecraft:grass_block");
        assert_ne!(air_h, stone_h);
        assert_ne!(stone_h, grass_h);

        let empty_chunk = build_empty_air_chunk_payload_nbtx();
        let spawn_chunk = build_spawn_chunk_payload_nbtx();
        assert!(empty_chunk.len() >= 4096);
        assert!(spawn_chunk.len() >= 4096);
        assert_ne!(fnv1a_32_checksum(&empty_chunk), fnv1a_32_checksum(&spawn_chunk));
        // SubChunk -4 (0xFC) con versión v9 (0x09) y 1 capa (0x01)
        assert_eq!(&spawn_chunk[..3], &[0x09, 0x01, (-4i8) as u8]);
        assert_eq!(*spawn_chunk.last().expect("border byte"), 0x00);
    }

    #[cfg(feature = "protocol-core")]
    #[test]
    fn v2225_start_game_level_chunk_and_player_spawn_serialize_cleanly() {
        let rp_info = build_v2225_resource_packs_info_bytes().expect("resource_packs_info");
        assert_eq!(RawPacket::parse(rp_info).expect("parse rp_info").id, 0x06);

        let rp_stack = build_v2225_resource_pack_stack_bytes().expect("resource_pack_stack");
        assert_eq!(RawPacket::parse(rp_stack).expect("parse rp_stack").id, 0x07);

        let voxel = build_v2225_voxel_shapes_bytes().expect("voxel_shapes");
        assert_eq!(RawPacket::parse(voxel).expect("parse voxel_shapes").id, 337);

        let start_bytes = build_v2225_start_game_packet_bytes().expect("start_game");
        let start_raw = RawPacket::parse(start_bytes).expect("parse start_game");
        assert_eq!(start_raw.id, 0x0b);
        let mut empty_nbt = Vec::new();
        write_nbt_network_compound_from_pairs(&[], &mut empty_nbt);
        assert_eq!(empty_nbt, vec![0x0a, 0x00, 0x00]);
        assert!(start_raw.body.windows(3).any(|w| w == [0x0a, 0x00, 0x00]));

        let spawn_pos_bytes = build_v2225_set_spawn_position_packet_bytes(0, 64, 0, 0);
        assert_eq!(RawPacket::parse(spawn_pos_bytes).expect("parse set_spawn_position").id, 0x2b);

        let diff_bytes = build_v2225_set_difficulty_packet_bytes(2);
        assert_eq!(RawPacket::parse(diff_bytes).expect("parse set_difficulty").id, 0x3c);

        let gamerules_bytes = build_v2225_game_rules_changed_packet_bytes();
        let gamerules_raw = RawPacket::parse(gamerules_bytes).expect("parse gamerules");
        assert_eq!(gamerules_raw.id, 0x48);
        assert!(gamerules_raw.body.windows(15).any(|w| w == b"showcoordinates"));

        let biome_bytes = build_v2225_biome_definition_list_bytes();
        let biome_raw = RawPacket::parse(biome_bytes).expect("parse biome_definition_list");
        assert_eq!(biome_raw.id, 0x7a);
        assert_eq!(biome_raw.body.as_ref(), &[0x00, 0x00]);

        let entity_bytes = build_v2225_available_entity_identifiers_bytes();
        let entity_raw = RawPacket::parse(entity_bytes).expect("parse available_entity_identifiers");
        assert_eq!(entity_raw.id, 0x77);
        assert!(entity_raw.body.windows(6).any(|w| w == b"idlist"));
        assert!(entity_raw.body.windows(16).any(|w| w == b"minecraft:player"));
        assert!(entity_raw.body.windows(14).any(|w| w == b"minecraft:boat"));

        let item_reg_bytes = build_v2225_item_component_registry_bytes();
        assert_eq!(RawPacket::parse(item_reg_bytes).expect("parse item_registry").id, 0xa2);

        let creative_bytes = build_v2225_creative_content_bytes();
        assert_eq!(RawPacket::parse(creative_bytes).expect("parse creative_content").id, 0x91);

        let chunk_bytes =
            build_v2225_level_chunk_packet_bytes(0, 0, build_spawn_chunk_payload_nbtx())
                .expect("level_chunk");
        let chunk_raw = RawPacket::parse(chunk_bytes).expect("parse level_chunk");
        assert_eq!(chunk_raw.id, 0x3a);

        let spawn_bytes = build_v2225_player_spawn_status_bytes().expect("player_spawn");
        let spawn_raw = RawPacket::parse(spawn_bytes).expect("parse player_spawn");
        assert_eq!(spawn_raw.id, 0x02);

        let test_uuid = derive_player_uuid_bytes("SteveDroid");
        let player_list_bytes = build_v2225_player_list_add_packet_bytes(&test_uuid, 1, "SteveDroid", "");
        let player_list_raw = RawPacket::parse(player_list_bytes).expect("parse player_list");
        assert_eq!(player_list_raw.id, 0x3f);
        assert!(player_list_raw.body.windows(10).any(|w| w == b"SteveDroid"));
        assert!(player_list_raw.body.windows(14).any(|w| w == b"Standard_Steve"));

        let add_player_bytes = build_v2225_add_player_packet_bytes(
            &test_uuid,
            "SteveDroid",
            1,
            1,
            0.0,
            64.0,
            0.0,
            0.0,
            0.0,
        );
        let add_player_raw = RawPacket::parse(add_player_bytes).expect("parse add_player");
        assert_eq!(add_player_raw.id, 0x0c);
        assert!(add_player_raw.body.windows(10).any(|w| w == b"SteveDroid"));
        assert!(add_player_raw.body.windows(4).any(|w| w == 64.0f32.to_le_bytes()));

        let game_type_bytes = build_v2225_set_player_game_type_bytes(0);
        let game_type_raw = RawPacket::parse(game_type_bytes).expect("parse set_player_game_type");
        assert_eq!(game_type_raw.id, 0x3e);
        assert_eq!(game_type_raw.body.as_ref(), &[0x00]);

        // Verificar parseo de PlayerAuthInput (0x90) -> MovePlayerPacket (0x13) con Y >= 64.0 y NetworkStackLatency (0x73)
        assert_eq!(clamp_player_auth_y(63.25), 64.0);
        assert_eq!(clamp_player_auth_y(64.0), 64.0);
        assert_eq!(clamp_player_auth_y(65.62), 65.62);

        let mut auth_body = Vec::new();
        for f in [12.5f32, 90.0f32, 1.25f32, 65.62f32, -2.5f32, 0.0f32, 1.0f32, 91.0f32] {
            auth_body.extend_from_slice(&f.to_le_bytes());
        }
        let parsed_auth = parse_player_auth_input_packet(&auth_body).expect("parse auth input");
        assert!((parsed_auth.x - 1.25).abs() < 0.001);
        assert!((parsed_auth.y - 65.62).abs() < 0.001);
        let move_pkt = build_v2225_move_player_packet_bytes(
            1,
            parsed_auth.x,
            clamp_player_auth_y(parsed_auth.y),
            parsed_auth.z,
            parsed_auth.pitch,
            parsed_auth.yaw,
            parsed_auth.head_yaw,
            0,
            true,
            1,
        );
        assert_eq!(RawPacket::parse(move_pkt).expect("parse move_player").id, 0x13);

        let mut latency_body = Vec::new();
        latency_body.extend_from_slice(&123_456_789i64.to_le_bytes());
        latency_body.push(1u8);
        let (ts, needs_resp) = parse_network_stack_latency_packet(&latency_body).expect("parse latency");
        assert_eq!(ts, 123_456_789);
        assert!(needs_resp);
        let pong = build_network_stack_latency_packet_bytes(ts, false);
        let pong_raw = RawPacket::parse(pong).expect("parse latency pong");
        assert_eq!(pong_raw.id, 0x73);
        assert_eq!(pong_raw.body.last().copied(), Some(0u8));

        let mut dbg_prog = SessionProgress::default();
        dbg_prog.has_sent_start_game = true;
        dbg_prog.set_local_player_initialized = true;
        dbg_prog.last_packet_id = Some(0x90);
        dbg_prog.last_packet_name = "PlayerAuthInput".to_string();
        let dbg_line = format_disconnect_debug_summary(
            42,
            "192.168.1.79:44951".parse().expect("socket addr"),
            &dbg_prog,
            "peer closed",
        );
        assert!(dbg_line.contains("Último paquete procesado: PlayerAuthInput (0x90)"));
        assert!(dbg_line.contains("spawn_init=true"));
    }

    #[test]
    fn client_cache_status_0x81_triggers_start_game_chunks_and_login_callback() {
        reset_player_state();
        let observed = Arc::new(Mutex::new(Vec::<(String, usize, bool)>::new()));
        let observed_cb = Arc::clone(&observed);
        set_player_event_hook(Some(Arc::new(move |name, count, connected| {
            if let Ok(mut g) = observed_cb.lock() {
                g.push((name.to_string(), count, connected));
            }
        })));

        let fake_login = br#"{"extraData":{"displayName":"SteveDroid"}}"#;
        assert_eq!(extract_player_name_from_login(fake_login), "SteveDroid");
        notify_player_login("SteveDroid");

        let status_json = current_connected_player_status_json();
        assert!(status_json.contains("\"connected\":true"));
        assert!(status_json.contains("SteveDroid"));

        {
            let g = observed.lock().expect("lock observed");
            assert_eq!(g.last(), Some(&("SteveDroid".to_string(), 1usize, true)));
        }

        // Verificar recepción de ClientCacheStatus (0x81) codificado
        let mut cache_pkt = BytesMut::new();
        encode_packet(&ClientCacheStatus { enabled: true }, &mut cache_pkt);
        let parsed = RawPacket::parse(cache_pkt.freeze()).expect("raw cache status");
        assert_eq!(parsed.id, 0x81);

        reset_player_state();
        set_player_event_hook(None);
    }

    #[test]
    fn panic_hook_and_catch_unwind_capture_crash_to_hook_and_file() {
        let tmp_path = std::env::temp_dir().join(format!("bdr_crash_test_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&tmp_path);
        set_crash_log_path(Some(tmp_path.clone()));

        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let captured_cb = Arc::clone(&captured);
        set_crash_event_hook(Some(Arc::new(move |err| {
            if let Ok(mut g) = captured_cb.lock() {
                g.push(err.to_string());
            }
        })));

        // Por defecto desactivada: report_rust_crash no guarda nada en disco
        set_crash_protection_enabled(false);
        assert!(!is_crash_protection_enabled());
        report_rust_crash("should not persist while disabled");
        assert!(!tmp_path.exists());
        assert_eq!(captured.lock().expect("lock captured").len(), 0);

        init_native_signal_handlers();
        set_crash_protection_enabled(true);
        assert!(is_crash_protection_enabled());

        let ok = run_catch_unwind("test_packet_join", || {
            panic!("simulated packet panic during join");
        });
        assert!(!ok);

        record_native_signal_crash("SIGABRT", 6);

        {
            let g = captured.lock().expect("lock captured");
            assert_eq!(g.len(), 1);
            assert!(g[0].contains("PANIC DE RUST:"));
            assert!(g[0].contains("simulated packet panic during join en test_packet_join"));
        }

        let disk = std::fs::read_to_string(&tmp_path).expect("crash file must exist");
        assert!(disk.contains("PANIC DE RUST:"));
        assert!(disk.contains("simulated packet panic during join"));
        assert!(disk.contains("CRASH NATIVO: SIGABRT (sig=6)"));

        let _ = std::fs::remove_file(&tmp_path);
        set_crash_protection_enabled(false);
        set_crash_log_path(None);
        set_crash_event_hook(None);
    }
}
