# ADR-0007: ASR replacement / upgrade

- **Status:** Open — recommendation drafted, not yet implemented
- **Date:** 2026-05-20 (revised after fresh May 2026 market survey)
- **Supersedes:** ADR-0003 once accepted
- **Related:** ADR-0003

## Context

ADR-0003 documents the current ASR (Sherpa-onnx streaming Zipformer,
2023-06-26 English model) and flags it Under Review. Symptoms motivating the
review (observed 2026-05-20):

- Acoustic confusions on common words (e.g. "wife" → "LIFE", "her" → "FOR").
- Word-boundary errors ("HER GET" → "FOR GET").
- No punctuation, ALL-CAPS output — downstream consumers (the gate, OpenCode)
  paper over this, but it makes garble harder to diagnose.

Constraints (confirmed 2026-05-20):

- **Streaming partials + native endpointing** are non-negotiable. The gate
  fires on endpointing and partials drive UI feedback.
- Modest accuracy is acceptable in absolute terms.
- **Technical vocabulary matters** — file names, identifiers, language
  keywords come up routinely. The single biggest weakness of the current
  setup.
- Local. ~1.8 GB VRAM headroom on a 3060 Ti or CPU. English-only fine.

### Important correction from the May 2026 survey

The original draft of this ADR proposed a "drop-in to a newer Sherpa-onnx
English streaming Zipformer." **That model does not exist.** As of
2026-05-20, k2-fsa has not shipped any newer English streaming Zipformer; the
2023-06-26 model is still the current one in the
[online-transducer index](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/online-transducer/zipformer-transducer-models.html).
What k2-fsa *has* shipped is sherpa-onnx support for **Moonshine v2** and
**Parakeet-Unified**, both as alternative streaming transducers usable through
the same Rust API. The decision below reflects that.

## Decision (proposed)

**Primary: Moonshine v2 medium-streaming via sherpa-onnx.**

- Model: `moonshine-ai/moonshine` medium streaming, **245 M params, MIT**.
- Streaming type: **native streaming encoder** (ergodic sliding-window
  self-attention with cached state); real-time partials as the user speaks.
- WER: **6.65 % on LibriSpeech test-other** (medium streaming variant).
- Punctuation/casing in output — bonus over the current ALL-CAPS Zipformer.
- VRAM: fits well under 1 GB; CPU-capable for edge use.
- Rust integration: **sherpa-onnx ≥ v1.12.28** ships C++/**Rust**/Python/JS
  bindings for Moonshine v2; ready-quantised bundles like
  `sherpa-onnx-moonshine-base-en-quantized-2026-02-27` are on HuggingFace.
  Same `OnlineRecognizer` API shape we already use — this is a config-and-
  model swap in `src/local_asr.rs`, not a code rewrite.

**Important caveat on hotwords.** Moonshine's OSS release does **not** ship
contextual biasing — the commercial Moonshine service handles domain
customisation. If hotword biasing for code identifiers turns out to be
load-bearing (and it might, given the constraints we set), drop to the
fallback below.

**Fallback if hotwords are required: Parakeet-Unified-en-0.6 B via
sherpa-onnx.**

- Model: `sherpa-onnx-nemo-parakeet-unified-en-0.6b`. 600 M params.
- License: NVIDIA Open Model License (commercial-permitted but not strictly
  OSI).
- Streaming type: **chunked / buffered RNNT** (sherpa-onnx v1.13.0 added the
  buffered streaming path). Expect ~300-700 ms first-partial latency
  depending on chunk size — slower than Moonshine v2 but still acceptable.
- WER: ~3 % on LibriSpeech test-other for the offline-flagship family;
  unified-streaming will be slightly worse.
- **Hotwords work** via sherpa-onnx's Aho-Corasick + `modified_beam_search`
  — same lever we've been planning to use for technical vocabulary.
- Punctuation/casing built in.
- ~0.6-1.2 GB GPU at int8/fp16; CPU runnable.

### Hotword landscape (the practical tie-breaker)

Hotword biasing is the cheapest fix for "wife→life" style confusions and for
code-identifier vocabulary. Support is uneven:

| Model | sherpa-onnx hotwords? |
|---|---|
| current streaming Zipformer (2023-06-26) | ✅ (Aho-Corasick + modified beam search) |
| Parakeet-Unified-en-0.6 B | ✅ (transducer RNNT path) |
| **Moonshine v2** | ❌ (no OSS biasing today) |
| Qwen3-ASR | offline only (sherpa-onnx v1.12.36); not streaming |
| Distil-Whisper | weak prompt-only biasing |
| Nemotron-Streaming | not documented; not in ONNX export |

Decision shape: try Moonshine v2 first because the WER jump from 2023
Zipformer is large and the integration is genuinely trivial; if measured
recognition of `async`, `tokio`, `kubectl`, project file names, etc. is
unacceptable, switch to Parakeet-Unified and trade ~half a second of partial
latency for the hotword lever.

Defer the **ASR repair stage** called out in the design doc (section 2.2)
until after this lands; it sits *after* this recogniser in the pipeline and
should be evaluated against the post-upgrade baseline, not the current one.

## Consequences (if accepted)

- Code shape is preserved — `src/local_asr.rs` keeps the same
  `OnlineRecognizer` interface. The change is model files + sherpa-onnx
  crate bump.
- **Punctuation and proper casing arrive** for free (both candidates emit
  cased, punctuated text). Logs become readable; downstream consumers don't
  need to handle ALL CAPS as a special case.
- Modest VRAM increase: Moonshine v2 medium ≈ a few hundred MB at int8;
  Parakeet-Unified ≈ ~1 GB. Both fit within the 1.8 GB headroom.
- If we pick Moonshine v2: **we lose the hotword lever** as a hedge against
  domain-specific errors. Plan for a repair stage sooner, or be ready to
  pivot to Parakeet-Unified.
- The 2023-06-26 Zipformer stays in `data/models/` as a rollback artefact
  for one release cycle.

## Alternatives considered — fresh May 2026 survey

| Candidate | Streaming | Params | License | VRAM | sherpa-onnx | Rust | Hotwords | WER (LS test-other) | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| **Moonshine v2 medium** | native | 245 M | MIT | <1 GB | ✅ v1.12.28+ | ✅ | ❌ | 6.65 % | **Primary** |
| **Parakeet-Unified-en-0.6 B** | buffered RNNT | 600 M | NVIDIA OM | ~1 GB | ✅ v1.13.0+ | ✅ | ✅ | ~3 % offline | **Fallback** if hotwords needed |
| **Nemotron-Speech-Streaming-en-0.6 B** | native cache-aware | 600 M | NVIDIA OM | fp16 fits | ❌ (export blocked, see [#2177](https://github.com/k2-fsa/sherpa-onnx/issues/2177)) | n/a | not documented | **2.56 %** @ 160 ms | Best on paper; integration is a project, park as later option |
| **Voxtral Mini 4B Realtime 2602** | native | 4 B | Apache 2.0 | **≥16 GB** | ❌ | community crate exists | unknown | 5.52 % | VRAM-disqualified |
| **Qwen3-ASR 1.7 B / 0.6 B** | vLLM only | 600 M / 1.7 B | Apache 2.0 | tight at BF16 | partial (offline) | n/a | offline only | 3.38 % (1.7 B) | Streaming path is vLLM-only; no ONNX |
| **Distil-Whisper / faster-whisper** | chunked windowed | varies | open | varies | n/a | n/a | weak prompt biasing | ~3 % | Chunk latency floor (300-500 ms) worse than Moonshine; no hotwords |
| **Parakeet-TDT-0.6 B v3** | *simulated* streaming only | 600 M | NVIDIA OM | ~1 GB | ⚠ | n/a | ✅ | n/a | **Avoid** — sherpa-onnx [#2918](https://github.com/k2-fsa/sherpa-onnx/issues/2918): gets slower as buffer grows |
| **Multitalker-Parakeet-Streaming-0.6 B** | native | 600 M | NVIDIA OM | ~1 GB | ❌ ([#3454](https://github.com/k2-fsa/sherpa-onnx/issues/3454)) | n/a | unknown | n/a | sherpa-onnx support not merged |
| **current Sherpa Zipformer 2023-06-26** | native | small | open | tiny | ✅ | ✅ | ✅ | ~7 %+ | Status quo — replaced |
| **Vosk** | native | small | open | tiny CPU | n/a | n/a | n/a | ~10 %+ | Worse than current |
| **Cloud ASR** (Deepgram/Cartesia/AssemblyAI) | native | n/a | n/a | none local | n/a | n/a | yes | ~2-3 % | Local-first violation; rejected |

## Notable 2026 releases

- **Moonshine v2** (Feb 2026, arXiv 2602.12241) — streaming-first redesign
  with the ergodic encoder. The big news for low-latency local ASR this
  year and the reason this ADR shifted away from the "newer Zipformer" path.
- **NVIDIA Nemotron-Speech-Streaming-en-0.6 B** (Jan 5 2026, refreshed Mar
  13 2026) — first NVIDIA model branded as natively streaming with a formal
  chunk-size/WER table.
- **Voxtral Mini 4B Realtime 2602** (Feb 2026) — Apache 2.0 but heavyweight.
- **Qwen3-ASR 1.7 B / 0.6 B** (Jan 29 2026) — Apache 2.0 LLM-style ASR with
  streaming via vLLM.
- **sherpa-onnx v1.12.28** added Rust bindings for Moonshine v2; **v1.13.0**
  added the Parakeet-Unified buffered RNNT streaming path.

## Open items before this lands

1. Bump the `sherpa-onnx` crate version to ≥ 1.12.28 (verify the latest
   Rust crate version on crates.io).
2. Download a quantised Moonshine v2 medium bundle (e.g.
   `sherpa-onnx-moonshine-base-en-quantized-2026-02-27`) into
   `data/models/`; update `Config::sherpa_model_dir` and the file names in
   `src/local_asr.rs`.
3. Build a small eval set of 20-30 representative utterances (including
   tricky ones like the wife/sleep example) and measure WER before/after.
4. If Moonshine v2 misrecognises project-specific identifiers ≥ N % of the
   time, pivot to Parakeet-Unified and prepare a `hotwords.txt` harvested
   from `src/**`.
5. Decide whether to keep the 2023-06-26 model artefact in `data/models/`
   for one release cycle as rollback (probably yes).

## Notes

- Knowledge of the field is fast-moving; this survey is current as of
  2026-05-20.
- The "Parakeet TDT v3" trap is worth remembering — its sherpa-onnx
  "streaming" path is a simulation that degrades with buffer length, not
  real streaming. Use the **Unified** export, not v3.
- The Nemotron streaming model is the right *future* target: best WER, best
  latency, native cache-aware streaming. Wait for sherpa-onnx ONNX export
  support to mature.
