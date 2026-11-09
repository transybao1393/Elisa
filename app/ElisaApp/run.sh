#!/bin/bash
# Elisa app: the native window for the Elisa agent (brand kit "Elisa Main Window v2").
#   app/ElisaApp/run.sh [--cwd DIR] [--theme dark|light] [--base-url URL]
#   --cwd DIR   the project folder (otherwise a folder picker opens; File > Open Folder… switches later)
#   Elisa talks to the gateway in its config (default http://127.0.0.1:8898): start it first with
#               `~/Development/streamdense/engine/target/release/streamdense up` (check `df -h /` and swap before loading the 27B)
# 1) builds elisa (cargo build --release --offline) if its binary is missing or older than its sources
# 2) builds the app (swift build -c release) if the binary is missing or older than the sources
# 3) launches it. Env: STREAMDENSE_ROOT (repo root), ELISA_BIN (elisa binary), ELISA_KIT_DIR (elisa/design).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export STREAMDENSE_ROOT="${STREAMDENSE_ROOT:-$(cd "$HERE/../.." && pwd)}"
ELISA_DIR="$STREAMDENSE_ROOT/elisa"
export ELISA_BIN="${ELISA_BIN:-$ELISA_DIR/target/release/elisa}"
APP_BIN="$HERE/.build/release/ElisaApp"

if [ ! -x "$ELISA_BIN" ] || [ -n "$(find "$ELISA_DIR/src" "$ELISA_DIR/Cargo.toml" -newer "$ELISA_BIN" -print -quit 2>/dev/null)" ]; then
  echo "building elisa (cargo build --release --offline)..."
  (cd "$ELISA_DIR" && cargo build --release --offline)
fi

if [ ! -x "$APP_BIN" ] || [ -n "$(find "$HERE/Sources" "$HERE/Package.swift" -newer "$APP_BIN" -print -quit)" ]; then
  echo "building the app (swift build -c release)..."
  (cd "$HERE" && swift build -c release)
fi
exec "$APP_BIN" "$@"
