# Análisis de brechas: BedrockDroid RS vs BDS oficial

**Fecha:** 2026-11-22 · **Referencia:** API oficial `net-secondary.web.minecraft-services.net/api/v1.0/download/links`

| Canal | Versión | Protocolo |
|---|---|---|
| BDS estable (Windows/Linux) | **1.26.66.1** (ya lo tenemos en `C:\IA\bedrock-server-1.26.66.1`) | 2225 |
| BDS preview | 1.26.70.26 | 2259 |
| Nuestro servidor | protocolo 2225 (anuncia 1.26.60) | 2225 |

Conclusión: estamos en la **versión de protocolo correcta**. La brecha es funcional, no de versión.
1.26.70 (2259) solo afecta a la rama preview; no se persigue todavía.

## Matriz de funcionalidades

| Área | BDS 1.26.66 | Nosotros | Prioridad |
|---|---|---|---|
| Transporte NetherNet + señalización `/v1/join` | ✅ | ✅ | — |
| Login / spawn / chunks | ✅ | ✅ (chunks planos sintéticos) | — |
| Movimiento (cliente autoritativo, sin eco al emisor) | ✅ | ❌ **eco de `MovePlayer` al propio emisor con `on_ground=true`** → causa el "salto sin bajar" | **P0** |
| Abrir inventario (`Interact` OpenInventory → `ContainerOpen`) | ✅ | ❌ `Interact 0x21` ignorado → el menú no abre | **P0** |
| Rotura de bloques server-auth (`PlayerAuthInput.block_actions`) | ✅ | ❌ anunciamos `server_authoritative_block_breaking=true` pero solo escuchamos `PlayerAction 0x24` (por eso dejó de llegar) | **P0** |
| Ver a otros jugadores (`PlayerList` + `AddPlayer`, IDs únicos) | ✅ | ❌ todos usan runtime id 1 | P1 |
| Colocar bloques (`InventoryTransaction` UseItem ClickBlock) | ✅ | ❌ solo log | P1 |
| ItemStackRequest/Response (inventario server-auth, creativo) | ✅ | ❌ `CreativeContent` vacío | P1 |
| Mundo real (LevelDB, generador, chunks reales, subchunks) | ✅ | ❌ overrides en `overrides.txt` | P2 |
| Entidades/mobs, IA, daño, hambre, física servidor | ✅ | ❌ | P2 |
| Crafteo, recetas, contenedores (cofres, hornos) | ✅ | ❌ | P2 |
| Comandos vanilla, permisos, `allowlist`, `server.properties` | ✅ | parcial (`/tp /gamemode /time /setblock /save`) | P2 |
| Autenticación Xbox Live, `serveridentity` | ✅ | ❌ (`auth-xbl` feature sin usar) | P2 |
| Add-ons / behavior packs / Script API | ✅ | ❌ | P3 |

## Fuentes reutilizables (licencia y commits concretos)

| Proyecto | Licencia | Commit de referencia | Qué tomamos |
|---|---|---|---|
| `Mojang/bedrock-protocol-docs` | docs oficiales | `d011926b` (1.26.66), `b0c990c8` (1.26.65) | Esquema de paquetes 2225; solo consulta. |
| `df-mc/dragonfly` | MIT | `0f9a3a57` (2026-11-07); `server/session/handler_interact.go`, `handler_player_auth_input.go` | Lógica: `OpenInventory` → `ContainerOpen{window 0, type 0xff, entity -1}` con guardia anti-duplicado; `handleBlockActions` → `PredictDestroyBlock`. |
| `Sandertv/gophertunnel` | MIT | `d7725bfc` (2026-11-22); `packet/player_auth_input.go`, `protocol/input_flags.go`, `protocol/player.go` | Wire format 2225: `InputFlagList` (varuint count + varint ids), opcionales con prefijo `bool`, `PlayerBlockAction{varint action, BlockPos, varint face}`, enumeraciones de flags/acciones. |
| `bedrock-crustaceans/bedrock-rs` | Apache-2.0 | fijado `a730b6bb`; HEAD `3584b7e8` (+17 commits) | Ya incluye `InteractPacket` (v898), `ContainerOpenPacket` (v662), `AddPlayerPacket` (v2225). Los 17 commits nuevos son un **refactor de crates** (`35b4c506` "one crate per protocol version"), sin paquetes nuevos: **no actualizar ahora**. |
| `CloudburstMC/Protocol` | Apache-2.0 | `17a89ac1` (v2225 diagnostics) | Contraste de codec 2225 para P1/P2. |
| `pmmp/BedrockData` | CC0-1.0 | `bdb44a48` (1.26.30) | Datos de recetas/ítems creativos para P1 (requiere ajustar a 1.26.6x). |
| `GeyserMC/Geyser` | MIT | `cb04ea8a` (colisiones 26.60+), `2a2f5afd` (26.66) | Referencia para colisiones y mapeos en P2. |
| `df-mc/goleveldb` | BSD-2 | — | Referencia de formato LevelDB para P2. |

**Hugging Face:** no hay repositorios con código de servidor ni capturas de protocolo 2225 aprovechables
(solo datasets de bloques/imágenes y trayectorias de agentes). No se usa.

> Regla aplicada: se reimplementa en Rust a partir de la especificación y de la lógica observada en proyectos
> con licencia permisiva (MIT/Apache/CC0); no se copia código ni datos propietarios de Mojang.

## Prototipo de esta iteración (P0)

1. **Movimiento:** broadcast con origen; el emisor no recibe su propio `MovePlayer`; los demás lo reciben con un runtime id único por peer.
2. **Inventario:** manejar `Interact (0x21)` acción `OpenInventory (6)` → `ContainerOpen (0x2e)`; `ContainerClose (0x2f)` → eco de cierre y reset de la guardia.
3. **Rotura de bloques:** parser propio de `PlayerAuthInput` 2225 hasta `block_actions`; `PredictDestroyBlock (26)`/`CreativePlayerDestroyBlock (13)` → override a aire + `UpdateBlock` broadcast.
4. **Pruebas unitarias** para los tres flujos (fixtures sintéticas con el wire format de gophertunnel).

P1/P2 quedan como siguiente iteración.
