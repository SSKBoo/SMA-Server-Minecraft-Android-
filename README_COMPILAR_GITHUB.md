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
- **Página de memoria 16 KB (Android 15+)**: `-Wl,-z,max-page-size=16384` en `.cargo/config.toml`.
- **JNI + R8/ProGuard**: Reglas `-keep` explícitas para `RustCoreBridge`.
- **Android 14/15 24/7**: `foregroundServiceType="dataSync"` + `PARTIAL_WAKE_LOCK` + `WIFI_MODE_FULL_LOW_LATENCY` + `MulticastLock`.
- **Red Local + WAN**: Bypass de jitter en `127.0.0.1`/LAN (`<1 ms`), `NsdManager` mDNS, `NetworkCallback` y SSDP/UPnP + detector CGNAT.
- **Diagnóstico Fase 1.5**: `LogBuffer` circular (2000 líneas), visor seleccionable, filtros de nivel y exportación `.log` con `FileProvider`.
