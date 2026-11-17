#![allow(unused_crate_dependencies)]

pub mod net;
pub mod perf;
pub mod runtime;

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jint, jstring, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;
use std::sync::Arc;

fn into_jstring_or_null(env: &JNIEnv, text: String) -> jstring {
    env.new_string(text)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_registerPlayerCallback(
    env: JNIEnv,
    _class: JClass,
    listener: JObject,
) {
    if listener.is_null() {
        runtime::set_player_event_hook(None);
        return;
    }
    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(err) => {
            log::warn!("[RustJNI] No se pudo obtener JavaVM para callback de jugadores: {}", err);
            return;
        }
    };
    let global_listener: GlobalRef = match env.new_global_ref(&listener) {
        Ok(r) => r,
        Err(err) => {
            log::warn!("[RustJNI] No se pudo fijar GlobalRef para listener de jugadores: {}", err);
            return;
        }
    };
    runtime::set_player_event_hook(Some(Arc::new(move |player_name: &str, connected_count: usize, connected: bool| {
        if let Ok(mut attach_env) = vm.attach_current_thread() {
            if let Ok(jname) = attach_env.new_string(player_name) {
                let jname_obj: JObject = jname.into();
                let count_arg = connected_count.min(i32::MAX as usize) as jint;
                let connected_arg: jboolean = if connected { JNI_TRUE } else { JNI_FALSE };
                let call_res = attach_env.call_method(
                    global_listener.as_obj(),
                    "onPlayerLoginStateChanged",
                    "(Ljava/lang/String;IZ)V",
                    &[
                        JValue::Object(&jname_obj),
                        JValue::Int(count_arg),
                        JValue::Bool(connected_arg),
                    ],
                );
                if let Err(err) = call_res {
                    log::warn!("[RustJNI] Fallo invocando onPlayerLoginStateChanged: {}", err);
                }
                if attach_env.exception_check().unwrap_or(false) {
                    let _ = attach_env.exception_clear();
                }
            }
        }
    })));
}

#[no_mangle]
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_pollConnectedPlayerStatus(
    env: JNIEnv,
    _class: JClass,
) -> jstring {
    into_jstring_or_null(&env, runtime::current_connected_player_status_json())
}

#[no_mangle]
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_initRuntime(
    mut env: JNIEnv,
    _class: JClass,
    engine_dir: JString,
    server_name: JString,
    lane_name: JString,
    bedrock_port: jint,
    max_ram_mb: jint,
    blocking_threads: jint,
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

    let lane: String = {
        env.get_string(&lane_name)
            .map(|s| s.into())
            .unwrap_or_else(|_| "BEDROCK".to_string())
    };

    let udp_port = if bedrock_port <= 0 || bedrock_port > 65535 {
        19132
    } else {
        bedrock_port as u16
    };

    let rust_ram_mb = if max_ram_mb <= 0 {
        4096u32
    } else {
        max_ram_mb as u32
    };

    let threads = if blocking_threads <= 0 {
        ((rust_ram_mb / 1024).clamp(4, 32)) as usize
    } else {
        (blocking_threads as usize).clamp(1, 64)
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

    let msg = match runtime::start_async_engine(&dir, &motd_name, &lane, udp_port, rust_ram_mb, threads, net_cfg) {
        Ok(summary) => format!(
            "OK bedrock={} motd={} proto={} {} max_ram={}MB blocking_threads={} dir={} fast_path={} nodelay={} jitter={}ms mtu={} {}",
            runtime::TARGET_BEDROCK_VERSION,
            runtime::MOTD_ADVERTISED_VERSION,
            runtime::TARGET_BEDROCK_PROTOCOL,
            summary,
            rust_ram_mb,
            threads,
            dir,
            net_cfg.local_fast_path,
            net_cfg.tcp_no_delay,
            net_cfg.local_jitter_ms,
            net_cfg.wan_mtu_bytes,
            aff
        ),
        Err(err) => format!("ERR engine_err={}", err),
    };
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
    let active_bind = runtime::current_bind_report()
        .map(|b| (b.bind_v4, b.bind_v6, b.ipv6_v6only));
    let report = net::upnp_nat::probe_local_and_wan_with_bind(
        bedrock_port as u16,
        java_port as u16,
        enable_upnp == JNI_TRUE,
        dual_stack_ipv6 == JNI_TRUE,
        active_bind,
        true,
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
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_sendPingCommand(
    env: JNIEnv,
    _class: JClass,
    bedrock_port: jint,
) -> jstring {
    runtime::init_android_logging();
    let udp_port = if bedrock_port <= 0 || bedrock_port > 65535 {
        19132
    } else {
        bedrock_port as u16
    };
    let result = runtime::send_ping_command(udp_port);
    into_jstring_or_null(&env, result)
}

#[no_mangle]
pub extern "system" fn Java_com_bedrockdroid_rs_nativebridge_RustCoreBridge_shutdownRuntime(
    _env: JNIEnv,
    _class: JClass,
) {
    runtime::stop_async_engine();
}
