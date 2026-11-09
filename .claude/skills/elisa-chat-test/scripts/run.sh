#!/bin/zsh
# Elisa real-model chat test: builds the app (debug, ~5 s), then runs a scenario in a visible window on the main display
# against the user's real model, with a temp HOME that copies only the active connection's non-secret settings.
#   run.sh chat     everyday conversation: greeting, facts, recall, time, follow-ups, joke, image, file + web intent, resume
#   run.sh more     language switch, math, markdown, thanks, /pin, /notes, cache, denied write, two documents,
#                   /compact and auto-compaction, continue-last-conversation, memory, /forget, server down, news search
#   run.sh features text, images, image follow-up, two images, image only, tool read, interrupt
# PACE (seconds between steps, default 2) gives the person watching time to read each reply
# Prints the results file when done. Never touches ~/.elisa, ~/.config/elisa or the user's own Elisa window.
set -u
MODE=${1:-chat}
ROOT=/Users/johnathan/Development/Elisa
APP=$ROOT/app/ElisaApp
OUT=${TMPDIR:-/tmp}/elisa-chat-test/$MODE-$(date +%H%M%S)
H=$OUT/home
IMG="$ROOT/elisa/design/Elisa Brand Kit/05-character/shade/png"
mkdir -p $H/.config/elisa $OUT/proj

# the user's active connection, keys left out (a local model needs none)
P=$HOME/.config/elisa/providers.toml
ACTIVE=$(grep -m1 '^active' $P | sed 's/.*= *"\(.*\)"/\1/')
{
  echo "active = \"$ACTIVE\""; echo
  awk -v s="[providers.$ACTIVE]" '$0==s{on=1;print;next} /^\[/{on=0} on && !/key/' $P
} > $H/.config/elisa/providers.toml
# MODEL=name runs the scenario on another installed model of the same connection; MODEL=random picks one of the
# installed chat models (capability "completion", asked from Ollama) so runs don't always use the same model
if [[ ${MODEL:-} == random ]]; then
  MODEL=$(curl -s http://127.0.0.1:11434/api/tags | python3 -c 'import sys,json;[print(m["name"]) for m in json.load(sys.stdin)["models"]]' | while read n; do
    curl -s http://127.0.0.1:11434/api/show -d "{\"model\":\"$n\"}" | python3 -c 'import sys,json;sys.exit(0 if "completion" in json.load(sys.stdin).get("capabilities",[]) else 1)' && echo $n
  done | sort -R | head -1)
fi
if [[ -n ${MODEL:-} ]]; then sed -i '' "s|^model *=.*|model = \"$MODEL\"|" $H/.config/elisa/providers.toml; OUT_MODEL=" · model $MODEL"; fi

(cd $ROOT/elisa && cargo build --release 2>&1 | grep -E '^error' && exit 1)
(cd $APP && swift build 2>&1 | grep -E 'error:' && exit 1)

# one test window at a time: the last run's window closes first (by its saved PID, never by name)
PIDFILE=${TMPDIR:-/tmp}/elisa-chat-test/window.pid
if [[ -f $PIDFILE ]]; then OLD=$(cat $PIDFILE); ps -p $OLD -o command= 2>/dev/null | grep -q -- '--selftest' && kill $OLD; fi

ARGS=(--cwd $OUT/proj --frame 320,130,1280,820 --selftest $OUT/out --selftest-pace ${PACE:-2} --selftest-image "$IMG/elisa-shade-celebrating-512.png")
case $MODE in
  chat) ARGS+=(--selftest-chat) ;;
  more) ARGS+=(--selftest-more) ;;
  *)    ARGS+=(--selftest-image "$IMG/elisa-shade-thinking-512.png") ;;
esac
# ELISA_DEV=1: the window comes to the front like a normal launch, so the person can watch
HOME=$H ELISA_DEV=1 $APP/.build/debug/ElisaApp "${ARGS[@]}" > $OUT/app.log 2>&1 &
PID=$!
echo $PID > $PIDFILE
echo "test window pid $PID · output $OUT${OUT_MODEL:-}"
for i in {1..1800}; do grep -q 'passed$' $OUT/app.log 2>/dev/null && break; kill -0 $PID 2>/dev/null || break; sleep 1; done
echo "--- results"; cat $OUT/out/results.txt 2>/dev/null || { echo "no results; app log:"; tail -20 $OUT/app.log; }
echo "--- transcript: $OUT/out/transcript.txt"
echo "--- pictures:   $OUT/out/*.png"
echo "the window stays open (pid $PID) so the person can look; close it when done"
