# Reporte Técnico — Fase 1 y Fase 1.5 (BedrockDroid RS)

## 1. Criterios de éxito validados en Fase 1 (CI & Núcleo Rust)
- **GitHub Actions CI (`android-rust-ci.yml`)**: `BUILD SUCCESSFUL` generando APK nativo (~9 MB) con Gradle 8.10.2, AGP 8.7.3, `compileSdk 35` y NDK unificado `27.2.12479018`.
- **Tests unitarios Rust**: `cargo test --lib` superado (3/3 tests, incluyendo `runtime_engine_starts_and_stops_cleanly`, `loopback_bypasses_jitter` y `detects_carrier_grade_nat_range`).
- **Dependencias pineadas en `rust-core/Cargo.toml`**:
  - `bedrock` pineado al commit `a730b6bbf28c80dee37ff82d2275f1db9078c03e` (`bedrock-rs`, soporte protocolo `2225` / `V2225`).
  - `raknet` (`9bd968ed`), `nbtx` (`9bcb986c`) y `leveldb` puro Rust desde git; `tokio-raknet = "0.2"` opcional.
  - Crates compilados en ARM64 (`aarch64-linux-android`): `bedrock_protocol_core`, `bedrock_protocol`, `bedrock_auth`, `bedrock_shared`, `bedrock_network`, `bedrock`, `bedrock_macros`.
  - Criptografía pura Rust (`p384`, `rsa`, `aes`, `ctr`, `chacha20`, `sha2`, `ring`) sin dependencia de `openssl-sys` ni `zlib-ng` en C.
- **Compatibilidad 16 KB (Android 15+)**: `rust-core/.cargo/config.toml` aplica `-Wl,-z,max-page-size=16384`.

## 2. Warnings conocidos (no bloqueantes)
1. **`rustls` sin invocación directa en Fase 1**:
   - *Motivo*: `rustls` + `webpki-roots` están declarados en `Cargo.toml` para el cliente HTTP/TLS de autenticación Xbox Live (Fase 3 / `auth-xbl` con `ureq`). Mientras no se compile la Fase 3, Rust puede emitir aviso de dependencia declarada sin uso directo en `lib.rs`.
   - *Por qué no bloquea*: No afecta al enlazado de `libbedrock_android_core.so` ni añade símbolos C nativos.
2. **Mismatch histórico de variables NDK en runner CI**:
   - *Motivo*: Las imágenes `ubuntu-latest` predefinen `ANDROID_NDK_HOME`/`ANDROID_NDK_LATEST_HOME` con versiones previas mientras `setup-android` instala `27.2.12479018`.
   - *Solución aplicada en Fase 1.5*: `android-rust-ci.yml` exporta `ANDROID_NDK_HOME`, `ANDROID_NDK`, `ANDROID_NDK_ROOT` y `NDK_HOME` apuntando todos a `${ANDROID_HOME}/ndk/27.2.12479018`.

## 3. Verificación con `adb` (cuando dispongas de PC)
```bash
# 1. Instalar verificando ABI ARM64
adb install -r --abi arm64-v8a app-debug.apk
adb shell dumpsys package com.bedrockdroid.rs | grep primaryCpuAbi

# 2. Observar arranque del runtime Tokio y panic hook desde Rust
adb logcat -c
adb logcat -s BedrockDroidRS:I AndroidRuntime:E

# 3. Probar cierre limpio de shutdownRuntime() al detener el servicio
adb logcat -d -s BedrockDroidRS:I | grep -E "cancelado ordenadamente|runtime detenido"
adb shell pidof com.bedrockdroid.rs
```

## 4. Cosas a verificar en dispositivo real antes de cerrar definitivamente la Fase 1
- [ ] Al pulsar **Iniciar**, el estado cambia a `ACTIVO` y aparece `[Rust JNI] OK bedrock=26.60 motd=1.26.60 proto=2225 engine=running` en color claro/legible (no rojo).
- [ ] Los chips de filtro (`ALL / DEBUG / INFO / WARN / ERROR`) muestran entradas de `BedrockDroidRS` capturadas desde `logcat --pid`.
- [ ] Es posible seleccionar texto libremente en el visor de logs, hacer *LongClick* / pulsar **Copiar todo** (muestra `Toast` *"Logs copiados"*) y hacer scroll vertical fluido.
- [ ] **Exportar .log** crea el archivo en `files/logs/bedrockdroid_YYYYMMDD_HHMMSS.log`, muestra el `Toast` con la ruta y abre el selector de compartir (`FileProvider`).
- [ ] Al pulsar **Detener**, el servicio libera los *WakeLocks* y `shutdownRuntime()` finaliza en `< 500 ms` sin congelar la interfaz (sin ANR).
