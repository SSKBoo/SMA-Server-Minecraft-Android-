#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${SCRIPT_DIR}"

if [ ! -f Cargo.lock ]; then
  echo "[BedrockDroid] rust-core/Cargo.lock no encontrado; generando automáticamente con cargo generate-lockfile..."
  cargo generate-lockfile
else
  echo "[BedrockDroid] rust-core/Cargo.lock presente; comprobando integridad --locked..."
fi

cargo metadata --locked --format-version 1 >/dev/null
echo "[BedrockDroid] Regla --locked verificada correctamente en rust-core/Cargo.lock."
