# Changelog

Todas las fases del plan siguen `docs/plan.md`. Cada fase termina con commit + tag.

## v0.1.0-spawn-ok — Fase 0: consolidación del repositorio

- Repositorio unificado con el historial remoto de `SSKBoo/SMA-Server-Minecraft-Android-` (commit base `b7372f6`).
- `.gitignore` ampliado: `target/`, `build/`, `.gradle/`, `*.apk`, `*.so`, logs, `local.properties`, `.idea/`.
- `Cargo.lock` (346 paquetes) trackeado y verificado con `cargo check --locked`.
- `docs/plan.md` añadido con el plan completo de trabajo.
- Estado verificado: spawn estable (`PlayStatus(PlayerSpawn)` → `0x71` → `spawn_init=true`), sin `peer closed`; APK x86_64 instalada en `emulator-5554`; servidor responde PONG (proto 2225 / 1.26.60).
