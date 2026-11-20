# Acciones sobre bloques — formato observado (Fase 3.3)

**Fuente:** logcat del playtest Fase 3, parche `bedrock_android_core` x86_64, cliente Bedrock 1.26.65 / proto 2225.
**Paquetes:** `InventoryTransaction (0x1e)` y `PlayerAction (0x24)`.

## Datos capturados

```
[Bedrock Action] InventoryTransaction (0x1e) recibido ... (len=51,
hex=[00 00 02 00 00 01 06 7c 01 01 00 00 00 00 00 00 00 00 00 00 c7 99 45 40 86 eb 84 42 65 06 8d bf])
-> solo registro (sin WorldUpdate)
```

- `InventoryTransaction (0x1e)`: **544 eventos** en la captura, cuerpo de **51 bytes**.
- Variante observada: los bytes `00 00 02 ...` (acción 2, probablemente *Use Item/Place*) y `00 00 01 ...` (acción 1, *Click/Mine*).
- Los floats little-endian `c7 99 45 40` ≈ `6.20`, `86 eb 84 42` ≈ `66.46` y `65 06 8d bf` ≈ `-1.10` coinciden con la posición del jugador en ese momento (`raw_y=66.46`). El paquete arrastra coordenadas del jugador, no solo el bloque.
- `PlayerAction (0x24)`: **no apareció** en esta captura; la rotura de bloques del cliente 1.26.65 llegó como `0x1e`.

## Estado del manejo

El servidor **parsea y registra** estos paquetes (ID, longitud, hex de los primeros 32 bytes) pero **no modifica el mundo** ni envía `BlockUpdate`/`LevelChunk` de vuelta. Por eso los bloques desaparecen solo en el cliente (predicción local) y vuelven a aparecer al reconectar.

## Trabajo pendiente

- Decodificar `0x1e` con el codec V2225 para obtener `action_type`, `block_pos`, `face` y `item`.
- Aplicar el cambio al estado del mundo y difundir `UpdateBlock (0x15)` / `LevelChunk`.
- Persistir el bloque modificado (Fase 4).
