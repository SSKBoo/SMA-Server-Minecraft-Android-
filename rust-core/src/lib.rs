#![allow(unused_crate_dependencies)]

pub mod net;
pub mod perf;
pub mod runtime;

use jni::objects::{JClass, JString};
use jni::sys::{jboolean, jint, jstring, JNI_TRUE};
use jni::JNIEnv;

fn into_jstring_or_null(env: &JNIEnv, text: String) -> jstring {
    env.new_string(text)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_initRuntime(
    mut env: JNIEnv,
    _class: JClass,
    engine_dir: JString,
    server_name: JString,
    bedrock_port: jint,
    local_fast_path: jboolean,
    tcp_no_delay: jboolean,
    ip_tos_low_delay: jboolean,
    local_jitter_ms: jint,
    wan_mtu_bytes: jint,
    snapdragon_pinning: jboolean,
    io_prio_be: jboolean,
    thermal_guard_c: jint,
) -> jstring {
    runtime::init_android_logging();

    let dir: String = {
        env.get_string(&engine_dir)
            .map(|s| s.into())
            .unwrap_or_else(|_| "/data/local/tmp".to_string())
    };

    let motd_name: String = {
        env.get_string(&server_name)
            .map(|s| s.into())
            .unwrap_or_else(|_| "BedrockDroid RS".to_string())
    };

    let udp_port = if bedrock_port <= 0 || bedrock_port > 65535 {
        19132
    } else {
        bedrock_port as u16
    };

    let net_cfg = net::low_latency::LowLatencyConfig {
        local_fast_path: local_fast_path == JNI_TRUE,
        tcp_no_delay: tcp_no_delay == JNI_TRUE,
        ip_tos_low_delay: ip_tos_low_delay == JNI_TRUE,
        local_jitter_ms: local_jitter_ms.max(0) as u32,
        wan_mtu_bytes: wan_mtu_bytes.clamp(1200, 1492) as u16,
    };

    let aff = perf::affinity::apply_runtime_tuning(
        snapdragon_pinning == JNI_TRUE,
        io_prio_be == JNI_TRUE,
        thermal_guard_c,
    );

    let engine_state = match runtime::start_async_engine(&dir, &motd_name, udp_port, net_cfg) {
        Ok(summary) => summary,
        Err(err) => format!("engine_err={}", err),
    };

    let msg = format!(
        "OK bedrock={} motd={} proto={} {} dir={} fast_path={} nodelay={} jitter={}ms mtu={} {}",
        runtime::TARGET_BEDROCK_VERSION,
        runtime::MOTD_ADVERTISED_VERSION,
        runtime::TARGET_BEDROCK_PROTOCOL,
        engine_state,
        dir,
        net_cfg.local_fast_path,
        net_cfg.tcp_no_delay,
        net_cfg.local_jitter_ms,
        net_cfg.wan_mtu_bytes,
        aff
    );
    into_jstring_or_null(&env, msg)
}

#[no_mangle]
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_probeNetworkAndUpnp(
    env: JNIEnv,
    _class: JClass,
    bedrock_port: jint,
    java_port: jint,
    enable_upnp: jboolean,
    dual_stack_ipv6: jboolean,
) -> jstring {
    let report = net::upnp_nat::probe_local_and_wan(
        bedrock_port as u16,
        java_port as u16,
        enable_upnp == JNI_TRUE,
        dual_stack_ipv6 == JNI_TRUE,
    );
    into_jstring_or_null(&env, report)
}

#[no_mangle]
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_readSoCTopologyJson(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    let json = perf::affinity::inspect_cpu_topology_json();
    into_jstring_or_null(&env, json)
}

#[no_mangle]
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_shutdownRuntime(
    _env: JNIEnv,
    _class: JClass,
) {
    runtime::stop_async_engine();
}
