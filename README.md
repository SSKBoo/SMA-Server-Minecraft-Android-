# BedrockDroid RS

Servidor de Minecraft Bedrock nativo para Android (Rust + Kotlin), con transporte NetherNet (protocolo 2225, cliente 1.26.60/1.26.62+).

## Estado

- Spawn estable: 25 chunks → `PlayStatus(PlayerSpawn)` → `SetLocalPlayerAsInitialized (0x71)`.
- Movimiento con broadcast (`MovePlayer 0x13`) y chat (`Text 0x09` / `CommandRequest 0x4d`).
- Persistencia de bloques por overrides (`/setblock`, `/save`, autosave).
- ABIs: `arm64-v8a`, `armeabi-v7a`, `x86_64` (este último para pruebas en emulador).

## Compilar

### Requisitos
- JDK 17, Android SDK (plataforma 35, build-tools 35.0.0), NDK 27.2.12479018
- Rust estable con targets `aarch64-linux-android`, `armv7-linux-androideabi`, `x86_64-linux-android`
- Gradle 8.10.2 (o el wrapper del repo), `cargo-ndk`

### Librería nativa
```bash
cd rust-core
cargo check --locked
cargo fmt --check
cargo clippy --locked --lib -- -D warnings
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -o ../app/src/main/jniLibs build --release
```
En Windows/x86_64 añadir `RUSTFLAGS="-C target-feature=+sse2 -C link-arg=-Wl,-z,max-page-size=16384"`.

### APK
```bash
gradle assembleDebug --no-daemon
# APK: app/build/outputs/apk/debug/app-debug.apk
```

## Probar en emulador (AVD x86_64)

El AVD usa NAT: el cliente de PC no puede entrar por UDP directamente. Se necesita un puente:

1. `adb -s emulator-5554 forward tcp:19132 tcp:19132` (señalización TCP NetherNet).
2. Puente UDP↔TCP: binario `udpbridge` dentro del AVD + relay Python en el host (ver `C:\Users\5697r\AppData\Local\Temp\opencode\` en el entorno de desarrollo).
3. Instalar la APK: `adb -s emulator-5554 install -r app/build/outputs/apk/debug/app-debug.apk`.
4. Arrancar la app y pulsar **Iniciar servidor**.

En el PC: Minecraft Bedrock → Servidores → Agregar servidor → `127.0.0.1:19132`.

## Comandos del servidor

| Comando | Efecto |
|---|---|
| `/save` | Guarda los bloques modificados en `files/engines/bedrock/world/overrides.txt`. |
| `/setblock <x> <y> <z> <id>` | Inyecta un override (0 = aire) y guarda. |

> Los comandos que empiezan con `/` llegan como `CommandRequest (0x4d)`, no como `Text (0x09)`.

## CI

GitHub Actions ejecuta 3 jobs: `checks` (fmt/clippy/test), `build-rust` (3 ABIs) y `build-apk` (artefacto `bedrockdroid-rs-apk`).

## Documentación

- `docs/plan.md`: plan de trabajo por fases.
- `docs/testing/`: resultados de cada fase.
- `docs/protocol/`: formatos de paquetes observados.
- `docs/world/persistence.md`: formato de persistencia.
