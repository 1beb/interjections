#!/usr/bin/env bash
# Benchmark llama.cpp gate-model flag variants.
# Each variant: launch llama-server with a flag set, run the eval, kill it.
# Results land in eval/results.csv (one row per variant). See local-setup.md
# for what each flag does.
set -u

SERVER=/home/b/llama.cpp/build/bin/llama-server
Q4=/home/b/models/qwen3.5-4b-Q4_K_M.gguf
Q3=/home/b/models/qwen3.5-4b-Q3_K_M.gguf
PORT=8091
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

run() {
  local label="$1" model="$2"; shift 2
  echo ""; echo "### $label   flags: $*"
  "$SERVER" -m "$model" --host 127.0.0.1 --port "$PORT" --alias qwen3.5-4b \
            --no-warmup "$@" >"/tmp/llama-$PORT.log" 2>&1 &
  local pid=$!
  local ok=0
  for _ in $(seq 1 90); do
    if curl -s "http://127.0.0.1:$PORT/health" 2>/dev/null | grep -q 'ok'; then ok=1; break; fi
    if ! kill -0 "$pid" 2>/dev/null; then break; fi
    sleep 1
  done
  if [ "$ok" -ne 1 ]; then
    echo "  !! server did not become healthy; tail of log:"; tail -8 "/tmp/llama-$PORT.log"
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; return
  fi
  python3 "$ROOT/eval/bench.py" only=llamacpp runs=3 "label=$label"
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
  sleep 2
}

#    label                model  flags (see local-setup.md)
run "llamacpp-V1-baseline" "$Q4" -ngl 99 -c 2048 --reasoning-budget 0
run "llamacpp-V2-fa"       "$Q4" -ngl 99 -c 2048 --reasoning-budget 0 -fa on
run "llamacpp-V3-kvq8"     "$Q4" -ngl 99 -c 2048 --reasoning-budget 0 -fa on -ctk q8_0 -ctv q8_0
run "llamacpp-V4-tight"    "$Q4" -ngl 99 -c 1024 --reasoning-budget 0 -fa on -ctk q8_0 -ctv q8_0 -b 2048 -ub 512
run "llamacpp-V5-Q3"       "$Q3" -ngl 99 -c 1024 --reasoning-budget 0 -fa on -ctk q8_0 -ctv q8_0 -b 2048 -ub 512
echo ""; echo "DONE - see eval/results.csv"
