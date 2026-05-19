# Design — Voice-control proxy: the gate + OpenCode command mode

**Date:** 2026-05-16
**Status:** Approved (brainstorm) — revised 2026-05-19 to a lean v1
**Supersedes parts of:** `docs/handoff.md`, `docs/plan.md`

---

## 1. Vision

interjections today is a voice layer bolted onto OpenCode web: a TLS reverse
proxy that injects a mic widget, does local ASR, pushes transcribed text into
OpenCode's chat, and reads responses aloud via Cartesia TTS.

This design adds two things:

1. **A gate.** Before anything is submitted, the accumulated ASR transcript
   passes through a fast LLM "gate" that does two jobs in one call: judges
   whether the utterance is a *complete thought*, and *classifies* it as a
   prompt for the assistant vs. a UI command. If it is not complete, the
   machine keeps listening.

2. **Command mode.** Voice can drive OpenCode's own UI — open a file, switch
   model, switch project, start a session — not just feed its chat. The code is
   structured so a site is a pluggable **adapter**; OpenCode is adapter #1.

**The north star.** The point is to make working with AI feel *conversational*
— interface friction should not grow as the models get smarter. Two behaviours
define success, both drawn from the "interaction model" idea (Thinking
Machines, 2025): the machine **knowing to wait** when a thought is unfinished,
and **knowing to stop and accept an interjection** when the user cuts in. The
gate (section 4) is how a turn-based upstream is made to approximate this.
Honest ceiling: a fast gate makes the *listening* side genuinely
indistinguishable from a native interaction model; the *responding* side stays
turn-based (OpenCode cannot perceive while it generates), so the best available
there is fast turns plus abort-based barge-in (section 4.9).

### 1.1 Current state (relative to `docs/handoff.md`)

`docs/handoff.md` predates a fix and is **stale on one point**: its "Current
Blocker" section says DOM injection cannot trigger OpenCode's SolidJS submit.
That has since been resolved. `ijInjectText()` now focuses the
`contenteditable` prompt editor, replaces its contents via
`document.execCommand('insertText')` (which fires the real `beforeinput` /
`input` events SolidJS reacts to), and clicks the real submit button. This is
verified working — spoken messages reach OpenCode's message store. The
gate → inject → submit path therefore builds on a *working* primitive, and the
same `execCommand('insertText')` mechanism underlies `pickFromList`
(section 6.1).

---

## 2. Scope

### 2.1 v1 — this spec

The smallest shippable thing that proves the idea on one real site:

- The **gate**: one fast LLM call — completeness + prompt/command classify.
- The **re-listen loop** and barge-in — "knowing to wait" / "knowing to stop".
- **Command mode** for OpenCode: an `Adapter` trait with one hand-written
  `OpenCodeAdapter`.
- **Tier A** drift check — a cheap in-page self-test.

Default ship config is fully local and zero per-call cost: the `qwen3.5:4b`
gate model on local ollama (benchmarked: ~340 ms, 84% — 100% on the core gate
categories). Remote endpoints stay configurable for anyone trading cost for
latency.

### 2.2 Next — deferred, not lost

These came up in the brainstorm and are worth doing — *after* v1 is in use.
They are explicitly **out of v1** so we can try the core idea soon:

| Deferred | Why later |
|---|---|
| **Prosodic turn detection** (Smart Turn v2 class) as an alternative to the semantic gate | Needs a model sourced + an audio eval set built. The semantic gate already meets the latency bar; prosody is an upgrade, not a prerequisite. |
| **Repair stage** — fixing ASR garble ("Aether Bay Jam" → "Azerbaijan") | Benchmarking showed repair is the hardest sub-task (~40–90% even for capable models). It belongs *outside* the gate as its own stage; v1 submits raw ASR and lets OpenCode's context + barge-in absorb garble. |
| **Declarative adapter bundles** + a recipe DSL + an LLM-assisted adapter *generator* | v1 needs only the *seam* (the `Adapter` trait), not the factory. Build the DSL when there is a second adapter to justify it. |
| **Tier B** — full Playwright end-to-end contract check | Tier A catches drift continuously and cheaply; a heavyweight E2E suite can wait. |
| Additional adapters; named model-variant selection; anaphora ("open *that* file") | Own specs / stretch goals. |

The benchmarking work in `eval/` is **done and kept** — it is why we can pick
the gate model with confidence. The repair eval category stays in the dataset
for when the repair stage is built.

---

## 3. Architecture: core vs. adapter

Everything divides into a site-agnostic **core** and a per-site **adapter**.
In v1 the adapter is a Rust type implementing one trait (section 7) — not a
data bundle. The seam is real; the machinery behind it is deferred.

| Concern | Core (generic) | Adapter (per-site) |
|---|---|---|
| TLS proxy, HTML interception, widget injection | ✅ | |
| Widget UI, WebSocket transport | ✅ | |
| ASR · VAD · noise suppression | ✅ | |
| Gate — completeness + classify | ✅ engine | supplies command vocabulary |
| TTS (Cartesia) | ✅ | |
| Recipe primitives (`pickFromList`, `clickByText`, `dispatchKeybind`, `pollFor`, `injectText`) | ✅ | composes them into recipes |
| Selectors / keybinds | | ✅ |
| Command set + recipes | | ✅ |
| Tier A self-test runner | ✅ | supplies the selector list |
| Response detection for TTS readback | ✅ runner | ✅ picks a strategy |

### 3.1 Data flow

```
Browser ─► https://host:8765  (core: TLS proxy + widget injection)
                │
                ▼  http://127.0.0.1:4096  (OpenCode upstream)

Voice pipeline (per utterance):
  Mic ─► WS ─► noise suppress ─► VAD ─► Sherpa ASR ─► reconciler buffer
                                                          │
                                            on ASR endpoint (pause)
                                                          ▼
                                              ╔════ the gate ════╗
                                              ║ complete thought? ║
                                              ║ prompt or command?║
                                              ╚════════╤══════════╝
                            ┌──────────────────────────┼──────────────────────┐
                       incomplete                   prompt                command
                            │                           │                      │
                  keep listening,            inject reconciled       adapter recipe runs
                  re-gate on next pause       text + submit           via core primitives
                                                        │                      │
                                          response strategy ─► text       widget hover line
                                          ─► Cartesia TTS                  "→ opened auth.rs"
```

While `Thinking`, a non-backchannel interjection cancels TTS, aborts the
in-flight response, and re-enters the gate — "barge-in" (section 4.9).

---

## 4. The gate

One fast LLM call between ASR and submit. It answers two questions: *is the
user done?* and *is this a prompt or a command?* That is the whole v1 listening
layer — repair and prosodic detection are Next (section 2.2).

### 4.1 The listening task and placement

Today `controller.rs::on_asr_result()` calls `on_speech_end()` on every ASR
`Final` result (`controller.rs:93-96`), reconciling and broadcasting `"submit"`
inline. The gate call is async (a model call, possibly networked) and cannot
run inline without stalling ASR handling.

So the gate runs in a **dedicated listening task**:

- `on_asr_result` still appends each `Final` to the reconciler, then sends a
  `SegmentFinalized` signal to the listening task — **regardless of current
  state**, so finals are never dropped.
- The listening task owns the gate call, the re-listen loop, the
  in-flight/`dirty` handling, and the silence-fallback timer (section 4.4).

The `Controller` gains a handle to the gate and an mpsc sender to the listening
task.

### 4.2 Verdict schema

The model must return *only* a JSON object:

```json
{
  "status": "incomplete" | "prompt" | "command",
  "hold": true | false,
  "command": { "action": "<action>", "target": "<search string|null>" }
}
```

- `status: "incomplete"` → return **only** `{"status":"incomplete","hold":<bool>}`.
  `hold: true` means the user explicitly signalled more is coming (a preface —
  "I'm going to tell you a story"); `hold: false` means an ambiguous fragment or
  trail-off. The flag drives how patiently the task waits (section 4.4). A
  missing `hold` is treated as `false` (fail-safe — never hang).
- `status: "prompt"` → no `command`, no `hold`. The task submits the reconciled
  buffer text **as-is** (no repair in v1).
- `status: "command"` → include `command`. No `hold`.

`command.target` is a search string, or `null` for actions that take no target
(`new_session`, `cycle_variant`). Every other action (`open_file`,
`switch_model`, `switch_project`, `switch_session`, `run_command`) requires a
non-empty `target`. The verdict parser and the recipe dispatcher (section 6)
validate this per-action and reject a `command` whose `target` presence does
not match its `action` (a rejected command fails open to `Prompt`,
section 4.7).

Rust side:
`enum Verdict { Incomplete { hold: bool }, Prompt, Command { action: CommandAction, target: Option<String> } }`,
where `CommandAction` is an enum over the section 6.2 actions.

### 4.3 Latency

The `incomplete` verdict re-fires on every pause, so it is latency-critical.
The incomplete verdict is deliberately **minimal output** — two flags — so
generation is as fast as possible. Other tactics:

- Persistent `reqwest::Client` (HTTP keep-alive — no per-call TLS handshake).
- `temperature: 0`, no thinking, `response_format: { type: "json_object" }`.
- Compact, static system prompt (cacheable / prefix-reused on the server side).
- Small `max_tokens` (~80).

No-think switch is per-endpoint (section 9).

### 4.4 Re-listen loop and adaptive patience

A single long-lived listening task owns the evaluate-and-commit logic. It
receives `SegmentFinalized` signals over an mpsc channel from `on_asr_result`.
ASR `Final` results always append to the reconciler buffer regardless of
current state — finals are never dropped. The buffer is `reset()` **only** on
commit.

The loop:

1. Wait for a `SegmentFinalized` signal (or the silence-fallback timer).
2. Debounce ~150 ms to coalesce rapid consecutive finals.
3. Snapshot `text = reconciler.reconcile()`; clear the `dirty` flag.
4. `verdict = gate.classify(text)` — the async LLM call. Any `SegmentFinalized`
   arriving *during* the call appends to the buffer and sets `dirty`.
5. On return:
   - If `dirty` is set (the user spoke more mid-call) → discard the verdict and
     loop to step 3 with the now-larger buffer.
   - `Incomplete` → apply adaptive patience by the verdict's `hold` flag (see
     below); loop to step 1.
   - `Prompt` / `Command` → commit (broadcast `"submit"` / `"command"`),
     `reconciler.reset()`, reset the re-check counter; loop to step 1.

Because finals always append and `dirty` forces re-evaluation, a verdict is
only ever committed on text the user has actually finished — there is no window
where mid-call speech is lost or a stale verdict is acted on.

The reconciler already merges a new `User` turn into the previous one when that
turn is still `Active` in the buffer (`reconciler.rs:84-93`); since the buffer
is reset only on commit, consecutive finals across re-listen iterations
accumulate into one coherent turn.

**Adaptive patience.** How long the task waits after an `Incomplete` verdict
depends on the verdict's `hold` flag. The safety nets must fire on gate
*uncertainty*, never on legitimate long input — a user telling a story across
many pauses must not be cut off:

- **`hold: true`** (a confident preface — "I'm going to tell you a story") →
  the task waits **indefinitely** for more speech. No auto-submit; the re-check
  counter does not advance. The widget shows a calm "listening…" (section 10).
  A long inactivity backstop (`gate_hold_backstop_ms`, default 120000) exists
  only so an abandoned session does not sit forever: on expiry with no new
  speech the task quietly returns to `Idle` and **discards** the buffer — it
  never submits a lone preface.
- **`hold: false`** (an ambiguous fragment or trail-off) → the task arms the
  **silence fallback**: a timer (`gate_silence_fallback_ms`, default 5000),
  reset by any `SegmentFinalized`, that on expiry force-commits the buffer as a
  `Prompt`. A **re-check cap** also applies: after `gate_max_rechecks`
  consecutive `hold:false` verdicts (default 3) the task force-commits. Both
  reset on commit.

These nets exist for when the gate is *wrong*, not to limit a genuine long
utterance — which is why only `hold:false` verdicts trip them.

### 4.5 State machine

`Idle → User → Gating → Thinking → Idle`:

- `Idle → User` — VAD detects speech (unchanged).
- `User → Gating` — first `SegmentFinalized`; the gate task is evaluating
  and/or waiting for more speech. The reconciler buffer is non-empty.
- `Gating → Gating` — an `Incomplete` verdict; the widget shows "go on…".
- `Gating → Thinking` — a `Prompt` / `Command` verdict is committed.
- `Thinking → Gating` — barge-in: the user interjects while the assistant is
  responding (section 4.9).
- `Thinking → Idle` — response complete, or command executed.

`Gating` is a new state; it replaces the old behaviour where `on_speech_end`
flipped straight `Thinking → Idle` inline. While `Thinking` the mic stays hot
and barge-in is handled per section 4.9. A widget signal distinguishes `Gating`
("listening…" / "go on…") from cold `Idle` (see section 10).

### 4.6 Gate system prompt (sketch)

Final wording is tuned during implementation; the contract is:

```
You are a fast gate between speech-to-text and a coding assistant.
Input: a raw ASR transcript of something a user said aloud.
Output ONLY a JSON object — no prose, no code fences.

status:
  "incomplete" — the user is NOT done. Judge the *thought*, not grammar:
                 a fragment ("open the"), a trail-off ("and then, um"), OR a
                 preface that promises more ("I'm going to tell you a story",
                 "okay so here's what I want", "let me explain").
                 Return ONLY {"status":"incomplete","hold":<bool>}.
                   hold=true  — the user explicitly signalled more is coming
                                (a preface); the machine should wait patiently.
                   hold=false — an ambiguous fragment or trail-off.
  "command"    — purely a request to navigate the app's UI, with NO task for
                 the assistant. Carries {action, target}.
  "prompt"     — anything else: a question or task for the assistant.
                 This is the default; when unsure, choose "prompt".

Rules:
- "open auth.rs" is a command. "open auth.rs and explain it" is a prompt
  — it asks the assistant to do work.
- A grammatically whole sentence can still be "incomplete": a preface is a
  promise of more. "I'm going to tell you a story." -> incomplete, hold=true.

<adapter command vocabulary injected here>
```

The command vocabulary block is supplied by the active adapter (section 7) —
the gate engine is core, but command-aware per site.

### 4.7 Fail-open

Any failure — network error, timeout (`gate_timeout_ms`, default 4000),
malformed JSON, unknown `status` — makes `classify` return `Prompt` and log the
failure. A flaky LLM must never swallow the user's words; worst case it submits
the raw ASR text, i.e. exactly today's behaviour. The `--no-gate` flag disables
the gate entirely (debugging, or an endpoint outage), reverting to immediate
submit.

### 4.8 Barge-in — "knowing to stop and accept an interjection"

While the assistant is responding (`Thinking` — OpenCode generating, TTS
playing), the mic stays hot and ASR keeps running. Two outcomes:

- **Backchannel** — "yeah", "uh-huh", "right", "mm-hm" (`cues.rs`
  `CueType::Backchannel`). Ignored: TTS and generation continue.
- **Anything else — a real interjection.** The system *stops*, then *accepts*:
  1. **Stop** (reflexive, local, instant). TTS playback is cancelled
     immediately and OpenCode's in-flight generation is aborted via
     `POST /session/{id}/abort` (verified to exist in OpenCode's API). The
     truncated assistant message stays in session history — correct: the
     assistant "was saying X when interrupted."
  2. **Accept** (deliberate, via the gate). The interjection's ASR text enters
     the normal gate pipeline as a fresh `SegmentFinalized`. It is gated like
     any utterance — it may be `incomplete` (→ wait), a new `prompt`, or a
     `command`. State moves `Thinking → Gating`.

**Stop** must feel instant, so it is triggered *locally*: sustained speech
during `Thinking` (VAD past a short threshold) whose partial transcript is not
a backchannel cue — no gate round-trip. **Accept** is a judgement, so it goes
through the gate. A short minimum-duration threshold keeps a quick "uh-huh"
from tripping the stop.

If `POST /abort` fails, the stop still happened (TTS is cancelled); the stale
generation finishes into a response stream the listening task no longer reads.
Logged, not fatal.

### 4.9 Situational examples

"→" is the verdict; the last column is what the user experiences.

**Knowing to wait**

| User says (aloud) | Gate | What happens |
|---|---|---|
| "I'm going to tell you a story" → [8 s pause] | `incomplete, hold=true` | Machine waits silently. Widget: "listening…". Nothing submitted. |
| "okay so what I want you to do is" | `incomplete, hold=true` | Waits patiently for the rest. |
| "open the" → [pause] | `incomplete, hold=false` | Waits ~5 s; if still nothing, force-commits "open the" as a prompt. |
| "and then we should, um…" → [silence] | `incomplete, hold=false` | After `gate_silence_fallback_ms`, commits what it has. |

**Accumulating one thought across pauses**

| User says (across pauses) | Gate per pause | What happens |
|---|---|---|
| "I'm going to tell you a story" / "about a race condition" / "in the auth module" | `incomplete,hold=true` → `incomplete,hold=true` → `prompt` | The fragments accumulate; only the assembled sentence is submitted, once. |

**Prompt vs. command**

| User says | Gate | What happens |
|---|---|---|
| "open the file auth.rs" | `command(open_file,"auth.rs")` | OpenCode file dialog opened to auth.rs. No assistant turn. |
| "open auth.rs and tell me why login fails" | `prompt` | Submitted to the assistant — it asks for *work*, not just navigation. |
| "start a new session" | `command(new_session, null)` | New session created. |
| "switch to Claude Opus" | `command(switch_model,"Claude Opus")` | Model dialog driven. |
| "compact the session" | `command(run_command,"compact session")` | Routed through the palette escape hatch. |

**Knowing to stop and accept an interjection** (assistant is mid-response)

| User says | Gate / cue | What happens |
|---|---|---|
| "yeah" / "mm-hm" | backchannel | Ignored. Response continues. |
| "wait — no, I meant the lexer" | barge-in → `prompt` | TTS stops, generation aborted; interjection gated and submitted. |
| "actually, hold on" | barge-in → `incomplete` | TTS stops, generation aborted; machine then waits for the rest. |

**Edge cases**

| Situation | What happens |
|---|---|
| User keeps talking while a gate call is in flight | `dirty` flag set → verdict discarded → re-gated on the larger buffer (section 4.4). |
| Endpoint unreachable / gate times out | Fail-open: utterance submitted as a `prompt` (section 4.7). |

---

## 5. Core module: the gate client

New module `src/gate.rs`:

- `struct Gate` holds a persistent `reqwest::Client` and config (endpoint, key,
  model, timeout).
- `async fn classify(&self, text: &str) -> Verdict`.
- POSTs an OpenAI-format chat completion to the configured endpoint, parses
  `choices[0].message.content` as JSON into `Verdict`, applies fail-open
  (section 4.7).
- The system prompt is assembled from a core template + the active adapter's
  command vocabulary.

---

## 6. Command mode

### 6.1 Two primitives, one escape hatch

The widget exposes two DOM primitives; every command is built from them (plus
`dispatchKeybind`, `pollFor`):

- `pickFromList(query)` — focus an on-screen search input, set its value
  (`execCommand('insertText')` + real input events — same mechanism as the
  working prompt injection), wait for the list to filter, press Enter. Drives
  the command palette **and** every sub-dialog (file / model / project), which
  all share the search-input + filtered-list + Enter interaction.
- `clickByText(selector, query)` — scrape elements matching `selector`,
  fuzzy-match their text against `query`, click the best match.

### 6.2 OpenCode v1 command set

| action | recipe |
|---|---|
| `new_session` | `pickFromList` "new session" in palette |
| `open_file` | `pickFromList` "open file" in palette → `pickFromList(target)` in file dialog |
| `switch_model` | `pickFromList` "model" in palette → `pickFromList(target)` in model dialog |
| `switch_project` | `pickFromList` "project" in palette → `pickFromList(target)` in directory dialog |
| `switch_session` | `clickByText('a[href*="/session/"]', target)` — by name, DOM scrape |
| `cycle_variant` | `pickFromList` "cycle variant" in palette |
| `run_command` | `pickFromList(target)` straight in the palette |

`run_command` is the generic escape hatch: it routes **any** command in
OpenCode's palette (`session.compact`, `terminal.toggle`, `session.share`, …)
with no new recipe code. The six named actions exist only because they need a
*second* step (a sub-dialog or a scrape) that bare palette routing can't
express.

### 6.3 Division of labour (robustness)

The gate produces a *search string*, not a resolved target. interjections never
needs to know the real file tree, model list, or project list — it types the
string and **the site's own fuzzy search resolves it**. "auth dot rs" → typed
into the file dialog → OpenCode matches it. (Repair would clean the spoken form
first; until the repair stage lands, OpenCode's fuzzy match absorbs most of the
slack.)

### 6.4 Notes on the OpenCode UI (investigated)

- **Model variant** is *not* a picker. `DialogSelectModel` selects the *model*
  only. Variant has one control — `model.variant.cycle` (keybind
  `shift+mod+d`), which steps `off → v1 → v2 → … → off`. v1 supports
  `cycle_variant` only; named-variant is a stretch goal.
- **Sessions are scrapable.** Each sidebar session is
  `<a href="/{slug}/session/{id}">` with the title text inside — so by-name
  session switching works via `clickByText`.
- **Recipe steps poll** (`pollFor`, ~1.5 s max) for their expected element
  rather than using fixed delays; on timeout the recipe aborts cleanly
  (section 8.2).

---

## 7. The Adapter trait

In v1 an adapter is a Rust type implementing one trait — not a data bundle.
`src/adapter.rs` defines:

```rust
trait Adapter {
    fn id(&self) -> &str;
    fn command_vocabulary(&self) -> &str;        // injected into the gate prompt
    fn recipe(&self, cmd: &Verdict) -> Vec<RecipeStep>;   // primitive steps
    fn selectors(&self) -> &[ContractSelector];  // for the widget + Tier A
    fn response_strategy(&self) -> ResponseStrategy;
}
```

- `RecipeStep` is an enum — `PickFromList`, `ClickByText`, `DispatchKeybind`,
  `PollFor`. The server resolves a `Command` verdict to a `Vec<RecipeStep>` and
  ships it to the widget over the WebSocket; the widget's recipe engine
  executes the steps against the live DOM.
- `ContractSelector` is `{ name, primary, fallbacks }` — the selectors the
  widget resolves through, and the list Tier A walks (section 8).
- `ResponseStrategy` for OpenCode is `Sse` against `GET /global/event`
  (section 7.1).

`OpenCodeAdapter` is the one implementation. Selectors and keybinds live as
constants in that module — declared **once**, so a drift fix is a one-line
edit, and so Tier A has something to iterate over. The trait is the seam; the
declarative bundle format and the adapter generator (section 2.2) replace these
constants later without disturbing the core.

### 7.1 OpenCode response strategy

The `Sse` strategy listens to `GET /global/event`:

- The stream emits headerless `data: {json}\n\n` blocks (no `event:` lines).
- JSON has a `payload` wrapper containing `type` and `properties`.
- Relevant event types: `message.part.updated` (carries `part.id`,
  `part.type`), `message.part.delta` (carries `partID`, `field`, `delta`),
  `message.updated` (carries `info.role`, `info.finish`).
- Part types: `step-start`, `reasoning`, `text`, `step-finish`.
- Both reasoning and text deltas arrive with `field: "text"`; they are
  distinguished by looking up the part's `type` via its id. **Only `text`
  parts are accumulated** — thinking/reasoning tokens are never read aloud.
- On `message.updated` with `role: "assistant"` and `finish` set, the
  accumulated text is sent to Cartesia TTS.

This logic exists today in `web.rs::listen_for_response()`; v1 keeps it where
it is, behind the `ResponseStrategy` the adapter returns.

---

## 8. Resilience to site drift (Tier A)

**Premise:** an adapter targets an external, actively-developed site. Its
selectors, keybinds, event shapes *will* break.

### 8.1 One contract, declared once

Today selectors are string literals scattered through the injected JS. They
move into `OpenCodeAdapter`'s `selectors()` (section 7) — one place. A drift
fix becomes a one-line edit there.

### 8.2 Tier A — in-page smoke test, every page load, automatic

On widget init, `ijSelfTest()` walks the adapter's selector list: every entry
must resolve (primary or a fallback). Result → a health dot on the widget
(green/red) + failures logged to the interjections server over the WebSocket,
naming the broken contract point. Continuous, zero effort; catches drift the
moment the user loads the page.

Every contract lookup at runtime also goes through a resolver: primary →
fallbacks → if all fail, the recipe aborts cleanly, the widget hover line shows
the failure, and the server logs *which* contract point died. A renamed
attribute becomes a visible, named failure — never silent misbehaviour.

Tier B (the full Playwright end-to-end check) is **Next** (section 2.2).

---

## 9. Configuration

New config (`config.rs` / `Cli` / `.env`):

| key | source | default |
|---|---|---|
| `gate_endpoint` | default | `http://localhost:11434/v1/chat/completions` (local ollama) |
| `gate_model` | default | `qwen3.5:4b` — free, local, ~340 ms, 84% (100% on the core gate categories) |
| `gate_timeout_ms` | default | 4000 |
| `gate_silence_fallback_ms` | default | 5000 — `hold:false` fallback (section 4.4) |
| `gate_max_rechecks` | default | 3 — `hold:false` re-check cap (section 4.4) |
| `gate_hold_backstop_ms` | default | 120000 — `hold:true` abandoned-session backstop (section 4.4) |
| `gate_debounce_ms` | default | 150 — coalesce rapid finals (section 4.4) |
| `recipe_poll_timeout_ms` | default | 1500 — max wait for a recipe step's element (section 6.4, section 8.2) |
| `OPENCODE_GO_API` / `CEREBRAS` / etc. | env | API keys — only if a remote endpoint is configured |
| `--no-gate` | CLI flag | gate disabled (immediate submit) |

**Default ship config — zero per-call cost, fully local:** `qwen3.5:4b` on
local ollama. Endpoints are OpenAI-compatible; benchmarked alternatives and
their per-provider no-think switch (request body):

| endpoint | model | latency / acc | no-think switch |
|---|---|---|---|
| ollama (local) | `qwen3.5:4b` | ~340 ms / 84% | `reasoning_effort: "none"` |
| llama.cpp (local) | qwen3.5-4b GGUF | ~340 ms / 85% | `chat_template_kwargs: {enable_thinking: false}` |
| Cerebras | `gpt-oss-120b` | ~140 ms / 83% | (non-thinking by default) — per-call cost |
| OpenCode Go | `deepseek-v4-flash` | ~1.3 s / 85% | `thinking: {type: "disabled"}` |

All four capable models land ~83–85% on the full 88-case set — the choice is
latency and cost, not accuracy. Full data: `eval/results.csv`,
`eval/local-setup.md`.

---

## 10. Widget UX

The widget gains, beyond today's mic button / state label / transcript:

- **Gate states** in the state label: `checking…` (gate call in flight) →
  `listening…` (incomplete `hold:true` — a preface; calm and patient) /
  `go on…` (incomplete `hold:false` — a fragment) → `thinking…` (submitted).
- **Barge-in** — speech during `thinking…` cancels TTS playback and returns the
  widget to a listening state (section 4.8).
- **Command feedback** — a small text line under the mic, shown on hover:
  `→ opened auth.rs` or `✗ no session matched 'refactor'`. (No toast; no TTS
  for commands.)
- **Health dot** — green/red, driven by the Tier A self-test (section 8.2).

---

## 11. Module / file layout

`web.rs` is currently 564 lines doing several jobs. v1 adds two new modules and
extracts the widget JS; the proxy/transport split is **Next** (pure refactor,
no feature value now).

```
src/
  main.rs         entry point, audio capture, ASR task loop          (existing)
  config.rs       CLI args, env, defaults                            (existing, extended)
  audio.rs vad.rs local_asr.rs cues.rs reconciler.rs tts.rs          (existing, unchanged)
  controller.rs   state machine; listening task; invokes the gate    (existing, extended)
  gate.rs         gate client + Verdict                              (new)
  adapter.rs      Adapter trait, RecipeStep, OpenCodeAdapter          (new)
  web.rs          proxy, widget injection, WS, SSE, command broadcast (existing, extended)

web/
  widget.js       widget runtime: mic capture, recipe engine,
                  contract resolver, self-test                       (new — from web.rs string)
  widget.html     widget markup                                      (new — from web.rs string)
```

Widget JS moves from string literals in `web.rs` into real `.js`/`.html` files,
embedded via `include_str!` — a prerequisite for the recipe engine and a
standalone improvement.

---

## 12. Error handling summary

| Failure | Behaviour |
|---|---|
| Gate network error / timeout / bad JSON | Fail-open: treat as `Prompt`, log (section 4.7) |
| Gate stuck on `incomplete` (`hold:false`) | Silence fallback + re-check cap force a submit (section 4.4) |
| Gate stuck on `incomplete` (`hold:true`) | Waits indefinitely; `gate_hold_backstop_ms` returns to `Idle`, buffer discarded (section 4.4) |
| Recipe step element never appears | `pollFor` times out (~1.5 s), recipe aborts, hover line + server log (section 8.2) |
| Contract selector unresolved | Resolver fails over to fallbacks; if none, named failure (section 8.2) |
| Unknown command `action` | Recipe dispatcher rejects it, hover line shows error |
| `POST /abort` fails during barge-in | TTS already cancelled; stale generation ignored; logged (section 4.8) |
| Endpoint unreachable at startup | `--no-gate` path; gate disabled, immediate submit |
| Response strategy emits unexpected shape | Logged loudly; no TTS for that turn (no crash) |

---

## 13. Testing

- **Unit:** verdict JSON parsing (all three statuses, `hold` flag, malformed →
  fail-open); reconciler non-reset on `incomplete`; adaptive-patience routing
  (`hold:true` vs `hold:false` → which safety net); barge-in classification
  (backchannel ignored vs interjection triggers stop); recipe dispatcher
  action→steps mapping; contract resolver primary/fallback logic.
- **Integration:** gate client against a mock OpenAI-compatible endpoint;
  barge-in path against a mock OpenCode `/abort` endpoint.
- **Prompt regression:** the `eval/` dataset — transcripts → expected verdicts
  (incl. `hold`), run against the real model to catch gate-prompt drift. The
  repair category stays in the dataset for when the repair stage is built.
- **Tier A:** the in-page self-test is itself the continuous check.

---

## 14. Open questions

- Exact endpoint behaviour of the chosen no-think switch per provider — confirm
  at implementation time.
- Barge-in stop threshold — the minimum speech duration that triggers a stop
  vs. lets a short backchannel pass (section 4.8) needs tuning against real
  use; start conservative.
- Whether the gate prompt's command vocabulary needs per-action examples to
  push command-vs-prompt accuracy above the benchmarked ~84%.

Everything in section 2.2 (prosodic detection, repair stage, declarative
bundles + adapter generator, Tier B, more adapters, named-variant, anaphora) is
deferred work, each with its own spec when its time comes.
