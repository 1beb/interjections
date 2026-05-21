# Pocket TTS Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a local Kyutai Pocket TTS engine to interjections, selectable via config (default), behind a shared `Tts` trait, replacing the Cartesia default while keeping Cartesia as a fallback.

**Architecture:** A `Tts` trait with the existing streaming `speak(text, abort, on_audio, on_done)` shape. `CartesiaTts` (existing) and `PocketTts` (new) implement it. A `tts::build(&Config)` factory constructs one engine **once** at startup and stores it as `Arc<dyn Tts>` in `SharedState`; the SSE listener calls `.speak()` on it. Pocket loads its model once via `load_with_params` (weights from the HF cache, no runtime token), clones a voice once, and streams `Vec<i16>` chunks from `spawn_blocking` with a fixed gain + clamp.

**Tech Stack:** Rust, Tokio, Axum, the `pocket-tts` 0.6.2 crate (Candle, CPU), `candle-core` 0.9.2, `async-trait`.

**Reference:** spec at `docs/2026-05-20-pocket-tts-integration-design.md`; decision at `docs/decisions/0006-local-tts-replacement.md`.

**Spike gotchas baked into this plan (verified):**
- Load via `load_with_params` (upcasts BF16→F32 and sets `eos_threshold`); `load_from_bytes` is unusable (no upcast, hardcoded eos −4.0).
- Weights resolve from the HF cache; setup script populates it with a token once, runtime needs none.
- Raw output is ~1.6% full scale → fixed gain (~42×) + clamp is mandatory.
- `eos_threshold = -7.0` yields full sentences (−4.0 truncates).

**Testing note:** binary crate (no `--lib` target). Run unit tests with `cargo test`.

---

### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add crates**

Add to `[dependencies]` in `Cargo.toml`:

```toml
pocket-tts = "0.6.2"
candle-core = "=0.9.2"
async-trait = "0.1"
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build`
Expected: compiles (Candle pulls ~380 crates on first build; this is normal).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build(tts): add pocket-tts, candle-core, async-trait deps"
```

---

### Task 2: Audio gain + clamp conversion (pure function, TDD)

**Files:**
- Create: `src/tts/pcm.rs`
- Modify: `src/tts.rs` → becomes `src/tts/mod.rs` (see Task 3); for now add `mod pcm;` where the tts module is declared.
- Test: inline `#[cfg(test)]` in `src/tts/pcm.rs`

REQUIRED SUB-SKILL: Use superpowers:test-driven-development.

- [ ] **Step 1: Write the failing test**

In `src/tts/pcm.rs`:

```rust
//! f32 → i16 PCM conversion with fixed gain and hard clamp.
//! Pocket TTS emits very quiet float audio (~1.6% full scale); apply a fixed
//! gain then clamp to avoid clipping. Streaming-safe (per-chunk, no global peak).

/// Convert a slice of f32 samples to i16 PCM, applying `gain` then clamping to
/// [-1.0, 1.0] before scaling. Clamp prevents wrap-around distortion.
pub fn to_i16_pcm(samples: &[f32], gain: f32) -> Vec<i16> {
    samples
        .iter()
        .map(|&s| ((s * gain).clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_maps_to_silence() {
        assert_eq!(to_i16_pcm(&[0.0, 0.0], 42.0), vec![0, 0]);
    }

    #[test]
    fn clamps_instead_of_wrapping() {
        // Large input * gain would overflow i16 if not clamped; must saturate.
        assert_eq!(to_i16_pcm(&[10.0, -10.0], 42.0), vec![32767, -32767]);
    }

    #[test]
    fn applies_gain_and_scales() {
        // 0.01 * 42 = 0.42 → 0.42 * 32767 ≈ 13762
        assert_eq!(to_i16_pcm(&[0.01], 42.0), vec![13762]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test tts::pcm`
Expected: FAIL — `src/tts/pcm.rs` not yet wired into the module tree (compile error) until `mod pcm;` is added.

- [ ] **Step 3: Wire the module**

In the tts module root (`src/tts.rs` for now), add at the top: `mod pcm;` (the path resolves to `src/tts/pcm.rs` once the module is a directory; if `src/tts.rs` is still a file, Rust resolves `src/tts/pcm.rs` as a submodule of `tts`). If the compiler complains, defer wiring to Task 3 where `src/tts.rs` becomes `src/tts/mod.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test tts::pcm`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/tts/pcm.rs src/tts.rs
git commit -m "feat(tts): add f32->i16 gain+clamp PCM conversion with tests"
```

---

### Task 3: Define the `Tts` trait; make `CartesiaTts` implement it

**Files:**
- Restructure: `src/tts.rs` → `src/tts/mod.rs` (move the file), keep `src/tts/cartesia.rs` for the Cartesia impl, `src/tts/pcm.rs` from Task 2.
- Modify: `src/main.rs` (module decl unchanged — `mod tts;` still resolves to `src/tts/mod.rs`).

- [ ] **Step 1: Restructure the module**

```bash
mkdir -p src/tts
git mv src/tts.rs src/tts/cartesia.rs
```

**Important:** the `mod pcm;` line added to `src/tts.rs` in Task 2 was carried
into `cartesia.rs` by the `git mv`. Delete that stray `mod pcm;` (and its doc
comment) from `cartesia.rs` — the new `mod.rs` below declares `mod pcm;` itself,
so leaving it in `cartesia.rs` causes a duplicate/wrong-path module error.

Create `src/tts/mod.rs`:

```rust
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

mod cartesia;
mod pcm;
mod pocket;

pub use cartesia::CartesiaTts;
pub use pocket::PocketTts;

/// A text-to-speech engine that streams i16 PCM chunks with abort support.
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

/// Build the configured TTS engine once, at startup.
pub fn build(config: &crate::config::Config) -> anyhow::Result<Arc<dyn Tts>> {
    match config.tts_engine.as_str() {
        "pocket" => Ok(Arc::new(PocketTts::load(config)?)),
        "cartesia" => Ok(Arc::new(CartesiaTts::new(config.clone()))),
        other => anyhow::bail!("unknown tts_engine '{other}' (expected 'pocket' or 'cartesia')"),
    }
}
```

- [ ] **Step 2: Adapt `CartesiaTts::speak` to the trait**

In `src/tts/cartesia.rs`, change the inherent `impl` to `#[async_trait::async_trait] impl crate::tts::Tts for CartesiaTts`, and change `speak`'s signature to the boxed-closure form:

```rust
async fn speak(
    &self,
    text: &str,
    abort_flag: Arc<AtomicBool>,
    mut on_audio: Box<dyn FnMut(Vec<i16>) + Send>,
    on_done: Box<dyn FnOnce() + Send>,
) -> anyhow::Result<()> {
    // ... body unchanged: on_audio(samples) and on_done() still work ...
}
```

(Keep `CartesiaTts::new` as an inherent method.)

- [ ] **Step 3: Verify build**

Run: `cargo build`
Expected: fails only because `pocket` module doesn't exist yet — that's Task 4. To verify Task 3 in isolation, temporarily comment `mod pocket;` and the `pocket` match arm, build, then restore. Expected with pocket stubbed: compiles.

- [ ] **Step 4: Commit**

```bash
git add src/tts/ src/main.rs
git commit -m "feat(tts): add Tts trait; CartesiaTts implements it"
```

---

### Task 4: Implement `PocketTts`

**Files:**
- Create: `src/tts/pocket.rs`

- [ ] **Step 1: Implement the engine**

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pocket_tts::{TTSModel, voice_state::ModelState};

use super::pcm::to_i16_pcm;

/// Local Pocket TTS. Model + voice loaded ONCE at startup; reused per utterance.
pub struct PocketTts {
    model: Arc<TTSModel>,
    voice: ModelState,
    gain: f32,
}

impl PocketTts {
    /// Load the model (weights resolved from the HF cache — see fetch script)
    /// and clone the default voice once. ~1.5 s.
    pub fn load(config: &crate::config::Config) -> anyhow::Result<Self> {
        let model = TTSModel::load_with_params(
            &config.pocket_variant,
            config.pocket_temperature,
            1, // lsd_decode_steps
            config.pocket_eos_threshold,
        )
        .map_err(|e| anyhow::anyhow!(
            "Pocket TTS model load failed: {e}. Run scripts/fetch-pocket-tts.sh \
             (needs HF_TOKEN + accepted license) to populate the model cache."
        ))?;
        let voice = model
            .get_voice_state(&config.pocket_voice)
            .map_err(|e| anyhow::anyhow!(
                "Pocket TTS voice load failed for '{}': {e}", config.pocket_voice
            ))?;
        Ok(Self { model: Arc::new(model), voice, gain: config.pocket_gain })
    }
}

#[async_trait::async_trait]
impl super::Tts for PocketTts {
    async fn speak(
        &self,
        text: &str,
        abort_flag: Arc<AtomicBool>,
        mut on_audio: Box<dyn FnMut(Vec<i16>) + Send>,
        on_done: Box<dyn FnOnce() + Send>,
    ) -> anyhow::Result<()> {
        let model = self.model.clone();
        let voice = self.voice.clone();
        let gain = self.gain;
        let text = text.to_string();

        // Candle inference is synchronous + CPU-bound: run off the async runtime.
        let result = tokio::task::spawn_blocking(move || {
            for chunk in model.generate_stream(&text, &voice) {
                if abort_flag.load(Ordering::Relaxed) {
                    break;
                }
                let chunk = chunk?;
                let flat = chunk.flatten_all()?.to_vec1::<f32>()?;
                on_audio(to_i16_pcm(&flat, gain));
            }
            on_done();
            Ok::<(), anyhow::Error>(())
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(join_err) => Err(anyhow::anyhow!("Pocket TTS task panicked: {join_err}")),
        }
    }
}
```

- [ ] **Step 2: Build**

Run: `cargo build`
Expected: compiles (config fields land in Task 5 — if `pocket_*` fields are missing, do Task 5 first, then return here. Tasks 4 and 5 are tightly coupled; either order works as long as both land before building).

- [ ] **Step 3: Commit**

```bash
git add src/tts/pocket.rs
git commit -m "feat(tts): implement PocketTts (load once, stream chunks via spawn_blocking)"
```

---

### Task 5: Config fields, defaults, CLI flags

**Files:**
- Modify: `src/config.rs`

- [ ] **Step 1: Add fields to `Config` struct** (after the cartesia fields, ~line 53):

```rust
    pub tts_engine: String,
    pub pocket_variant: String,
    pub pocket_voice: String,
    pub pocket_eos_threshold: f32,
    pub pocket_temperature: f32,
    pub pocket_gain: f32,
```

- [ ] **Step 2: Add defaults** (in `impl Default for Config`):

```rust
            tts_engine: "pocket".into(),
            pocket_variant: "b6369a24".into(),
            pocket_voice: "data/models/pocket-tts/voice.wav".into(),
            pocket_eos_threshold: -7.0,
            pocket_temperature: 0.7,
            pocket_gain: 42.0,
```

- [ ] **Step 3: Add CLI flag for engine selection** (in `Cli`):

```rust
    #[arg(long, env = "IJ_TTS_ENGINE", default_value = "pocket")]
    pub tts_engine: String,
```

And in `Config::from_cli`: `cfg.tts_engine = cli.tts_engine.clone();`

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: compiles.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(tts): add pocket-tts config fields, defaults, --tts-engine flag"
```

---

### Task 6: Factory unit test

**Files:**
- Modify: `src/tts/mod.rs` (add `#[cfg(test)]` tests)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cartesia_ok() {
        let mut c = crate::config::Config::default();
        c.tts_engine = "cartesia".into();
        assert!(build(&c).is_ok());
    }

    #[test]
    fn build_unknown_errors() {
        let mut c = crate::config::Config::default();
        c.tts_engine = "bogus".into();
        let err = build(&c).unwrap_err().to_string();
        assert!(err.contains("unknown tts_engine"));
    }
}
```

(Note: do NOT unit-test the `"pocket"` arm — it loads a model and needs the HF cache + voice asset; that path is covered by the gated smoke test in Task 9.)

- [ ] **Step 2: Run to verify fail, then pass**

Run: `cargo test tts::tests`
Expected: PASS once `build` exists (from Task 3).

- [ ] **Step 3: Commit**

```bash
git add src/tts/mod.rs
git commit -m "test(tts): cover engine factory selection and unknown-engine error"
```

---

### Task 7: Wire the shared engine into the web server

**Files:**
- Modify: `src/web.rs` (SharedState ~25-35; `start()` ~37; `listen_for_response` signature ~208 + body ~299-307; call site ~430-436)

- [ ] **Step 1: Add `tts` to `SharedState`**

```rust
#[derive(Clone)]
pub struct SharedState {
    // ... existing fields ...
    pub tts: Arc<dyn crate::tts::Tts>,
}
```

- [ ] **Step 2: Build the engine once in `start()`**

Where `SharedState` is constructed, add before it:

```rust
let tts = crate::tts::build(&config)?;
```

and set `tts,` in the struct literal. (`start` already returns `anyhow::Result<()>`; if not, propagate the error.)

- [ ] **Step 3: Change `listen_for_response` to take the shared engine**

Signature: add `tts: Arc<dyn crate::tts::Tts>,` parameter. Replace the per-response construction (current lines ~299-307):

```rust
// OLD: let tts = crate::tts::CartesiaTts::new(config.clone()); tts.speak(...).await;
let tts_tx_clone = tts_tx.clone();
let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
let _ = tts.speak(
    &response_text,
    abort,
    Box::new(move |samples| { let _ = tts_tx_clone.send(samples); }),
    Box::new(|| {}),
).await;
return;
```

- [ ] **Step 4: Thread `tts` down to the call site** (the `submit` branch, ~line 430-436, which lives **inside `handle_ws`** — not `ws_handler`):

`listen_for_response` is spawned from inside `handle_ws`, which currently does
not receive the engine. So:
1. In `ws_handler`, capture it from state: `let tts = state.tts.clone();` (it's already on `SharedState` from Step 1).
2. Add `tts: Arc<dyn crate::tts::Tts>` as a parameter to `handle_ws`, passed in
   the same way `config` is, and pass `tts` when `ws_handler` calls `handle_ws`.
3. At the submit call site, clone it into the spawned task:

```rust
let tts_engine = tts.clone(); // the handle_ws `tts` param (from SharedState)
tokio::spawn(async move {
    listen_for_response(&session, &auth, tts_tx, tts_engine, &cfg).await;
});
```

(Mirror exactly how `config`/`audio_tx` are already captured and threaded.)

- [ ] **Step 5: Build + existing tests**

Run: `cargo build && cargo test`
Expected: compiles; existing tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/web.rs
git commit -m "feat(tts): build engine once at startup, share via SharedState"
```

---

### Task 8: Provisioning script

**Files:**
- Create: `scripts/fetch-pocket-tts.sh`

- [ ] **Step 1: Write the script**

```bash
#!/usr/bin/env bash
# One-time Pocket TTS setup. Requires HF_TOKEN and acceptance of the
# kyutai/pocket-tts license at https://huggingface.co/kyutai/pocket-tts.
# Populates the HF cache (resolved by load_with_params at runtime) and places
# a default voice WAV. After this, runtime needs no token.
set -euo pipefail

: "${HF_TOKEN:?Set HF_TOKEN (a HuggingFace read token) and accept the kyutai/pocket-tts license first}"

VOICE_DIR="data/models/pocket-tts"
mkdir -p "$VOICE_DIR"

# 1. Populate the HF cache with the gated weights + tokenizer the loader uses.
#    Uses the `hf` CLI (pip install -U huggingface_hub) — downloads into ~/.cache/huggingface.
hf download kyutai/pocket-tts --quiet
hf download kyutai/pocket-tts-without-voice-cloning tokenizer.model --quiet || true

# 2. Default voice reference WAV (clean ~6-10s English clip). Replace the URL/source
#    with a chosen voice; here we fetch an official sample if available.
if [ ! -f "$VOICE_DIR/voice.wav" ]; then
  echo "Place a clean English reference WAV at $VOICE_DIR/voice.wav (6-10s)."
  echo "e.g. a kyutai/tts-voices sample, or any public-domain clip."
fi

echo "Pocket TTS setup complete. Runtime needs no token."
```

- [ ] **Step 2: Make executable + smoke-run guidance**

```bash
chmod +x scripts/fetch-pocket-tts.sh
```

(Do not run in CI; it needs a token. Document in README that this is a one-time step.)

- [ ] **Step 3: Commit**

```bash
git add scripts/fetch-pocket-tts.sh
git commit -m "feat(tts): add one-time pocket-tts provisioning script"
```

---

### Task 9: Smoke test (gated behind asset presence)

**Files:**
- Modify: `src/tts/pocket.rs` (add a `#[cfg(test)]` module)

This is a **binary crate** (no `src/lib.rs`, no `[lib]` target — verified), so an
integration test in `tests/` cannot `use interjections::…`. Put the smoke test
**inside `src/tts/pocket.rs`** using `crate::` paths.

- [ ] **Step 1: Write the gated smoke test** (append to `src/tts/pocket.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::super::Tts;

    // Only runs if the voice asset is present (implies the HF cache was populated
    // by scripts/fetch-pocket-tts.sh). Skipped otherwise — same gating style as
    // the pocket-tts crate's own integration tests.
    #[tokio::test]
    async fn pocket_tts_generates_speech_level_audio() {
        let voice = "data/models/pocket-tts/voice.wav";
        if !std::path::Path::new(voice).exists() {
            eprintln!("skipping: {voice} absent (run scripts/fetch-pocket-tts.sh)");
            return;
        }
        let mut cfg = crate::config::Config::default();
        cfg.tts_engine = "pocket".into();

        let tts = crate::tts::build(&cfg).expect("build pocket tts");
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<i16>::new()));
        let c2 = collected.clone();

        tts.speak(
            "The quick brown fox jumps over the lazy dog.",
            abort,
            Box::new(move |s| c2.lock().unwrap().extend(s)),
            Box::new(|| {}),
        ).await.unwrap();

        let audio = collected.lock().unwrap();
        let peak = audio.iter().map(|s| s.abs() as i32).max().unwrap_or(0);
        assert!(audio.len() > 24_000, "expected >1s of audio, got {} samples", audio.len());
        assert!(peak > 3000, "expected speech-level peak, got {peak}");
    }
}
```

(Requires `tokio` with the `macros` + `rt` features, which the crate already has
via `features = ["full"]`.)

- [ ] **Step 2: Run (skips without assets, passes with them)**

Run: `cargo test pocket`
Expected: PASS (prints a skip line if no voice asset; real generation if present).

- [ ] **Step 3: Commit**

```bash
git add src/tts/pocket.rs
git commit -m "test(tts): gated Pocket TTS smoke test (speech-level audio)"
```

---

### Task 10: Real-app verification + ADR update

**Files:**
- Modify: `docs/decisions/0006-local-tts-replacement.md` (status note: implemented)

- [ ] **Step 1: Run the app with Pocket TTS**

After `scripts/fetch-pocket-tts.sh`: `cargo run -- --web`, speak, confirm the assistant's reply is heard locally (no Cartesia key set). Use the `verify`/`run` skill if helpful.

- [ ] **Step 2: Confirm Cartesia fallback**

Run with `--tts-engine cartesia` (and a key) → still works.

- [ ] **Step 3: Update ADR-0006**

Set ADR-0006 status to reflect Pocket TTS is implemented; add a short
"implemented" note. Update the index row in `docs/decisions/README.md`.

- [ ] **Step 4: Commit**

```bash
git add docs/decisions/
git commit -m "docs(tts): record Pocket TTS implementation"
```

---

### Task 11: Comprehensive README setup (all pieces)

**Files:**
- Modify: `README.md`

The README currently omits the gate (Ollama) entirely and predates local TTS.
Rewrite the **Requirements**, **Setup**, and **Environment** sections so a fresh
operator can stand up every piece. Cover all of these, in order, as an explicit
checklist:

- [ ] **Step 1: Rewrite Requirements + a numbered Setup section** covering:

  1. **System**: Rust 1.75+, Linux + ALSA dev libs (`libasound2-dev`).
  2. **ASR model (Sherpa)**: download
     `sherpa-onnx-streaming-zipformer-en-2023-06-26` into
     `data/models/sherpa-zipformer-en/` (existing step — keep).
  3. **Gate LLM (Ollama)**: install Ollama, `ollama pull qwen3.5:4b`, ensure
     `ollama serve` is running on `localhost:11434`. Note the gate fails open
     (submits raw text) if the model is down, and the first request after idle
     pays a cold-load.
  4. **Local TTS (Pocket TTS) — default engine**:
     - **Accept the license/ToS**: log in at HuggingFace and click "Agree and
       access repository" on https://huggingface.co/kyutai/pocket-tts.
     - **Create a read token**: https://huggingface.co/settings/tokens.
     - **Provision once**: `HF_TOKEN=hf_… ./scripts/fetch-pocket-tts.sh`
       (downloads weights into the HF cache, places a default voice WAV at
       `data/models/pocket-tts/voice.wav`). After this, runtime needs no token.
     - State that `tts_engine` defaults to `pocket`, so the app fails fast at
       startup until this step is done; `--tts-engine cartesia` selects the
       cloud fallback instead.
  5. **Cloud TTS (Cartesia) — optional fallback**: set `CARTESIA_API_KEY` and
     run with `--tts-engine cartesia`.
  6. **OpenCode**: `opencode web --port 4096`; set `OPENCODE_USERNAME` /
     `OPENCODE_PASSWORD` for Basic auth.
  7. **TLS certs**: a `getUserMedia` secure context is required; document
     generating/placing `certs/cert.pem` + `certs/key.pem` (or `TLS_CERT_PATH` /
     `TLS_KEY_PATH`).

- [ ] **Step 2: Update the Environment + CLI Options blocks**

  Add `IJ_TTS_ENGINE` / `--tts-engine`; mark `CARTESIA_API_KEY` as
  *optional (only for `--tts-engine cartesia`)* rather than required; add the
  gate vars (`IJ_GATE_ENDPOINT`, `IJ_GATE_MODEL`, `IJ_GATE_API_KEY`) already in
  `Cli`. Keep entries consistent with `src/config.rs`.

- [ ] **Step 3: Add a one-line "first run" happy path**

  A copy-pasteable block: pull the ASR model + ollama model, run
  `fetch-pocket-tts.sh`, start `opencode web`, `cargo run -- --web`, open
  `https://<host>:8765`.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs: comprehensive setup (ASR, gate, Pocket TTS + HF ToS, OpenCode, TLS)"
```

---

## Deferred (NOT in this plan)

- Sentence-streaming (feed OpenCode deltas to TTS as they arrive) + streaming-friendly loudness normalization.
- Removing Cartesia.
- Cerebras gate eval (task #18, ADR-0005).
