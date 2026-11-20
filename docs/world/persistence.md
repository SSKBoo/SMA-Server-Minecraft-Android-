# Persistencia del mundo — Fase 4

## Diseño (opción A aprobada: mapa de overrides)

El servidor no guarda chunks completos. Los chunks se regeneran desde la plantilla vanilla al arrancar, y encima se reaplican las **modificaciones de bloques** mediante `UpdateBlock (0x15)` antes del `PlayStatus(PlayerSpawn)`.

- Estado en memoria: `HashMap<(x, y, z), block_runtime_id>` global (`WORLD_OVERRIDES`).
- `block_runtime_id = 0` significa aire (bloque roto).
- Archivo: `<engine_dir>/world/overrides.txt`, donde `<engine_dir>` es el `dir` que la app Kotlin pasa al core (`.../files/engines/bedrock`).

## Formato

Una línea por bloque modificado:

```
x,y,z,block_runtime_id
-3,63,5,0
10,64,-2,245
```

- `x,y,z`: coordenadas `i32` (zigzag en el wire, decimales en el archivo).
- `block_runtime_id`: `u32`; `0` = aire.

## Origen de los cambios

`InventoryTransaction (0x1e)` se decodifica con `acacia_proto`. De `transaction.transaction_data` se toma la variante `ItemUse`:

- `block_position` → `(x, y, z)`.
- `action_type == BreakBlock (2)` → `block_runtime_id = 0`.
- Otras acciones (por ejemplo colocar) → `block_runtime_id` del paquete.

## Comandos y guardado

> **Nota de protocolo:** los mensajes que empiezan con `/` **no** llegan como `Text (0x09)` sino como `CommandRequest (0x4d)` (confirmed: `unknown_top=[0x4d:1]`). El servidor maneja ambos.

- **`/setblock <x> <y> <z> <block_runtime_id>`**: inyecta un override y guarda inmediatamente (usable como banco de pruebas cuando el cliente no emite `InventoryTransaction`).
- **`/save`** en el chat: guarda inmediatamente el mapa y **no** se retransmite a los demás clientes.
- **Autosave**: cada 300 s (`autosave_tick` en el bucle principal).
- **Carga**: `load_world_overrides()` al iniciar el motor NetherNet.
- **Reenvío**: `send_spawn_finish_sequence()` envía un `UpdateBlock` por override tras los 25 chunks y antes de `PlayStatus`.

## Criterio de aceptación

Romper un bloque → `/save` → reiniciar la app/servidor → reconectar → el servidor registra `Reenviados N bloques modificados ... antes de PlayStatus` y el cliente ve el bloque roto.
