# interjections

Stream-of-thought voice interaction — fully local, open source.

---

## Principle

**No cloud APIs. No external services. Everything runs locally on the primary machine.** A lightweight client (browser or native) connects over Tailscale — no need for the client to have models, GPU, or heavy compute.

---

## Deployment Architecture

```
┌──────────────────────┐          Tailscale          ┌──────────────────────┐
│    PRIMARY (this)    │     ─── 2-5ms ───           │    CLIENT (client)    │
│                      │    ◄──────────────────►     │                      │
│  Models + GPU + CPU  │    WebSocket (raw PCM)      │  Browser or native   │
│                      │                              │  (mic + speaker      │
│  ┌────────────────┐  │                              │   only, no models)   │
│  │  Server mode   │  │                              │                      │
│  │  ───────────── │  │                              │                      │
│  │  • nnnoiseless │  │                              │                      │
│  │  • Energy VAD  │  │                              │                      │
│  │  • Vosk/Sherpa │  │                              │                      │
│  │  • Local LLM   │  │                              │                      │
│  │  • Local TTS   │  │                              │                      │
│  │  • Controller  │  │                              │                      │
│  └────────────────┘  │                              │                      │
└──────────────────────┘                              └──────────────────────┘
         ▲ latency budget ▲
         │                │
    mic → server ← 5ms    server → speaker + 5ms
    (client mic)          (client speaker)
```

**Latency:** 2-5ms Tailscale = 0.2-0.5% of total budget (600-1000ms target). Negligible.

---

## Overview

A central controller orchestrates streaming ASR → LLM → TTS with a focus on:
- Natural interruption ("interjections") via lexical + VAD cues
- Stream-of-thought dictation with corrections collapsed into clean context
- Music/noise filtering via nnnoiseless + VAD
- Client-server split: server runs all models, client is just mic + speaker
- Target: ~1-2s end-to-end latency

---

## Technology Stack

| Component | Choice | Status | Why |
|---|---|---|---|
| Audio transport | Raw WebSocket (axum) | ✅ Done | Zero dependencies, full control |
| Noise suppression | nnnoiseless (RNNoise Rust port) | ✅ Done | <1ms/chunk, strips noise before VAD |
| VAD | Energy-based VAD | ⚠️ Temporary | Simple, needs upgrade to Silero VAD |
| ASR | Vosk (partial) + Sherpa-onnx (final) | ✅ Done | Both local, ONNX-based |
| Server binding | Tailscale IP or 0.0.0.0 | ✅ Done | Already configurable via `--host` |
| Client (browser) | Axum WS + browser mic/speaker | ⚠️ Partial | Opus/webm decode broken — needs fix |
| Client (native) | `--mode client` (Rust binary) | ❌ Not done | Raw PCM WS → server, avoids Opus issue |
| LLM | OpenCode Go → local LLM (llama.cpp) | ❌ Not done | Must replace cloud LLM |
| TTS | Cartesia → local TTS (Piper/Supertonic) | ❌ Not done | Must replace cloud TTS |

---

## Build-Up Plan

Ordered so each step produces something testable from client immediately, building up to the full local system.

### Phase 0: Test what we have via Tailscale

**Goal:** Get current system working from client browser, even if Opus decode is rough.

**Steps:**
1. Bind server to `0.0.0.0` so client can reach it:
   ```bash
   cargo run -- --web --host 0.0.0.0 --port 8765 --debug
   ```
2. Open `http://client-tailscale-ip:8765` in browser on client
3. Partial ASR won't work (Opus → PCM mismatch), but you'll see WebSocket connect, state changes, and audio playback if you set Cartesia/OpenCode Go keys

**Deliverable:** Basic connectivity verified, state machine visible in browser.

### Phase 1: Fix client audio pipeline

**Goal:** Client mic audio reaches server ASR correctly — two options, do one or both.

**Option A — Native client (`--mode client`):**
- New `src/client.rs` — captures mic via cpal, sends raw PCM to server via WebSocket
- Receives audio chunks back, plays via cpal speaker
- `--mode client --server <primary-tailscale-ip>:8765`
- Bypasses Opus entirely, clean audio pipeline

**Option B — Fix browser Opus decode:**
- Server-side: add opus decode via `audiopus` crate (receive webm/opus, decode to PCM)
- Or client-side: use `AudioContext` + `MediaStreamSource` + `ScriptProcessor` to send raw PCM instead of MediaRecorder

**Recommendation:** Do Option A first (fastest, cleanest), Option B as polish.

**Deliverable:** client can speak → audio streams to primary → ASR transcribes → transcripts visible on client browser UI. Full audio pipeline testable with cloud LLM/TTS keys.

### Phase 2: Replace TTS with local TTS

**Goal:** No cloud dependency for speech output.

**Options:**

| Option | Params | Speed | Quality | Integration |
|---|---|---|---|---|
| **Piper** | ~30-100M | Very fast, ONNX | Good | Existing crate (`piper-tts`) |
| **Supertonic** | ~99M | Fast, ONNX | Best | Needs chunking + WS relay layer |
| **Kokoro** | ~82M | Fast | Good | Less mature Rust bindings |

**Recommendation:** Start with **Piper** — simplest API, ONNX-based, fast on CPU, easiest to integrate as a WebSocket relay.

**Steps:**
1. Add `piper-tts` crate (or bind to piper binary via stdin/stdout)
2. Download Piper voice model (e.g., `en_US-lessac-medium`)
3. Replace `src/tts.rs`: on `speak(text)`, synthesize locally, stream raw PCM chunks to client
4. Support abort via dropping the pipe
5. Wire into controller like the current Cartesia code

**Deliverable:** client speaks → ASR → LLM (still cloud) → Piper TTS → audio plays on client.

### Phase 3: Replace LLM with local LLM

**Goal:** No cloud dependency at all.

**Options:**

| Option | Speed | Streaming | Integration |
|---|---|---|---|
| **llama.cpp** (via `llama-cpp-2` crate) | Fast, CPU+GPU | ✅ Native | Best — no daemon, streaming callback |
| **Ollama** | Fast | ✅ via HTTP API | Easier but adds a daemon process |

**Recommendation:** **llama.cpp** via `llama-cpp-2` crate. Direct Rust bindings, streaming tokens callback, full abort control.

**Steps:**
1. Add `llama-cpp-2` crate (requires building llama.cpp with CUDA if GPU available)
2. Download a model (e.g., `Llama-3.2-3B-Instruct-Q4_K_M.gguf`)
3. Replace `src/llm.rs`: instantiate model at startup, implement streaming generation with abort flag
4. Wire into controller

**Deliverable:** Fully local pipeline — client mic → primary server (ASR → LLM → TTS) → client speaker. No cloud APIs.

### Phase 4: Polish

| Priority | Task | Details |
|---|---|---|
| **P1** | Upgrade VAD to Silero VAD | More robust than energy threshold |
| **P1** | Web opus fix (Option B) | So browser UI works without native client |
| **P2** | Streaming TTS | Send LLM tokens incrementally to TTS, not as full text |
| **P2** | Music rejection tuning | Spectral flatness guard if noise bleeds through |
| **P3** | Config file (TOML) | Replace env vars + defaults |
| **P3** | Model download scripts | Automate fetching all models |

---

## Pipeline Flow

```
┌─ CLIENT (client) ──────────────────────────────┐
│  Mic ──► cpal capture ──► WebSocket send ──────┼──┐
│                                                  │  │
│  Speaker ◄── cpal playback ◄── WebSocket recv ◄─┼──┘
└──────────────────────────────────────────────────┘
         │ ▲                          │ ▲
    raw PCM │                     raw PCM
         ▼ │                          ▼ │
┌─ SERVER (primary) ─────────────────────────────┐
│  ┌──────────────────────────────────────────┐   │
│  │           WebSocket Handler               │   │
│  │  rx: client PCM ──► nnnoiseless ──► VAD   │   │
│  │  tx: TTS audio ◄── Controller ◄── ASR     │   │
│  └──────────────────────────────────────────┘   │
│         │                                       │
│    ┌────┴────────┐         ┌───────────────┐    │
│    │  Vosk/Sherpa │         │ Local LLM     │    │
│    │  (ASR)       │         │ (llama.cpp)   │    │
│    └─────────────┘         └───────┬───────┘    │
│                                    │             │
│                             ┌──────▼───────┐    │
│                             │  Local TTS   │    │
│                             │  (Piper)     │    │
│                             └──────────────┘    │
└──────────────────────────────────────────────────┘
```

---

## Client/Server Protocol

Same WebSocket protocol already implemented for the browser UI, extended for bidirectional PCM:

**Client → Server:**
- `Message::Binary(samples)` — raw PCM i16 mono, 16kHz (not Opus/webm)

**Server → Client:**
- `Message::Binary(samples)` — TTS audio PCM i16 mono
- `Message::Text(json)` — state updates, transcripts, model text

**Session lifecycle:**
1. Client connects WebSocket
2. Client starts sending PCM chunks (every 30ms)
3. Server processes ASR, detects VAD, runs state machine
4. When Model state: server streams TTS audio chunks back
5. Client plays audio, updates UI state from JSON messages

---

## State Machine

```
                    ┌─────────┐
         ┌────────►│  IDLE   │◄────────┐
         │         └────┬─────┘         │
         │              │               │
    VAD=silent     VAD=speech      VAD=silent
    after model        │           (no more
    response           │           user input)
         │              ▼               │
         │         ┌─────────┐         │
         └─────────│  USER   │─────────┘
         │         └────┬─────┘
         │              │
         │         ASR turn detected
         │         OR silence > threshold
         │              │
         │              ▼
         │         ┌─────────┐
         │         │ THINKING│
         │         └────┬─────┘
         │              │
         │         LLM first token
         │              │
         │              ▼
         │         ┌─────────┐
         │         │  MODEL  │◄──────────┐
         │         └────┬─────┘          │
         │              │           VAD=speech
         │         TTS complete      (interjection)
         │              │               │
         │              ▼               │
         │         ┌──────────────┐     │
         │         │ AWAITING_    │─────┘
         │         │ CONFIRMATION │
         │         └──────────────┘
         │              │
         └──────────────┘
           explicit cue
           ("wait", "actually")
```

---

## Context Reconciler

Takes the timestamped context buffer and collapses it into a clean LLM prompt.

```rust
struct Turn {
    id: String,
    turn_type: TurnType,
    text: String,
    start_time: f64,
    end_time: f64,
    status: TurnStatus,
    corrected_by: Vec<String>,
    signal: Option<String>,
}
```

**Algorithm:**
1. Filter out backchannel turns ("uh-huh", "mhm", "yeah", "right")
2. Mark superseded: any user content AFTER a "correction" signal replaces content BEFORE the signal
3. Merge active turns into a single coherent prompt
4. Strip interjection signals from final output

**Example:**
- Input: `"write an email... actually no... make it a memo... cc Sarah"`
- Reconciled: `"Write a memo cc Sarah"`

---

## Current Implementation Status

### ✅ Completed

| Component | File | Notes |
|---|---|---|
| Audio capture (cpal) | `src/audio.rs` | Mic input, resampling, nnnoiseless suppression |
| Energy VAD | `src/vad.rs` | Threshold-based with min speech/silence frames |
| Local ASR (dual engine) | `src/local_asr.rs` | Vosk + Sherpa-onnx |
| Cue detector | `src/cues.rs` | Lexical matching for interjection/correction etc. |
| Context reconciler | `src/reconciler.rs` | Turn buffer, status tracking, reconciliation |
| Controller state machine | `src/controller.rs` | Full state machine, interrupt, LLM→TTS pipeline |
| LLM client (OpenAI-compatible) | `src/llm.rs` | Streaming SSE parser, abort support |
| TTS client (Cartesia WS) | `src/tts.rs` | WebSocket streaming, abort support |
| Web interface (server) | `src/web.rs` + `web/index.html` | Axum WS, browser UI, state display |
| Configuration | `src/config.rs` | CLI args, env vars, defaults |
| Server entry point | `src/main.rs` | Async runtime, audio thread, ASR task, controller |

### ❌ Remaining Work

| Phase | Task | Details |
|---|---|---|
| **0** | Bind to 0.0.0.0 for Tailscale | Already supported via `--host 0.0.0.0`, just document it |
| **1** | `--mode client` binary | New `src/client.rs` — cpal mic + speaker ↔ server WebSocket |
| **2** | Piper TTS integration | Replace Cartesia with local Piper ONNX synthesis |
| **3** | llama.cpp LLM integration | Replace OpenCode Go with local llama.cpp inference |
| **4** | Silero VAD upgrade | Via sherpa-onnx (already a dep) |
| **4** | Browser Opus decode fix | Server-side opus decoding for browser clients |
| **4** | Streaming TTS | Incremental LLM→TTS, not full-text then synthesize |
| **4** | Config file | TOML support for all settings |
| **4** | Model download automation | Script to fetch all models |

---

## Key Latency Budget

| Step | Current | Target | Notes |
|---|---|---|---|
| Mic capture + resample | ~20ms | ~20ms | On client (client) |
| Tailscale network | — | ~5ms | bidirectional, negligible |
| nnnoiseless | <1ms | <1ms | On primary |
| VAD | <1ms | <5ms | Energy → Silero |
| ASR (Sherpa partial) | ~100ms | ~100ms | On primary |
| Local LLM first token | TBD | ~200-500ms | llama.cpp |
| Local TTS first chunk | TBD | ~100-300ms | Piper |
| **Total** | N/A | **~600-1000ms** | |

---

## Project Structure

```
interjections/
├── PLAN.md                  # This file
├── Cargo.toml
├── README.md
├── data/
│   └── models/
│       ├── vosk-small-en-us/
│       └── sherpa-zipformer-en/
├── src/
│   ├── main.rs              # Server entry point (model host)
│   ├── client.rs            # Native client entry point (mic/speaker only)
│   ├── config.rs            # Configuration + CLI
│   ├── audio.rs             # Mic capture, noise suppression, output
│   ├── vad.rs               # Energy VAD → Silero VAD
│   ├── local_asr.rs         # Vosk + Sherpa-onnx
│   ├── llm.rs               # OpenCode Go → llama.cpp
│   ├── tts.rs               # Cartesia → Piper
│   ├── cues.rs              # Lexical cue detection
│   ├── reconciler.rs        # Context buffer + reconciliation
│   ├── controller.rs        # State machine + orchestration
│   └── web.rs               # Axum WebSocket server
├── web/
│   └── index.html           # Browser UI
└── tests/
```

---

## Quick Start (Tailscale Setup)

```bash
# On primary (this machine) — run all models:
cargo run -- --web --host 0.0.0.0 --port 8765 --debug

# On client (client) — native client:
cargo run -- --mode client --server <primary-tailscale-ip>:8765

# Or on client — browser:
# Open http://<primary-tailscale-ip>:8765 in any browser
```
