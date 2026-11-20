# Changelog

Todas las fases del plan siguen `docs/plan.md`. Cada fase termina con commit + tag.

## v0.3.0-fase2 — Fase 2: diagnóstico de paquetes no manejados

- Instrumentación en `runtime.rs`: `unknown_pkts`, mapa `unknown_pkt_ids`, log WARN por paquete no clasificado y `unknown_top` en el resumen de desconexión.
- `docs/protocol/unhandled-packets.md`: lista ordenada por frecuencia (25.608 no manejados; `Animate 0x2c` 25.332, `InventoryTransaction 0x1e` 252, `MobEquipment 0x1f` 13, `Interact 0x21` 5, `Text 0x09` 3).
- No aparecieron `PlayerAction (0x24)` ni `CommandRequest (0x4d)`.

## v0.2.0-fase1 — Fase 1: pruebas post-spawn

- `docs/testing/post-spawn.md`: tabla de acciones, paquetes esperados y evidencia de logcat.
- Verificado: 5 min+ de juego continuo (sesión de ~17 min), 0 `ICE disconnected`, 4 `peer closed` intencionales del cliente, keepalives 515/98/58, reconexiones correctas.
- Hallazgos para fases posteriores: menú de objetos sin `InventoryContent`, salto sin gravedad en `MovePlayer`, bloques sin `BlockUpdate`/persistencia.

## v0.1.0-spawn-ok — Fase 0: consolidación del repositorio

- Repositorio unificado con el historial remoto de `SSKBoo/SMA-Server-Minecraft-Android-` (commit base `b7372f6`).
- `.gitignore` ampliado: `target/`, `build/`, `.gradle/`, `*.apk`, `*.so`, logs, `local.properties`, `.idea/`.
- `Cargo.lock` (346 paquetes) trackeado y verificado con `cargo check --locked`.
- `docs/plan.md` añadido con el plan completo de trabajo.
- Estado verificado: spawn estable (`PlayStatus(PlayerSpawn)` → `0x71` → `spawn_init=true`), sin `peer closed`; APK x86_64 instalada en `emulator-5554`; servidor responde PONG (proto 2225 / 1.26.60).
