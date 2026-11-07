#!/bin/bash
# Elisa companion: Shade in a small floating window, following Elisa's live state (no model, no network: a Unix socket).
#   app/ElisaCompanion/run.sh [--size small|medium|large] [--no-top] [--socket PATH] [--shots DIR] [--background]
# 1) builds the app (swift build -c release) if the binary is missing or older than the sources
# 2) launches it; it listens on $ELISA_COMPANION_SOCK or ~/.elisa/companion.sock
# Then run Elisa with --companion (or config `companion = true`): the terminal dock hides while the window is up.
# --background: start it detached (log in ~/.elisa/companion.log) and return; does nothing if one is already running.
# Quit: the paw icon in the menu bar > Quit, or Ctrl+C here.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
export STREAMDENSE_ROOT="${STREAMDENSE_ROOT:-$(cd "$HERE/../.." && pwd)}"
APP_BIN="$HERE/.build/release/ElisaCompanion"

BG=0
ARGS=()
for a in "$@"; do
  if [ "$a" = "--background" ]; then BG=1; else ARGS+=("$a"); fi
done

if [ ! -x "$APP_BIN" ] || [ -n "$(find "$HERE/Sources" "$HERE/Package.swift" -newer "$APP_BIN" -print -quit)" ]; then
  echo "building the companion (swift build -c release)..."
  (cd "$HERE" && swift build -c release)
fi

if [ "$BG" = 1 ]; then
  if pgrep -x ElisaCompanion >/dev/null; then echo "companion already running"; exit 0; fi
  mkdir -p "$HOME/.elisa"
  nohup "$APP_BIN" ${ARGS[@]+"${ARGS[@]}"} >"$HOME/.elisa/companion.log" 2>&1 &
  echo "companion started (pid $!, log ~/.elisa/companion.log)"
  exit 0
fi
exec "$APP_BIN" ${ARGS[@]+"${ARGS[@]}"}
