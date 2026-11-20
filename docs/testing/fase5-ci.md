# Fase 5 — Robustez y CI

## Verificación local (Windows)

| Comando | Resultado |
|---|---|
| `cargo fmt --check` | ✅ PASS |
| `cargo clippy --locked --lib -- -D warnings` | ✅ PASS (9 lint errors corregidos) |
| `cargo test --locked --lib` | ✅ 16 passed, 0 failed, 2 ignored (Windows) |

Tests nuevos:
- `decodes_set_local_player_as_initialized_0x71_runtime_entity_id`
- `parses_player_auth_input_0x90_body_and_rejects_short_payload`
- `disconnect_summary_reports_unknown_packet_top5`

Tests ignorados **solo en Windows** (se ejecutan en CI/Ubuntu):
- `ipv6_dual_stack_socket_reports_real_fd_only_v6...`: limitación de `socket2` en Windows.
- `runtime_engine_starts_and_stops_cleanly`: requiere el puerto 19132 libre (ocupado aquí por el puente del emulador).

## CI (`.github/workflows/android-rust-ci.yml`)

3 jobs:
1. `checks`: `cargo fmt --check`, `cargo clippy --locked --lib -- -D warnings`, `cargo test --locked --lib`.
2. `build-rust`: NDK 27.2.12479018, targets `arm64-v8a`, `armeabi-v7a`, `x86_64`, `RUSTFLAGS` con SSE2; sube `bedrockdroid-rs-native-libs`.
3. `build-apk`: descarga las `.so`, JDK 17 + Gradle 8.10.2, `assembleDebug`, sube `bedrockdroid-rs-apk`.

Criterio de aceptación: CI verde en 3 jobs (verificación en GitHub Actions tras el push).
