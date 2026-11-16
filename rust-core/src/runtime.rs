use crate::net::low_latency::LowLatencyConfig;
use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
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

/// Genera un ServerID aleatorio de 64 bits que permanece estático mientras el servidor esté encendido.
pub fn server_id() -> u64 {
    *SERVER_GUID.get_or_init(|| {
        use std::hash::BuildHasher;
        let mut id = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
        if id == 0 {
            id = 0x5241_4B4E_4554_0001;
        }
        id
    })
}

/// Construye el string MOTD estándar de Minecraft Bedrock para Unconnected Pong (0x1c).
pub fn build_bedrock_advertisement(motd_name: &str, ipv4_port: u16) -> String {
    let clean_name = motd_name.replace(';', " ");
    let name = if clean_name.trim().is_empty() {
        "BedrockDroid RS"
    } else {
        clean_name.trim()
    };
    let effective_port = if ipv4_port == 0 { 19132 } else { ipv4_port };
    let ipv6_port = effective_port.saturating_add(1);
    format!(
        "MCPE;{};{};{};0;10;{};Bedrock World;Survival;1;{};{};",
        name,
        TARGET_BEDROCK_PROTOCOL,
        MOTD_ADVERTISED_VERSION,
        server_id(),
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

pub fn start_async_engine(
    data_dir: &str,
    server_name: &str,
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

    #[cfg(feature = "transport-raknet")]
    let bound_summary: String = {
        let bind_addr: SocketAddr = format!("0.0.0.0:{}", bedrock_port)
            .parse()
            .map_err(|e| format!("invalid bind addr: {}", e))?;

        let raknet_cfg = RaknetListenerConfig {
            max_mtu: net_cfg.wan_mtu_bytes.clamp(576, 1492),
            advertisement: motd_string.as_bytes().to_vec(),
            ..RaknetListenerConfig::default()
        };

        let mut listener = rt
            .block_on(RaknetListener::bind_with_config(bind_addr, raknet_cfg))
            .map_err(|e| format!("raknet bind 0.0.0.0:{} error: {}", bedrock_port, e))?;

        let actual_addr = listener.local_addr();
        let advertised_motd = motd_string.clone();

        rt.spawn(async move {
            log::info!(
                "BedrockDroid RS Fase 2 listo: dir={} target={} motd_ver={} proto={} fast_path={}",
                dir_owned,
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

            log::info!("Servidor RakNet escuchando en 0.0.0.0:{}", actual_addr.port());
            log::info!("MOTD Unconnected Pong (ServerID={}): {}", server_id(), advertised_motd);

            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => {
                        log::info!("Listener RakNet cancelado por CancellationToken");
                        break;
                    }
                    maybe_stream = listener.accept() => {
                        match maybe_stream {
                            Some(stream) => {
                                // Fase 2: Solo Ping/Pong y conexión RakNet base; aún no se procesa Login Handshake de Bedrock.
                                log::info!(
                                    "Cliente RakNet conectado (handshake UDP completado) desde {}",
                                    stream.peer_addr()
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

        format!("engine=running raknet=0.0.0.0:{}", actual_addr.port())
    };

    #[cfg(not(feature = "transport-raknet"))]
    let bound_summary: String = {
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
        // Límite duro de 500 ms para no bloquear el hilo UI de Android en onDestroy().
        // shutdown_timeout abandona cualquier tarea de socket y libera el puerto UDP.
        handle.rt.shutdown_timeout(Duration::from_millis(500));
        log::info!("bedrock_android_core runtime detenido y socket RakNet liberado");
    }
}

pub fn is_engine_running() -> bool {
    handle_slot()
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motd_advertisement_matches_minecraft_bedrock_format() {
        let motd = build_bedrock_advertisement("BedrockDroid RS", 19132);
        let expected_prefix = format!(
            "MCPE;BedrockDroid RS;{};{};0;10;{};Bedrock World;Survival;1;19132;19133;",
            TARGET_BEDROCK_PROTOCOL,
            MOTD_ADVERTISED_VERSION,
            server_id()
        );
        assert_eq!(motd, expected_prefix);
        assert_eq!(server_id(), server_id());
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
        let status = start_async_engine("/tmp/bedrock_test", "BedrockDroid RS", 0, cfg)
            .expect("start failed");
        assert!(status.starts_with("engine=running"));
        assert!(is_engine_running());
        stop_async_engine();
        assert!(!is_engine_running());
    }
}
