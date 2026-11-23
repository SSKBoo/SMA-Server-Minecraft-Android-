# Changelog

Todas las fases del plan siguen `docs/plan.md`. Cada fase termina con commit + tag.

## v0.10.0-p1 — P1: inventario jugable, rotura por transacción y corrección de movimiento

- **Inventario**: `CreativeContent` ya no se envía vacío: 12 ítems del registry vanilla 1.26.65 (stone=1, dirt=3, cobblestone=4, planks=5, bedrock=7, sand=12, log=17, glass=20, torch=50, stick=323, apple=883...), con hashes de bloque derivados por `block_state_hash_nbtx`.
- **Hotbar**: `InventoryContent` envía 5 stacks reales (dirt, stone, cobblestone, planks, torch) en los slots 0–4 en vez de 36 slots vacíos.
- **Rotura de bloques**: `InventoryTransaction (0x1e)` con `UseItem/ClickBlock` o `BreakBlock` aplica `apply_block_break` (override + `UpdateBlock` difundido). El cliente 1.26.65 usaba esta ruta aunque `StartGame` anuncie rotura server-authoritative.
- **Movimiento**: cada 10 `PlayerAuthInput` se envía `CorrectPlayerMovePrediction (0xa1)` al emisor con su propia posición, delta cero y `on_ground` desde el flag `VerticalCollision (50)`; experimento controlado para la gravedad/salto.
- **Tests**: 3 nuevos (`creative_content_is_populated_with_starter_items`, `starter_inventory_has_playable_hotbar`, `correct_player_move_prediction_encodes_for_sender`); 22 tests, 0 fallos, clippy limpio.

## v0.9.0-gap-p0 — P0: Movimiento autoritativo, inventario y rotura server-auth

- **Movimiento sin eco (solución a la gravedad/salto)**: `BroadcastTarget::AllExcept(peer_addr)` en `MovePlayer (0x13)` evita sobreescribir la física local del jugador con su propio eco. Asignación de `remote_entity_id` incremental único por jugador para vista entre clientes.
- **Apertura de inventario (`Interact 0x21`)**: manejo de acción `OpenInventory (6)` respondiendo con `ContainerOpen (0x2e, window=0, type=0xff)` protegido con guardia anti-duplicados y cierre con `ContainerClose (0x2f)`.
- **Rotura server-authoritative**: parseo wire format 2225 de `PlayerAuthInput (0x90).block_actions` para acciones `PredictDestroyBlock (26)` y `CreativePlayerDestroyBlock (13)`.
- **Bloques y overrides**: difusión inmediata de `UpdateBlock (0x15)` con hash real de `minecraft:air` (`air_block_runtime_id()`) a todos los clientes y actualización del mapa de overrides.
- **Tests unitarios**: 3 nuevos tests en `runtime.rs` (`broadcast_target_filtering_works`, `interact_and_container_packets_wire_roundtrip`, `auth_input_extras_parses_block_actions_and_applies_break`) con 21 tests totales (19 ok, 2 ignorados en Windows). Verificado `cargo fmt` y `clippy -D warnings`.

## v0.8.0-dev-server — Iteración rápida en PC

- `rust-core/src/bin/dev_server.rs`: binario NetherNet para probar el motor desde la PC sin Gradle/cargo-ndk.
- `load_or_create_host_key()`: identidad PKCS#8 persistida en `<data_dir>/host_key.pk8` para evitar re-solicitudes de confianza.
- Verificado en release (debug rompe por un `debug_assert!` de `socket2` en Windows): escucha `0.0.0.0:19140`, PONG por LAN, firewall abierto. `docs/testing/dev-server.md`.

## v0.7.0-fase6 — Fase 6: opcional / futuro

- Comandos `/tp`, `/gamemode`, `/time` sobre `CommandRequest (0x4d)` (verificados).
- Endpoint Prometheus `/metrics` en el puerto 19132 (verificado).
- `InventoryContent (0x31)` con 36 slots en el spawn y parseo de `PlayerAction (0x24)` para overrides.
- Foreground service existente verificado y documentado.
- `docs/testing/fase6.md`: pendientes (menú de inventario no abre en 1.26.65, rotura real no validada de extremo a extremo).

## v0.6.0-fase5 — Fase 5: robustez y CI

- `cargo fmt` aplicado; `fmt --check` y `clippy -D warnings` limpios (9 lints corregidos).
- 3 tests nuevos: parseo de `0x71`, parseo de `0x90`, resumen `unknown_top`.
- 2 tests de red/engine marcados como ignorados solo en Windows (limitación de `socket2` y puerto 19132 ocupado por el puente del emulador).
- CI reescrito en 3 jobs: `checks`, `build-rust` (3 ABIs, incluido x86_64) y `build-apk` con artefactos.
- `README.md` con compilación, pruebas y conexión. `docs/testing/fase5-ci.md`.

## v0.5.0-fase4 — Fase 4: persistencia del mundo

- Mapa de overrides `(x,y,z) -> block_runtime_id` persistido en `files/engines/bedrock/world/overrides.txt`.
- Replay de `UpdateBlock (0x15)` tras los chunks y antes de `PlayStatus`.
- Comandos `/setblock` y `/save`, autosave cada 300 s, carga al arrancar.
- Fix de protocolo: los comandos llegan como `CommandRequest (0x4d)`, no `Text (0x09)`.
- Verificado: guardar → reiniciar (Cargados 1 overrides) → reconectar (Reenviados 1 bloques modificados). `docs/world/persistence.md` y `docs/testing/fase4-persistence.md`.

## v0.4.0-fase3 — Fase 3: mínimo viable de interacción

- Canal de broadcast (`OnceLock` + `tokio::sync::broadcast`) para re-codificar paquetes por peer sin re-bloquear el mapa de conexiones.
- Movimiento: `PlayerAuthInput (0x90)` → `MovePlayer (0x13)` publicado al canal; 1.094 inputs validados.
- Chat: parseo `Text`, saneo del mensaje y broadcast a todos (7 mensajes verificados). Corregido el falso positivo de caracteres de control del primer intento.
- Acciones: `InventoryTransaction (0x1e)` registrado con hex (544 eventos); formato en `docs/protocol/player-actions.md`.
- `docs/testing/fase3-interaction.md`: resultados y criterio adaptado (prueba con 2 clientes pendiente de un segundo cliente).

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
