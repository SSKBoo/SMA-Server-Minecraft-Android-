# Fase 6 — Opcional / futuro

**Fecha:** 2026-11-20 · **Capturas:** `fase6_test_logcat.log`, `fase6b_logcat.log`

## Items implementados

| Item | Estado | Evidencia |
|---|---|---|
| Comandos `/tp`, `/gamemode`, `/time` | ✅ | `[Bedrock Cmd] /gamemode Creative` · `/time 1000` · `/tp (0,70,0)` ejecutados desde `CommandRequest (0x4d)` |
| Endpoint `/metrics` | ✅ | `GET 127.0.0.1:19132/metrics` devuelve `bedrockdroid_connected_peers`, `players_spawned`, `world_overrides`, `uptime_seconds`, `outbound_packets`, `outbound_bytes` |
| Verificación foreground service | ✅ | `ServerForegroundService.kt`: notificación con canal, `START_STICKY`, `FOREGROUND_SERVICE_TYPE_DATA_SYNC`, `PARTIAL_WAKE_LOCK`, WifiLock de baja latencia y MulticastLock; logs `[Locks 24/7]` |
| Inventario funcional | ⚠️ Parcial | `InventoryContent (0x31)` con 36 slots se envía tras el spawn (`[Bedrock World] InventoryContent ... enviado`), pero **el cliente no abre la ventana de inventario**. Se probó `ContainerSlotType::HotbarAndInventory` y luego `ContainerSlotType::Inventory`; sigue sin abrir. Requiere más investigación del wire format 1.26.65 (posiblemente `InventorySlot`/`ContainerOpen` adicionales). |
| Parseo de `PlayerAction (0x24)` para overrides | ⚠️ No validado | Implementado y desplegado; en la sesión anterior al fix se capturaron 28 `PlayerAction` (`hex=[01 1a ...]`), pero no llegaron nuevos tras el despliegue, así que no se pudo comprobar el `Override por PlayerAction`. La persistencia por `/setblock` sí quedó validada en Fase 4. |

## Verificación de calidad

- `cargo fmt --check`: PASS
- `cargo clippy --locked --lib -- -D warnings`: PASS
- `cargo test --locked --lib`: 16 passed, 0 failed, 2 ignored (Windows)

## Pendientes conocidos

- Menú de inventario no abre (posible `InventorySlot`/`ContainerOpen` o contenedor distinto en 1.26.65).
- Gravedad: el jugador salta pero no baja (observado en Fase 1); el servidor hace eco de `MovePlayer` sin simular físicas.
- Persistencia por rotura real del cliente no validada de extremo a extremo.
