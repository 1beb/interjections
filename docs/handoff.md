# Interjections Handoff

**Voice layer over OpenCode web** — speak naturally, text appears in the chat, response read aloud via TTS.

## Architecture

Interjections is a **reverse proxy + voice widget** running on the primary machine at port 8765 over HTTPS. It proxies all traffic to OpenCode server at `http://127.0.0.1:4096`, injecting a voice widget into the HTML page.

```
Browser → https://<host>:8765
             ↓ (reverse proxy + widget injection)
         http://127.0.0.1:4096 (OpenCode server)
```

The widget floats at the bottom-right of the OpenCode page: a mic button (🎤), state label, and transcript display.

## Current State

- **ASR**: Working (Sherpa-onnx transcribes mic audio)
- **DOM injection**: `ijInjectText()` sets the input value natively but **cannot trigger SolidJS reactivity** — the text appears in the DOM but OpenCode's `handleSubmit` never fires
- **TTS**: Partially wired — SSE listener connects to `/global/event` and waits for `message.updated` with `finish: stop`, but since DOM injection doesn't trigger submission, no response is generated for TTS to read
- **Voice widget**: Connects, mic works, ASR transcribes, server broadcasts `"submit"` type to widget, widget runs `ijInjectText()` — but the message never reaches OpenCode's JS state

## Approaches Tried

### 1. Direct OpenCode API (removed)

- `OpenCodeLlm` struct that called `POST /session` + `POST /session/:id/prompt_async` + `GET /global/event` SSE
- Problems: SSE event matching unreliable (messageID vs sessionID matching), broadcast channel timing issues, authentication complexity, session management
- Removed entirely. Code lives in git history.

### 2. DOM Injection (current, working except for SolidJS trigger)

- Server does ASR on mic audio from widget WebSocket
- Transcribed text broadcast to widget as `{"type": "submit", "text": "..."}`
- Widget JS injects text into OpenCode's input box and tries to trigger submit
- ASR pipeline works, broadcast works, `ijInjectText()` sets the DOM value — but SolidJS doesn't react to the programmatic input event, so `handleSubmit` never fires

### 3. SSE Listener for TTS (partial)

- After submit broadcast, server connects to `GET /global/event` SSE to listen for response
- Accumulates `message.part.delta` events, waits for `message.updated` with `finish: "stop"`
- Runs Cartesia TTS on response text, sends audio to widget via broadcast channel
- SSE connection succeeds (status=200) but `message.updated` with `finish` never arrives — likely because DOM injection never actually submits, so no response is generated

## Key Files

| File | Purpose |
|---|---|
| `src/main.rs` | Server entry, audio capture, ASR task, VAD loop |
| `src/controller.rs` | State machine (Idle → User → Thinking), reconciler, broadcasts `"submit"` |
| `src/web.rs` | Reverse proxy, voice widget HTML/JS injection, WebSocket handler, SSE listener + TTS trigger |
| `src/tts.rs` | Cartesia WebSocket TTS client |
| `src/config.rs` | CLI args, env vars |
| `src/audio.rs` | Mic capture, noise suppression (nnnoiseless), resampling |
| `src/vad.rs` | Energy-based VAD |
| `src/local_asr.rs` | Sherpa-onnx streaming ASR |
| `src/cues.rs` | Lexical cue detection (interjection, correction signals) |
| `src/reconciler.rs` | Turn buffer, context reconciliation |
| `web/index.html` | (unused) Old separate UI page |

## Current Blocker

> **STALE (2026-05-16).** This blocker is resolved and this section is kept
> only for history. `ijInjectText()` now uses `document.execCommand('insertText')`
> (which fires the real `beforeinput`/`input` events SolidJS reacts to) and
> clicks the real submit button — verified working. See
> `docs/2026-05-16-voice-control-gate-and-adapters-design.md` section 1.1 for the
> current architecture, which supersedes this document.

`ijInjectText()` in `web.rs` (embedded in the `VOICE_WIDGET` constant) can't trigger SolidJS reactivity. The function:

1. Finds the input element (`textarea` or `contenteditable div`)
2. Sets its value using native property descriptor (bypassing SolidJS setter)
3. Dispatches `input` and `keyboard` events
4. Tries to click a submit button or dispatch Enter key

The DOM value IS set (confirmed), but SolidJS doesn't react — `handleSubmit` (in `packages/app/src/components/prompt-input/submit.ts`) never fires.

## OpenCode Frontend Details

- **Framework**: SolidJS v1.9.10 with `@solidjs/router`, `@solidjs/start`
- **Prompt input**: `packages/app/src/components/prompt-input/`
- **Submit handler**: `packages/app/src/components/prompt-input/submit.ts` calls `client.session.promptAsync()`
- **Client**: Created via `createOpencodeClient()`, stored in a SolidJS context
- **No global API**: No window-level function for programmatic message sending

## Possible Solutions

1. **Find the SolidJS signal/store** for the input value and set it directly
2. **Use `document.execCommand('insertText', false, text)`** on a contenteditable element (deprecated but functional)
3. **Dispatch `beforeinput` event** with `inputType: 'insertText'` and proper `data` field (most natural simulation)
4. **Patch fetch/XMLHttpRequest** to intercept and submit via API instead of DOM
5. **Hybrid: API for submit, DOM for observation** — widget knows session ID from URL, call `prompt_async` directly (we confirmed it works), read response from DOM or SSE

## How to Run

```bash
# On the primary machine:
cargo run -- --web --host 0.0.0.0 --port 8765

# OpenCode server runs separately at http://127.0.0.1:4096
# (started via: opencode web --hostname 0.0.0.0 --port 4096)
# Auth: Basic auth with OPENCODE_USERNAME / OPENCODE_PASSWORD from .env
```

## Environment

`.env` file (see `.env.example`):
```
CARTESIA_API_KEY=...
OPENCODE_USERNAME=opencode
OPENCODE_PASSWORD=...
TLS_CERT_PATH=/path/to/cert.pem
TLS_KEY_PATH=/path/to/key.pem
```

## Connection Details

- **Interjections**: `https://<host>:8765/`
- **OpenCode**: `http://127.0.0.1:4096`

## Build Notes

- TLS certs configured via `TLS_CERT_PATH` / `TLS_KEY_PATH` env vars
- Self-signed certs at `certs/cert.pem`, `certs/key.pem` (development fallback)
