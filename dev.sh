#!/bin/bash
# Development mode: start the app once, then rebuild on every change.
# - core (elisa/src, prompts, evals): debug build, and the open window reloads the core on the same session (DevReload.swift)
# - app (app/ElisaApp/Sources): rebuild and relaunch the window (folder and session come back with --continue on open)
# Usage: ./dev.sh [--frame x,y,w,h]   (Ctrl-C stops the watcher and the app it started)
set -u
ROOT="$(cd "$(dirname "$0")" && pwd)"
CORE="$ROOT/elisa"; APP="$ROOT/app/ElisaApp"
FRAME="320,130,1280,820"; [ "${1:-}" = "--frame" ] && FRAME="${2:-$FRAME}"
export ELISA_DEV=1 ELISA_BIN="$CORE/target/debug/elisa"

stamp() { find "$@" -type f \( -name '*.rs' -o -name '*.md' -o -name '*.toml' -o -name '*.swift' -o -name '*.plist' \) -exec stat -f '%m' {} + 2>/dev/null | sort -n | tail -1; }

build_core() { echo "[dev] core: building…"; t=$(date +%s); if (cd "$CORE" && cargo build -q 2>&1 | grep -E '^(error|warning: unused)' -A5); then echo "[dev] core: build FAILED (window keeps the last good core)"; else echo "[dev] core: ready in $(( $(date +%s) - t )) s"; fi; }
build_app()  { echo "[dev] app: building…"; t=$(date +%s); (cd "$APP" && swift build -c release 2>&1 | grep -E 'error:' -A3) && { echo "[dev] app: build FAILED"; return 1; }; echo "[dev] app: ready in $(( $(date +%s) - t )) s"; }
APP_PID=""
# a relaunch (an app change) continues the project's last session, so the open conversation is never lost
launch_app() { local cont=(--continue); [ -n "$APP_PID" ] && kill "$APP_PID" 2>/dev/null; sleep 0.5
  "$APP/.build/release/ElisaApp" --frame "$FRAME" "${cont[@]}" >/dev/null 2>&1 & APP_PID=$!; echo "[dev] app running (pid $APP_PID), continuing the last session"; }
trap '[ -n "$APP_PID" ] && kill "$APP_PID" 2>/dev/null; echo; echo "[dev] stopped"; exit 0' INT TERM

build_core; build_app && launch_app
C=$(stamp "$CORE/src" "$CORE/prompts" "$CORE/evals" "$CORE/mem/src"); A=$(stamp "$APP/Sources" "$APP/Info.plist")
echo "[dev] watching for changes (Ctrl-C to stop)"
while sleep 1; do
  c=$(stamp "$CORE/src" "$CORE/prompts" "$CORE/evals" "$CORE/mem/src"); a=$(stamp "$APP/Sources" "$APP/Info.plist")
  if [ "$c" != "$C" ]; then C=$c; sleep 1; build_core; fi
  # app changes reopen the window, so wait until edits have stopped for 20 s (agents save files in bursts)
  if [ "$a" != "$A" ]; then A=$a; q=0; while [ $q -lt 20 ]; do sleep 1; n=$(stamp "$APP/Sources" "$APP/Info.plist"); if [ "$n" != "$A" ]; then A=$n; q=0; else q=$((q+1)); fi; done
    build_app && launch_app; fi
  kill -0 "$APP_PID" 2>/dev/null || { echo "[dev] app closed"; exit 0; }
done
