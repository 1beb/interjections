# ADR-0006: Local TTS replacement

- **Status:** Accepted (primary verified by spike) — implementation pending
- **Date:** 2026-05-20 (revised after fresh May 2026 market survey + spike)
- **Supersedes:** ADR-0004 (Cartesia transitional) once implemented
- **Related:** ADR-0004

## Spike verdict (2026-05-20)

Pocket TTS was verified end-to-end in Rust on the dev machine (RTX 3060 Ti
box, CPU inference). A throwaway crate using `pocket-tts` 0.6.2 produced a
clear, full-length spoken utterance ("The quick brown fox jumps over the lazy
dog", confirmed audible by ear). Measured:

- builds clean (~38 s), model ready in ~1.5 s including weight load;
- **time-to-first-audio ~59 ms**; **~2.6× realtime** generation on CPU; **zero VRAM**;
- pull-based streaming iterator of audio tensors; `pcm_i16_le_bytes` maps
  straight onto our WebSocket relay.

Gotchas learned (must carry into implementation):

1. **Use the documented gated weights + `HF_TOKEN`.** The token-avoiding
   detour through the ungated `kyutai/pocket-tts-without-voice-cloning` repo
   yields **BF16** weights that `load_from_bytes` runs in low precision
   (`VarBuilder::from_tensors` does not upcast) → degenerate near-silent
   output. The canonical `TTSModel::load*` path (mmaped) upcasts BF16→F32
   correctly.
2. **`normalize_peak` is mandatory** — raw model output peaks at ~1.6% of full
   scale.
3. **`eos_threshold` controls utterance length** (more negative = longer).
   Default `-4.0` truncated the sentence to ~0.5 s; `-7.0` gave the full
   3.28 s utterance.
4. Voice: cloning from a reference WAV works; official voices live in the
   (gated) `kyutai/tts-voices` repo. Pick/ship a default voice at
   implementation time.

Kokoro-82M remains the documented fallback but was not needed — Pocket TTS
clears the bar.

## Context

ADR-0004 records Cartesia Sonic-3 as the transitional choice. The goal of this
ADR is to pick a **local** replacement that preserves the user-perceived
behaviour.

Constraints (confirmed 2026-05-20):

- **Streaming is the dominant requirement** — audio must start playing while
  the LLM is still producing tokens. This is the property that makes the loop
  feel conversational, more than absolute first-chunk latency.
- Voice quality / persona is not load-bearing. One good default voice is fine.
- Must run locally. The 3060 Ti has roughly 1.8 GB of VRAM headroom alongside
  `qwen3.5:4b` (ADR-0005); a sub-1 GB model is comfortable, 2+ GB starts
  competing. CPU-only is even better.
- Must be cancellable mid-utterance for barge-in
  (`docs/2026-05-16-…-design.md`, section 4.9).
- Existing Rust integration shape: `src/tts.rs` produces PCM chunks consumed
  by the WebSocket relay; the controller can drop the stream to abort.

A note on "streaming": for a long time no widely-deployed local TTS streamed
*within* a sentence; the practical pattern was sentence-level chunked
synthesis. That changed in 2026 — Kyutai's Delayed Streams Modeling produces
audio tokens interleaved with text tokens, so first audio appears before the
sentence ends. This is the strongest fit for our pipeline and the biggest
shift in the landscape since the original plan-doc shortlist.

## Decision (proposed)

**Primary: Kyutai Pocket TTS** (~100 M params, MIT, **CPU-only**, true
token-level streaming via Delayed Streams Modeling). Released January 2026.
HF: [kyutai/pocket-tts](https://huggingface.co/kyutai/pocket-tts).
Repo: [kyutai-labs/pocket-tts](https://github.com/kyutai-labs/pocket-tts).

- ~200 ms time-to-first-chunk on CPU; ~6× real-time on consumer hardware.
- **Zero VRAM cost** — leaves the entire 8 GB on the 3060 Ti for the gate
  model and any future upgrades.
- MIT-licensed weights.
- A Candle community port (`pocket-tts-candle`) provides a Rust path; the
  fallback is a subprocess.

**Stable fallback: Kokoro-82M via Rust ONNX.** If the Pocket TTS Rust port
turns out flaky or its integration cost is higher than expected, swap to
Kokoro-82M (Apache-2.0 weights). Mature Rust ecosystem
([`kokoroxide`](https://crates.io/crates/kokoroxide),
[`kokorox`](https://lib.rs/crates/kokorox),
[`tts-rs`](https://github.com/rishiskhare/tts-rs)). Streaming is
sentence-chunked (not token-level), but at ~80 MB int8 and sub-second
per-sentence synth, the pipelined feel is acceptable. Climbed to #1 on TTS
Arena in January 2026.

Keep Cartesia behind a feature flag during the transition for A/B comparison.

## Consequences (if accepted)

- **No more cloud TTS dependency or API key.** Voice loop works offline.
- **Pocket TTS at CPU = no GPU pressure.** Major change from the old plan:
  TTS no longer competes with the gate for VRAM, and we keep room to grow
  the gate model later (ADR-0005 mentions a Parakeet-class ASR alongside).
- **Token-level streaming** finally available locally. First audio can land
  before the LLM has finished a sentence, which closes the gap with
  Cartesia's perceived responsiveness.
- The controller still needs to feed TTS as the LLM streams text, but no
  longer has to do clever sentence-splitting just to get audio out fast —
  Pocket TTS will start synthesising on partial input. Current `src/tts.rs`
  only synthesises on `finish: "stop"`; that has to change.
- Rust integration cost for Pocket TTS is the main *unknown*. The Candle
  port is community-maintained; the safe fallback (Kokoro-82M ONNX) has
  multiple production-ready Rust crates.

## Alternatives considered — fresh May 2026 survey

| Candidate | Streaming type | Params | License | VRAM/CPU | Rust path | Verdict |
|---|---|---|---|---|---|---|
| **Kyutai Pocket TTS** | token-level (DSM) | ~100 M | MIT | CPU only | Candle port + community PyO3/WASM | **Primary** |
| **Kokoro-82M** | sentence-chunked | 82 M | Apache-2.0 weights | <500 MB GPU or CPU | `kokoroxide`, `kokorox`, `tts-rs` (Feb 2026) | **Fallback** |
| **Supertonic 3** | chunk / windowed | ~99 M | OpenRAIL-M | CPU OK | Official `rust/` example | Close third; license restrictions need review |
| **Chatterbox-Turbo** | sentence-chunked | 350 M | MIT | PyTorch BF16, won't fit 1.8 GB | PyTorch only | Lost on Rust integration + VRAM |
| **Kyutai TTS 1.6 B** | token-level (DSM) | 1.6 B | server-class | needs GPU, ~1.5 GB+ | Moshi Rust server | Higher quality but eats most of our VRAM headroom |
| **Orpheus 3 B** | token-level | 3 B | open weights | needs vLLM + FP8, doesn't fit | Python stack | VRAM-disqualified |
| **Sesame CSM-1B** | token-level | 1 B | open (1B only) | borderline VRAM fit | Python stack | VRAM-borderline; bigger variants stay proprietary |
| **Mistral Voxtral 4B TTS** | streaming-native | 4 B | **CC-BY-NC** | ≥16 GB VRAM | Python stack | License + VRAM, double-disqualified |
| **CosyVoice 2 (0.5 B)** | streaming | 500 M | open | GPU-friendly | no first-party Rust | Lost on Rust integration |
| **OuteTTS 1.0 (0.6 B)** | token-level (in theory) | 600 M | open | GPU | sparse | Streaming endpoint marked "not implemented" in early 2026; skip until stable |
| **Piper** | sentence-chunked | 30-100 M | GPL-3.0 | CPU only | mature | **Demoted.** Still maintained (1.4.2, Apr 2026, now under `OHF-Voice/piper1-gpl`) but Kokoro matches it on quality with a more permissive licence and better Rust ONNX tooling. |
| **Browser SpeechSynthesis** | per-utterance | n/a | n/a | none | n/a | Quality unacceptable; abort behaviour inconsistent |

## Notable 2026 releases

These post-date the original ADR draft and the project's plan doc:

- **Kyutai Pocket TTS** (Jan 2026) — first credible **CPU-only token-streaming
  TTS**. The most important shift in the landscape for our use case.
- **Qwen3-TTS** (Feb 2026) — large; a Rust inference port exists
  ([second-state/qwen3_tts_rs](https://github.com/second-state/qwen3_tts_rs)).
  Doesn't fit VRAM.
- **Mistral Voxtral 4B TTS** (Mar 2026) — streaming-native but CC-BY-NC and
  16 GB VRAM floor.
- **OmniVoice** (Apr 2026) — 600+ languages, diffusion-LM style, cloning-first.
- **Chatterbox-Turbo** (early 2026) — distilled fast variant; MIT but PyTorch
  only.
- **Supertonic 3** (Apr 29 2026) — smaller, tighter, 31 languages.
- **Piper 1.4.x line** (Jan-Apr 2026) — active under `OHF-Voice/piper1-gpl`.

## Open items before this lands

1. Verify the `pocket-tts-candle` Rust port is current and tractable. If
   not, fall back to a subprocess wrapper around the Python reference impl
   (still tolerable since synth is CPU and fast).
2. Pick a default Pocket TTS voice; ship the model file under
   `data/models/pocket-tts-…` to match the Sherpa pattern.
3. Move TTS triggering from `finish: "stop"` to streaming-on-partials in the
   controller. With Pocket TTS this is the unlock for token-level audio;
   with Kokoro it's the unlock for sentence-pipelined audio.
4. Decide whether to keep Cartesia behind a flag or remove at cutover.
5. If we go with Kokoro instead of Pocket TTS, pick between `kokoroxide` and
   `tts-rs` (Feb 2026 v2026.2.3) on the basis of maintenance velocity.

## Notes

- Knowledge of the field is fast-moving; this survey is current as of
  2026-05-20. Re-check before locking in if implementation slips by more
  than a quarter.
- Once Pocket TTS or Kokoro is wired and the streaming-on-partials pattern
  is proven, swapping to a heavier model later (Voxtral, Orpheus, Kyutai
  1.6 B) is just a model swap, not a re-architecture.
