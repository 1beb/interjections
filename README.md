# interjections

Voice layer for OpenCode web — speak naturally into your mic, ASR transcribes locally, text is injected into OpenCode's prompt, and the assistant's response is read aloud via TTS.

Interjections runs as a **TLS reverse proxy** in front of `opencode web`, injecting a voice widget into the HTML page. It handles the full voice pipeline: noise suppression, VAD, local ASR (Sherpa-onnx), lexical cue detection, context reconciliation, and streaming TTS (Cartesia).

## Architecture

```
Browser ──► https://<host>:8765
                 │ (TLS reverse proxy + widget injection)
                 ▼
           http://127.0.0.1:4096 (OpenCode server)

Voice widget (injected into OpenCode HTML):
  Mic ──► WebSocket ──► nnnoiseless ──► Energy VAD ──► Sherpa-onnx ASR
                                                            │
                                                   Controller (state machine)
                                                   Idle ─► User ─► Thinking
                                                            │
                                                   Context Reconciler
                                                            │
                                                   WebSocket broadcast
                                                   ├─ "submit" → DOM injection → OpenCode prompt
                                                   └─ "partial" → widget transcript display
                                                        
OpenCode SSE /global/event ──► response_text ──► Cartesia Sonic-3 TTS ──► audio to widget
```

## Requirements

- Rust 1.75+
- Linux with ALSA dev libraries (`libasound2-dev` on Debian/Ubuntu)
- [Sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) ASR model (downloaded separately)
- API key: [Cartesia](https://cartesia.ai) — Sonic-3 TTS
- OpenCode server running (`opencode web`)

## Quick Start

```bash
# Download ASR model
# sherpa-onnx-streaming-zipformer-en-2023-06-26.tar.bz2 → data/models/sherpa-zipformer-en/

# Build
cargo build --release

# Run with terminal mic (no web, no TTS)
cargo run -- --debug

# Run with web interface (browser mic + OpenCode proxy)
opencode web --port 4096 &
export CARTESIA_API_KEY="sk_car_..."
export OPENCODE_USERNAME="opencode"
export OPENCODE_PASSWORD="..."
cargo run -- --web --debug

# Open https://<host>:8765 (voice widget auto-injected into OpenCode page)
```

## CLI Options

```
--web                   Start web interface (reverse proxy + voice widget)
--port <PORT>           Web server port (default: 8765)
--host <HOST>           Web server host (default: 0.0.0.0)
--debug                 Enable debug logging
--cartesia-key <KEY>    Cartesia API key (or CARTESIA_API_KEY env)
--sherpa-model-dir <DIR> Sherpa-onnx model directory (default: data/models/sherpa-zipformer-en)
--opencode-url <URL>    OpenCode server URL (default: http://127.0.0.1:4096)
```

## Environment

```
CARTESIA_API_KEY=sk_car_...     (required for TTS)
OPENCODE_USERNAME=opencode      (Basic auth for OpenCode server)
OPENCODE_PASSWORD=...           (Basic auth for OpenCode server)
TLS_CERT_PATH=certs/cert.pem    (TLS certificate, default: certs/cert.pem)
TLS_KEY_PATH=certs/key.pem      (TLS key, default: certs/key.pem)
```

## How It Works

1. **Reverse proxy** — all HTTP requests to `:8765` are forwarded to the OpenCode server at `:4096`. HTML responses are intercepted and a voice widget `<div>` + `<script>` is injected before `</body>`.

2. **Voice widget** — a floating mic button at the bottom-right. Clicking opens a WebSocket to the interjections server. Mic audio is captured via `ScriptProcessor`, downsampled to 16kHz PCM, and sent over the WebSocket.

3. **ASR pipeline** — server receives PCM chunks → nnnoiseless noise suppression → energy VAD → Sherpa-onnx streaming ASR. Partial results are broadcast back to the widget for real-time transcript display.

4. **State machine** — `Idle` → `User` (VAD detects speech) → `Thinking` (ASR endpoint detected). Context reconciler collapses turns, handling interjection/correction cues ("wait", "actually", etc.).

5. **DOM injection** — when the reconciler produces final text, a `"submit"` message is sent to the widget. The widget's `ijInjectText()` uses `document.execCommand('insertText')` + submit button click to push text into OpenCode's SolidJS prompt input.

6. **SSE listener + TTS** — after submitting, the server connects to OpenCode's `GET /global/event` SSE endpoint, listens for `message.part.delta` events for the session, accumulates the response text, and on `message.updated` with `finish: "stop"`, runs Cartesia TTS and streams audio chunks back to the widget.

## Project Structure

```
src/
├── main.rs        # Entry point, audio capture thread, ASR task loop
├── config.rs      # CLI args, env vars, defaults
├── audio.rs       # Mic capture (cpal), noise suppression (nnnoiseless), output, resampling
├── vad.rs         # Energy-based VAD (threshold + min speech/silence frames)
├── local_asr.rs   # Sherpa-onnx streaming ASR
├── tts.rs         # Cartesia Sonic-3 WebSocket TTS client
├── cues.rs        # Lexical cue detection (interjection, correction, backchannel)
├── reconciler.rs  # Turn buffer with status tracking, correction collapsing
├── controller.rs  # State machine (Idle → User → Thinking), broadcast channels
└── web.rs         # TLS reverse proxy, WebSocket handler, SSE listener + TTS relay, voice widget
```

## Docs

- `docs/plan.md` — full architecture plan, build-up phases, latency budget
- `docs/handoff.md` — detailed handoff notes, blocker analysis, OpenCode frontend details
