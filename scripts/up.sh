#!/usr/bin/env bash
#
# interjections launcher + first-run setup wizard.
#
#   ./scripts/up.sh                # configure (first run) then launch
#   ./scripts/up.sh --reconfigure  # re-ask the questions, then launch
#
# Idempotent: choices and keys are stored in .env (gitignored); on later runs
# everything is already present, so it goes straight to build + launch.
set -euo pipefail

cd "$(dirname "$0")/.."   # project root
ENV_FILE=".env"
RECONFIGURE=0
[ "${1:-}" = "--reconfigure" ] && RECONFIGURE=1

# ---------------------------------------------------------------------------
# .env helpers — values are stored quoted so sourcing is safe.
# ---------------------------------------------------------------------------
touch "$ENV_FILE"
set -a; . "./$ENV_FILE"; set +a   # load existing config into the environment

env_set() {  # KEY VALUE  -> upsert into .env and export for this process
    local key="$1" val="$2"
    grep -vE "^${key}=" "$ENV_FILE" > "$ENV_FILE.tmp" 2>/dev/null || true
    mv "$ENV_FILE.tmp" "$ENV_FILE"
    printf '%s="%s"\n' "$key" "$val" >> "$ENV_FILE"
    export "${key}=${val}"
}

# Prompt for a secret only if it's not already set (env or .env).
need_secret() {  # KEY  "prompt text"
    local key="$1" prompt="$2"
    if [ -z "${!key:-}" ]; then
        read -rsp "  $prompt: " val; echo
        [ -n "$val" ] && env_set "$key" "$val"
    fi
}

ask() {  # "prompt" default  -> echoes the answer
    local prompt="$1" default="$2" ans
    read -rp "  $prompt [$default]: " ans
    echo "${ans:-$default}"
}

LAUNCH_ARGS=()

# ---------------------------------------------------------------------------
# 1. Gate engine
# ---------------------------------------------------------------------------
if [ "$RECONFIGURE" = 1 ] || { [ -z "${IJ_GATE_ENDPOINT:-}" ] && [ -z "${IJ_NO_GATE:-}" ]; }; then
    echo "Gate LLM:"
    echo "  1) Cerebras gpt-oss-120b   (default — fast, best hold accuracy, needs a key)"
    echo "  2) Local Ollama qwen3.5:4b (fully offline, no key)"
    echo "  3) None                    (submit raw transcripts, no gating)"
    case "$(ask 'Choice' 1)" in
        1) env_set IJ_GATE_ENDPOINT "https://api.cerebras.ai/v1/chat/completions"
           env_set IJ_GATE_MODEL "gpt-oss-120b"
           env_set IJ_GATE_REASONING_EFFORT "low"
           env_set IJ_NO_GATE ""
           need_secret CEREBRAS "Cerebras API key (https://cloud.cerebras.ai)" ;;
        2) env_set IJ_GATE_ENDPOINT "http://localhost:11434/v1/chat/completions"
           env_set IJ_GATE_MODEL "qwen3.5:4b"
           env_set IJ_GATE_REASONING_EFFORT "none"
           env_set IJ_NO_GATE "" ;;
        3) env_set IJ_NO_GATE 1 ;;
        *) echo "  invalid choice"; exit 1 ;;
    esac
fi
[ "${IJ_NO_GATE:-}" = 1 ] && LAUNCH_ARGS+=(--no-gate)

# ---------------------------------------------------------------------------
# 2. TTS engine
# ---------------------------------------------------------------------------
if [ "$RECONFIGURE" = 1 ] || [ -z "${IJ_TTS_ENGINE:-}" ]; then
    echo "TTS engine:"
    echo "  1) Pocket TTS (default — local, CPU, no key)"
    echo "  2) Cartesia   (cloud, needs a key)"
    case "$(ask 'Choice' 1)" in
        1) env_set IJ_TTS_ENGINE "pocket" ;;
        2) env_set IJ_TTS_ENGINE "cartesia"
           need_secret CARTESIA_API_KEY "Cartesia API key (https://cartesia.ai)" ;;
        *) echo "  invalid choice"; exit 1 ;;
    esac
fi

# ---------------------------------------------------------------------------
# 3. OpenCode credentials (always needed)
# ---------------------------------------------------------------------------
if [ -z "${OPENCODE_USERNAME:-}" ]; then env_set OPENCODE_USERNAME "$(ask 'OpenCode username' opencode)"; fi
need_secret OPENCODE_PASSWORD "OpenCode password"

echo
echo "==> Checking assets..."

# ---------------------------------------------------------------------------
# 4. One-time setup (each step skipped if already present)
# ---------------------------------------------------------------------------
# 4a. ASR model
if [ ! -d data/models/sherpa-zipformer-en ]; then
    echo "  - Downloading Sherpa ASR model..."
    mkdir -p data/models
    wget -qO- https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-streaming-zipformer-en-2023-06-26.tar.bz2 \
        | tar -xjf - -C data/models
    mv data/models/sherpa-onnx-streaming-zipformer-en-2023-06-26 data/models/sherpa-zipformer-en
fi

# 4b. TLS certs (browsers require a secure context for getUserMedia)
if [ ! -f certs/cert.pem ]; then
    echo "  - Generating self-signed TLS cert..."
    mkdir -p certs
    openssl req -x509 -newkey rsa:2048 -keyout certs/key.pem -out certs/cert.pem \
        -days 365 -nodes -subj "/CN=localhost" 2>/dev/null
fi

# 4c. Pocket TTS weights + default voice (only when Pocket is the engine)
if [ "${IJ_TTS_ENGINE:-pocket}" = "pocket" ] && [ ! -f data/models/pocket-tts/voice.wav ]; then
    echo "  - Provisioning Pocket TTS (one-time)."
    echo "    Accept the license at https://huggingface.co/kyutai/pocket-tts first."
    need_secret HF_TOKEN "HuggingFace read token (https://huggingface.co/settings/tokens)"
    HF_TOKEN="${HF_TOKEN:?HF_TOKEN required to provision Pocket TTS}" ./scripts/fetch-pocket-tts.sh
    # The fetch script populates weights; supply a default voice from the ASR
    # sample clips if one wasn't placed manually.
    if [ ! -f data/models/pocket-tts/voice.wav ]; then
        mkdir -p data/models/pocket-tts
        cp data/models/sherpa-zipformer-en/test_wavs/0.wav data/models/pocket-tts/voice.wav
        echo "    Using an ASR sample clip as the default voice (swap data/models/pocket-tts/voice.wav to change it)."
    fi
fi

# 4d. Local gate model (only when the local Ollama gate is selected)
if [ "${IJ_GATE_MODEL:-}" = "qwen3.5:4b" ]; then
    if ! curl -sf http://localhost:11434/api/tags >/dev/null 2>&1; then
        echo "  ! Ollama not reachable on :11434 — start it with 'ollama serve' (local gate selected)."
    elif ! ollama list 2>/dev/null | grep -q 'qwen3.5:4b'; then
        echo "  - Pulling qwen3.5:4b..."; ollama pull qwen3.5:4b
    fi
fi

# ---------------------------------------------------------------------------
# 5. Build (release — debug Pocket TTS inference is far too slow)
# ---------------------------------------------------------------------------
echo
echo "==> Building (release)..."
cargo build --release

# ---------------------------------------------------------------------------
# 6. Launch: opencode web (background, if not already up) + interjections
# ---------------------------------------------------------------------------
if ! curl -sf http://127.0.0.1:4096 >/dev/null 2>&1; then
    echo "==> Starting opencode web on :4096..."
    opencode web --port 4096 &
    OPENCODE_PID=$!
    trap '[ -n "${OPENCODE_PID:-}" ] && kill "$OPENCODE_PID" 2>/dev/null || true' EXIT
    for _ in $(seq 1 30); do curl -sf http://127.0.0.1:4096 >/dev/null 2>&1 && break; sleep 0.5; done
fi

echo "==> Starting interjections (open https://localhost:8765)"
./target/release/interjections --web ${LAUNCH_ARGS[@]+"${LAUNCH_ARGS[@]}"}
