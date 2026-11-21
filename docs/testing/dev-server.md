# Servidor de desarrollo (dev_server)

## Qué es

Binario Rust (`rust-core/src/bin/dev_server.rs`) que llama directamente a `start_async_engine(...)` y permite probar el motor NetherNet desde la PC, sin cargo-ndk ni Gradle.

```bash
cd rust-core
cargo run --release --bin dev_server -- <data_dir> <port>   # puerto 19140 por defecto
```

**Usar `--release` en Windows:** el build debug dispara un `debug_assert!` interno de `socket2 0.5.10` al crear el socket IPv6 dual-stack (mismo fallo del test ignorado `ipv6_dual_stack...`). En release no se activa.

## Estado verificado

- `DEV_SERVER listo: engine=running nethernet=[::]:19140 (bind_v4=0.0.0.0:19140 ipv6_v6only=true)`
- TCP y UDP `0.0.0.0:19140` en escucha.
- `Unconnected Ping` desde `192.168.3.163:19140` → `MCPE;BedrockDroid Dev;2225;1.26.60;...`
- Firewall: reglas `BedrockDroid Dev TCP/UDP 19140` y `UDP 7551`.

## Identidad persistida

`load_or_create_host_key()` guarda la `SigningKey` en PKCS#8 (`<data_dir>/host_key.pk8`, 185 bytes) y la reutiliza en arranques posteriores, evitando que el cliente vuelva a pedir confianza.

## Conexión desde el celular

`192.168.3.163:19140` (o la IP Wi-Fi correspondiente). El BDS vanilla queda en `19134` y el servidor dentro del AVD en `19132`/`19136`.
