# Plan de trabajo para OpenCode — BedrockDroid RS

## Contexto de estado actual

- Spawn estabilizado: `PlayStatus(PlayerSpawn)` tras los 25 chunks → cliente envía `0x71` → `spawn_init=true`. Sin `peer closed`.
- `Cargo.lock` generado y versionado (346 paquetes), `cargo check --locked` pasa.
- APK x86_64 reconstruida e instalada en `emulator-5554`; servidor responde PONG (proto 2225 / 1.26.60).
- Repo Git iniciado; solo 3 archivos commiteados (`.gitignore`, `runtime.rs`, `Cargo.lock`).

---

## Fase 0 — Consolidar el repositorio (antes de tocar nada más)

**Objetivo:** tener un punto de restauración completo y reproducible.

1. Revisar `.gitignore` para excluir: `target/`, `build/`, `.gradle/`, `*.apk`, `*.so`, logs, `local.properties`, `.idea/`.
2. `git add -A` y commit: `chore: initial full project snapshot`.
3. Verificar que `git status` quede limpio y que `Cargo.lock` siga trackeado.
4. Etiquetar: `git tag v0.1.0-spawn-ok`.

**Criterio de aceptación:** repo limpio, tag creado, `cargo check --locked` sigue pasando.

---

## Fase 1 — Pruebas post-spawn (sin cambios de código)

**Objetivo:** confirmar que el jugador puede interactuar sin desconectarse.

1. En el emulador, tras spawn:
   - Mover al jugador (WASD) durante ~30 s. Verificar en logs que llegan `PlayerAuthInput (0x90)` y que el servidor los procesa.
   - Abrir chat y enviar un mensaje. Verificar recepción en el servidor (aunque no se reenvíe aún).
   - Romper y colocar un bloque. Verificar que el servidor recibe `PlayerAction` / `InventoryTransaction`.
2. Verificar que `NetworkStackLatency (0x73)` se responde (keepalives > 0 en `format_disconnect_debug_summary`).
3. Probar reconexión: cerrar el cliente, reconectar. Verificar que el servidor libera la sesión anterior y acepta la nueva.
4. Documentar en `docs/testing/post-spawn.md` los resultados (tabla: acción, paquete esperado, resultado, log).

**Criterio de aceptación:** 5 minutos de juego continuo sin `peer closed` ni `ICE disconnected`; keepalives > 0; reconexión exitosa.

---

## Fase 2 — Diagnóstico de paquetes no manejados

**Objetivo:** descubrir qué envía el cliente que el servidor ignora o rechaza.

1. Añadir en `runtime.rs` un log genérico para **cualquier** paquete entrante no clasificado: ID, longitud, peer, timestamp.
2. Añadir contador en `SessionProgress`: `unknown_pkts: u32`.
3. Exponer en `format_disconnect_debug_summary` el listado de IDs no manejados (top 5).
4. Ejecutar Fase 1 de nuevo y recolectar la lista de paquetes no manejados.
5. Priorizar los que aparezcan: `PlayerAction (0x24)`, `InventoryTransaction (0x1e)`, `Text (0x09)`, `CommandRequest (0x4d)`, `Interact (0x21)`, `MobEquipment (0x1f)`, etc.

**Criterio de aceptación:** lista documentada de paquetes entrantes no manejados, ordenada por frecuencia.

---

## Fase 3 — Mínimo viable de interacción

**Objetivo:** que el jugador pueda moverse, chatear y ver otros jugadores.

### 3.1 Movimiento (`PlayerAuthInput 0x90` → `MovePlayer 0x13` broadcast)
1. Parsear `PlayerAuthInput` con el codec V2225.
2. Actualizar posición del jugador en el estado del servidor.
3. Reenviar `MovePlayer (0x13)` a los demás clientes conectados (si hay >1).
4. Loguear cada N inputs para no saturar.

### 3.2 Chat (`Text 0x09`)
1. Recibir `Text` del cliente.
2. Reenviar `Text` a todos los clientes (incluido el emisor, según comportamiento vanilla).
3. Sanitizar: longitud máxima, sin caracteres de control.

### 3.3 Acciones sobre bloques (`PlayerAction 0x24`, `InventoryTransaction 0x1e`)
1. Parsear y loguear (aún sin modificar el mundo).
2. Documentar el formato en `docs/protocol/player-actions.md`.

**Criterio de aceptación:** dos clientes conectados se ven moverse y leer el chat del otro.

---

## Fase 4 — Persistencia y mundo

**Objetivo:** que el mundo no se regenere en cada arranque.

1. Serializar los chunks modificados a disco (`world/chunks/`).
2. Cargar chunks existentes al arrancar; generar solo los ausentes.
3. Añadir comando `/save` y autosave cada N minutos.
4. Documentar formato en `docs/world/persistence.md`.

**Criterio de aceptación:** romper un bloque, reiniciar el servidor, el bloque sigue roto.

---

## Fase 5 — Robustez y CI

**Objetivo:** que el proyecto sea reproducible y verificable.

1. Añadir a CI: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test --locked`.
2. Añadir tests unitarios para: parseo de `0x71`, parseo de `0x90`, `format_disconnect_debug_summary`.
3. Añadir job de build APK en CI con artifacts.
4. Documentar en `README.md`: cómo compilar, cómo probar en emulador, cómo conectar cliente real.

**Criterio de aceptación:** CI verde en 3 jobs (fmt/clippy/test, build Rust, build APK).

---

## Fase 6 — Opcional / futuro

- Soporte multi-jugador real (broadcast de entidades, `AddPlayer`/`RemovePlayer`).
- Inventario funcional (`InventoryContent`, `InventorySlot`).
- Comandos (`/tp`, `/gamemode`, `/time`).
- Telemetría Prometheus / endpoint `/metrics`.
- Empaquetado como servicio Android (foreground service + notificación).

---

## Orden de ejecución recomendado

```
Fase 0 → Fase 1 → Fase 2 → Fase 3.1 → Fase 3.2 → Fase 3.3 → Fase 4 → Fase 5 → Fase 6
```

Cada fase termina con: commit + tag + entrada en `CHANGELOG.md`.

---

## Instrucción lista para pegar en OpenCode

> Ejecuta el plan en `docs/plan.md` fase por fase. Tras cada fase: commit con mensaje `feat(fase-N): <resumen>`, tag `v0.x.0-faseN`, y actualiza `CHANGELOG.md`. No avances a la siguiente fase sin cumplir el criterio de aceptación. Si una fase requiere decisión de diseño, detente y pregunta. Empieza por Fase 0.
