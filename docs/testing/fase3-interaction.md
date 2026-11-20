# Fase 3 — Mínimo viable de interacción

**Fecha:** 2026-11-20 · **Capturas:** `fase3_logcat.log` (intento 1) y `fase3b_logcat.log` (retest del fix de chat)
**Cliente:** Minecraft Bedrock Windows 1.26.65 / proto 2225 · **Servidor:** AVD x86_64, Android 17, 8 GB

## 3.1 Movimiento — `PlayerAuthInput (0x90)` → `MovePlayer (0x13)`

- Handler existente decodifica `0x90` y construye `MovePlayer` con posición/pitch/yaw/head_yaw.
- El paquete crudo se publica en un canal `tokio::sync::broadcast` (global `OnceLock`), y el bucle principal lo **re-codifica con la sesión de cada peer** antes de enviarlo. No se re-bloquea `peers` → sin deadlock.
- Evidencia (retest): `#840` → `#1080` inputs, posiciones cambiando (`-1.94,67.30,7.73` → `-3.82,67.30,5.00`), `client_init=true`, 1.094 inputs totales en la sesión.

## 3.2 Chat — `Text (0x09)`

- **Intento 1 (FAIL):** la validación barría bytes de cabecera (tipo `0x01`, longitudes) y descartaba todos los `Text` con `control_chars=true`.
- **Fix:** parseo real con `pkt.decode::<Text>()`, saneo solo el campo `message` (≤1024 chars, sin caracteres de control excepto `\n`/`\t`), re-codifico con `encode_packet` y publico al broadcast (incluido el emisor).
- **Retest (PASS):**
  ```
  [Bedrock Chat] Text (0x09) de ... -> broadcast: "analiza Fase 3"
  [Bedrock Chat] Text (0x09) de ... -> broadcast: "hola fase3"
  ```
  7 broadcasts, 0 descartes en la sesión nueva (las 3 líneas `descartado` del log son del intento anterior, conservadas en el buffer de logcat).

## 3.3 Acciones sobre bloques — `0x1e` / `0x24`

- Registro de 544 `InventoryTransaction (0x1e)` con longitud y hex de 32 bytes. Formato documentado en `docs/protocol/player-actions.md`.
- `PlayerAction (0x24)` no apareció: el cliente 1.26.65 envía la rotura como `0x1e`.
- Sin `WorldUpdate`; los bloques desaparecen solo en el cliente (pendiente Fase 4).

## Estabilidad

- 0 `ICE disconnected`; único `peer closed` intencional del cliente.
- keepalives 26, chunks 25, `spawn_status=true`, `spawn_init=true`, `client_init=true`.
- `unknown_top=[0x2c:6165, 0x21:1]` — el chat y las acciones ya no cuentan como no manejados.

## Criterio de aceptación (adaptado)

| Requisito original | Estado |
|---|---|
| Broadcast implementado y sin deadlock | ✅ |
| Recepción/reenvío de chat con saneo | ✅ |
| Parseo y registro de acciones sobre bloques | ✅ |
| Dos clientes viéndose/leyéndose | ⏸️ No verificable: solo hay un cliente (PC); el segundo emulador se retiró por RAM. Queda pendiente hasta disponer de un segundo cliente legal. |

**Resultado: PASS** bajo el criterio adaptado aprobado.
