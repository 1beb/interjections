# interjections

Stream-of-thought voice interaction with DeepSeek V4 Flash via OpenCode Go.

## Architecture

```
Mic ──► nnnoiseless ──► Silero VAD ──► Deepgram ASR (WebSocket)
              │               │               │
         noise filter    speech/noise    partial text
              │               │               │
              ▼               ▼               ▼
┌──────────────────────────────────────────────────┐
│                  CONTROLLER                       │
│  State machine:                                   │
│  IDLE ─► USER ─► THINKING ─► MODEL ─► AWAITING   │
│                                                   │
│  Features:                                        │
│  - Lexical cue detection ("wait", "actually"... ) │
│  - Context reconciliation (collapses corrections)  │
│  - Interruption handling (VAD barge-in + cues)    │
└──────────────────────────────────────────────────┘
         │                               │
    reconciled text               LLM tokens stream
         │                               │
         ▼                               ▼
  DeepSeek V4 Flash ──────────► Cartesia Sonic-3 TTS
  (OpenCode Go)                   (WebSocket)
                                          │
                                      audio out
```

## Requirements

- Rust 1.75+
- Linux with ALSA dev libraries (`libasound2-dev` on Debian/Ubuntu)
- API keys:
  - [Deepgram](https://deepgram.com) — ASR ($200 free credit)
  - [OpenCode Go](https://opencode.ai/go) — DeepSeek V4 Flash ($10/month)
  - [Cartesia](https://cartesia.ai) — Sonic-3 TTS

## Quick Start

```bash
# Build
cargo build --release

# Run with terminal mic
export DEEPGRAM_API_KEY="..."
export OPENCODE_GO_API_KEY="..."
export CARTESIA_API_KEY="..."
cargo run -- --debug

# Run with web interface (browser mic)
cargo run -- --web --debug
# Open http://127.0.0.1:8765
```

## OpenCode Web Integration

Run alongside `opencode web`:

```bash
opencode web --port 4096 &
cargo run -- --web --port 8765
```

The web UI at http://127.0.0.1:8765 provides:
- Browser mic capture via MediaRecorder + WebSocket
- Audio playback via Web Audio API
- Real-time state display and transcript log

## CLI Options

```
--web              Start web interface
--port <PORT>      Web server port (default: 8765)
--host <HOST>      Web server host (default: 127.0.0.1)
--debug            Enable debug logging
--deepgram-key     Deepgram API key (or DEEPGRAM_API_KEY env)
--opencode-key     OpenCode Go API key (or OPENCODE_GO_API_KEY env)
--cartesia-key     Cartesia API key (or CARTESIA_API_KEY env)
```

## Project Structure

```
src/
├── main.rs        # Entry point
├── config.rs      # Configuration + CLI
├── audio.rs       # Mic capture (cpal) + noise suppression (nnnoiseless)
├── vad.rs         # Energy-based VAD
├── asr.rs         # Deepgram WebSocket client
├── llm.rs         # OpenCode Go streaming HTTP client
├── tts.rs         # Cartesia Sonic-3 WebSocket client
├── cues.rs        # Lexical cue detection
├── reconciler.rs  # Context buffer + reconciliation
├── controller.rs  # State machine + orchestration
└── web.rs         # Axum WebSocket server for browser UI
```
