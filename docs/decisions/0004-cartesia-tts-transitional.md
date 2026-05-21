# ADR-0004: Cartesia Sonic-3 TTS (transitional)

- **Status:** Transitional (to be superseded by ADR-0006)
- **Date:** 2026-05-20 (backfilled)
- **Related:** ADR-0006

## Context

The voice loop needs spoken responses with three properties:

1. **Low first-chunk latency** — the user hears the assistant start almost
   immediately. The plan doc targets ~100-300 ms first chunk.
2. **Streaming** — chunks arrive as the LLM produces text, not after.
3. **Cancellable** — barge-in must cut audio mid-utterance with no leftover
   playback after the user starts talking.

The original plan (`docs/plan.md`, Phase 2) explicitly called out replacing
cloud TTS with a local model (Piper recommended). That work has not landed —
the voice loop has been gated on getting the rest of the pipeline working
first.

## Decision

Use **Cartesia Sonic-3** over its streaming WebSocket as the **transitional**
TTS. Authentication via `CARTESIA_API_KEY`. Implementation in `src/tts.rs`.

This choice is provisional. ADR-0006 supersedes it once a local replacement is
in place.

## Consequences

- TTS quality and latency are excellent out of the box; this hides the
  remaining rough edges in the pipeline while we work on them.
- **Adds a cloud dependency** to a project that is otherwise local-first;
  every TTS turn requires a working network path and a paid API key.
- Bills accrue per-character on the user's account; demo and dev usage is
  metered.
- Privacy: the assistant's full response text is sent to Cartesia.
- WebSocket-based abort is clean; barge-in works.

## Alternatives considered

At the time of the initial choice, no local TTS had been wired up yet — see
`docs/plan.md:109-117`. The candidates considered (Piper, Supertonic, Kokoro)
remain valid starting points for ADR-0006; they were not rejected, merely
deferred.

- **ElevenLabs** — comparable quality, similar latency, same cloud-dependency
  objection.
- **OpenAI TTS** — adequate but higher latency, less robust streaming.
- **Browser `SpeechSynthesisUtterance`** — zero-dep, but quality is poor and
  streaming is not controllable.
