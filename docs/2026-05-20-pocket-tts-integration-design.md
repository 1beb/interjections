# Design — Pocket TTS integration (engine swap)

**Date:** 2026-05-20
**Status:** Approved (brainstorm)
**Related:** ADR-0004 (Cartesia transitional), ADR-0006 (local TTS replacement, spike-verified)

## 1. Goal & scope

Replace the cloud Cartesia TTS with a **local** TTS engine (Kyutai Pocket TTS),
selectable by config, so the assistant's spoken responses are generated on-device
with no per-utterance cloud cost or API key.

**In scope (this spec):**
- A `Tts` trait with two implementations: `CartesiaTts` (existing) and
  `PocketTts` (new), selected by a `tts_engine` config switch (default Pocket).
- Pocket TTS model loaded **once** at startup and shared; voice state cloned
  once and reused.
- Streaming `Vec<i16>` PCM chunks to the existing broadcast → widget path, with
  a fixed gain + clamp (not global peak-normalization).
- One-time provisioning of model + voice assets into `data/models/pocket-tts/`.

**Explicitly out of scope (deferred follow-ups):**
- **Sentence-streaming** — feeding OpenCode response deltas to TTS as they
  arrive (today TTS triggers only after the full response). This is the main
  latency win and gets its own spec, together with a streaming-friendly loudness
  normalization to replace the fixed gain.
- **Removing Cartesia** — kept as the config-selectable fallback for now.
- **Cerebras gate evaluation** — separate workstream (gate, ADR-0005; task #18).

## 2. Context — current TTS path

`src/tts.rs` defines `CartesiaTts` with:

```rust
async fn speak(
    &self,
    text: &str,
    abort_flag: Arc<AtomicBool>,
    on_audio: impl FnMut(Vec<i16>) + Send + 'static,
    on_done: impl FnOnce() + Send + 'static,
) -> anyhow::Result<()>
```

It opens a WebSocket to Cartesia, streams `pcm_s16le` chunks, and calls
`on_audio(Vec<i16>)` per chunk. In `src/web.rs::listen_for_response`, the SSE
listener accumulates the assistant's full response and **only on
`message.updated` with `finish`** constructs `CartesiaTts::new(config)` and calls
`speak`, forwarding chunks to a `broadcast::Sender<Vec<i16>>` consumed by the
voice widget.

Two facts shape the design:
1. The `speak` signature is already a clean streaming-with-abort interface — a
   good trait boundary.
2. Cartesia is constructed **per response** (stateless, cheap). Pocket TTS must
   be constructed **once** (model load ≈ 1.5 s); constructing it per utterance
   would inject unacceptable latency.

The current `abort_flag` is a placeholder (`AtomicBool::new(false)`); barge-in is
not yet wired into this path. The new engine will honor the flag; wiring barge-in
to set it is out of scope here.

## 3. Spike findings carried in (from ADR-0006)

Verified on the dev machine (RTX 3060 Ti box, CPU): `pocket-tts` 0.6.2 crate,
builds clean, ~1.5 s model load, **~59 ms time-to-first-chunk, ~2.6× realtime,
zero VRAM**, produced a clear full-length utterance. Required practices:

- **Load via `TTSModel::load_with_params`** (the mmaped path) — it upcasts
  BF16→F32 and lets us set `eos_threshold`. The `load_from_bytes` path is
  unusable: it does **not** upcast (near-silent) and hardcodes
  `eos_threshold = -4.0` (truncates).
- **Weights resolve from the HuggingFace cache.** `load_with_params` always
  reads the crate's bundled config (which points at the gated `kyutai/pocket-tts`
  repo) — there is no public way to point it at arbitrary local files without
  forking the crate. The one-time setup script populates the HF cache (with
  `HF_TOKEN` + license acceptance); **at runtime no token is needed** — the
  loader hits the cache. Verified: running with `HF_TOKEN` unset after the cache
  is populated produces correct audio with no 401.
- **Raw output is very quiet** (peak ≈ 0.0167 of full scale) — gain is mandatory.
- **`eos_threshold` controls utterance length** (more negative = longer). `-4.0`
  truncated; `-7.0` produced complete sentences. Make it configurable, default
  `-7.0`.

## 4. Architecture

```
config.tts_engine ─► tts::build(&config) ─► Arc<dyn Tts + Send + Sync>
                                            (built ONCE at startup, stored in SharedState)
                          ┌─────────────────┴─────────────────┐
                       CartesiaTts                          PocketTts
                     (holds Config)              (holds TTSModel + cached voice ModelState)
                          └──────── speak(text, abort_flag, on_audio, on_done) ───────┘
                                            │
                              on_audio(Vec<i16>) ─► broadcast::Sender ─► widget
```

### 4.1 The `Tts` trait

```rust
#[async_trait::async_trait]
pub trait Tts: Send + Sync {
    async fn speak(
        &self,
        text: &str,
        abort_flag: Arc<AtomicBool>,
        on_audio: Box<dyn FnMut(Vec<i16>) + Send>,
        on_done: Box<dyn FnOnce() + Send>,
    ) -> anyhow::Result<()>;
}
```

(The closure args become boxed trait objects so the method is object-safe.
`CartesiaTts::speak` is adapted to this signature; behavior unchanged.)

### 4.2 Factory

```rust
pub fn build(config: &Config) -> anyhow::Result<Arc<dyn Tts>> {
    match config.tts_engine.as_str() {
        "pocket"  => Ok(Arc::new(PocketTts::load(config)?)),  // loads model now
        "cartesia"=> Ok(Arc::new(CartesiaTts::new(config.clone()))),
        other     => anyhow::bail!("unknown tts_engine '{other}' (expected 'pocket' or 'cartesia')"),
    }
}
```

Called once during web server setup; the `Arc<dyn Tts>` is added to
`SharedState`. `listen_for_response` clones the `Arc` from state and calls
`speak` instead of constructing an engine.

### 4.3 PocketTts

- **Construction (`PocketTts::load`)**: call `TTSModel::load_with_params(variant,
  temp, 1, eos_threshold)`. Weights resolve from the HF cache (pre-populated by
  the setup script); no token needed at runtime. Then clone the voice once via
  `get_voice_state(<pocket_voice path>)` and cache the resulting `ModelState`.
  Store `Arc<TTSModel>` + `ModelState` + gain. This whole step happens **once**
  at startup (~1.5 s).
- **`speak`**: spawn the synchronous generation on `tokio::task::spawn_blocking`
  (Candle inference is sync, CPU-bound). For each chunk from
  `model.generate_stream(text, &voice_state)`:
  1. check `abort_flag` → if set, stop and finish;
  2. apply fixed gain, clamp to [-1.0, 1.0], convert f32 → `i16`;
  3. `on_audio(samples)`.
  After the stream ends (or on abort), call `on_done`.

### 4.4 Audio level — fixed gain + clamp (not peak-normalize)

**This intentionally departs from ADR-0006**, which lists `normalize_peak` as
"mandatory". Peak-normalization needs the whole utterance, which would block
chunk streaming and re-introduce a full-synth wait — unacceptable given the
latency priority. Instead apply a configurable fixed gain (`pocket_gain`) and
hard-clamp to [−1, 1] to prevent clipping.

Calibration starting point: the spike measured a nominal peak ≈ 0.0167 of full
scale. Targeting ≈ −3 dBFS implies a gain of ≈ 0.71 / 0.0167 ≈ **~42×** (≈ +32 dB)
as the default; tune against real responses. Per-utterance level may vary
slightly; acceptable for v1. Proper loudness normalization is deferred with
sentence-streaming.

## 5. Config & provisioning

### 5.1 New config fields (`src/config.rs`)

| field | default | meaning |
|---|---|---|
| `tts_engine` | `"pocket"` | `"pocket"` or `"cartesia"` |
| `pocket_variant` | `"b6369a24"` | model variant passed to `load_with_params` |
| `pocket_voice` | `data/models/pocket-tts/voice.wav` | default voice reference WAV (cloned at startup) |
| `pocket_eos_threshold` | `-7.0` | generation length control |
| `pocket_temperature` | `0.7` | sampling temperature |
| `pocket_gain` | `42.0` | fixed output gain before clamp (≈ −3 dBFS from the spike's ~0.0167 peak) |

Weights are **not** a config path — they live in the HF cache (section 3).
Existing Cartesia fields remain. CLI flags mirror the config as the codebase
already does.

### 5.2 `scripts/fetch-pocket-tts.sh`

One-time setup. Uses `HF_TOKEN` (operator accepts the `kyutai/pocket-tts`
license once) to:
- populate the HF cache with the gated weights + tokenizer that
  `load_with_params` resolves (e.g. via the `hf` CLI, or by running the binary
  once with `HF_TOKEN` set);
- place a default voice reference WAV at `data/models/pocket-tts/voice.wav`
  (a clean ~6–10 s English clip; an official `kyutai/tts-voices` sample or a
  chosen public-domain clip).

After setup, runtime needs no token. `data/models/` is not committed (same as
the existing model dirs); the HF cache lives under `~/.cache/huggingface`.

**Rollout note:** `tts_engine` defaults to `"pocket"`, so a fresh checkout
fails fast at startup until `scripts/fetch-pocket-tts.sh` has run (section 6, no
silent fallback). This is a behavior change for anyone currently relying on
Cartesia-by-default; the implementation plan should call out the setup step as a
prerequisite. Setting `tts_engine = "cartesia"` restores the old path.

**Planning note:** exact `pocket-tts` 0.6.2 method signatures
(`load_with_params`, `get_voice_state`, `generate_stream`) and the repo's
existing smoke-test asset-gating mechanism (path-existence vs env var) should be
verified against the source during planning rather than taken as confirmed.

## 6. Error handling

- **Startup**: if the configured engine fails to construct — for Pocket, missing
  or unreadable model files — fail fast with a message naming the missing path
  and pointing at `scripts/fetch-pocket-tts.sh`. No silent fallback to the other
  engine: the operator selected `tts_engine`, and a quiet substitute would hide
  misconfiguration.
- **Per-utterance**: an inference error inside `speak` is logged; `on_done` is
  still called so the SSE/broadcast pipeline does not wedge. Abort mid-utterance
  stops pulling chunks and calls `on_done` cleanly.

## 7. Testing

- **Unit (no model)**:
  - gain + clamp + f32→i16 conversion: silence maps to silence; out-of-range
    floats clamp (not wrap); a known buffer maps to expected `i16` values.
  - factory selects the correct engine per `tts_engine`; unknown value errors.
- **Smoke (gated behind asset presence)**: if `data/models/pocket-tts/` exists,
  load the model, generate a short phrase, and assert non-empty, speech-level
  output of plausible duration. Skipped when assets are absent (same pattern as
  the crate's own integration tests). Not run in CI without assets.

## 8. Components & boundaries

| Unit | Responsibility | Depends on |
|---|---|---|
| `Tts` trait | One streaming-with-abort speak interface | — |
| `CartesiaTts` | Cloud TTS over WebSocket (existing) | Config, network |
| `PocketTts` | Local TTS: load once, clone voice, stream chunks | `pocket-tts` crate, local model files |
| `tts::build` | Pick + construct the engine once from config | Config |
| `scripts/fetch-pocket-tts.sh` | Provision model + voice into data/models/ | HF_TOKEN |

Each is independently understandable and testable; `web.rs` depends only on the
`Tts` trait, not on which engine is active.

## 9. Deferred follow-ups (tracked, not built here)

1. **Sentence-streaming**: feed OpenCode response deltas to TTS as they stream,
   so speech starts before the full response is in. Replaces fixed gain with
   streaming-friendly loudness handling. Own spec.
2. **Retire Cartesia** once Pocket is proven in real use.
3. **Cerebras gate eval** (task #18, ADR-0005) — unrelated to TTS.
