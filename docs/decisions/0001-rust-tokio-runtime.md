# ADR-0001: Rust + Tokio runtime

- **Status:** Accepted
- **Date:** 2026-05-20 (backfilled)
- **Related:** ADR-0002

## Context

interjections is a real-time voice layer: it sits in the audio path between a
microphone and an LLM-backed assistant. The pipeline has to absorb 16 kHz audio
chunks, run ASR incrementally, fan out events over WebSocket and SSE, drive an
HTTPS reverse proxy, talk to local and cloud HTTP services, and abort
mid-utterance without leaking work. Latency budgets are in the low hundreds of
milliseconds (see [`docs/plan.md`](../plan.md) and the gate eval table).

The runtime needs:

- predictable, low-latency concurrency with many small tasks (audio, ASR,
  WebSocket, SSE, TTS, gate, controller state machine);
- first-class cancellation so barge-in actually stops in-flight work;
- a healthy crate ecosystem for ONNX, audio, WebSockets, HTTP/2, TLS;
- no GC pauses interrupting the audio path.

## Decision

Use Rust on Tokio with the standard async stack (`axum`, `hyper`, `reqwest`,
`tokio-tungstenite`, `tokio-rustls`). Single binary; the dev loop is
`cargo run -- --web`.

## Consequences

- Cancellation semantics are explicit; aborting a TTS/gate request is a drop,
  not a flag-check, which makes barge-in reliable.
- ONNX bindings (`sherpa-onnx`, ORT) integrate cleanly without a Python sidecar.
- Compile times and the learning curve are real costs; iteration is slower than
  in a Node or Python prototype.
- No GIL or GC means audio-path code stays predictable, but lifetimes and async
  trait ergonomics raise the floor of contributor expertise.

## Alternatives considered

- **Node.js / TypeScript** — fastest prototyping, weak ONNX/audio story, GC
  jitter in the audio path.
- **Python (asyncio)** — best ML ecosystem, but cancellation and concurrent
  audio handling are awkward and a sidecar architecture defeats the
  single-binary goal.
- **Go** — strong concurrency and HTTP story, but the audio / ONNX / streaming
  ASR ecosystem is thinner than Rust's.
