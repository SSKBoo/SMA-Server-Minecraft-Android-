# Pruebas post-spawn — Fase 1

**Fecha:** 2026-11-20 · **Rango de captura:** 20:44:58 → 21:35:09 (UTC local del logcat)
**Entorno:** AVD `bedrock_mcp` (x86_64, Android 17, 8 GB RAM), cliente Minecraft Bedrock Windows **1.26.65** (GDK), protocolo **2225**, puente UDP 19132 + reenvío TCP 19132 vía `adb forward`.
**Captura:** `C:\Users\5697r\AppData\Local\Temp\opencode\fase1_logcat.log` (272.505 líneas).

## Resultados

| Acción | Paquete esperado | Resultado | Evidencia (logcat) |
|---|---|---|---|
| Spawn tras 25 chunks | `PlayStatus(0x02, status=3)` → `0x71` → `spawn_init=true` | ✅ | `21:33:08.892 PlayStatus... tras 25 chunks` · `21:33:09.943 SetLocalPlayerAsInitialized (0x71) ... spawn_init=true, entity_runtime_id=Some(1)` |
| Movimiento WASD ~30 s | `PlayerAuthInput (0x90)` → `MovePlayer (0x13)` | ✅ | `#2379` inputs en la última sesión; `21:34:49 raw_y=66.46, pos=6.21,65.62→66.46,4.09 -> MovePlayerPacket (0x13) enviado` |
| Chat | `Text (0x09)` recibido | ✅ | `21:30:12.678 Paquete de Bedrock recibido (ID: 0x09 / Text) ... (44 bytes)` (4 eventos) |
| Romper/colocar bloque | `InventoryTransaction (0x1e)` / `PlayerAction (0x24)` | ⚠️ Parcial | `0x1e` recibido 2.947 veces (`21:30:26`). No aparece `0x24` en esta captura. El servidor solo registra el paquete; **no envía `BlockUpdate`/`LevelChunk` de vuelta**, por lo que el bloque desaparece solo en el cliente (predicción local). |
| Keepalives | `NetworkStackLatency (0x73)` respondido | ✅ | `0x73` recibido 678 veces; cierres con `keepalives=515`, `keepalives=98`, `keepalives=58` |
| Cerrar cliente | `peer closed` (limpio) | ✅ | 4 cierres, todos `motivo=peer closed`, `diagnóstico=cierre limpio iniciado por el cliente`. **0 `ICE disconnected`**. |
| Reconectar | Nuevo `Login (0x01)` + spawn + `0x71` | ✅ | `21:33:03.559 peer closed` → `21:33:08.251 Login validado` → `21:33:09.943 0x71`. También `21:26:05` → `21:29:45`. |

## Observaciones del jugador

1. **No abre el menú de objetos.** El servidor no envía `InventoryContent` / `InventorySlot` ni responde al intercambio de inventario con contenido vanilla. Fuera del alcance de Fase 1; se atiende en Fase 3/6.
2. **Salta pero no vuelve a bajar.** El servidor hace eco de `MovePlayer` con la `raw_y` recibida (`65.62` → `66.46`) sin aplicar gravedad, y el cliente no corrige la posición. Requiere simulación de gravedad o dejar que el cliente sea autoridad vertical (Fase 3.1).
3. **Los bloques desaparecen pero no persisten.** Confirmado: se recibe `0x1e` pero no hay `BlockUpdate` de vuelta ni persistencia (Fase 4).

## Paquetes entrantes observados (conteo)

| ID | Nombre | Conteo |
|---|---|---|
| `0x2c` | (sin clasificar) | 11.477 |
| `0x1e` | InventoryTransaction | 2.947 |
| `0x73` | NetworkStackLatency | 678 |
| `0x1f` | MobEquipment | 48 |
| `0x21` | Interact | 16 |
| `0x138` | ServerboundLoadingScreen | 8 |
| `0x09` | Text | 4 |
| `0x01` / `0x71` / `0x45` / `0x81` / `0xc1` / `0x04` / `0x08` | Login / SetLocalPlayerAsInitialized / RequestChunkRadius / otros | 4 cada uno |

## Criterio de aceptación

| Requisito | Estado |
|---|---|
| 5 min de juego continuo sin `peer closed` ni `ICE disconnected` | ✅ PASS — sesión 21:08:50→21:26:05 (~17 min, 19.491 inputs, 515 keepalives), 0 `ICE disconnected` |
| keepalives > 0 | ✅ PASS |
| Reconexión exitosa | ✅ PASS (21:29:45 y 21:33:08) |

**Resultado: PASS.** Los 4 `peer closed` son cierres intencionales del cliente para probar reconexión, no fallos del servidor. Las tres observaciones del jugador son limitaciones funcionales conocidas y quedan registradas para fases posteriores.
