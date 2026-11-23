# Changelog

Todas las fases del plan siguen `docs/plan.md`. Cada fase termina con commit + tag.

## v0.16.0-p1g — Simulación vertical del servidor con correcciones

- **Movimiento**: se implementa la simulación vertical real (salto `v=0.42`, gravedad `0.08` por tick, suelo en la Y inicial) y se envía `CorrectPlayerMovePrediction (0xa1)` al jugador cuando su Y simulada difiere de la predicha (>0.03), con el `client_tick` del `PlayerAuthInput`. Esto sigue la guía oficial de rewind del cliente.
- **Helper testeable**: `step_vertical_simulation`; test `vertical_simulation_jumps_and_lands`.
- **Motivo**: tras 134 eventos `[Bedrock Jump]` la Y del cliente seguía fija en 65.62 y `on_ground=false`; aceptar ciegamente no fue suficiente en este cliente, necesita la corrección con tick.
- **Tests**: 24 tests, 0 fallos, clippy limpio.

## v0.15.0-p1f — Suelo sin propiedades y sin eco de MovePlayer

- **Suelo**: la capa superficial del chunk de spawn pasa de `minecraft:grass_block` (tiene propiedad `snowy`, su hash sin estados puede no resolverse como bloque sólido) a `minecraft:dirt` (sin propiedades). Hipótesis: el cliente no encontraba colisión y por eso nunca marcaba `VerticalCollision` ni permitía el salto.
- **Movimiento**: se retira el eco de `MovePlayer` con `on_ground=true`; la doc oficial indica que `On Ground` en paquetes client-bound es ignorado y se reinicia cada frame.
- **Verificación previa**: `StartGame` decodificado correctamente (Survival, pos 0/65.62/0, rewind 40, block-breaking server-auth, ids por hash). Los 51 eventos `[Bedrock Jump]` confirmaban que el input sí llegaba.
- **Tests**: 23 tests, 0 fallos, clippy limpio.

## v0.14.0-p1e — Referencia de suelo para el salto y quitar la antorcha

- **Salto (experimento)**: el log mostró 51 eventos `[Bedrock Jump]` con `flags=[3,6,16,60,61]` pero `on_ground=false` (flag 50 `VerticalCollision` ausente). El cliente sí envía el input, pero no se considera apoyado. Se le devuelve `MovePlayer (0x13)` con `on_ground=true` solo cuando **no** hay flags de salto y `y <= 66.0`, para darle referencia de suelo sin pisar el salto ni congelarlo en el aire.
- **Antorcha**: se quita de creative/hotbar (el hash de `minecraft:torch` sin propiedades de orientación no es un bloque válido y el cliente la hacía desaparecer). Se sustituye por `minecraft:glass` (id 20).
- **Tests**: 23 tests, 0 fallos, clippy limpio.

## v0.13.0-p1d — Revertir AvailableCommands (rompía la conexión)

- **Revert**: dejar de enviar `AvailableCommands (0x4c)`. El cliente respondía con `PacketViolationWarning (0x9c)` y `InitialConnection-90` antes del spawn. La codificación generada por `acacia-proto` no es aceptada por el cliente 1.26.65; se revisará en una iteración separada.
- Se mantienen la colocación por `held_item`, la rotura por `PredictDestroyBlock` y el log `[Bedrock Jump]`.
- **Tests**: 23 tests, 0 fallos, clippy limpio.

## v0.12.0-p1c — Colocación por ítem, comandos visibles e instrumentación de salto

- **Colocación**: `ClickBlock` usa el `held_item.network_id` del hotbar (resuelto contra `STARTER_ITEMS`) en vez del `block_runtime_id` del bloque apuntado. El log ahora muestra `held=` e `id=` por separado.
- **Comandos**: se envía `AvailableCommands (0x4c)` con `help, save, setblock, tp, time, gamemode, list` tras `SetCommandsEnabled`, para que el autocompletado del cliente no esté vacío.
- **Diagnóstico de salto**: log `[Bedrock Jump]` con flags del `PlayerAuthInput` cuando aparecen `StartJumping (31)`, `Jumping (6)` o `JumpCurrentRaw (61)`, más `on_ground` (flag 50). Instrumentación temporal para el próximo playtest.
- **Evidencia previa**: tras quitar las correcciones de movimiento, el cliente empezó a enviar `block_actions` reales (`PredictDestroyBlock=26`); la rotura server-authoritative ya funciona.
- **Tests**: `available_commands_lists_server_commands`; 23 tests, 0 fallos, clippy limpio.

## v0.11.0-p1b — Ajustes según guías oficiales de Mojang 1.26.x

- **Movimiento**: se elimina `CorrectPlayerMovePrediction`. La guía oficial permite aceptar ciegamente las posiciones predichas de `PlayerAuthInput`; nuestras correcciones Reiniciaban la simulación del cliente (salto bloqueado, `ClientMovementPredictionSync 0x142`). El cliente vuelve a predecir libremente.
- **Rotura**: solo `InventoryTransaction/UseItem/BreakBlock` destruye bloques; `ClickBlock` ya no rompe (era lo que borraba el bloque al colocar encima).
- **Colocación**: `ClickBlock` con ítem en mano y `block_runtime_id` válido coloca el bloque en la cara destino (`face_offset`) y difunde `UpdateBlock`.
- **Documentación consultada**: `mojang.github.io/bedrock-protocol-docs/guides/player-movement-overview/`, `.../guides/block-breaking-overview/`, `.../guides/configuring-anti-cheat/` y el packet 322 `ClientMovementPredictionSync`.
- **Tests**: `face_offsets_match_bedrock_block_faces`; 22 tests, 0 fallos, clippy limpio.

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
