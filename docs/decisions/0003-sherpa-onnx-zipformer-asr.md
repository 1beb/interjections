# ADR-0003: Sherpa-onnx streaming Zipformer for ASR

- **Status:** Under review (see ADR-0007)
- **Date:** 2026-05-20 (backfilled)
- **Related:** ADR-0001, ADR-0007

## Context

The voice pipeline needs **streaming** ASR — partial hypotheses must arrive
while the user is still speaking, so the gate can be triggered on endpointing
and barge-in can react during assistant playback. The recogniser has to run
locally on commodity hardware, expose endpointing (silence detection) so the
gate fires at the right moment, and have first-class Rust integration.

When this project started, the practical streaming-ASR options for Rust were
limited. Vosk is older and weaker; Whisper variants are not natively streaming
(they're chunk-based) and require windowing tricks. Sherpa-onnx ships a
streaming Zipformer transducer with native endpointing, ONNX runtime, and a
Rust crate.

## Decision

Use [`sherpa-onnx`](https://github.com/k2-fsa/sherpa-onnx) with the
`sherpa-onnx-streaming-zipformer-en-2023-06-26` model (INT8 encoder/joiner,
FP32 decoder), greedy search, with the built-in endpointer (rule1 2.4 s,
rule2 1.2 s trailing silence). See `src/local_asr.rs`.

## Consequences

- Streaming partial transcripts are emitted naturally; endpointing fires the
  gate without us writing a VAD-driven controller layer for ASR finalisation.
- Pure CPU inference is fast enough on the dev machine; the audio path does
  not contend with the gate model's GPU residence.
- The 2023-06-26 model is **English-only, no punctuation, all-caps**, and it
  makes audible mistakes on common phrases (observed: "MY WIFE IS REFUSING TO
  SLEEP" recognised as "MY LIFE IS REFUSING TO SLEEP HOW CAN I HELP FOR GET TO
  SLEEP"). Acoustic confusions (life/wife) and word-boundary errors are
  recurring failure modes.
- The design doc explicitly defers an **ASR repair stage** (`docs/2026-05-16-…
  design.md`, section 2.2). v1 submits raw ASR and lets the LLM's context plus
  barge-in absorb garble.

## Status note (2026-05-20)

Reopened. The accuracy floor is low enough that even the gate sees malformed
inputs ("FOR GET TO SLEEP" is not something a gate prompt can easily classify).
ADR-0007 evaluates replacement candidates.

## Alternatives considered

- **Whisper (`whisper.cpp` / `faster-whisper`)** — much better accuracy and
  punctuation, but not natively streaming; needs chunked decoding with
  context windowing and adds first-token latency.
- **Vosk** — older models, weaker than Zipformer at comparable sizes;
  retained as a fallback artefact in `data/models/vosk-small-en-us`.
- **Cloud ASR (Deepgram/Cartesia/AssemblyAI)** — better quality, but kills the
  "local-first" property and adds per-utterance cost.
