# BedrockDroid RS — Compilación directa en GitHub Actions

## Pasos para compilar tu APK sin instalar nada local
1. Descomprime este archivo `.zip` en una carpeta vacía.
2. Sube el contenido a un repositorio nuevo en GitHub:
   ```bash
   git init
   git add .
   git commit -m "BedrockDroid RS ARM64 inicial"
   git branch -M main
   git remote add origin https://github.com/TU_USUARIO/BedrockDroidRS.git
   git push -u origin main
   ```
3. En GitHub abre la pestaña **Actions** y selecciona el workflow **BedrockDroid RS Android + Rust CI**.
4. Al finalizar, descarga el artefacto **bedrockdroid-rs-apk** con los binarios nativos `libbedrock_android_core.so` ya incluidos.

## Correcciones integradas contra fallos comunes
- **Rust NDK puro**: Sin `openssl-sys` ni `zlib-ng-sys`; usa `rustls` + `miniz_oxide`.
- **Builds reproducibles (`--locked`) automáticos dentro del `.zip`**: El proyecto incluye `rust-core/ensure-cargo-lock.sh`, la regla `!rust-core/Cargo.lock` en `.gitignore`, ejecución automática en CI antes de `Swatinem/rust-cache@v2` (usando `--locked` estricto en `cargo test` y `cargo ndk`) y publicación del artefacto `bedrockdroid-rs-cargo-lock`. No requiere pasos manuales en futuras pruebas.
- **Página de memoria 16 KB (Android 15+)**: `-Wl,-z,max-page-size=16384` en `.cargo/config.toml`.
- **JNI + R8/ProGuard**: Reglas `-keep` explícitas para `RustCoreBridge`.
- **Android 14/15 24/7**: `foregroundServiceType="dataSync"` + `PARTIAL_WAKE_LOCK` + `WIFI_MODE_FULL_LOW_LATENCY` + `MulticastLock`.
- **Red Local + WAN**: Dual-stack IPv4/IPv6 (`[::]:19132`), anuncio mDNS con registro `A + AAAA`, `NetworkCallback`, SSDP/UPnP con cooldown y detector CGNAT vía `api.ipify.org`.
- **Diagnóstico Fase 1.5**: `LogBuffer` circular (2000 líneas), filtro anti-spam `INT32_MAX` (con modo `--verbose`), exportación `.log` con `FileProvider`.

## Matriz de versiones Bedrock soportadas (Protocol Match)
| Cliente Minecraft Bedrock | Protocolo | Estado en BedrockDroid RS | Notas |
|---|---|---|---|
| `1.26.60` / `26.60` | `2225` | Soportado (NetherNet WebRTC) | MOTD `1.26.60` + `GET/POST /v1/join` |
| `1.26.62` / `26.63` | `2225` | Soportado (`protocol-match`) | Aceptado aunque el semver difiera de `1.26.60` |
| `1.21.51` | `766` | Soportado | Selección desde Carriles / Ajustes |
| `1.21.44` | `748` | Compatibilidad legacy | Selección desde Carriles / Ajustes |
