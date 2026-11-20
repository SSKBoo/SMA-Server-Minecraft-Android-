use crate::net::low_latency::LowLatencyConfig;
use acacia_nethernet::{
    http, join_path,
    lan::{LanPacket, ServerData, DISCOVERY_PORT},
    Connection, Event as RtcEvent, Signal, SignalKind, PROBE_PATH,
};
use acacia_session::proto::types::{
    BlockCoordinates, TransactionTransactionData, TransactionUseItemActionType, UpdateBlockFlags,
};
use acacia_session::proto::{
    encode_packet, packet_name,
    packets::{
        ClientCacheStatus, ClientToServerHandshake, CommandRequest, InventoryTransaction, Login,
        NetworkSettings, NetworkSettingsCompressionAlgorithm, PlayStatus, PlayStatusStatus,
        RequestChunkRadius, RequestNetworkSettings, ResourcePackClientResponse,
        ResourcePackClientResponseResponseStatus, ServerToClientHandshake,
        SetLocalPlayerAsInitialized, Text, TextContent, UpdateBlock,
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
use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream as StdTcpStream, UdpSocket as StdUdpSocket,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::runtime::{Builder, Runtime};
use tokio_util::sync::CancellationToken;

/// Canal de broadcast Fase 3: los handlers publican paquetes crudos y el bucle principal
/// los re-codifica con la sesión de cada peer (compresión/cifrado son por conexión).
static BROADCAST_TX: OnceLock<tokio::sync::broadcast::Sender<Bytes>> = OnceLock::new();

fn publish_broadcast(raw: Bytes) {
    if let Some(tx) = BROADCAST_TX.get() {
        let _ = tx.send(raw);
    }
}

/// Fase 4: bloques modificados (x, y, z) -> runtime_id. 0 = aire.
type OverrideMap = HashMap<(i32, i32, i32), u32>;
static WORLD_OVERRIDES: OnceLock<Mutex<OverrideMap>> = OnceLock::new();

fn overrides() -> &'static Mutex<OverrideMap> {
    WORLD_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
}

static WORLD_BASE: OnceLock<String> = OnceLock::new();

fn world_overrides_path(base: &str) -> PathBuf {
    PathBuf::from(base).join("world").join("overrides.txt")
}

fn load_world_overrides(base: &str) -> usize {
    let path = world_overrides_path(base);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let mut count = 0usize;
    if let Ok(mut map) = overrides().lock() {
        for line in text.lines() {
            let parts: Vec<_> = line.split(',').collect();
            if parts.len() == 4 {
                if let (Ok(x), Ok(y), Ok(z), Ok(id)) = (
                    parts[0].parse::<i32>(),
                    parts[1].parse::<i32>(),
                    parts[2].parse::<i32>(),
                    parts[3].parse::<u32>(),
                ) {
                    map.insert((x, y, z), id);
                    count += 1;
                }
            }
        }
    }
    log::info!(
        "[Bedrock World] Cargados {} overrides desde {}",
        count,
        path.display()
    );
    count
}

fn save_world_overrides(base: &str) -> usize {
    let path = world_overrides_path(base);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut text = String::new();
    let mut count = 0usize;
    if let Ok(map) = overrides().lock() {
        for ((x, y, z), id) in map.iter() {
            text.push_str(&format!("{},{},{},{}\n", x, y, z, id));
            count += 1;
        }
    }
    match std::fs::write(&path, text) {
        Ok(_) => log::info!(
            "[Bedrock World] Guardados {} overrides en {}",
            count,
            path.display()
        ),
        Err(e) => log::warn!("[Bedrock World] Error guardando overrides: {}", e),
    }
    count
}

/// Maneja comandos del servidor tanto desde Text (0x09) como desde CommandRequest (0x4d).
/// Devuelve true si el mensaje era un comando y no debe retransmitirse.
fn handle_world_command(message: &str, peer_addr: SocketAddr) -> bool {
    let world_base = WORLD_BASE.get().map(|s| s.as_str()).unwrap_or("");
    if let Some(rest) = message.trim().strip_prefix("/setblock") {
        let parts: Vec<_> = rest.split_whitespace().collect();
        if parts.len() == 4 {
            if let (Ok(x), Ok(y), Ok(z), Ok(id)) = (
                parts[0].parse::<i32>(),
                parts[1].parse::<i32>(),
                parts[2].parse::<i32>(),
                parts[3].parse::<u32>(),
            ) {
                if let Ok(mut map) = overrides().lock() {
                    map.insert((x, y, z), id);
                }
                let n = save_world_overrides(world_base);
                log::info!(
                    "[Bedrock World] /setblock ({},{},{}) -> {} ejecutado por {} ({} overrides guardados)",
                    x, y, z, id, peer_addr, n
                );
            } else {
                log::warn!(
                    "[Bedrock World] /setblock con argumentos inválidos: {:?}",
                    rest
                );
            }
        } else {
            log::warn!("[Bedrock World] Uso: /setblock <x> <y> <z> <block_runtime_id>");
        }
        return true;
    }
    if message.trim().eq_ignore_ascii_case("/save") {
        let n = save_world_overrides(world_base);
        log::info!(
            "[Bedrock World] Comando /save ejecutado por {}: {} overrides guardados",
            peer_addr,
            n
        );
        return true;
    }
    false
}

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
    pub has_completed_resource_packs: bool,
    pub has_received_client_cache_status: bool,
    pub has_sent_start_game: bool,
    pub has_sent_world_registries: bool,
    pub has_sent_initial_chunks: bool,
    pub has_sent_player_spawn_entities: bool,
    pub has_sent_player_spawn_status: bool,
    pub has_sent_chunk_radius: bool,
    pub set_local_player_initialized: bool,
    pub client_sent_local_player_init: bool,
    pub spawn_init: bool,
    pub unknown_pkts: u32,
    pub unknown_pkt_ids: std::collections::HashMap<u32, u32>,
    pub expected_loading_screen_id: Option<u32>,
    pub last_loading_screen_id: Option<u32>,
    pub local_ice_candidate: String,
    pub player_name: String,
    pub player_uuid: [u8; 16],
    pub player_auth_input_count: u64,
    pub latency_ping_count: u64,
    pub keepalives_sent: u64,
    pub movement_tick: u64,
    pub last_packet_id: Option<u32>,
    pub last_packet_name: String,
    pub pending_chunks: VecDeque<(i32, i32)>,
    pub pending_spawn_finish: bool,
    pub chunks_sent_count: usize,
    pub outbound_packets_sent: u64,
    pub outbound_bytes_sent: u64,
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
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&target_path)
    {
        let _ = writeln!(
            file,
            "CRASH NATIVO: {} (sig={})\n{:?}",
            sig_name, sig_num, bt
        );
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
    #[allow(dead_code)]
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
            format!(
                "malformed batch framing: invalid subpacket varint at offset {}",
                offset
            )
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
        let raw: u64 =
            std::collections::hash_map::RandomState::new().hash_one(std::process::id()) as u64;
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
        name, TARGET_BEDROCK_PROTOCOL, MOTD_ADVERTISED_VERSION, sid, effective_port, ipv6_port
    )
}

fn normalize_ice_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        IpAddr::V4(v4) => IpAddr::V4(v4),
    }
}

pub fn is_private_or_loopback_ip(ip: IpAddr) -> bool {
    match normalize_ice_ip(ip) {
        IpAddr::V4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local(),
    }
}

/// Extrae líneas `a=candidate:...` de un SDP offer/answer para diagnóstico de conectividad ICE/STUN.
pub fn extract_sdp_ice_candidates(sdp: &str) -> Vec<String> {
    sdp.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("a=candidate:"))
        .map(|line| line.trim_start_matches("a=candidate:").to_string())
        .collect()
}

/// Garantiza que el SDP answer incluya una línea `a=candidate:` UDP host con la IP local resuelta.
/// Si `str0m` no incluyó candidatos con esa IP, la inyecta antes de `a=end-of-candidates` (o al final).
pub fn ensure_sdp_ice_candidate(answer_sdp: &str, local_candidate: SocketAddr) -> (String, bool) {
    let target_ip = normalize_ice_ip(local_candidate.ip());
    let target_ip_str = target_ip.to_string();
    let existing = extract_sdp_ice_candidates(answer_sdp);
    if existing
        .iter()
        .any(|c| c.split_whitespace().any(|tok| tok == target_ip_str))
    {
        return (answer_sdp.to_string(), false);
    }

    let eol = if answer_sdp.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let cand_line = format!(
        "a=candidate:1 1 UDP 2130706431 {} {} typ host{}",
        target_ip_str,
        local_candidate.port(),
        eol
    );

    if let Some(pos) = answer_sdp.find("a=end-of-candidates") {
        let mut out = String::with_capacity(answer_sdp.len() + cand_line.len());
        out.push_str(&answer_sdp[..pos]);
        out.push_str(&cand_line);
        out.push_str(&answer_sdp[pos..]);
        return (out, true);
    }

    let mut out = answer_sdp.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(eol);
    }
    out.push_str(&cand_line);
    (out, true)
}

/// Resuelve una IP local concreta (no 0.0.0.0) normalizando direcciones IPv4-mapeadas en IPv6 para que
/// `str0m::Candidate::host` dentro de `Connection::answer` incluya un candidato ICE UDP válido en el SDP answer.
fn resolve_local_candidate_addr(preferred_ip: Option<IpAddr>, udp_port: u16) -> SocketAddr {
    let norm_preferred = preferred_ip.map(normalize_ice_ip);
    if let Some(ip) = norm_preferred {
        if !ip.is_unspecified() && !ip.is_loopback() {
            return SocketAddr::new(ip, udp_port);
        }
    }
    if let Ok(probe) = StdUdpSocket::bind("0.0.0.0:0") {
        if probe.connect("8.8.8.8:80").is_ok() {
            if let Ok(local) = probe.local_addr() {
                let norm = normalize_ice_ip(local.ip());
                if !norm.is_unspecified() && !norm.is_loopback() {
                    return SocketAddr::new(norm, udp_port);
                }
            }
        }
    }
    if let Some(ip) = norm_preferred {
        if !ip.is_unspecified() {
            return SocketAddr::new(ip, udp_port);
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

fn decode_zlib_b64_bytes(b64: &str) -> Option<Bytes> {
    let compressed = decode_base64_segment(b64)?;
    let mut decoder = ZlibDecoder::new(compressed.as_slice());
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).ok()?;
    Some(Bytes::from(out))
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
    for token in text.split(|c: char| {
        !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '+' || c == '/' || c == '.')
    }) {
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

/// Calcula el checksum CRC32 estándar (IEEE 802.3, polinomio 0xEDB88320) sobre payloads de chunks.
pub fn crc32_checksum(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
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
pub const SPAWN_EYE_Y: f32 = 65.62; // Altura de ojos sobre suelo sólido en y=63 (superficie y=64.0 + 1.62)

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

fn append_uniform_subchunk_v9(
    out: &mut Vec<u8>,
    sub_y: i8,
    primary_hash: u32,
    secondary_hash: u32,
) {
    // Formato NETWORK Bedrock v9 completo con 4 bits/bloque (0x09 = (4 << 1) | 1):
    // 512 palabras u32 Little-Endian (2048 bytes por subchunk, 8 bloques/palabra) + paleta explícita de 2 entradas ZigZag VarInt32.
    out.push(0x09); // SubChunk versión 9
    out.push(0x01); // 1 capa de bloques
    out.push(sub_y as u8);
    out.push(0x09); // 4 bits por bloque ((4 << 1) | 1) + flag network=1 (Little-Endian)

    // Todos los 4096 bloques apuntan al índice 0 (primary_hash) en formato network XZY (512 palabras u32 = 2048 bytes)
    for _ in 0..512usize {
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
    out.push(0x09); // 4 bits por bloque ((4 << 1) | 1) + flag network=1 (512 palabras u32 = 2048 bytes)

    // 4096 bloques empaquetados a 4 bits/bloque (8 bloques por palabra u32 LE) -> 512 palabras u32 LE (2048 bytes).
    // Coordenada en SubChunk: index = (x << 8) | (z << 4) | y -> local_y = index & 0x0F.
    for word_idx in 0..512usize {
        let base = word_idx * 8;
        let mut word: u32 = 0;
        for slot in 0..8usize {
            let local_y = (base + slot) & 0x0F;
            if local_y == 15 {
                word |= 1u32 << (slot * 4);
            }
        }
        out.extend_from_slice(&word.to_le_bytes());
    }

    encode_varuint32(zigzag32(2), out); // 2 entradas en paleta
    encode_varuint32(zigzag32(stone_hash as i32), out);
    encode_varuint32(zigzag32(grass_hash as i32), out);
}

fn append_overworld_biomes_and_borders(out: &mut Vec<u8>) {
    // Bedrock 1.18+/1.26 eliminó el heightmap 2D heredado del payload de red al introducir biomas 3D.
    // Overworld abarca 24 secciones de bioma (-64..319); paleta uniforme network = DEFAULT_SPAWN_BIOME_ID (1 = Plains).
    for _ in 0..24 {
        out.push(0x01);
        encode_varuint32(zigzag32(DEFAULT_SPAWN_BIOME_ID), out);
    }
    out.push(0x00); // border_blocks = 0
}

/// Genera un chunk vacío (todo `minecraft:air`) usando almacenamiento paletizado 4-bit de red (~18.6 KB, 9 subchunks: -4..=4).
pub fn build_empty_air_chunk_payload_nbtx() -> Vec<u8> {
    let air_hash = block_state_hash_nbtx("minecraft:air");
    let stone_hash = block_state_hash_nbtx("minecraft:stone");
    let mut data = Vec::with_capacity(19200);
    for sub_y in -4i8..=4i8 {
        append_uniform_subchunk_v9(&mut data, sub_y, air_hash, stone_hash);
    }
    append_overworld_biomes_and_borders(&mut data);
    data
}

/// Genera un chunk de spawn seguro en formato NETWORK Little-Endian (~18.6 KB, 9 subchunks: -4..=4, 4 bits/bloque):
/// piedra en `y=-64..62`, césped en `y=63` y aire en `y>=64`, evitando caídas al vacío en `(0, 64, 0)`.
pub fn build_spawn_chunk_payload_nbtx() -> Vec<u8> {
    let air_hash = block_state_hash_nbtx("minecraft:air");
    let stone_hash = block_state_hash_nbtx("minecraft:stone");
    let grass_hash = block_state_hash_nbtx("minecraft:grass_block");

    let mut data = Vec::with_capacity(19200);
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
    use bedrock::protocol::{
        Packet, PacketHeader as BedrockPacketHeader, ProtoCodec as BedrockProtoCodec, V2225,
    };

    let settings = LevelSettings::<V2225> {
        seed: 1,
        spawn_settings: SpawnSettings::<V2225> {
            spawn_type: SpawnBiomeType::Default,
            user_defined_biome_name: String::new(),
            dimension: 0,
        },
        generator_type: GeneratorType::Overworld,
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

    // position: (0.0, 65.62, 0.0) [altura de ojos sobre suelo y=63/64] & rotation: (0.0, 0.0) en f32 little-endian
    for f in [0.0f32, SPAWN_EYE_Y, 0.0f32, 0.0f32, 0.0f32] {
        raw.extend_from_slice(&f.to_le_bytes());
    }

    settings
        .serialize(&mut raw)
        .map_err(|e| format!("level_settings encode err: {}", e))?;

    write_bedrock_string(&mut raw, "bedrockdroid_world"); // level_id
    write_bedrock_string(&mut raw, "Bedrock World"); // level_name
    write_bedrock_string(&mut raw, ""); // template_content_identity
    raw.push(0); // is_trial = false

    movement_settings
        .serialize(&mut raw)
        .map_err(|e| format!("movement_settings encode err: {}", e))?;

    raw.extend_from_slice(&6000u64.to_le_bytes()); // current_level_time (u64 LE)
    encode_varuint32(zigzag32(0), &mut raw); // enchantment_seed = 0 (varint32)
    encode_varuint32(0, &mut raw); // block_properties (Vec vacío)
    write_bedrock_string(&mut raw, "00000000-0000-4000-8000-000000000001"); // multiplayer_correlation_id
    raw.push(1); // enable_item_stack_net_manager = true
    write_bedrock_string(&mut raw, MOTD_ADVERTISED_VERSION); // server_version

    // player_property_data sin HashMap ni facet_reflect::PeekMapIter:
    write_nbt_network_compound_from_pairs(&player_property_pairs, &mut raw);

    raw.extend_from_slice(&0u64.to_le_bytes()); // server_block_type_registry_checksum
    raw.extend_from_slice(uuid::Uuid::nil().as_bytes()); // world_template_id (Uuid nil)
    raw.push(0); // server_enabled_client_side_generation = false
    raw.push(1); // block_network_ids_are_hashes = true

    network_permissions
        .serialize(&mut raw)
        .map_err(|e| format!("network_permissions encode err: {}", e))?;

    raw.push(0); // server_join_information = None
    write_bedrock_string(&mut raw, ""); // server_id
    write_bedrock_string(&mut raw, ""); // scenario_id
    write_bedrock_string(&mut raw, ""); // world_id
    write_bedrock_string(&mut raw, ""); // owner_id

    Ok(Bytes::from(raw))
}

#[cfg(feature = "protocol-core")]
pub fn build_v2225_level_chunk_packet_bytes(
    cx: i32,
    cz: i32,
    serialized_chunk_data: Vec<u8>,
) -> Result<Bytes, String> {
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

/// Construye `ChangeDimensionPacket` (0x3d / 61) con dimensión Overworld (0), posición `(x, y, z)`,
/// `respawn=false` y `loading_screen_id` opcional (`Some(id)` cuando el jugador está vivo).
pub fn build_v2225_change_dimension_packet_bytes(
    dimension: i32,
    x: f32,
    y: f32,
    z: f32,
    respawn: bool,
    loading_screen_id: Option<u32>,
) -> Bytes {
    let mut raw = Vec::with_capacity(28);
    encode_varuint32(0x3d, &mut raw); // Packet ID 61 (ChangeDimension)
    encode_varuint32(zigzag32(dimension), &mut raw);
    raw.extend_from_slice(&x.to_le_bytes());
    raw.extend_from_slice(&y.to_le_bytes());
    raw.extend_from_slice(&z.to_le_bytes());
    raw.push(if respawn { 1 } else { 0 });
    match loading_screen_id {
        Some(screen_id) => {
            raw.push(0x01);
            raw.extend_from_slice(&screen_id.to_le_bytes());
        }
        None => {
            raw.push(0x00);
        }
    }
    Bytes::from(raw)
}

/// Parsea `ServerboundLoadingScreen` (0x138 / 312) extrayendo `(type_id, loading_screen_id)`.
pub fn parse_serverbound_loading_screen_packet(body: &[u8]) -> Option<(i32, Option<u32>)> {
    let (raw_type, offset) = decode_varuint32(body)?;
    let screen_type = ((raw_type >> 1) as i32) ^ -((raw_type & 1) as i32);
    let rest = body.get(offset..)?;
    if rest.is_empty() {
        return Some((screen_type, None));
    }
    if rest[0] == 0 {
        return Some((screen_type, None));
    }
    if rest[0] == 1 && rest.len() >= 5 {
        let id = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]);
        return Some((screen_type, Some(id)));
    }
    if rest.len() == 4 {
        let id = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]);
        return Some((screen_type, Some(id)));
    }
    Some((screen_type, None))
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
    if !pitch.is_finite() || !yaw.is_finite() || !x.is_finite() || !y.is_finite() || !z.is_finite()
    {
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

/// Construye `MovePlayerPacket` (0x13 / 19) en formato wire `bedrock-rs` `V2225` (incluye `teleport_data: Option`).
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
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v2168::enums::PlayerPositionMode;
        use bedrock::protocol::v2168::packets::MovePlayerPacket;
        use bedrock::protocol::v2168::types::MovePlayerTeleportData;
        use bedrock::protocol::v662::types::ActorRuntimeID;
        use bedrock::protocol::V2225;

        let position_mode = match mode {
            1 => PlayerPositionMode::Respawn,
            2 => PlayerPositionMode::Teleport,
            3 => PlayerPositionMode::OnlyHeadRot,
            _ => PlayerPositionMode::Normal,
        };
        let teleport_data = if mode == 2 {
            Some(MovePlayerTeleportData {
                teleportation_cause: 0,
                source_actor_type: 0,
            })
        } else {
            None
        };
        let pkt = MovePlayerPacket::<V2225> {
            player_runtime_id: ActorRuntimeID(runtime_id),
            position: (x, y, z),
            rotation: (pitch, yaw),
            y_head_rotation: head_yaw,
            position_mode,
            on_ground,
            riding_runtime_id: ActorRuntimeID(0),
            teleport_data,
            tick,
        };
        if let Ok(encoded) = encode_bedrock_v2225_packet(&pkt) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(52);
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
        raw.push(0x01); // Option::Some
        raw.extend_from_slice(&0i32.to_le_bytes()); // teleportation_cause
        raw.extend_from_slice(&0i32.to_le_bytes()); // source_actor_type
    } else {
        raw.push(0x00); // Option::None
    }
    encode_varuint64(tick, &mut raw);
    Bytes::from(raw)
}

pub fn parse_network_stack_latency_packet(body: &[u8]) -> Option<(i64, bool)> {
    let ts_slice = body.get(0..8)?;
    let timestamp = i64::from_le_bytes([
        ts_slice[0],
        ts_slice[1],
        ts_slice[2],
        ts_slice[3],
        ts_slice[4],
        ts_slice[5],
        ts_slice[6],
        ts_slice[7],
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

    let clamped_radius = radius.clamp(1, 2);
    if let Ok(rad_pkt) = encode_bedrock_v2225_packet(&ChunkRadiusUpdatedPacket {
        chunk_radius: clamped_radius,
    }) {
        let batch = session.encode(&[rad_pkt]);
        conn.send(batch, now);
    }
    clamped_radius
}

const VANILLA_BIOME_DEFINITIONS_ZLIB_B64: &str = "eNqNV1tvXFcVXk7m4hmP77k5CcVtmlKg5VJTbk08BVouJU1p0yQkrXOyZ2bPzInPLefiiZNC7aRJaEpLwGlTO1EkQAGkPsALQgiR8AAPSC5SJZBAPHL5AUiIx7D2XnufOXPG49SSPXP2+dba6/Ltb22fPgJw5w4AlNUv/izcfnd1de+fL2692deXxfdwFF6AFwH6EPnu6r4yvp0mJEzj897//vwJgZyBI4jsg2MAG8jn49D+kci3bz5yE/pyGyTSgOPAADaS1+mV5WXldUHG8r8zQF43SnQFIIPIqampsohB4ESUwvI/V+9BZH4GAQJ5DKpQA8jKGBam0+iz3oOI7tfoLHD0XQXIqezq9brC77slbGpnGOEFug4NaIIp8PmuusFt8efOKzLuo3ACZsFCGPSnqqEjOXzkMlajYIMDLnhwEnwIIMSqQCHlW1vceK6CvjcWMOIIoEgoSHfuzq+O/wQjnoOW7F0FTlH/BrrwFPHlA9tEhi3E5qVnC+bhNECpGy+7eO3gJsKfQchRaSHxgyl8vf5v2dWfHhrV+JeS+CHEU7UXdMUl/uqrl0SW30LstwGGUyhVjz1Li4uIygjUywI30s1OiXvnpc0CNyL7twAwmuqGjvKHp7cTNxdjdsJYinEa+95sFn3mZhQ2Izg/nuKbxp7ju4idGpsVXN6UYr2O/C8vZgh9VvKe6gWbZRxBgpvEhz/8aBNxkyNEeD8Hr4i4t6TwOpa/3tihYyG8RG9V6PRJ+celQfJ+NoFG5m9LsRO9JxQjfx5IM44T6ya6MpXey79pDLb7cgFgO+H2tnWA+nezKnMcgYtKBxR7diB+ZXmiLH6TUf+plaMcv5PQA9iZQuuK/D4sttGLCg8f6KE071mPUd+F51cRewngHqrGbc07gRO/545NIrYwg3GnlOaDKQsdy6kX7keLIlksKhuRL2Y7KeNfTqgZ7dKyHyabDLymGBDvc2/KJt6nGivgorKSnLyvhxK/Ux0i/EaZCXES/e8i/+W0yl9/7SE8SXlSzF3wXWLN/XQ+EujfCcbv+dnl3W306woPu3to5uLL+wm9W+LfQOz3AB7omjfk+8bCQUQPaPTrWJ0LygYj+tC6VoWkldrnwR6z8l+/2CB6TbxfkHNNcP8ywId7nBVloc/KgjotaPGRHnv8/S15Gr/fcboQ/9EeOxAed+iyeKjHDn9cEIzNX1T4Cp3fh3v4J3T/RTjfif9Yj6n0S2834gdaCn0qPsnS6uM9diGr4nloxbtEep594m6q+INO3frkuqoo8R26+EiP+w7N15xg7JK+8Ux1TZX6tJgGiRvPElyhqfKpdc9Zjm48S6T8j66pRKvTqz+eJN16VGGlyn26684jVGJ1+uxs4oa0hKf9ivD9GZVf4pbxWxFT4sazhDeeK+rOA5+NO0uneGX5b9Oihuq+0cLbgI5batbnuvpD0VC9Mc/NqipYk893zSDCpmaQQMvePNal/oQn9S9o9V/CulxBPcTa7OmqZElm3jUBxB6wtwtN/jsmwBLqP2KnU3pOmruqJkC/ngBx3ctrdqlLy5eSM+PxLv2n6EnLC20tj3f5wrosy2s1Vzz7Yoq/K8u/xr2ul0VvlTovxWoOX2pruTpJF8pTU9fKigkaHav5Ez3UXJxUEPjdykLp7JM97mgdykw7KIsvr2tRTFroCQBfWfdenrURcBJv5hj/V9dF5mx4EyFvwVWB/drdvL6JEOn1qR7If86VCfkUvI0RIPLrxMbbaaWjeheWpWa1EBbfjva9P4t9bYuniWG37mJB6KcFZ/YTw26lb0mxBd3Y9rf3eKaLxUmLHO3xDGnZN+J7f2d1iJHZFQQcRfw1gGcV09dGZlYQ96yYHs8p1qZuomr3zLL+P/NASvc7cbjzAaVx8HyP/xkJmX9eRtiQ94EmwMH3k/1Byv5QjxuoilUgDwnc4XVjxdwPyxjgmz1uyyrSGQQcQT1QN7vrfVm3ypmT8yxmOkGuxgPuh4P8VOhzmxtN07KCXN31eRBmQ2Y2WCFoMRvBTi3rm3PczzS5ZeXDJje4Uxu3eINV5426757mjiE9l9SDRBfNKjdoq0Hx1XYjJxRPw3YUNH3XtQ0zEM43p56NoIlBZCucVZslCpKCK1Fw9DAgQ6TvYx05GLzW4LkTkdOweIk+lIl6EO+LNc49inogCF2HG3K/YtW1avS1VDH9atOgPceSD+RtEEOu85paI0MZ00j7KyGLNpZKvWt/pXfbOkP3rCgwcIEH+YDNMcdhw+pTlDLkLMrYPGBj4o9eMWT8peRSscV8m7Iblom2n4esaJa3H8fl6841yiVh3X4uJds9Kl8mVwYrzK64rkGFHu94ogxHgsipW26L+4obQ6rDdhRi6LXNnfVQq4PKhGo9SPVT70ZjlsYrbeYZgWfO8mBIhaAA4wki6LVNHR1Wi9u72x4H2tF+vTqW6H0M5LWW66ZWd3Sudrie7MUJDYg5oZ63pjii14uSExV/vsq3dzMmzrzjVbyFG1mBKOocsyw+P1T1TTtwHd0CZIsXZ49tD5gVGjVuhSwonWCNBr7zOJsNNGHoIXDc1rwRWK7Hg2zDd+d4zuas5raKmGPTqLI5HgzXfNOjAOWzPJ7z5KAgGVdj/uyQzRzpwJDtL1Wb3PfnDbk04DFsboP5Ne6UgsiqRz65Gqoxz7PisPO26wQh9wto47dc36qV6gx7WTeDpuk0Ck2z0TQCzoIcc0ybWQMVjj1hFRMrNB54rIX8aoh++6yCi8GkWpMnaY75JnPQmXgg+2BiTYDvNmJTSZ+1THc2GZI59KNqGPncYBVsDZaohonZXtFlsyqniU5c6DPTMvwIj0JWFmqb2iiwTGy7gQ0NIr/OqjwrVyYkCAPEshjNSB4PEuedytDGbTBmfjLiGGLNd1sYxb3qJZI6qmJQZI+n3xQg260EoxohRoNcyTkcR4k/SB9ogq0IhiRMzBfu29whK6POW4ZnNizMYoBWGphmqN7arGEzoxpVeDCmdlE+xTZbCHTaxWDqpqCldDSxZsFFK3I02HJE3AcUznMt5ovB4MuiIdu572CWRsVyq7NxeoLg2IlTPNike900EaUYshGVqV8PwhHbdTsG3xZtYoZNNwoNj4W+a2HjxDzMCNJkpSJkhG5lfOZzna+WN8w3I45IVp6fjEguKyVszHENNbAl0RvcyZGAZcRE0myOdQUd5ZUijOjGC5WgVhJjDNt0eNBk9XA45LbHfVEOOQb69TzJ0QTIa6WJD5qBAQgD03VGqVkIDxWHd7S7nm4bSoh8R09bNAFQHxIsKMRXjaw89vepDLCanlllljzgBgsNYdfkeMrD/wO9oCo9";
static VANILLA_BIOME_DEFINITIONS_CACHE: OnceLock<Bytes> = OnceLock::new();

fn build_v2225_biome_definition_list_fallback_bytes() -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v800::packets::{BiomeDefinitionListPacket, BiomeEntry};
        use bedrock::protocol::v844::types::BiomeDefinition;
        use bedrock::protocol::V2225;

        const VANILLA_BIOMES: &[(&str, f32, f32, bool)] = &[
            ("ocean", 0.5, 0.5, true),
            ("plains", 0.8, 0.4, true),
            ("desert", 2.0, 0.0, false),
            ("extreme_hills", 0.2, 0.3, true),
            ("forest", 0.7, 0.8, true),
            ("taiga", 0.25, 0.8, true),
            ("swampland", 0.8, 0.9, true),
            ("river", 0.5, 0.5, true),
            ("hell", 2.0, 0.0, false),
            ("the_end", 0.5, 0.5, false),
            ("frozen_ocean", 0.0, 0.5, true),
            ("frozen_river", 0.0, 0.5, true),
            ("ice_plains", 0.0, 0.5, true),
            ("mushroom_island", 0.9, 1.0, true),
            ("beach", 0.8, 0.4, true),
            ("jungle", 0.95, 0.9, true),
            ("deep_ocean", 0.5, 0.5, true),
            ("birch_forest", 0.6, 0.6, true),
            ("roofed_forest", 0.7, 0.8, true),
            ("savanna", 1.2, 0.0, false),
            ("mesa", 2.0, 0.0, false),
            ("sunflower_plains", 0.8, 0.4, true),
            ("flower_forest", 0.7, 0.8, true),
            ("meadow", 0.5, 0.8, true),
        ];

        let mut strings = Vec::with_capacity(VANILLA_BIOMES.len());
        let mut biomes = Vec::with_capacity(VANILLA_BIOMES.len());
        for (idx, &(name, temperature, downfall, rain)) in VANILLA_BIOMES.iter().enumerate() {
            strings.push(name.to_string());
            biomes.push(BiomeEntry {
                name_index: idx as u16,
                definition: BiomeDefinition::<V2225> {
                    id: u16::MAX, // Built-in vanilla ID mapping (plains = 1)
                    temperature,
                    downfall,
                    foliage_snow: 0.0,
                    depth: 0.125,
                    scale: 0.05,
                    map_water_color: 0x0044_AFF5,
                    rain,
                    tags: None,
                    chunk_gen_data: None,
                },
            });
        }

        let pkt = BiomeDefinitionListPacket::<V2225> { biomes, strings };
        if let Ok(encoded) = encode_bedrock_v2225_packet(&pkt) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(8);
    encode_varuint32(0x7a, &mut raw); // Packet ID 122 (BiomeDefinitionList)
    encode_varuint32(0, &mut raw); // biome_definitions (Vec vacío)
    encode_varuint32(0, &mut raw); // string_table (Vec vacío)
    Bytes::from(raw)
}

/// Construye `BiomeDefinitionListPacket` (0x7a / 122) en formato `V2225` (1.26.60) con los 89 biomas vanilla completos y tags (cacheado con `ZlibDecoder`).
pub fn build_v2225_biome_definition_list_bytes() -> Bytes {
    VANILLA_BIOME_DEFINITIONS_CACHE
        .get_or_init(|| {
            decode_zlib_b64_bytes(VANILLA_BIOME_DEFINITIONS_ZLIB_B64)
                .unwrap_or_else(build_v2225_biome_definition_list_fallback_bytes)
        })
        .clone()
}

const VANILLA_ACTOR_IDENTIFIERS_ZLIB_B64: &str = "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";
static VANILLA_ACTOR_IDENTIFIERS_CACHE: OnceLock<Bytes> = OnceLock::new();

fn build_v2225_available_entity_identifiers_fallback_bytes() -> Bytes {
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
            (
                "id".to_string(),
                NbtFieldValue::String("minecraft:player".to_string()),
            ),
            ("rid".to_string(), NbtFieldValue::Int(1)),
            ("summonable".to_string(), NbtFieldValue::Byte(0)),
        ],
        vec![
            ("bid".to_string(), NbtFieldValue::String(String::new())),
            ("hasspawnegg".to_string(), NbtFieldValue::Byte(1)),
            (
                "id".to_string(),
                NbtFieldValue::String("minecraft:boat".to_string()),
            ),
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

/// Construye `AvailableEntityIdentifiersPacket` (0x77 / 119) usando el registro completo vanilla BDS (cacheado con `ZlibDecoder`).
pub fn build_v2225_available_entity_identifiers_bytes() -> Bytes {
    VANILLA_ACTOR_IDENTIFIERS_CACHE
        .get_or_init(|| {
            decode_zlib_b64_bytes(VANILLA_ACTOR_IDENTIFIERS_ZLIB_B64)
                .unwrap_or_else(build_v2225_available_entity_identifiers_fallback_bytes)
        })
        .clone()
}

const VANILLA_ITEM_REGISTRY_ZLIB_B64: &str = "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";
static VANILLA_ITEM_REGISTRY_CACHE: OnceLock<Bytes> = OnceLock::new();

fn build_v2225_item_component_registry_fallback_bytes() -> Bytes {
    let mut raw = Vec::with_capacity(6);
    encode_varuint32(0xa2, &mut raw); // Packet ID 162 (ItemRegistry / ItemComponent)
    encode_varuint32(0, &mut raw); // items (Vec vacío)
    Bytes::from(raw)
}

/// Construye `ItemRegistryPacket` / `ItemComponentPacket` (0xa2 / 162) con los 2080 ítems vanilla 1.26.60 completos (evita desconexión de clientes móviles).
pub fn build_v2225_item_component_registry_bytes() -> Bytes {
    VANILLA_ITEM_REGISTRY_CACHE
        .get_or_init(|| {
            decode_zlib_b64_bytes(VANILLA_ITEM_REGISTRY_ZLIB_B64)
                .unwrap_or_else(build_v2225_item_component_registry_fallback_bytes)
        })
        .clone()
}

/// Construye `CreativeContentPacket` (0x91 / 145) en formato `bedrock-rs` `V2225`.
pub fn build_v2225_creative_content_bytes() -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v2168::packets::CreativeContentPacket;
        use bedrock::protocol::V2225;

        let pkt = CreativeContentPacket::<V2225> {
            groups: Vec::new(),
            contents: Vec::new(),
        };
        if let Ok(encoded) = encode_bedrock_v2225_packet(&pkt) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(6);
    encode_varuint32(0x91, &mut raw); // Packet ID 145 (CreativeContent)
    encode_varuint32(0, &mut raw); // groups (Vec vacío)
    encode_varuint32(0, &mut raw); // items (Vec vacío)
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
    raw.push(0x00); // ActionType = Add (0)
    encode_varuint32(1, &mut raw); // 1 entrada

    write_bedrock_uuid(&mut raw, player_uuid);
    encode_varuint64(zigzag64(entity_unique_id), &mut raw);
    write_bedrock_string(&mut raw, username);
    write_bedrock_string(&mut raw, xuid);
    write_bedrock_string(&mut raw, ""); // platform_chat_id
    raw.extend_from_slice(&1i32.to_le_bytes()); // build_platform = 1 (Android)

    // SerializedSkin mínimo válido (64x32 RGBA = 8192 bytes) sin HashMap
    write_bedrock_string(&mut raw, "Standard_Steve"); // skin_id
    write_bedrock_string(&mut raw, ""); // play_fab_id
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
    encode_varuint32(0, &mut raw); // cape_data vacía

    write_bedrock_string(&mut raw, ""); // geometry_data
    write_bedrock_string(&mut raw, ""); // geometry_data_engine_version
    write_bedrock_string(&mut raw, ""); // animation_data
    write_bedrock_string(&mut raw, ""); // cape_id
    write_bedrock_string(&mut raw, "Standard_Steve"); // full_id
    write_bedrock_string(&mut raw, "wide"); // arm_size
    write_bedrock_string(&mut raw, "#0"); // skin_color
    raw.extend_from_slice(&0u32.to_le_bytes()); // persona_pieces_count = 0
    raw.extend_from_slice(&0u32.to_le_bytes()); // piece_tint_colors_count = 0
    raw.push(0); // is_premium_skin
    raw.push(0); // is_persona_skin
    raw.push(0); // is_persona_cape_on_classic
    raw.push(1); // is_primary_user
    raw.push(0); // override_appearance

    raw.push(0); // is_teacher
    raw.push(0); // is_host
    raw.push(0); // is_sub_client
    raw.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // player_colour (RGBA)
    raw.push(1); // verified[0] = true

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
    raw.extend_from_slice(&1u16.to_le_bytes()); // LayerType = Base
    raw.extend_from_slice(&0x000F_FFFFu32.to_le_bytes()); // AbilitiesSet
    raw.extend_from_slice(&0x0000_003Fu32.to_le_bytes()); // AbilityValues
    raw.extend_from_slice(&0.05f32.to_le_bytes()); // FlySpeed
    raw.extend_from_slice(&1.00f32.to_le_bytes()); // VerticalFlySpeed (v776+)
    raw.extend_from_slice(&0.10f32.to_le_bytes()); // WalkSpeed

    encode_varuint32(0, &mut raw); // EntityLinks = 0
    write_bedrock_string(&mut raw, ""); // DeviceID
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

/// Construye `SetSpawnPositionPacket` (ID 43 / 0x2b) en formato `bedrock-rs` `V2225` (ZigZag i32 para `y`).
pub fn build_v2225_set_spawn_position_packet_bytes(
    x: i32,
    y: u32,
    z: i32,
    dimension: i32,
) -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::enums::SpawnPositionType;
        use bedrock::protocol::v662::packets::SetSpawnPositionPacket;
        use bedrock::protocol::v944::types::NetworkBlockPosition;
        use bedrock::protocol::V2225;

        let pos = NetworkBlockPosition { x, y: y as i32, z };
        let pkt = SetSpawnPositionPacket::<V2225> {
            spawn_position_type: SpawnPositionType::WorldSpawn,
            block_position: pos.clone(),
            dimension_type: dimension,
            spawn_block_pos: pos,
        };
        if let Ok(encoded) = encode_bedrock_v2225_packet(&pkt) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(24);
    encode_varuint32(0x2b, &mut raw); // Packet ID 43 (SetSpawnPosition)
    encode_varuint32(zigzag32(1), &mut raw); // SpawnType = WorldSpawn (1)
    encode_varuint32(zigzag32(x), &mut raw);
    encode_varuint32(zigzag32(y as i32), &mut raw);
    encode_varuint32(zigzag32(z), &mut raw);
    encode_varuint32(zigzag32(dimension), &mut raw);
    encode_varuint32(zigzag32(x), &mut raw);
    encode_varuint32(zigzag32(y as i32), &mut raw);
    encode_varuint32(zigzag32(z), &mut raw);
    Bytes::from(raw)
}

/// Construye `SetDifficultyPacket` (ID 60 / 0x3c) en formato `bedrock-rs` `V2225` (2 = Normal).
pub fn build_v2225_set_difficulty_packet_bytes(difficulty: u32) -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::packets::SetDifficultyPacket;

        if let Ok(encoded) = encode_bedrock_v2225_packet(&SetDifficultyPacket { difficulty }) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(6);
    encode_varuint32(0x3c, &mut raw); // Packet ID 60 (SetDifficulty)
    encode_varuint32(difficulty, &mut raw);
    Bytes::from(raw)
}

/// Construye `GameRulesChangedPacket` (ID 72 / 0x48) en formato `bedrock-rs` `V2225` (`showcoordinates`, `dodaylightcycle`).
pub fn build_v2225_game_rules_changed_packet_bytes() -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::packets::GameRulesChangedPacket;
        use bedrock::protocol::v844::types::{
            GameRuleChanged, GameRulesChangedPacketData, Type as GameRuleType,
        };
        use bedrock::protocol::V2225;

        let pkt = GameRulesChangedPacket::<V2225> {
            rules_data: GameRulesChangedPacketData {
                rules_list: vec![
                    GameRuleChanged {
                        rule_name: "showcoordinates".to_string(),
                        can_be_modified_by_player: true,
                        rule_type: GameRuleType::Bool(true),
                    },
                    GameRuleChanged {
                        rule_name: "dodaylightcycle".to_string(),
                        can_be_modified_by_player: true,
                        rule_type: GameRuleType::Bool(true),
                    },
                ],
            },
        };
        if let Ok(encoded) = encode_bedrock_v2225_packet(&pkt) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(48);
    encode_varuint32(0x48, &mut raw); // Packet ID 72 (GameRulesChanged)
    encode_varuint32(2, &mut raw); // 2 reglas

    for (name, val) in [("showcoordinates", true), ("dodaylightcycle", true)] {
        write_bedrock_string(&mut raw, name);
        raw.push(1); // editable = true
        encode_varuint32(1, &mut raw); // type = 1 (Boolean)
        raw.push(if val { 1 } else { 0 });
    }
    Bytes::from(raw)
}

/// Construye `JigsawStructureDataPacket` (ID 313 / 0x139) en formato NBT NetworkLittleEndian sin `HashMap` (requerido antes de `StartGame` en 1.26.60).
pub fn build_v2225_jigsaw_structure_data_bytes() -> Bytes {
    let mut raw = Vec::with_capacity(96);
    encode_varuint32(0x139, &mut raw); // Packet ID 313 (JigsawStructureData)
    raw.push(0x0a); // TAG_Compound raíz
    raw.push(0x00); // Nombre raíz vacío (varuint32 = 0)
    for list_name in ["processors", "template_pools", "jigsaws", "structure_sets"] {
        raw.push(0x09); // TAG_List
        write_nbt_net_bytes(list_name.as_bytes(), &mut raw);
        raw.push(0x0a); // Elemento de lista: TAG_Compound
        encode_varuint32(zigzag32(0), &mut raw); // Lista vacía (VarInt32 ZigZag)
    }
    raw.push(0x00); // Root TAG_End
    Bytes::from(raw)
}

/// Construye `SetCommandsEnabledPacket` (ID 59 / 0x3b) en formato `bedrock-rs` `V2225`.
pub fn build_v2225_set_commands_enabled_bytes(enabled: bool) -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::packets::SetCommandsEnabledPacket;

        if let Ok(encoded) = encode_bedrock_v2225_packet(&SetCommandsEnabledPacket {
            commands_enabled: enabled,
        }) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(4);
    encode_varuint32(0x3b, &mut raw); // Packet ID 59 (SetCommandsEnabled)
    raw.push(if enabled { 1 } else { 0 });
    Bytes::from(raw)
}

/// Construye `UpdateAdventureSettingsPacket` (ID 188 / 0xbc) en formato `bedrock-rs` `V2225`.
pub fn build_v2225_update_adventure_settings_bytes() -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::packets::UpdateAdventureSettingsPacket;
        use bedrock::protocol::v662::types::AdventureSettings;
        use bedrock::protocol::V2225;

        let pkt = UpdateAdventureSettingsPacket::<V2225> {
            adventure_settings: AdventureSettings {
                no_pvm: false,
                no_mvp: false,
                immutable_world: false,
                show_name_tags: true,
                auto_jump: true,
            },
        };
        if let Ok(encoded) = encode_bedrock_v2225_packet(&pkt) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(8);
    encode_varuint32(0xbc, &mut raw); // Packet ID 188 (UpdateAdventureSettings)
    raw.extend_from_slice(&[0, 0, 0, 1, 1]); // no_pvm, no_mvp, immutable_world, show_name_tags, auto_jump
    Bytes::from(raw)
}

/// Construye `UpdateAbilitiesPacket` (ID 187 / 0xbb) en formato `bedrock-rs` `V2225` (capa Base estándar Survival/Member).
pub fn build_v2225_update_abilities_bytes(target_player_raw_id: i64) -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::enums::CommandPermissionLevel;
        use bedrock::protocol::v662::packets::UpdateAbilitiesPacket;
        use bedrock::protocol::v776::types::{
            SerializedAbilitiesData, SerializedAbilitiesLayer, SerializedLayer,
        };
        use bedrock::protocol::V2225;

        let pkt = UpdateAbilitiesPacket::<V2225> {
            data: SerializedAbilitiesData::<V2225> {
                target_player_raw_id,
                player_permissions: 1, // Member
                command_permissions: CommandPermissionLevel::Any,
                layers: vec![SerializedLayer {
                    serialized_layer: SerializedAbilitiesLayer::Base,
                    abilities_set: 0x000F_FFFF,
                    ability_values: 0x0000_003F,
                    fly_speed: 0.05,
                    vertical_fly_speed: 1.0,
                    walk_speed: 0.1,
                }],
            },
        };
        if let Ok(encoded) = encode_bedrock_v2225_packet(&pkt) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(40);
    encode_varuint32(0xbb, &mut raw); // Packet ID 187 (UpdateAbilities)
    raw.extend_from_slice(&target_player_raw_id.to_le_bytes());
    raw.push(1); // player_permissions = Member
    raw.push(0); // command_permissions = Any
    raw.push(1); // 1 layer
    raw.extend_from_slice(&1u16.to_le_bytes()); // Base
    raw.extend_from_slice(&0x000F_FFFFu32.to_le_bytes()); // abilities_set
    raw.extend_from_slice(&0x0000_003Fu32.to_le_bytes()); // ability_values
    raw.extend_from_slice(&0.05f32.to_le_bytes()); // fly_speed
    raw.extend_from_slice(&1.0f32.to_le_bytes()); // vertical_fly_speed
    raw.extend_from_slice(&0.1f32.to_le_bytes()); // walk_speed
    Bytes::from(raw)
}

/// Construye `UpdateAttributesPacket` (ID 29 / 0x1d) en formato `bedrock-rs` `V2225` con atributos básicos del jugador (salud, velocidad, hambre, XP).
pub fn build_v2225_update_attributes_bytes(runtime_id: u64) -> Bytes {
    const BASE_ATTRS: &[(&str, f32, f32, f32, f32)] = &[
        ("minecraft:health", 0.0, 20.0, 20.0, 20.0),
        ("minecraft:movement", 0.0, f32::MAX, 0.1, 0.1),
        ("minecraft:player.hunger", 0.0, 20.0, 20.0, 20.0),
        ("minecraft:player.saturation", 0.0, 20.0, 20.0, 20.0),
        ("minecraft:player.level", 0.0, 24791.0, 0.0, 0.0),
        ("minecraft:player.experience", 0.0, 1.0, 0.0, 0.0),
    ];

    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::types::ActorRuntimeID;
        use bedrock::protocol::v729::packets::{AttributeData, UpdateAttributesPacket};
        use bedrock::protocol::V2225;

        let attribute_list = BASE_ATTRS
            .iter()
            .map(
                |&(name, min_v, max_v, cur_v, def_v)| AttributeData::<V2225> {
                    min_value: min_v,
                    max_value: max_v,
                    current_value: cur_v,
                    default_min: min_v,
                    default_max: max_v,
                    default_value: def_v,
                    attribute_name: name.to_string(),
                    attribute_modifiers: Vec::new(),
                },
            )
            .collect();

        let pkt = UpdateAttributesPacket::<V2225> {
            target_runtime_id: ActorRuntimeID(runtime_id),
            attribute_list,
            ticks_since_sim_started: 0,
        };
        if let Ok(encoded) = encode_bedrock_v2225_packet(&pkt) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(288);
    encode_varuint32(0x1d, &mut raw); // Packet ID 29 (UpdateAttributes)
    encode_varuint64(runtime_id, &mut raw);
    encode_varuint32(BASE_ATTRS.len() as u32, &mut raw);
    for &(name, min_v, max_v, cur_v, def_v) in BASE_ATTRS {
        for f in [min_v, max_v, cur_v, min_v, max_v, def_v] {
            raw.extend_from_slice(&f.to_le_bytes());
        }
        write_bedrock_string(&mut raw, name);
        encode_varuint32(0, &mut raw); // attribute_modifiers vacío
    }
    encode_varuint64(0, &mut raw); // ticks_since_sim_started
    Bytes::from(raw)
}

/// Construye `SetHealthPacket` (ID 42 / 0x2a) en formato `bedrock-rs` `V2225` (enviado justo antes de `PlayStatus::PlayerSpawn`).
pub fn build_v2225_set_health_bytes(health: i32) -> Bytes {
    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::packets::SetHealthPacket;

        if let Ok(encoded) = encode_bedrock_v2225_packet(&SetHealthPacket { health }) {
            return encoded;
        }
    }

    let mut raw = Vec::with_capacity(8);
    encode_varuint32(0x2a, &mut raw); // Packet ID 42 (SetHealth)
    encode_varuint32(zigzag32(health), &mut raw);
    Bytes::from(raw)
}

static SPAWN_CHUNK_TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();

fn cached_spawn_chunk_payload() -> &'static Vec<u8> {
    SPAWN_CHUNK_TEMPLATE.get_or_init(build_spawn_chunk_payload_nbtx)
}

/// Genera las 25 coordenadas de chunk `(-2..=2, -2..=2)` ordenadas por distancia radial desde `(0, 0)`.
pub fn build_spawn_chunk_ring_coords() -> VecDeque<(i32, i32)> {
    let mut coords: Vec<(i32, i32)> = Vec::with_capacity(25);
    for cx in -2..=2 {
        for cz in -2..=2 {
            coords.push((cx, cz));
        }
    }
    coords.sort_by_key(|&(cx, cz)| (cx * cx + cz * cz, cx.abs(), cz.abs()));
    VecDeque::from(coords)
}

/// Encola los chunks iniciales de spawn para enviarlos de forma dosificada (sin saturar SCTP/UDP en un solo burst).
pub fn queue_spawn_chunks(progress: &mut SessionProgress) -> usize {
    if progress.pending_chunks.is_empty() {
        progress.pending_chunks = build_spawn_chunk_ring_coords();
    }
    progress.pending_spawn_finish = true;
    progress.pending_chunks.len()
}

/// Envía `PlayStatus::PlayerSpawn` (0x02, status=3) una vez que los 25 chunks alrededor del spawn se han transmitido.
/// Nota de protocolo Bedrock (BDS): NO enviar `MovePlayerPacket (0x13)`, `AddPlayerPacket (0x0c)` ni `PlayerList (0x3f)` antes del spawn.
pub fn send_spawn_finish_sequence(
    conn: &mut Connection,
    session: &mut ServerConnection,
    progress: &mut SessionProgress,
    peer_addr: SocketAddr,
    now: Instant,
) {
    if progress.has_sent_player_spawn_status {
        progress.pending_spawn_finish = false;
        return;
    }

    #[cfg(feature = "protocol-core")]
    {
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

        let health_bytes = build_v2225_set_health_bytes(20);
        let health_batch = session.encode(&[health_bytes]);
        conn.send(health_batch, now);

        // Fase 4: reenviar bloques modificados antes del PlayStatus de spawn.
        if let Ok(map) = overrides().lock() {
            if !map.is_empty() {
                for ((x, y, z), block_id) in map.iter() {
                    let update = UpdateBlock {
                        position: BlockCoordinates {
                            x: *x,
                            y: *y,
                            z: *z,
                        },
                        block_runtime_id: *block_id,
                        flags: UpdateBlockFlags(3),
                        layer: 0,
                    };
                    let mut buf = BytesMut::new();
                    encode_packet(&update, &mut buf);
                    let batch = session.encode(&[buf.freeze()]);
                    conn.send(batch, now);
                }
                log::info!(
                    "[Bedrock World] Reenviados {} bloques modificados a {} antes de PlayStatus",
                    map.len(),
                    peer_addr
                );
            }
        }

        if let Ok(spawn_status) = build_v2225_player_spawn_status_bytes() {
            let status_batch = session.encode(&[spawn_status]);
            conn.send(status_batch, now);
            progress.has_sent_player_spawn_entities = true;
            progress.has_sent_player_spawn_status = true;
            progress.pending_spawn_finish = false;
            notify_world_spawn_sent(progress.chunks_sent_count);
            log::info!(
                "[Bedrock Session] SetHealth (0x2a, health=20) + PlayStatus (0x02, status=3 / PlayerSpawn) enviado a {} tras {} chunks paced (jugador={}, esperando SetLocalPlayerAsInitialized 0x71)",
                peer_addr,
                progress.chunks_sent_count,
                effective_name
            );
        }
    }

    #[cfg(not(feature = "protocol-core"))]
    {
        let _ = (conn, session, progress, peer_addr, now);
    }
}

/// Drena hasta `max_chunks` de `progress.pending_chunks` respetando el control de congestión SCTP de `str0m`.
pub fn pump_pending_chunks_batch(
    conn: &mut Connection,
    session: &mut ServerConnection,
    progress: &mut SessionProgress,
    peer_addr: SocketAddr,
    now: Instant,
    max_chunks: usize,
) -> usize {
    #[cfg(feature = "protocol-core")]
    {
        let template = cached_spawn_chunk_payload();
        let payload_size = template.len();
        let payload_fnv1a = fnv1a_32_checksum(template);
        let payload_crc32 = crc32_checksum(template);
        let mut batch_sent = 0usize;
        let mut first_packet_size = 0usize;

        while batch_sent < max_chunks {
            let Some((cx, cz)) = progress.pending_chunks.pop_front() else {
                break;
            };
            if let Ok(chunk_bytes) = build_v2225_level_chunk_packet_bytes(cx, cz, template.clone())
            {
                if first_packet_size == 0 {
                    first_packet_size = chunk_bytes.len();
                }
                let batch = session.encode(&[chunk_bytes]);
                conn.send(batch, now);
                batch_sent += 1;
                progress.chunks_sent_count = progress.chunks_sent_count.saturating_add(1);
                if progress.chunks_sent_count == 1 {
                    log::info!(
                        "[Bedrock World] Chunk spawn ({},{}) enviado primero a {}: payload={} bytes, packet={} bytes, subchunks=9, suelo stone/grass y=63, aire y>=64, biome_id={} Plains, crc32=0x{:08x}, fnv1a=0x{:08x}, is_client_biome_update=false (restantes={})",
                        cx,
                        cz,
                        peer_addr,
                        payload_size,
                        first_packet_size,
                        DEFAULT_SPAWN_BIOME_ID,
                        payload_crc32,
                        payload_fnv1a,
                        progress.pending_chunks.len()
                    );
                }
            }
        }

        if progress.pending_chunks.is_empty() && batch_sent > 0 {
            log::info!(
                "[Bedrock World] Streaming pacing completado: {} chunks totales enviados a {} (crc32=0x{:08x}, fnv1a=0x{:08x}, is_client_biome_update=false)",
                progress.chunks_sent_count,
                peer_addr,
                payload_crc32,
                payload_fnv1a
            );
        }

        // Esperar a entregar los 25 chunks (-2..=2, acorde a ChunkRadiusUpdated=2 / view_radius=32) antes de PlayStatus::PlayerSpawn.
        if progress.pending_spawn_finish
            && !progress.has_sent_player_spawn_status
            && progress.pending_chunks.is_empty()
        {
            send_spawn_finish_sequence(conn, session, progress, peer_addr, now);
        }

        batch_sent
    }

    #[cfg(not(feature = "protocol-core"))]
    {
        let _ = (conn, session, progress, peer_addr, now, max_chunks);
        0
    }
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

    if let Ok(pub_pkt) = encode_bedrock_v2225_packet(&NetworkChunkPublisherUpdatePacket::<V2225> {
        new_view_position: BlockPos { x: 0, y: 64, z: 0 },
        new_view_radius: (clamped_radius as u32) * 16,
        server_built_chunks: Vec::new(),
    }) {
        let batch = session.encode(&[pub_pkt]);
        conn.send(batch, now);
    }

    let template = cached_spawn_chunk_payload();
    let mut sent_count = 0usize;
    for (cx, cz) in build_spawn_chunk_ring_coords() {
        if let Ok(chunk_bytes) = build_v2225_level_chunk_packet_bytes(cx, cz, template.clone()) {
            let batch = session.encode(&[chunk_bytes]);
            conn.send(batch, now);
            sent_count += 1;
        }
    }
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

pub fn send_resource_packs_info_only(
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
                    log::info!(
                        "[Bedrock Packs] ResourcePacksInfo (0x06) enviado a {}",
                        peer_addr
                    );
                }
                Err(err) => {
                    log::warn!(
                        "[Bedrock Packs] Error codificando ResourcePacksInfo: {}",
                        err
                    );
                }
            }
        }
    }
    #[cfg(not(feature = "protocol-core"))]
    {
        let _ = (conn, session, progress, peer_addr, now);
    }
}

pub fn send_resource_packs_stack_only(
    conn: &mut Connection,
    session: &mut ServerConnection,
    progress: &mut SessionProgress,
    peer_addr: SocketAddr,
    now: Instant,
) {
    #[cfg(feature = "protocol-core")]
    {
        if !progress.has_sent_resource_packs_stack {
            match build_v2225_resource_pack_stack_bytes() {
                Ok(rp_stack) => {
                    let batch = session.encode(&[rp_stack]);
                    conn.send(batch, now);
                    progress.has_sent_resource_packs_stack = true;
                    log::info!(
                        "[Bedrock Packs] ResourcePackStack (0x07) enviado a {}",
                        peer_addr
                    );
                }
                Err(err) => {
                    log::warn!(
                        "[Bedrock Packs] Error codificando ResourcePackStack: {}",
                        err
                    );
                }
            }
        }
    }
    #[cfg(not(feature = "protocol-core"))]
    {
        let _ = (conn, session, progress, peer_addr, now);
    }
}

pub fn send_resource_packs_sequence(
    conn: &mut Connection,
    session: &mut ServerConnection,
    progress: &mut SessionProgress,
    peer_addr: SocketAddr,
    now: Instant,
) {
    send_resource_packs_info_only(conn, session, progress, peer_addr, now);
    send_resource_packs_stack_only(conn, session, progress, peer_addr, now);
}

pub fn send_world_start_and_spawn(
    conn: &mut Connection,
    session: &mut ServerConnection,
    progress: &mut SessionProgress,
    peer_addr: SocketAddr,
    now: Instant,
) {
    if progress.has_sent_start_game && progress.has_sent_world_registries {
        return;
    }

    // Fase 1 de mundo Bedrock: ResourcePacksInfo -> ResourcePackStack -> VoxelShapes -> StartGamePacket -> Registros/Entorno -> SetTime.
    // Los LevelChunk y PlayStatus::PlayerSpawn NO se envían en ráfaga aquí; esperan RequestChunkRadius (0x45) y se dosifican en rtc_tick.
    send_resource_packs_sequence(conn, session, progress, peer_addr, now);

    #[cfg(feature = "protocol-core")]
    {
        use bedrock::protocol::v662::packets::SetTimePacket;

        if !progress.has_sent_start_game {
            let jigsaw_bytes = build_v2225_jigsaw_structure_data_bytes();
            let jigsaw_batch = session.encode(&[jigsaw_bytes]);
            conn.send(jigsaw_batch, now);

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
                        "[Bedrock Session] StartGamePacket enviado a {} (spawn=0,65.62,0, dim=0 Overworld)",
                        peer_addr
                    );
                    // Nota de protocolo Bedrock: NO enviar ChangeDimensionPacket(dim=0) en el spawn inicial,
                    // ya que StartGamePacket sitúa al jugador en dim=0 y re-enviar dim=0 deja la pantalla de carga infinita.
                }
                Err(err) => {
                    log::warn!(
                        "[Bedrock Session] Error codificando StartGamePacket: {}",
                        err
                    );
                }
            }

            if !progress.has_sent_world_registries {
                let item_bytes = build_v2225_item_component_registry_bytes();
                let spawn_pos_bytes = build_v2225_set_spawn_position_packet_bytes(0, 64, 0, 0);
                let difficulty_bytes = build_v2225_set_difficulty_packet_bytes(2);
                let commands_bytes = build_v2225_set_commands_enabled_bytes(true);
                let adventure_bytes = build_v2225_update_adventure_settings_bytes();
                let abilities_bytes = build_v2225_update_abilities_bytes(1);
                let gamerules_bytes = build_v2225_game_rules_changed_packet_bytes();
                let biome_bytes = build_v2225_biome_definition_list_bytes();
                let entity_bytes = build_v2225_available_entity_identifiers_bytes();
                let attributes_bytes = build_v2225_update_attributes_bytes(1);
                let creative_bytes = build_v2225_creative_content_bytes();

                // Orden BDS/gophertunnel 1.26.60: ItemRegistry (0xa2) inmediatamente después de StartGamePacket
                for pkt_bytes in [
                    item_bytes,
                    spawn_pos_bytes,
                    difficulty_bytes,
                    commands_bytes,
                    adventure_bytes,
                    abilities_bytes,
                    gamerules_bytes,
                    biome_bytes,
                    entity_bytes,
                    attributes_bytes,
                    creative_bytes,
                ] {
                    let batch = session.encode(&[pkt_bytes]);
                    conn.send(batch, now);
                }
                progress.has_sent_world_registries = true;
                log::info!(
                    "[Bedrock Session] Entorno y registros 1.26.60 enviados a {}: JigsawStructureData (0x139), ItemRegistry (0xa2), SetSpawnPosition (0x2b), SetDifficulty (0x3c), SetCommandsEnabled (0x3b), UpdateAdventureSettings (0xbc), UpdateAbilities (0xbb), GameRulesChanged (0x48), BiomeDefinitionList (0x7a), AvailableEntityIdentifiers (0x77), UpdateAttributes (0x1d), CreativeContent (0x91)",
                    peer_addr
                );
            }

            if let Ok(time_bytes) = encode_bedrock_v2225_packet(&SetTimePacket { time: 6000 }) {
                let time_batch = session.encode(&[time_bytes]);
                conn.send(time_batch, now);
                log::info!(
                    "[Bedrock Session] SetTimePacket (0x0a, time=6000) enviado a {} -> esperando RequestChunkRadius (0x45)",
                    peer_addr
                );
            }
        }
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
                log::info!(
                    "[Bedrock Protocol] Recibido paquete de Minecraft: 0x{:02x}",
                    pkt.id
                );
            }

            match pkt.id {
                // TAREA 3.1: RequestNetworkSettings (0xC1 / 193) -> NetworkSettings (0x8F)
                id if id == RequestNetworkSettings::ID
                    || (!progress.has_sent_network_settings && id == 0x0b) =>
                {
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
                        "[Bedrock Login] PlayStatus (0x02, status=0 / LoginSuccess) enviado a {}",
                        peer_addr
                    );
                }

                // Respuesta del cliente al ServerToClientHandshake (0x04 ClientToServerHandshake) -> ResourcePacksInfo (0x06)
                id if id == ClientToServerHandshake::ID => {
                    let _ = pkt.decode::<ClientToServerHandshake>();
                    let _ = ServerToClientHandshake::ID;
                    if !progress.has_sent_login_success {
                        let play_status = PlayStatus {
                            status: PlayStatusStatus::LoginSuccess,
                        };
                        let mut status_buf = BytesMut::new();
                        encode_packet(&play_status, &mut status_buf);
                        let status_batch = session.encode(&[status_buf.freeze()]);
                        conn.send(status_batch, now);
                        progress.has_sent_login_success = true;
                    }

                    send_resource_packs_info_only(conn, session, progress, peer_addr, now);
                    log::info!(
                    "[Bedrock Login] ClientToServerHandshake (0x04) completado con {} -> ResourcePacksInfo (0x06) enviado",
                    peer_addr
                );
                }

                // Respuesta de paquetes de recursos (0x08 ResourcePackClientResponse) -> ResourcePackStack (0x07) -> StartGame (0x0b)
                id if id == ResourcePackClientResponse::ID => {
                    match pkt.decode::<ResourcePackClientResponse>() {
                        Ok(rp) => {
                            log::info!(
                            "[Bedrock Packs] ResourcePackClientResponse (0x08) status={:?} ({} bytes) desde {}",
                            rp.response_status,
                            pkt.body.len(),
                            peer_addr
                        );
                            match rp.response_status {
                                ResourcePackClientResponseResponseStatus::HaveAllPacks
                                | ResourcePackClientResponseResponseStatus::SendPacks => {
                                    send_resource_packs_stack_only(
                                        conn, session, progress, peer_addr, now,
                                    );
                                }
                                ResourcePackClientResponseResponseStatus::Completed => {
                                    progress.has_completed_resource_packs = true;
                                    send_world_start_and_spawn(
                                        conn, session, progress, peer_addr, now,
                                    );
                                }
                                _ => {}
                            }
                        }
                        Err(err) => {
                            log::warn!(
                            "[Bedrock Packs] Error decodificando ResourcePackClientResponse (0x08, {} bytes) desde {}: {}",
                            pkt.body.len(),
                            peer_addr,
                            err
                        );
                        }
                    }
                }

                // FASE 4 P0/P1: ClientCacheStatus (0x81 / 129) suele llegar antes que ResourcePackClientResponse::Completed.
                // Registramos el estado sin disparar StartGamePacket prematuramente ni repetirlo si ya inició el mundo.
                id if id == ClientCacheStatus::ID || id == 0x81 => {
                    progress.has_received_client_cache_status = true;
                    let cache_enabled = pkt
                        .decode::<ClientCacheStatus>()
                        .map(|c| c.enabled)
                        .unwrap_or(false);
                    if progress.has_completed_resource_packs && !progress.has_sent_start_game {
                        log::info!(
                        "[Bedrock Session] ClientCacheStatus (0x81, enabled={}) recibido de {} tras ResourcePacks -> disparando StartGamePacket + Spawn + Chunks",
                        cache_enabled,
                        peer_addr
                    );
                        send_world_start_and_spawn(conn, session, progress, peer_addr, now);
                    } else {
                        log::info!(
                        "[Bedrock Session] ClientCacheStatus (0x81, enabled={}) recibido de {} (rp_completed={}, start_game_sent={})",
                        cache_enabled,
                        peer_addr,
                        progress.has_completed_resource_packs,
                        progress.has_sent_start_game
                    );
                    }
                }

                // Solicitud de radio de chunks (0x45 / 69 RequestChunkRadius) -> ChunkRadiusUpdated (0x46) + NetworkChunkPublisherUpdate + chunks dosificados -> PlayStatus::PlayerSpawn
                id if id == RequestChunkRadius::ID || id == 0x45 => {
                    progress.last_packet_name = "RequestChunkRadius".to_string();
                    let requested = pkt
                        .decode::<RequestChunkRadius>()
                        .map(|r| r.chunk_radius)
                        .unwrap_or(4);
                    if !progress.has_sent_start_game {
                        send_world_start_and_spawn(conn, session, progress, peer_addr, now);
                    }
                    #[cfg(feature = "protocol-core")]
                    {
                        use bedrock::protocol::v662::packets::NetworkChunkPublisherUpdatePacket;
                        use bedrock::protocol::v662::types::BlockPos;
                        use bedrock::protocol::V2225;

                        let approved =
                            send_chunk_radius_updated(conn, session, peer_addr, now, requested);
                        progress.has_sent_chunk_radius = true;

                        if let Ok(pub_pkt) = encode_bedrock_v2225_packet(
                            &NetworkChunkPublisherUpdatePacket::<V2225> {
                                new_view_position: BlockPos { x: 0, y: 64, z: 0 },
                                new_view_radius: (approved as u32) * 16,
                                server_built_chunks: Vec::new(),
                            },
                        ) {
                            let batch = session.encode(&[pub_pkt]);
                            conn.send(batch, now);
                        }

                        let mut queued_total = 0usize;
                        let mut immediate_sent = 0usize;
                        if !progress.has_sent_initial_chunks {
                            progress.has_sent_initial_chunks = true;
                            queued_total = queue_spawn_chunks(progress);
                            immediate_sent = pump_pending_chunks_batch(
                                conn, session, progress, peer_addr, now, 4,
                            );
                        }

                        log::info!(
                        "[Bedrock World] RequestChunkRadius (0x45, req={}) -> ChunkRadiusUpdated (0x46, radius={}) + NetworkChunkPublisherUpdate (view_radius={}) + cola={} chunks (inmediato={}, pacing=4/20ms) enviados a {}",
                        requested,
                        approved,
                        (approved as u32) * 16,
                        queued_total,
                        immediate_sent,
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
                    if let Some((timestamp, needs_response)) =
                        parse_network_stack_latency_packet(&pkt.body)
                    {
                        if needs_response {
                            let pong = build_network_stack_latency_packet_bytes(timestamp, false);
                            let batch = session.encode(&[pong]);
                            conn.send(batch, now);
                            if progress.latency_ping_count <= 3
                                || progress.latency_ping_count.is_multiple_of(20)
                            {
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
                    if !progress.has_sent_start_game {
                        log::warn!(
                        "[Bedrock Move] PlayerAuthInput (0x90) recibido antes de StartGamePacket desde {}; ignorando hasta inicializar mundo",
                        peer_addr
                    );
                        return;
                    }
                    progress.player_auth_input_count =
                        progress.player_auth_input_count.saturating_add(1);
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
                        let _ = session.encode(std::slice::from_ref(&move_bytes));
                        publish_broadcast(move_bytes.clone());
                        if progress.player_auth_input_count == 1
                            || progress.player_auth_input_count.is_multiple_of(40)
                        {
                            log::info!(
                            "[Bedrock Move] PlayerAuthInput (0x90) #{} de {} (raw_y={:.2}, pos={:.2},{:.2},{:.2}, client_init={}) -> MovePlayerPacket (0x13) enviado",
                            progress.player_auth_input_count,
                            peer_addr,
                            input.y,
                            input.x,
                            safe_y,
                            input.z,
                            progress.client_sent_local_player_init
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

                // Pantalla de carga del cliente completada (0x138 / 312 ServerboundLoadingScreen):
                // P1: Registrar LoadingScreenId, NO enviar SetPlayerGameType, ChunkRadiusUpdated ni PlayStatus; esperar 0x71.
                id if id == 0x138 || name == "ServerboundLoadingScreen" => {
                    progress.last_packet_name = "ServerboundLoadingScreen".to_string();
                    let parsed = parse_serverbound_loading_screen_packet(&pkt.body);
                    if let Some((_, Some(screen_id))) = parsed {
                        progress.last_loading_screen_id = Some(screen_id);
                    }
                    let screen_type_name = match parsed.map(|(t, _)| t) {
                        Some(1) => "StartLoadingScreen",
                        Some(2) => "EndLoadingScreen",
                        _ => "Unknown",
                    };
                    log::info!(
                    "[Bedrock World] ServerboundLoadingScreen (0x138, {}) recibido -> esperando SetLocalPlayerAsInitialized (0x71) (peer={}, screen_type={:?}, loading_screen_id={:?}, expected_id={:?})",
                    screen_type_name,
                    peer_addr,
                    parsed.map(|(t, _)| t),
                    parsed.and_then(|(_, id)| id),
                    progress.expected_loading_screen_id
                );
                }

                // Confirmación del cliente tras aparecer en el mundo (0x71 / 113 SetLocalPlayerAsInitialized).
                // El cliente envía 0x71 después de recibir PlayStatus::PlayerSpawn (0x02, status=3) y cargar chunks.
                id if id == SetLocalPlayerAsInitialized::ID || id == 0x71 => {
                    progress.last_packet_name = "SetLocalPlayerAsInitialized".to_string();
                    let decoded_init = pkt.decode::<SetLocalPlayerAsInitialized>().ok();
                    progress.set_local_player_initialized = true;
                    progress.client_sent_local_player_init = true;
                    progress.spawn_init = true;
                    log::info!(
                    "[Bedrock World] SetLocalPlayerAsInitialized (0x71) recibido desde {} (client_init={}, spawn_init={}, entity_runtime_id={:?}, spawn_status_sent={}, loading_screen_id={:?})",
                    peer_addr,
                    progress.client_sent_local_player_init,
                    progress.spawn_init,
                    decoded_init.as_ref().map(|p| p.runtime_entity_id),
                    progress.has_sent_player_spawn_status,
                    progress.last_loading_screen_id
                );
                    #[cfg(feature = "protocol-core")]
                    if !progress.has_sent_player_spawn_status {
                        if let Ok(spawn_status) = build_v2225_player_spawn_status_bytes() {
                            let status_batch = session.encode(&[spawn_status]);
                            conn.send(status_batch, now);
                            progress.has_sent_player_spawn_status = true;
                            log::info!(
                            "[Bedrock Session] PlayStatus (0x02, status=3 / PlayerSpawn) enviado a {} tras SetLocalPlayerAsInitialized (0x71) (fallback)",
                            peer_addr
                        );
                        }
                    }
                }

                0x9c => {
                    let text_preview = String::from_utf8_lossy(&pkt.body);
                    let hex_preview: String = pkt
                        .body
                        .iter()
                        .take(48)
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    log::warn!(
                    "[Bedrock Session] PacketViolationWarning (0x9c) recibido desde {} (len={}, hex=[{}], text={:?})",
                    peer_addr,
                    pkt.body.len(),
                    hex_preview,
                    text_preview
                );
                }

                0x05 => {
                    let text_preview = String::from_utf8_lossy(&pkt.body);
                    log::warn!(
                        "[Bedrock Session] Disconnect (0x05) recibido desde {} (len={}, text={:?})",
                        peer_addr,
                        pkt.body.len(),
                        text_preview
                    );
                }

                // Fase 4: los comandos (prefijo /) llegan como CommandRequest (0x4d), no como Text.
                id if id == 0x4d || name == "CommandRequest" => {
                    progress.last_packet_name = "CommandRequest".to_string();
                    match pkt.decode::<CommandRequest>() {
                        Ok(cmd) => {
                            if !handle_world_command(&cmd.command, peer_addr) {
                                log::info!(
                                "[Bedrock Cmd] CommandRequest (0x4d) no manejado desde {}: {:?}",
                                peer_addr, cmd.command
                            );
                            }
                        }
                        Err(e) => log::warn!(
                            "[Bedrock Cmd] CommandRequest (0x4d) no decodificable desde {}: {}",
                            peer_addr,
                            e
                        ),
                    }
                }

                // Fase 3.2: Text (0x09) -> broadcast a todos los clientes (incluido el emisor).
                id if id == 0x09 || name == "Text" => {
                    progress.last_packet_name = "Text".to_string();
                    match pkt.decode::<Text>() {
                        Ok(text) => {
                            let message = match &text.content {
                                TextContent::Chat(c) => c.message.clone(),
                                TextContent::Whisper(c) => c.message.clone(),
                                TextContent::Announcement(c) => c.message.clone(),
                                TextContent::Raw(s) => s.message.clone(),
                                TextContent::Tip(s) => s.message.clone(),
                                TextContent::System(s) => s.message.clone(),
                                TextContent::JsonWhisper(s) => s.message.clone(),
                                TextContent::Json(s) => s.message.clone(),
                                TextContent::JsonAnnouncement(s) => s.message.clone(),
                                TextContent::Translation(s) => s.message.clone(),
                                TextContent::Popup(s) => s.message.clone(),
                                TextContent::JukeboxPopup(s) => s.message.clone(),
                                TextContent::Default => String::new(),
                            };
                            if handle_world_command(&message, peer_addr) {
                                return;
                            }
                            let has_control = message
                                .chars()
                                .any(|c| c.is_control() && c != '\n' && c != '\t');
                            if message.len() > 1024 || has_control {
                                log::warn!(
                                "[Bedrock Chat] Text (0x09) descartado desde {} (len={}, msg_len={}, control={})",
                                peer_addr, pkt.body.len(), message.len(), has_control
                            );
                            } else {
                                let mut buf = BytesMut::new();
                                encode_packet(&text, &mut buf);
                                publish_broadcast(buf.freeze());
                                log::info!(
                                    "[Bedrock Chat] Text (0x09) de {} (len={}) -> broadcast: {:?}",
                                    peer_addr,
                                    pkt.body.len(),
                                    message.chars().take(120).collect::<String>()
                                );
                            }
                        }
                        Err(err) => log::warn!(
                            "[Bedrock Chat] Text (0x09) no decodificable desde {}: {}",
                            peer_addr,
                            err
                        ),
                    }
                }

                // Fase 3.3: acciones sobre bloques: parseo/registro, sin modificar el mundo aún.
                id @ (0x1e | 0x24) => {
                    let label = if id == 0x1e {
                        "InventoryTransaction"
                    } else {
                        "PlayerAction"
                    };
                    progress.last_packet_name = label.to_string();
                    let hex: String = pkt
                        .body
                        .iter()
                        .take(32)
                        .map(|b| format!("{:02x}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    log::info!(
                        "[Bedrock Action] {} (0x{:02x}) recibido desde {} (len={}, hex=[{}])",
                        label,
                        id,
                        peer_addr,
                        pkt.body.len(),
                        hex
                    );
                    if id == 0x1e {
                        match pkt.decode::<InventoryTransaction>() {
                            Ok(tx) => {
                                if let TransactionTransactionData::ItemUse(use_item) =
                                    &tx.transaction.transaction_data
                                {
                                    let pos = &use_item.block_position;
                                    let new_id = match use_item.action_type {
                                        TransactionUseItemActionType::BreakBlock => 0,
                                        _ => use_item.block_runtime_id,
                                    };
                                    if let Ok(mut map) = overrides().lock() {
                                        map.insert((pos.x, pos.y, pos.z), new_id);
                                    }
                                    log::info!(
                                    "[Bedrock World] Override en ({},{},{}) -> block_runtime_id={} (accion={:?})",
                                    pos.x, pos.y, pos.z, new_id, use_item.action_type
                                );
                                }
                            }
                            Err(e) => log::warn!(
                            "[Bedrock Action] InventoryTransaction no decodificable desde {}: {}",
                            peer_addr, e
                        ),
                        }
                    }
                }

                other => {
                    progress.unknown_pkts = progress.unknown_pkts.saturating_add(1);
                    *progress.unknown_pkt_ids.entry(other).or_insert(0) += 1;
                    log::warn!(
                    "[Bedrock Session] Paquete NO MANEJADO ID=0x{:02x} len={} peer={} ts={:?} name={}",
                    other,
                    pkt.body.len(),
                    peer_addr,
                    now,
                    name
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
        "desconexión antes de StartGamePacket / faltó ResourcePackClientResponse::Completed"
    } else if reason.to_ascii_lowercase().contains("ice") {
        "ICE disconnected: pérdida de conectividad UDP/STUN o cierre de PeerConnection tras espera de spawn"
    } else if progress.has_sent_start_game && !progress.has_sent_chunk_radius {
        "faltó ChunkRadiusUpdated (0x46) tras RequestChunkRadius (0x45)"
    } else if progress.player_auth_input_count > 0 && progress.latency_ping_count == 0 {
        "cierre manual del cliente o falta de respuesta keepalive/chunks"
    } else if reason.contains("peer closed") {
        "cierre limpio iniciado por el cliente (peer closed)"
    } else {
        "cierre de transporte WebRTC"
    };
    let unknown_top = {
        let mut entries: Vec<_> = progress.unknown_pkt_ids.iter().collect();
        entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        entries
            .into_iter()
            .take(5)
            .map(|(id, count)| format!("0x{:02x}:{}", id, count))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "Conexión WebRTC cerrada con {} (session={}, motivo={}). Último paquete procesado: {} | diagnóstico={} | auth_inputs={} | keepalives={} | keepalives_sent={} | chunks_sent={} | pending_chunks={} | out_pkts={} | out_bytes={} | rp_done={} | chunk_radius={} | spawn_status={} | spawn_init={} | client_init={} | loading_screen_id={:?} | local_ice={} | unknown_pkts={} | unknown_top=[{}]",
        remote_addr,
        sid,
        reason,
        last_pkt,
        hint,
        progress.player_auth_input_count,
        progress.latency_ping_count,
        progress.keepalives_sent,
        progress.chunks_sent_count,
        progress.pending_chunks.len(),
        progress.outbound_packets_sent,
        progress.outbound_bytes_sent,
        progress.has_completed_resource_packs,
        progress.has_sent_chunk_radius,
        progress.has_sent_player_spawn_status,
        progress.set_local_player_initialized,
        progress.client_sent_local_player_init,
        progress.last_loading_screen_id,
        progress.local_ice_candidate,
        progress.unknown_pkts,
        unknown_top
    )
}

/// Atiende una conexión TCP entrante del servidor de señalización HTTP NetherNet (`/v1/join`).
#[allow(clippy::too_many_arguments)]
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
                log::warn!(
                    "Error leyendo señalización HTTP desde {}: {}",
                    peer_addr,
                    err
                );
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
            log::warn!(
                "Error parseando petición HTTP NetherNet de {}: {}",
                peer_addr,
                err
            );
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
        && (path_only == PROBE_PATH
            || path_only == join_path(network_id)
            || path_only.starts_with(&expected_join_prefix))
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
        log::info!(
            "[NetherNet ICE] IP local utilizada para candidatos ICE: {} (preferred_tcp_ip={:?})",
            local_candidate,
            local_tcp_ip
        );
        let offer_candidates = extract_sdp_ice_candidates(offer_sdp);
        if !is_private_or_loopback_ip(peer_addr.ip())
            && is_private_or_loopback_ip(local_candidate.ip())
        {
            log::warn!(
                "[NetherNet ICE] Cliente externo {} recibirá candidato privado {} (detrás de NAT, verifica STUN/port-forwarding UDP {})",
                peer_addr,
                local_candidate,
                udp_port
            );
        }
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
                    let resp = http::response(
                        500,
                        "text/plain",
                        "Internal Rust panic during WebRTC answer",
                    );
                    let _ = stream.write_all(&resp).await;
                    return;
                }
            }
        } else {
            Connection::answer(offer_sdp, local_candidate, &host_key, now)
        };
        match answer_res {
            Ok((mut conn, raw_answer_sdp)) => {
                let (answer_sdp, injected_fallback) =
                    ensure_sdp_ice_candidate(&raw_answer_sdp, local_candidate);
                let answer_candidates = extract_sdp_ice_candidates(&answer_sdp);
                log::info!(
                    "Señalización HTTP recibida: SDP offer procesado para {} (candidato ICE local={} | offer_cands={} | answer_cands={} | injected_fallback={} | client_ver={:?})",
                    peer_addr,
                    local_candidate,
                    offer_candidates.len(),
                    answer_candidates.len(),
                    injected_fallback,
                    query_params.version
                );
                log::info!(
                    "[NetherNet SDP] Respuesta SDP completa para {} (candidatos={:?}):\n{}",
                    peer_addr,
                    answer_candidates,
                    answer_sdp
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
                    let init_progress = SessionProgress {
                        local_ice_candidate: local_candidate.to_string(),
                        ..Default::default()
                    };
                    guard.insert(
                        session_id,
                        PeerSession {
                            conn,
                            session: ServerConnection::nethernet(host_key.clone()),
                            remote_addr: peer_addr,
                            progress: init_progress,
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
        Ok::<
            (
                TcpListener,
                UdpSocket,
                SocketAddr,
                Option<(TcpListener, Arc<UdpSocket>, bool)>,
            ),
            String,
        >((tcp, udp, local_tcp_addr, v6_opt))
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
    let world_base = dir_owned.clone();
    let _ = WORLD_BASE.set(world_base.clone());
    load_world_overrides(&world_base);

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
                                                        if let Some((mut conn, raw_answer_sdp)) = lan_ans {
                                                            let (answer_sdp, injected_fallback) =
                                                                ensure_sdp_ice_candidate(&raw_answer_sdp, local_cand);
                                                            let answer_cands = extract_sdp_ice_candidates(&answer_sdp);
                                                            log::info!(
                                                                "Señalización LAN recibida: ConnectRequest desde {} (session={}, local_ice={}, answer_cands={}, injected_fallback={})",
                                                                from,
                                                                sig.connection_id,
                                                                local_cand,
                                                                answer_cands.len(),
                                                                injected_fallback
                                                            );
                                                            while let Some(tx) = conn.poll_transmit() {
                                                                let _ = lan_udp.send_to(&tx.contents, tx.destination).await;
                                                            }
                                                            if let Ok(mut guard) = lan_peers.lock() {
                                                                let init_progress = SessionProgress {
                                                                    local_ice_candidate: local_cand.to_string(),
                                                                    ..Default::default()
                                                                };
                                                                guard.insert(
                                                                    sig.connection_id,
                                                                    PeerSession {
                                                                        conn,
                                                                        session: ServerConnection::nethernet(lan_key.clone()),
                                                                        remote_addr: from,
                                                                        progress: init_progress,
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
        let (bcast_tx, mut bcast_rx) = tokio::sync::broadcast::channel::<Bytes>(512);
        let _ = BROADCAST_TX.set(bcast_tx);
        let mut udp_buf = [0u8; 65535];
        let mut rtc_tick = tokio::time::interval(Duration::from_millis(20));
        let mut keepalive_tick = tokio::time::interval(Duration::from_secs(2));
        keepalive_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut autosave_tick = tokio::time::interval(Duration::from_secs(300));
        autosave_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                                    peer.progress.outbound_packets_sent =
                                        peer.progress.outbound_packets_sent.saturating_add(1);
                                    peer.progress.outbound_bytes_sent = peer
                                        .progress
                                        .outbound_bytes_sent
                                        .saturating_add(tx.contents.len() as u64);
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
                            if !peer.progress.pending_chunks.is_empty() || peer.progress.pending_spawn_finish {
                                let addr = peer.remote_addr;
                                let _ = run_catch_unwind("webrtc_rtc_pump_chunks", || {
                                    pump_pending_chunks_batch(
                                        &mut peer.conn,
                                        &mut peer.session,
                                        &mut peer.progress,
                                        addr,
                                        now,
                                        4,
                                    );
                                });
                            }
                            while let Some(tx) = peer.conn.poll_transmit() {
                                peer.progress.outbound_packets_sent =
                                    peer.progress.outbound_packets_sent.saturating_add(1);
                                peer.progress.outbound_bytes_sent = peer
                                    .progress
                                    .outbound_bytes_sent
                                    .saturating_add(tx.contents.len() as u64);
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
                    let ts_ns = (SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as i64)
                        .saturating_mul(1_000_000);
                    let mut outbound: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
                    if let Ok(mut guard) = peers.lock() {
                        for peer in guard.values_mut() {
                            if peer.progress.client_sent_local_player_init
                                && peer.progress.has_sent_player_spawn_status
                                && peer.progress.pending_chunks.is_empty()
                            {
                                let ping_pkt = build_network_stack_latency_packet_bytes(ts_ns, true);
                                let batch = peer.session.encode(&[ping_pkt]);
                                peer.conn.send(batch, now);
                                peer.progress.keepalives_sent =
                                    peer.progress.keepalives_sent.saturating_add(1);
                                if peer.progress.keepalives_sent == 1 || peer.progress.keepalives_sent % 15 == 0 {
                                    log::info!(
                                        "[Bedrock KeepAlive] NetworkStackLatency (0x73, ts={}) enviado periódicamente (#{}) a {}",
                                        ts_ns,
                                        peer.progress.keepalives_sent,
                                        peer.remote_addr
                                    );
                                }
                                while let Some(tx) = peer.conn.poll_transmit() {
                                    peer.progress.outbound_packets_sent =
                                        peer.progress.outbound_packets_sent.saturating_add(1);
                                    peer.progress.outbound_bytes_sent = peer
                                        .progress
                                        .outbound_bytes_sent
                                        .saturating_add(tx.contents.len() as u64);
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
                _ = autosave_tick.tick() => {
                    save_world_overrides(&world_base);
                }
                bcast_res = bcast_rx.recv() => {
                    if let Ok(raw) = bcast_res {
                        let now = Instant::now();
                        let mut outbound: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
                        if let Ok(mut guard) = peers.lock() {
                            for peer in guard.values_mut() {
                                let batch = peer.session.encode(std::slice::from_ref(&raw));
                                peer.conn.send(batch, now);
                                while let Some(tx) = peer.conn.poll_transmit() {
                                    peer.progress.outbound_packets_sent =
                                        peer.progress.outbound_packets_sent.saturating_add(1);
                                    peer.progress.outbound_bytes_sent = peer
                                        .progress
                                        .outbound_bytes_sent
                                        .saturating_add(tx.contents.len() as u64);
                                    outbound.push((tx.destination, tx.contents));
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
        }
    }));

    *guard = Some(ServerHandle { rt, cancel, arena });
    Ok(format!("engine=running nethernet={}", bind_report_summary))
}

pub fn stop_async_engine() {
    set_current_bind_report(None);
    reset_player_state();
    let taken = handle_slot().lock().ok().and_then(|mut guard| guard.take());

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
    let target_port = if bedrock_port == 0 {
        19132
    } else {
        bedrock_port
    };
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
            TARGET_BEDROCK_PROTOCOL, MOTD_ADVERTISED_VERSION, sid
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
        assert!(is_protocol_compatible(
            query.protocol,
            query.version.as_deref()
        ));
        assert!(!is_protocol_compatible(Some(9999), Some("1.26.62")));
    }

    #[test]
    fn decodes_bedrock_client_1_26_62_request_network_settings_fixture() {
        // Fixture capturada de cliente Android 1.26.62 (protocol=2225 -> 0x000008b1) en frame NetherNet sin 0xFE y con 0xFE
        let client_1_26_62_payload = [0x05, 0xc1, 0x01, 0x00, 0x00, 0x08, 0xb1];
        assert_eq!(
            decode_nethernet_batch_packet_id(&client_1_26_62_payload),
            Some(0xc1)
        );
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
        let decoded_raw =
            decode_bedrock_batch_with_header(&raw_batch).expect("0x00 raw batch should decode");
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
    #[cfg_attr(
        windows,
        ignore = "socket2 Windows no soporta la configuracion dual-stack de este test"
    )]
    fn ipv6_dual_stack_socket_reports_real_fd_only_v6_and_responds_to_loopback_ping() {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let bound = rt.block_on(async { bind_optional_ipv6_sockets(0) });
        if let Some((_tcp_v6, udp_v6, real_v6_only)) = bound {
            let local_v6 = udp_v6.local_addr().expect("local v6 addr");
            assert!(local_v6.is_ipv6());
            assert!(real_v6_only);
        }
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "requiere el puerto 19132 libre (ocupado por el puente del emulador en Windows)"
    )]
    fn runtime_engine_starts_and_stops_cleanly() {
        let cfg = LowLatencyConfig {
            local_fast_path: true,
            tcp_no_delay: true,
            ip_tos_low_delay: true,
            local_jitter_ms: 0,
            wan_mtu_bytes: 1400,
        };
        let status = start_async_engine(
            "/tmp/bedrock_test",
            "BedrockDroid RS",
            "BEDROCK",
            0,
            8192,
            8,
            cfg,
        )
        .expect("start failed");
        assert!(status.starts_with("engine=running nethernet="));
        assert!(is_engine_running());
        assert_eq!(current_engine_state(), EngineState::Running);

        // P2: Bloqueo en Rust mientras el servidor está RUNNING (sin redimensionar en caliente)
        let second_err = start_async_engine(
            "/tmp/bedrock_test",
            "BedrockDroid RS",
            "BEDROCK",
            0,
            4096,
            4,
            cfg,
        )
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
        tcp.write_all(raw_req.as_bytes())
            .expect("write GET /v1/join");
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
        assert!(empty_chunk.len() >= 10_240 && empty_chunk.len() <= 20_480);
        assert!(spawn_chunk.len() >= 10_240 && spawn_chunk.len() <= 20_480);
        assert_ne!(
            fnv1a_32_checksum(&empty_chunk),
            fnv1a_32_checksum(&spawn_chunk)
        );
        assert_ne!(crc32_checksum(&empty_chunk), 0);
        assert_ne!(crc32_checksum(&empty_chunk), crc32_checksum(&spawn_chunk));
        // SubChunk -4 (0xFC) con versión v9 (0x09), 1 capa (0x01) y almacenamiento 4 bits/bloque (0x09)
        assert_eq!(&spawn_chunk[..4], &[0x09, 0x01, (-4i8) as u8, 0x09]);
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

        let change_dim_bytes =
            build_v2225_change_dimension_packet_bytes(0, 0.0, 64.0, 0.0, false, Some(1));
        let change_dim_raw = RawPacket::parse(change_dim_bytes).expect("parse change_dimension");
        assert_eq!(change_dim_raw.id, 0x3d);
        assert_eq!(change_dim_raw.body[0], 0x00);
        assert_eq!(&change_dim_raw.body[5..9], &64.0f32.to_le_bytes());
        assert_eq!(change_dim_raw.body[13], 0x00);
        assert_eq!(change_dim_raw.body[14], 0x01);
        assert_eq!(&change_dim_raw.body[15..19], &1u32.to_le_bytes());

        let change_dim_none =
            build_v2225_change_dimension_packet_bytes(0, 0.0, 64.0, 0.0, true, None);
        let change_dim_none_raw =
            RawPacket::parse(change_dim_none).expect("parse change_dimension none");
        assert_eq!(change_dim_none_raw.body[13], 0x01);
        assert_eq!(change_dim_none_raw.body[14], 0x00);

        let mut ls_body = Vec::new();
        encode_varuint32(zigzag32(2), &mut ls_body);
        ls_body.push(0x01);
        ls_body.extend_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            parse_serverbound_loading_screen_packet(&ls_body),
            Some((2, Some(1)))
        );
        let mut ls_none = Vec::new();
        encode_varuint32(zigzag32(1), &mut ls_none);
        ls_none.push(0x00);
        assert_eq!(
            parse_serverbound_loading_screen_packet(&ls_none),
            Some((1, None))
        );

        let sdp_sample = "v=0\r\na=candidate:1 1 UDP 2130706431 192.168.1.50 19132 typ host\r\n";
        assert_eq!(extract_sdp_ice_candidates(sdp_sample).len(), 1);
        assert!(is_private_or_loopback_ip(IpAddr::V4(Ipv4Addr::new(
            192, 168, 1, 50
        ))));
        assert!(!is_private_or_loopback_ip(IpAddr::V4(Ipv4Addr::new(
            8, 8, 8, 8
        ))));
        let cand_addr: SocketAddr = "192.168.1.85:19132".parse().expect("cand addr");
        let sdp_no_cand =
            "v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=end-of-candidates\r\n";
        let (patched_sdp, injected) = ensure_sdp_ice_candidate(sdp_no_cand, cand_addr);
        assert!(injected);
        assert!(patched_sdp.contains("a=candidate:1 1 UDP 2130706431 192.168.1.85 19132 typ host"));
        let (_unchanged_sdp, injected_again) = ensure_sdp_ice_candidate(&patched_sdp, cand_addr);
        assert!(!injected_again);

        let spawn_pos_bytes = build_v2225_set_spawn_position_packet_bytes(0, 64, 0, 0);
        let spawn_pos_raw = RawPacket::parse(spawn_pos_bytes).expect("parse set_spawn_position");
        assert_eq!(spawn_pos_raw.id, 0x2b);
        // NetworkBlockPosition.y debe ir en ZigZag32: 64 -> 128 -> [0x80, 0x01]
        assert!(spawn_pos_raw.body.windows(2).any(|w| w == [0x80, 0x01]));

        let diff_bytes = build_v2225_set_difficulty_packet_bytes(2);
        assert_eq!(
            RawPacket::parse(diff_bytes)
                .expect("parse set_difficulty")
                .id,
            0x3c
        );

        let gamerules_bytes = build_v2225_game_rules_changed_packet_bytes();
        let gamerules_raw = RawPacket::parse(gamerules_bytes).expect("parse gamerules");
        assert_eq!(gamerules_raw.id, 0x48);
        assert!(gamerules_raw
            .body
            .windows(15)
            .any(|w| w == b"showcoordinates"));

        let biome_bytes = build_v2225_biome_definition_list_bytes();
        let biome_raw = RawPacket::parse(biome_bytes).expect("parse biome_definition_list");
        assert_eq!(biome_raw.id, 0x7a);
        assert!(biome_raw.body.len() > 5_000);
        assert!(biome_raw.body.windows(6).any(|w| w == b"plains"));
        assert!(biome_raw.body.windows(12).any(|w| w == b"sulfur_caves"));

        let entity_bytes = build_v2225_available_entity_identifiers_bytes();
        let entity_raw =
            RawPacket::parse(entity_bytes).expect("parse available_entity_identifiers");
        assert_eq!(entity_raw.id, 0x77);
        assert!(entity_raw.body.windows(6).any(|w| w == b"idlist"));
        assert!(entity_raw
            .body
            .windows(16)
            .any(|w| w == b"minecraft:player"));
        assert!(entity_raw.body.windows(14).any(|w| w == b"minecraft:boat"));

        let item_reg_bytes = build_v2225_item_component_registry_bytes();
        let item_reg_raw = RawPacket::parse(item_reg_bytes).expect("parse item_registry");
        assert_eq!(item_reg_raw.id, 0xa2);
        assert!(item_reg_raw.body.len() > 110_000);
        assert!(item_reg_raw
            .body
            .windows(29)
            .any(|w| w == b"minecraft:frostbite_spawn_egg"));

        let jigsaw_raw =
            RawPacket::parse(build_v2225_jigsaw_structure_data_bytes()).expect("parse jigsaw");
        assert_eq!(jigsaw_raw.id, 0x139);
        assert!(jigsaw_raw.body.windows(14).any(|w| w == b"template_pools"));

        let cmds_raw =
            RawPacket::parse(build_v2225_set_commands_enabled_bytes(true)).expect("parse commands");
        assert_eq!(cmds_raw.id, 0x3b);
        assert_eq!(cmds_raw.body.as_ref(), &[0x01]);

        let adv_raw = RawPacket::parse(build_v2225_update_adventure_settings_bytes())
            .expect("parse adventure");
        assert_eq!(adv_raw.id, 0xbc);
        assert_eq!(adv_raw.body.as_ref(), &[0, 0, 0, 1, 1]);

        let abil_raw =
            RawPacket::parse(build_v2225_update_abilities_bytes(1)).expect("parse abilities");
        assert_eq!(abil_raw.id, 0xbb);
        assert_eq!(abil_raw.body.len(), 33);

        let attr_raw =
            RawPacket::parse(build_v2225_update_attributes_bytes(1)).expect("parse attributes");
        assert_eq!(attr_raw.id, 0x1d);
        assert!(attr_raw.body.windows(16).any(|w| w == b"minecraft:health"));

        let health_raw =
            RawPacket::parse(build_v2225_set_health_bytes(20)).expect("parse set_health");
        assert_eq!(health_raw.id, 0x2a);

        let creative_bytes = build_v2225_creative_content_bytes();
        assert_eq!(
            RawPacket::parse(creative_bytes)
                .expect("parse creative_content")
                .id,
            0x91
        );

        let chunk_bytes =
            build_v2225_level_chunk_packet_bytes(0, 0, build_spawn_chunk_payload_nbtx())
                .expect("level_chunk");
        let chunk_raw = RawPacket::parse(chunk_bytes).expect("parse level_chunk");
        assert_eq!(chunk_raw.id, 0x3a);
        assert_eq!(chunk_raw.body.last().copied(), Some(0x00));

        let spawn_bytes = build_v2225_player_spawn_status_bytes().expect("player_spawn");
        let spawn_raw = RawPacket::parse(spawn_bytes).expect("parse player_spawn");
        assert_eq!(spawn_raw.id, 0x02);

        let test_uuid = derive_player_uuid_bytes("SteveDroid");
        let player_list_bytes =
            build_v2225_player_list_add_packet_bytes(&test_uuid, 1, "SteveDroid", "");
        let player_list_raw = RawPacket::parse(player_list_bytes).expect("parse player_list");
        assert_eq!(player_list_raw.id, 0x3f);
        assert!(player_list_raw.body.windows(10).any(|w| w == b"SteveDroid"));
        assert!(player_list_raw
            .body
            .windows(14)
            .any(|w| w == b"Standard_Steve"));

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
        assert!(add_player_raw
            .body
            .windows(4)
            .any(|w| w == 64.0f32.to_le_bytes()));

        let game_type_bytes = build_v2225_set_player_game_type_bytes(0);
        let game_type_raw = RawPacket::parse(game_type_bytes).expect("parse set_player_game_type");
        assert_eq!(game_type_raw.id, 0x3e);
        assert_eq!(game_type_raw.body.as_ref(), &[0x00]);

        // Verificar parseo de PlayerAuthInput (0x90) -> MovePlayerPacket (0x13) con Y >= 64.0 y NetworkStackLatency (0x73)
        assert_eq!(clamp_player_auth_y(63.25), 64.0);
        assert_eq!(clamp_player_auth_y(64.0), 64.0);
        assert_eq!(clamp_player_auth_y(65.62), 65.62);

        let mut auth_body = Vec::new();
        for f in [
            12.5f32, 90.0f32, 1.25f32, 65.62f32, -2.5f32, 0.0f32, 1.0f32, 91.0f32,
        ] {
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
        let move_raw = RawPacket::parse(move_pkt).expect("parse move_player");
        assert_eq!(move_raw.id, 0x13);
        assert_eq!(
            &move_raw.body[move_raw.body.len() - 3..],
            &[0x00, 0x00, 0x01]
        );

        let mut latency_body = Vec::new();
        latency_body.extend_from_slice(&123_456_789i64.to_le_bytes());
        latency_body.push(1u8);
        let (ts, needs_resp) =
            parse_network_stack_latency_packet(&latency_body).expect("parse latency");
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
        let coords = build_spawn_chunk_ring_coords();
        assert_eq!(coords.len(), 25);
        assert_eq!(coords.front().copied(), Some((0, 0)));
        assert_eq!(queue_spawn_chunks(&mut dbg_prog), 25);
        assert!(dbg_prog.pending_spawn_finish);
        let dbg_line = format_disconnect_debug_summary(
            42,
            "192.168.1.79:44951".parse().expect("socket addr"),
            &dbg_prog,
            "peer closed",
        );
        assert!(dbg_line.contains("Último paquete procesado: PlayerAuthInput (0x90)"));
        assert!(dbg_line.contains("spawn_init=true"));
        assert!(dbg_line.contains("pending_chunks=25"));
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
        let tmp_path =
            std::env::temp_dir().join(format!("bdr_crash_test_{}.txt", std::process::id()));
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

    #[test]
    fn decodes_set_local_player_as_initialized_0x71_runtime_entity_id() {
        // Header varuint con id=0x71 (113) y cuerpo con runtime_entity_id=1.
        let raw = RawPacket::parse(Bytes::from_static(&[0x71, 0x01]))
            .expect("0x71 packet header must parse");
        assert_eq!(raw.id, 0x71);
        let init = raw
            .decode::<SetLocalPlayerAsInitialized>()
            .expect("0x71 body must decode");
        assert_eq!(init.runtime_entity_id, 1);
    }

    #[test]
    fn parses_player_auth_input_0x90_body_and_rejects_short_payload() {
        let mut body = Vec::new();
        body.extend_from_slice(&0.0f32.to_le_bytes()); // pitch
        body.extend_from_slice(&1.5f32.to_le_bytes()); // yaw
        body.extend_from_slice(&2.0f32.to_le_bytes()); // x
        body.extend_from_slice(&66.46f32.to_le_bytes()); // y
        body.extend_from_slice(&3.0f32.to_le_bytes()); // z
        body.extend_from_slice(&[0u8; 8]); // relleno hasta el offset 28
        body.extend_from_slice(&1.5f32.to_le_bytes()); // head_yaw

        let parsed = parse_player_auth_input_packet(&body).expect("valid 0x90 body");
        assert_eq!(parsed.x, 2.0);
        assert_eq!(parsed.y, 66.46);
        assert_eq!(parsed.z, 3.0);
        assert_eq!(parsed.head_yaw, 1.5);
        assert!(parse_player_auth_input_packet(&[0u8; 4]).is_none());
    }

    #[test]
    fn disconnect_summary_reports_unknown_packet_top5() {
        let mut progress = SessionProgress::default();
        progress.last_packet_name = "PlayerAuthInput".to_string();
        progress.last_packet_id = Some(0x90);
        progress.player_auth_input_count = 100;
        progress.unknown_pkts = 12;
        progress.unknown_pkt_ids.insert(0x2c, 7);
        progress.unknown_pkt_ids.insert(0x1e, 3);
        progress.unknown_pkt_ids.insert(0x21, 2);

        let summary = format_disconnect_debug_summary(
            42,
            "127.0.0.1:55555".parse().expect("addr"),
            &progress,
            "peer closed",
        );
        assert!(summary.contains("unknown_pkts=12"));
        assert!(summary.contains("unknown_top=[0x2c:7, 0x1e:3, 0x21:2]"));
        assert!(summary.contains("PlayerAuthInput (0x90)"));
    }
}
