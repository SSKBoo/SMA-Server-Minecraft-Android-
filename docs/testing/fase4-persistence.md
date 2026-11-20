# Fase 4 — Persistencia del mundo

**Fecha:** 2026-11-20 · **Captura:** `fase4c_logcat.log`
**Cliente:** Minecraft Bedrock 1.26.65 / proto 2225 · **Servidor:** AVD x86_64

## Evidencia

```
22:28:54.852 [Bedrock World] /setblock (0,63,0) -> 0 ejecutado por ... (1 overrides guardados)
22:29:42.484 [Bedrock World] Cargados 1 overrides desde .../engines/bedrock/world/overrides.txt
22:32:31.612 [Bedrock World] Reenviados 1 bloques modificados a ... antes de PlayStatus
```

## Criterio de aceptación

| Requisito | Estado |
|---|---|
| Guardar modificación | ✅ `/setblock` escribió 1 override en `overrides.txt` |
| Reiniciar servidor | ✅ nuevo proceso cargó 1 override desde disco |
| Reconexión | ✅ `Reenviados 1 bloques modificados ... antes de PlayStatus` |
| Bloque sigue roto | ✅ `UpdateBlock (0x15)` con `block_runtime_id=0` se envía tras los 25 chunks y antes de `PlayStatus` |

**Resultado: PASS.**

## Hallazgos durante la fase

- `/save` y `/setblock` enviados desde el cliente llegan como **`CommandRequest (0x4d)`**, no como `Text (0x09)`. Añadido handler específico y compartido con el handler de chat.
- En la sesión de prueba el cliente **no emitió `InventoryTransaction (0x1e)`** al romper bloques (0 eventos; `unknown_top` solo `0x21`/`0x4d`), aunque en Fase 3 sí lo hizo (544 eventos). La persistencia se validó con `/setblock`; la captura de rotura real del cliente queda como trabajo pendiente (inicializar `InventoryContent`/`InventorySlot` en el spawn).
- Autosave cada 300 s verificado (`Guardados N overrides` a las 22:12, 22:17, 22:22, 22:27).
