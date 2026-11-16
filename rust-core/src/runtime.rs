use crate::net::low_latency::LowLatencyConfig;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use tokio_util::sync::CancellationToken;

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

fn handle_slot() -> &'static Mutex<Option<ServerHandle>> {
    SERVER_HANDLE.get_or_init(|| Mutex::new(None))
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

pub fn start_async_engine(data_dir: &str, net_cfg: LowLatencyConfig) -> Result<String, String> {
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

    rt.spawn(async move {
        log::info!(
            "BedrockDroid RS Fase 1 listo: dir={} target={} motd={} proto={} fast_path={}",
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

        task_cancel.cancelled().await;
        log::info!("Runtime Tokio cancelado ordenadamente");
    });

    *guard = Some(ServerHandle { rt, cancel });
    Ok("engine=running".to_string())
}

pub fn stop_async_engine() {
    let taken = handle_slot()
        .lock()
        .ok()
        .and_then(|mut guard| guard.take());

    if let Some(handle) = taken {
        handle.cancel.cancel();
        // Límite duro de 500 ms para no bloquear el hilo UI de Android en onDestroy().
        // shutdown_timeout abandona cualquier tarea bloqueada tras expirar.
        handle.rt.shutdown_timeout(Duration::from_millis(500));
        log::info!("bedrock_android_core runtime detenido");
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
    fn runtime_engine_starts_and_stops_cleanly() {
        let cfg = LowLatencyConfig {
            local_fast_path: true,
            tcp_no_delay: true,
            ip_tos_low_delay: true,
            local_jitter_ms: 0,
            wan_mtu_bytes: 1400,
        };
        let status = start_async_engine("/tmp/bedrock_test", cfg).expect("start failed");
        assert_eq!(status, "engine=running");
        assert!(is_engine_running());
        stop_async_engine();
        assert!(!is_engine_running());
    }
}
