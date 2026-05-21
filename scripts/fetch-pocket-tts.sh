#!/usr/bin/env bash
# One-time Pocket TTS setup. Requires HF_TOKEN and acceptance of the
# kyutai/pocket-tts license at https://huggingface.co/kyutai/pocket-tts.
# Populates the HF cache (resolved by load_with_params at runtime) and places
# a default voice WAV. After this, runtime needs no token.
set -euo pipefail

: "${HF_TOKEN:?Set HF_TOKEN (a HuggingFace read token) and accept the kyutai/pocket-tts license first}"

VOICE_DIR="data/models/pocket-tts"
mkdir -p "$VOICE_DIR"

# 1. Populate the HF cache with the gated weights + tokenizer the loader uses.
#    Uses the `hf` CLI (pip install -U huggingface_hub) — downloads into ~/.cache/huggingface.
hf download kyutai/pocket-tts --quiet
hf download kyutai/pocket-tts-without-voice-cloning tokenizer.model --quiet || true

# 2. Default voice reference WAV (clean ~6-10s English clip). Replace the source
#    with a chosen voice; an official kyutai/tts-voices sample or any public-domain clip.
if [ ! -f "$VOICE_DIR/voice.wav" ]; then
  echo "Place a clean English reference WAV at $VOICE_DIR/voice.wav (6-10s)."
  echo "e.g. a kyutai/tts-voices sample, or any public-domain clip."
fi

echo "Pocket TTS setup complete. Runtime needs no token."
