use bedrock_android_core::net::low_latency::LowLatencyConfig;
use bedrock_android_core::runtime::{init_android_logging, start_async_engine};

/// Servidor de desarrollo NetherNet para probar desde la PC sin compilar la APK.
/// Uso: cargo run --bin dev_server -- <data_dir> <port>
fn main() {
    // En escritorio el logger de Android no imprime: instalamos un hook visible
    // después del init del core para diagnosticar fallos de arranque.
    init_android_logging();
    std::panic::set_hook(Box::new(|info| eprintln!("DEV_SERVER PANIC: {}", info)));

    let args: Vec<String> = std::env::args().collect();
    let data_dir = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "C:/IA/bedrockdroid-rs-android-arm64/rust-core/dev_world".to_string());
    let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(19140);

    let cfg = LowLatencyConfig {
        local_fast_path: true,
        tcp_no_delay: true,
        ip_tos_low_delay: true,
        local_jitter_ms: 0,
        wan_mtu_bytes: 1400,
    };

    match start_async_engine(&data_dir, "BedrockDroid Dev", "BEDROCK", port, 4096, 4, cfg) {
        Ok(status) => println!("DEV_SERVER listo: {}", status),
        Err(e) => {
            eprintln!("DEV_SERVER ERROR: {}", e);
            std::process::exit(1);
        }
    }

    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
