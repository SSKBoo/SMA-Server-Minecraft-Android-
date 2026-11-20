# Paquetes entrantes no manejados — Fase 2

**Fecha:** 2026-11-20 · **Captura:** `C:\Users\5697r\AppData\Local\Temp\opencode\fase2_logcat.log` (311.556 líneas)
**Fuente de verdad:** contador en memoria `SessionProgress.unknown_pkt_ids`, expuesto en `format_disconnect_debug_summary` como `unknown_top`. El conteo de líneas de logcat es un **límite inferior** (el buffer descartó líneas).

## Resultado del playtest

```
Conexión 21:43:07 → cierre 21:45:26 (peer closed, iniciado por cliente)
auth_inputs=2761 | keepalives=68 | chunks_sent=25 | unknown_pkts=25608
unknown_top=[0x2c:25332, 0x1e:252, 0x1f:13, 0x21:5, 0x09:3]

Conexión 21:45:31 → cierre 21:45:34 (peer closed)
unknown_pkts=1 | unknown_top=[0x21:1]
```

## Lista ordenada por frecuencia

| # | ID | Nombre (según `name=` del log) | Conteo (en memoria) | Muestras en logcat | Prioridad | Observación |
|---|---|---|---|---|---|---|
| 1 | `0x2c` | `Animate` | 25.332 | 23.019 | Baja | Spam de animación de brazo al caminar/romper. Ignorar está bien; broadcast opcional en Fase 3. |
| 2 | `0x1e` | `InventoryTransaction` | 252 | 208 | **Alta** | Romper/colocar bloques. Actualmente solo se registra; sin `BlockUpdate` de vuelta ni persistencia (Fase 3.3 / 4). |
| 3 | `0x1f` | `MobEquipment` | 13 | 13 | Media | Cambio de objeto en la hotbar. Relacionado con el menú de objetos que no abre. |
| 4 | `0x21` | `Interact` | 5 | 6 | Media | Interacción con entidades/bloques. |
| 5 | `0x09` | `Text` | 3 | 3 | **Alta** | Chat del jugador. Recibido pero no reenviado (Fase 3.2). |
| 6 | `0x22` | `BlockPickRequest` | (fuera del top 5) | 2 | Baja | Pick-block con botón medio / tecla. |
| 7 | `0x66` | `ServerSettingsRequest` | (fuera del top 5) | 1 | Baja | El cliente pide settings; no se responde. |

**Total no manejados (sesión principal):** 25.608, de los cuales `0x2c` representa el 98,9 %.

## Paquetes previstos en el plan que NO aparecieron

| ID | Nombre | Estado |
|---|---|---|
| `0x24` | `PlayerAction` | No apareció en esta captura; la rotura de bloques llegó vía `0x1e`. |
| `0x4d` | `CommandRequest` | No apareció (no se probaron comandos). |

## Instrumentación añadida

- `SessionProgress.unknown_pkts: u32` y `unknown_pkt_ids: HashMap<u32, u32>`.
- Log `WARN` por paquete no clasificado: `ID`, `len`, `peer`, `ts`, `name`.
- `format_disconnect_debug_summary` incluye `unknown_pkts=N` y `unknown_top=[...]` (top 5).

## Criterio de aceptación

✅ Lista documentada de paquetes entrantes no manejados, ordenada por frecuencia (tabla anterior).
