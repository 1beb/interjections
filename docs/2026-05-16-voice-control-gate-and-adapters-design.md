# Design — Voice-control proxy: agentic gate + modular site adapters

**Date:** 2026-05-16
**Status:** Approved (brainstorm) — revised post-review, pending re-review
**Supersedes parts of:** `docs/handoff.md`, `docs/plan.md`

---

## 1. Vision

interjections today is a voice layer bolted onto OpenCode web: a TLS reverse
proxy that injects a mic widget, does local ASR, pushes transcribed text into
OpenCode's chat, and reads responses aloud via Cartesia TTS.

This design takes it two steps further:

1. **An agentic listening layer.** Before anything is submitted, the raw ASR
   transcript passes through a fast LLM "gate" (DeepSeek V4 Flash, no thinking)
   that does three jobs in one call: judges whether the utterance is a
   *complete thought*, *repairs speech-to-text garble*, and *classifies* the
   utterance as a prompt for the assistant vs. a UI command.

2. **A generic voice-control proxy.** interjections stops being
   OpenCode-specific. The core becomes site-agnostic; each controllable site is
   a pluggable **adapter**. OpenCode is adapter #1. Over time, a battery of
   adapters lets the same voice layer drive many sites.

**The north star.** The point is to make working with AI feel *conversational*
— interface friction should not grow as the models get smarter. Two behaviours
define success, both drawn from the "interaction model" idea (Thinking
Machines, 2025): the machine **knowing to wait** when a thought is unfinished,
and **knowing to stop and accept an interjection** when the user cuts in. The
gate (section 4) is how a turn-based upstream is made to approximate this. Honest
ceiling: a fast gate makes the *listening* side genuinely indistinguishable
from a native interaction model; the *responding* side stays turn-based
(OpenCode cannot perceive while it generates), so the best available there is
fast turns plus abort-based barge-in (section 4.9).

This document specifies **sub-project 1** only (see Scope).

### 1.1 Current state (relative to `docs/handoff.md`)

`docs/handoff.md` predates a fix and is **stale on one point**: its "Current
Blocker" section says DOM injection cannot trigger OpenCode's SolidJS submit.
That has since been resolved. `ijInjectText()` now focuses the
`contenteditable` prompt editor, replaces its contents via
`document.execCommand('insertText')` (which fires the real `beforeinput` /
`input` events SolidJS reacts to), and clicks the real submit button. This is
verified working — spoken messages reach OpenCode's message store. The
gate → `adapter.injectText` → `adapter.submit` path in this spec therefore
builds on a *working* primitive, not an open blocker, and the same
`execCommand('insertText')` mechanism underlies `pickFromList` (section 6.1).

---

## 2. Scope

The full vision is a platform and is decomposed into three sub-projects, each
with its own spec:

| # | Sub-project | Status |
|---|---|---|
| 1 | **Modular core + OpenCode adapter** | **This spec** |
| 2 | Adapter generator — "analyze a new site, emit a bundle" (LLM-assisted) | Deferred — own spec later |
| 3 | The battery — additional adapters | Deferred — own spec later |

**This spec delivers:** the agentic gate, the core/adapter architecture, and a
working OpenCode adapter — the smallest shippable thing that proves the
architecture on one real site. The adapter is hand-authored. The seam is
designed so sub-projects 2 and 3 slot in without rework, but neither is built
now.

**Explicitly out of scope for v1:**

- The adapter generator / site-structure analyzer.
- Any adapter other than OpenCode.
- Switching to a session *by name* across the network — handled in v1 via DOM
  scrape (see section 6), but anaphora ("open *that* file") is deferred.
- Setting a *named* model variant — OpenCode exposes only variant *cycling*
  (see section 6); named-variant selection is a stretch goal.
- LLM-assisted self-healing of broken selectors (noted in section 8; not built).

---

## 3. Architecture: core vs. adapter

Everything divides into a site-agnostic **core** and a per-site **adapter**.

| Concern | Core (generic, built once) | Adapter (per-site) |
|---|---|---|
| TLS proxy, HTML interception, widget injection | ✅ | |
| Widget UI, WebSocket transport | ✅ | |
| ASR · VAD · noise suppression | ✅ | |
| DeepSeek gate — completeness, repair, classify | ✅ engine | supplies command vocabulary |
| TTS (Cartesia) | ✅ | |
| Contract resolver + Tier A/B check framework | ✅ engine | supplies the manifest |
| Recipe primitives (`pickFromList`, `clickByText`, `dispatchKeybind`, `pollFor`, `injectText`) | ✅ | composes them into recipes |
| Contract manifest (selectors, keybinds, titles) | | ✅ |
| Command set + recipes | | ✅ |
| Response detection for TTS readback | ✅ strategy runner | ✅ picks a strategy |

The proxy maps `upstream host → adapter`. The injected page script is the core
widget runtime plus the matching adapter bundle.

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
                                              ╔═══ DeepSeek gate ═══╗
                                              ║ complete? repair?   ║
                                              ║ prompt or command?  ║
                                              ╚═════════╤═══════════╝
                            ┌───────────────────────────┼───────────────────────┐
                       incomplete                    prompt                  command
                            │                            │                       │
                  keep listening,             adapter.injectText +      adapter recipe runs
                  re-gate on next pause        adapter.submit            via core primitives
                                                         │                       │
                                          adapter response strategy        widget hover line
                                          ─► response text ─► Cartesia TTS  "→ opened auth.rs"
```

While `Thinking`, a non-backchannel interjection cancels TTS, aborts the
in-flight response, and re-enters the gate — "barge-in" (section 4.9).

---

## 4. The agentic listening layer

> **Revised 2026-05-18.** What was "one DeepSeek call doing three jobs" is now
> three independent, individually-swappable stages. Driven by the gate-model
> benchmarking in `eval/` (see `eval/local-setup.md`, `eval/results.csv`).

### 4.1 Three stages

Between ASR and submit, the accumulated utterance passes through three stages,
each independently configurable:

```
ASR text ─► [1 turn detector: done?] ─► [2 repair: optional] ─► [3 classifier] ─► commit
              prosodic | semantic           on | off              prompt | command
```

**Stage 1 — turn detector** (the "knowing to wait" job). Decides whether the
user has finished or more is coming. Pluggable, selected by `IJ_TURN_DETECTOR`
(section 9):

- **`prosodic`** (default) — an audio-native turn-detection model (Smart Turn
  v2 class: ~6M params, ~10 ms, runs locally). It consumes the raw mic audio
  *next to VAD* and hears rising/falling intonation — the natural "more coming"
  vs "done" signal. Free, instant, no network.
- **`semantic`** — an LLM reads the accumulated ASR *text* and judges
  completeness from grammar and meaning. No prosody and slower, but needs no
  extra model hosted.

Either implementation outputs the same thing: `wait` (with a `hold` strength —
a confident preface holds longer) or `done`. This drives the re-listen loop
(section 4.5).

**Stage 2 — repair** (optional). Fixes speech-to-text garble ("get hub" →
"GitHub", "auth dot rs" → "auth.rs"). It is the hardest sub-task — benchmarking
showed even capable models reach only ~40–90%, small models ~0% — and it is a
different *kind* of work from classification, so it is its own stage, not part
of the gate. Controlled by `--repair on|off` (default **off**):

- **off** — raw ASR text is submitted as-is; OpenCode (which has the
  conversation context) and barge-in correction absorb any garble. The
  "let the main model handle it" path.
- **on** — a dedicated repair LLM call (its own `repair_model`) cleans the
  text before stage 3 and submit.

**Stage 3 — classifier** (LLM). Decides `prompt` vs `command`, and for commands
extracts `{action, target}`. This is the only stage intrinsically
interjections' to do — the main model *is* the prompt destination, so it
cannot route on its own behalf. When unsure it returns `prompt` (the safe
default — the main model with context resolves ambiguity better than the gate,
confirmed by the edge / interjection eval results).

The combined result the pipeline commits is still the `Verdict` of section 4.3
(`status` from stage 3, `hold` from stage 1, repaired `text` from stage 2);
sections 4.3–4.10 describe that combined runtime — the re-listen loop, state
machine, barge-in, and worked examples all carry over unchanged.

### 4.2 The listening task and placement

Today `controller.rs::on_asr_result()` calls `on_speech_end()` on every ASR
`Final` result (`controller.rs:93-96`), reconciling and broadcasting `"submit"`
inline. Stages 1 and 3 involve async work (a model call, possibly networked)
that cannot run inline without stalling ASR handling.

So the pipeline runs in a **dedicated listening task**:

- `on_asr_result` still appends each `Final` to the reconciler, then sends a
  `SegmentFinalized` signal to the listening task — **regardless of current
  state**, so finals are never dropped.
- The listening task owns the three-stage pipeline, the re-listen loop, the
  in-flight/`dirty` handling, and the silence-fallback timer (section 4.5).

The `Controller` gains handles to the configured stages and an mpsc sender to
the listening task.

### 4.3 Verdict schema

The model must return *only* a JSON object:

```json
{
  "status": "incomplete" | "prompt" | "command",
  "hold": true | false,
  "text": "<repaired reconstruction of the utterance>",
  "command": { "action": "<action>", "target": "<search string|null>" }
}
```

- `status: "incomplete"` → return **only** `{"status":"incomplete","hold":<bool>}`
  — see section 4.4. `hold: true` means the user explicitly signalled more is coming
  (a preface — "I'm going to tell you a story"); `hold: false` means an
  ambiguous fragment or trail-off. The flag drives how patiently the gate task
  waits (section 4.5). A missing `hold` is treated as `false` (fail-safe — never hang).
- `status: "prompt"` → include `text` (repaired). No `command`, no `hold`.
- `status: "command"` → include `text` and `command`. No `hold`.

`command.target` is a search string, or `null` for actions that take no target
(`new_session`, `cycle_variant`). Every other action (`open_file`,
`switch_model`, `switch_project`, `switch_session`, `run_command`) requires a
non-empty `target`. The verdict parser and the recipe dispatcher (section 6) validate
this per-action and reject a `command` whose `target` presence does not match
its `action` (a rejected command fails open to `Prompt`, section 4.8).

Rust side:
`enum Verdict { Incomplete { hold: bool }, Prompt { text: String }, Command { action: CommandAction, target: Option<String>, text: String } }`,
where `CommandAction` is an enum over the section 6.2 actions.

### 4.4 Latency — protecting the continuance path

The `incomplete` verdict re-fires on every pause, so it is latency-critical.
Verdicts are deliberately **asymmetric**:

- **Incomplete** → model returns only `{"status":"incomplete","hold":<bool>}`.
  Minimal output (two flags) = fastest possible generation. No repair work
  spent on a fragment.
- **Prompt / command** → the *terminal* call does the full repair into `text`,
  once, on the complete utterance (also where repair is most accurate).

Other latency tactics:

- Persistent `reqwest::Client` (HTTP keep-alive — no per-call TLS handshake).
- `temperature: 0`, no thinking, `response_format: { type: "json_object" }`.
- Compact, static system prompt (cacheable on the Zen side).
- Small `max_tokens` (~120).

### 4.5 Concurrency model and the re-listen loop

A single long-lived **gate task** owns the evaluate-and-commit logic. It
receives `SegmentFinalized` signals over an mpsc channel from `on_asr_result`.
ASR `Final` results always append to the reconciler buffer regardless of
current state — finals are never dropped. The buffer is `reset()` **only** on
commit.

The gate task loop:

1. Wait for a `SegmentFinalized` signal (or the silence-fallback timer).
2. Debounce ~150 ms to coalesce rapid consecutive finals.
3. Snapshot `text = reconciler.reconcile()`; clear the `dirty` flag.
4. `verdict = gate.classify(text)` — the async DeepSeek call. Any
   `SegmentFinalized` arriving *during* the call appends to the buffer and sets
   `dirty`.
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
accumulate into one coherent turn. Repair happens once, on the committing call,
over the whole buffer.

**Adaptive patience.** How long the task waits after an `Incomplete` verdict
depends on the verdict's `hold` flag. The safety nets must fire on gate
*uncertainty*, never on legitimate long input — a user telling a story across
many pauses must not be cut off:

- **`hold: true`** (a confident preface — "I'm going to tell you a story") →
  the task waits **indefinitely** for more speech. No auto-submit; the
  re-check counter does not advance. The widget shows a calm "listening…"
  (section 10). A long inactivity backstop (`gate_hold_backstop_ms`, default 120000)
  exists only so an abandoned session does not sit forever: on expiry with no
  new speech the task quietly returns to `Idle` and **discards** the buffer —
  it never submits a lone preface.
- **`hold: false`** (an ambiguous fragment or trail-off — the user may have
  lost the thread) → the task arms the **silence fallback**: a timer
  (`gate_silence_fallback_ms`, default 5000), reset by any `SegmentFinalized`,
  that on expiry force-commits the buffer as a `Prompt`. A **re-check cap**
  also applies: after `gate_max_rechecks` consecutive `hold:false` verdicts
  (default 3) the task force-commits. Both reset on commit.

These nets exist for when the gate is *wrong*, not to limit a genuine long
utterance — which is why only `hold:false` verdicts trip them.

### 4.6 State machine

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

### 4.7 Gate system prompt (sketch)

Final wording is tuned during implementation; the contract is:

```
You are a fast gate between speech-to-text and a coding assistant.
Input: a raw, possibly garbled ASR transcript of something a user said aloud.
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

For "prompt" and "command", also return `text`: the utterance with obvious
speech-to-text errors repaired (phonetic garbles, wrong word splits). Repair
ONLY — never add, drop, reorder meaning, or answer. e.g. "Aether Bay Jam"
-> "Azerbaijan".

Rules:
- "open auth.rs" is a command. "open auth.rs and explain it" is a prompt
  — it asks the assistant to do work.
- A grammatically whole sentence can still be "incomplete": a preface is a
  promise of more. "I'm going to tell you a story." -> incomplete, hold=true.

<adapter command vocabulary injected here>
```

The command vocabulary block is supplied by the active adapter (section 7) — the gate
engine is core, but command-aware per site.

### 4.8 Fail-open

Any failure — network error, timeout (`gate_timeout_ms`, default 4000),
malformed JSON, unknown `status` — makes `classify` return
`Prompt { text: <raw ASR> }` and log the failure. A flaky LLM must never
swallow the user's words; worst case it submits un-repaired text, i.e. exactly
today's behavior. The `--no-gate` flag disables the gate entirely (debugging,
or a Zen outage), reverting to immediate submit.

### 4.9 Barge-in — "knowing to stop and accept an interjection"

While the assistant is responding (`Thinking` — OpenCode generating, TTS
playing), the mic stays hot and ASR keeps running. Two outcomes:

- **Backchannel** — "yeah", "uh-huh", "right", "mm-hm" (`cues.rs`
  `CueType::Backchannel`). Ignored: TTS and generation continue. The user is
  just affirming.
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

The two-step split mirrors the two verbs in the behaviour. **Stop** must feel
instant, so it is triggered *locally*: sustained speech during `Thinking` (VAD
past a short threshold) whose partial transcript is not a backchannel cue —
no gate round-trip required. **Accept** is a judgement, so it goes through the
gate. A short minimum-duration threshold keeps a quick "uh-huh" from tripping
the stop.

If `POST /abort` fails, the stop still happened (TTS is cancelled); the stale
generation finishes into a response stream the gate task no longer reads.
Logged, not fatal.

### 4.10 Situational examples

Concrete behaviours the gate produces — "→" is the verdict, the last column is
what the user experiences.

**Knowing to wait**

| User says (aloud) | Gate | What happens |
|---|---|---|
| "I'm going to tell you a story" → [8 s pause] | `incomplete, hold=true` | Machine waits silently. Widget: "listening…". Nothing submitted. |
| "okay so what I want you to do is" | `incomplete, hold=true` | Waits patiently for the rest. |
| "open the" → [pause] | `incomplete, hold=false` | Waits ~5 s; if still nothing, force-commits "open the" as a prompt. |
| "and then we should, um…" → [silence] | `incomplete, hold=false` | After `gate_silence_fallback_ms`, commits what it has — the gate was unsure, so it stops guessing. |

**Accumulating one thought across pauses**

| User says (across pauses) | Gate per pause | What happens |
|---|---|---|
| "I'm going to tell you a story" / "about a race condition" / "in the auth module" | `incomplete,hold=true` → `incomplete,hold=true` → `prompt` | The fragments accumulate; only the assembled sentence is submitted, once. |

**Repairing speech-to-text garble**

| User says (ASR heard) | Gate | What happens |
|---|---|---|
| "what's the capital of Aether Bay Jam" | `prompt, text="what's the capital of Azerbaijan"` | Repaired text submitted, not the garble. |
| "open auth dot R S" | `command(open_file,"auth.rs"), text="open auth.rs"` | File dialog driven with the repaired target. |

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
| "yeah" / "mm-hm" | backchannel | Ignored. Response continues uninterrupted. |
| "wait — no, I meant the lexer" | barge-in → `prompt` | TTS stops, generation aborted; interjection gated and submitted. |
| "actually, hold on" | barge-in → `incomplete` | TTS stops, generation aborted; machine then waits for the rest. |

**Edge cases**

| Situation | What happens |
|---|---|
| User keeps talking while a gate call is in flight | `dirty` flag set → verdict discarded → re-gated on the larger buffer (section 4.5). |
| Zen unreachable / gate times out | Fail-open: utterance submitted as a `prompt`, un-repaired (section 4.8). |

---

## 5. Core module: the gate client

New module `src/gate.rs`:

- `struct Gate` holds a persistent `reqwest::Client` and config (endpoint, key,
  model, timeout).
- `async fn classify(&self, text: &str) -> Verdict`.
- POSTs an OpenAI-format chat completion to the Zen endpoint, parses
  `choices[0].message.content` as JSON into `Verdict`, applies fail-open (section 4.8).
- The system prompt is assembled from a core template + the active adapter's
  command vocabulary.

---

## 6. Command mode

### 6.1 Two primitives, one escape hatch

The widget exposes exactly two DOM primitives; every command is built from
them (plus `dispatchKeybind`, `pollFor`):

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
string and **the site's own fuzzy search resolves it**. "auth dot rs" → gate
repairs to `auth.rs` → typed into the file dialog → OpenCode matches it.

### 6.4 Notes on the OpenCode UI (investigated)

- **Model variant** is *not* a picker. `DialogSelectModel` selects the *model*
  only. Variant has one control — `model.variant.cycle` (keybind
  `shift+mod+d`), which steps `off → v1 → v2 → … → off`. v1 supports
  `cycle_variant` only; named-variant is a stretch goal (would require reading
  the current variant and cycling until match).
- **Sessions are scrapable.** Each sidebar session is
  `<a href="/{slug}/session/{id}">` with the title text inside — so by-name
  session switching works via `clickByText`.
- **Recipe steps poll** (`pollFor`, ~1.5 s max) for their expected element
  rather than using fixed delays; on timeout the recipe aborts cleanly (section 8.3).

---

## 7. Adapter bundle format

An adapter is a **declarative bundle** — `adapters/<id>/`:

- **`contract.json`** — every assumption the adapter makes about the site:
  ```json
  {
    "id": "opencode",
    "verifiedAgainst": "<opencode git commit/version>",
    "selectors": {
      "promptInput":  { "primary": "[data-component=\"prompt-input\"]",
                        "fallbacks": ["[contenteditable=\"true\"]", "[role=\"textbox\"]"] },
      "submitButton": { "primary": "[data-action=\"prompt-submit\"]", "fallbacks": [] },
      "sessionLinks": { "primary": "a[href*=\"/session/\"]", "fallbacks": [] }
    },
    "keybinds": { "palette": "mod+shift+p" }
  }
  ```
- **`commands.json`** — command actions, declarative recipe steps, and the
  vocabulary string fed into the gate's system prompt (section 4.7). Recipe steps are
  drawn from the core primitive set (`pick_from_list`, `click_by_text`,
  `dispatch_keybind`, `poll_for`).
- **`response.json`** — the TTS readback strategy:
  - `sse` — listen to a server-sent-events endpoint (OpenCode: `GET /global/event`).
  - `dom-mutation` — observe a DOM container for new text (generic chat sites).
  - `none` — control-only site, no readback.
- **`adapter.js`** *(optional)* — escape hatch for site-specific JS that
  declarative recipes can't express.

The core's recipe engine interprets the declarative steps; the core's response
runner executes the chosen strategy. Pure-data adapters are easy to
hand-author and — critically — easy for sub-project 2 to machine-generate.

### 7.1 The OpenCode adapter (response strategy)

`response.json` for OpenCode uses the `sse` strategy against `GET /global/event`:

- The stream emits headerless `data: {json}\n\n` blocks (no `event:` lines).
- JSON has a `payload` wrapper containing `type` and `properties`.
- Relevant event types: `message.part.updated` (carries `part.id`, `part.type`),
  `message.part.delta` (carries `partID`, `field`, `delta`),
  `message.updated` (carries `info.role`, `info.finish`).
- Part types: `step-start`, `reasoning`, `text`, `step-finish`.
- Both reasoning and text deltas arrive with `field: "text"`; they are
  distinguished by looking up the part's `type` via its id. **Only `text`
  parts are accumulated** — thinking/reasoning tokens are never read aloud.
- On `message.updated` with `role: "assistant"` and `finish` set, the
  accumulated text is sent to Cartesia TTS.

This logic exists today in `web.rs::listen_for_response()` and is re-homed into
the core SSE strategy runner, parameterised by `response.json`.

---

## 8. Resilience to site drift

**Premise:** an adapter targets an external, actively-developed site. Its
selectors, keybinds, event shapes *will* break. This is designed for as
inevitable.

### 8.1 One contract, declared once

Today selectors are string literals scattered through the injected JS. They
move into the adapter's `contract.json` (section 7). A drift fix becomes a one-line
edit there. The manifest also gives the checks something to iterate over.

### 8.2 Two-tier check

- **Tier A — in-page smoke test, every page load, automatic.** On widget init,
  `ijSelfTest()` walks the contract: every selector entry must resolve (primary
  or a fallback). Result → a health dot on the widget (green/red) + failures
  logged to the interjections server over the WebSocket, naming the broken
  contract point. Continuous, zero effort; catches drift the moment the user
  loads the page.
- **Tier B — full end-to-end check, on demand — the regression guard.** A
  Playwright suite (`scripts/contract-check`) drives a real upstream +
  interjections and asserts the whole pipeline: widget injected → text
  injection reaches the upstream's message store → palette opens, every command
  findable → each recipe runs and produces its expected UI change → the
  response strategy emits the expected shape. Pass/fail per contract point.
  One command: `./scripts/contract-check.sh`. **Run after every upstream
  update.** Trigger is on-demand for v1 (a schedule can be added later).

### 8.3 Degrade, don't crash

Every contract lookup goes through a resolver: primary → fallbacks → if all
fail, the recipe aborts cleanly, the widget hover line shows the failure, and
the server logs *which* contract point died. A renamed attribute becomes a
visible, named failure — never silent misbehaviour.

### 8.4 i18n caveat

Palette command titles are localized strings — inherently fragile. Where the
site exposes a stable command id / slash alias in the DOM, the contract matches
on that; the localized title is the fallback. Tier B catches it either way.

### 8.5 Version pinning

`contract.json.verifiedAgainst` records the upstream commit/version the
contract was last verified against; Tier A surfaces a mismatch as a "re-run
Tier B" nudge.

*Noted, not built for v1:* LLM-assisted "find the new selector" self-heal.
YAGNI until Tier B proves insufficient.

---

## 9. Configuration

New config (`config.rs` / `Cli` / `.env`). The three stages (section 4.1) are
configured independently.

**Stage 1 — turn detector:**

| key | source | default |
|---|---|---|
| `IJ_TURN_DETECTOR` / `--turn-detector` | env / CLI | `prosodic` — or `semantic` |
| `turn_detector_model` | default | Smart Turn v2 GGUF path (prosodic) / classifier model (semantic) |
| `gate_silence_fallback_ms` | default | 5000 — `hold:false` fallback (section 4.5) |
| `gate_max_rechecks` | default | 3 — `hold:false` re-check cap (section 4.5) |
| `gate_hold_backstop_ms` | default | 120000 — `hold:true` abandoned-session backstop (section 4.5) |
| `gate_debounce_ms` | default | 150 — coalesce rapid finals (section 4.5) |

**Stage 2 — repair (optional):**

| key | source | default |
|---|---|---|
| `--repair` | CLI | `off` — or `on` |
| `repair_endpoint` / `repair_model` | default | (used only when `--repair on`) |

**Stage 3 — classifier, and shared LLM access:**

| key | source | default |
|---|---|---|
| `classifier_endpoint` | default | local — `http://localhost:11434/v1/chat/completions` (ollama) |
| `classifier_model` | default | `qwen3.5:4b` — free, local, ~340ms, 84% (100% on the core gate categories) |
| `classifier_timeout_ms` | default | 4000 |
| `OPENCODE_GO_API` / `CEREBRAS` / etc. | env | API keys — only if a remote endpoint is configured |
| `recipe_poll_timeout_ms` | default | 1500 — max wait for a recipe step's element (section 6.4, section 8.3) |
| `--no-gate` | CLI flag | gate disabled (immediate submit) |

**Default ship config — zero per-call cost, fully local:** prosodic turn
detector (Smart Turn, local) + `qwen3.5:4b` classifier (local ollama) + repair
off. Remote options stay configurable for anyone trading cost for speed.

**Endpoints are OpenAI-compatible.** Benchmarked alternatives and their
per-provider no-think switch (passed in the request body):

| endpoint | model | latency / acc | no-think switch |
|---|---|---|---|
| ollama (local) | `qwen3.5:4b` | ~340ms / 84% | `reasoning_effort: "none"` |
| llama.cpp (local) | qwen3.5-4b GGUF | ~340ms / 85% | `chat_template_kwargs: {enable_thinking: false}` |
| Cerebras | `gpt-oss-120b` | ~140ms / 93% | (non-thinking by default) — per-call cost |
| OpenCode Go | `deepseek-v4-flash` | ~1.3s / 85% | `thinking: {type: "disabled"}` |

Full data: `eval/results.csv`, `eval/local-setup.md`.

---

## 10. Widget UX

The widget gains, beyond today's mic button / state label / transcript:

- **Gate states** in the state label: `checking…` (gate call in flight) →
  `listening…` (incomplete `hold:true` — a preface; calm and patient) /
  `go on…` (incomplete `hold:false` — a fragment) → `thinking…` (submitted).
- **Barge-in** — speech during `thinking…` cancels TTS playback and returns
  the widget to a listening state (section 4.9).
- **Repaired-text flash** — the repaired `text` is shown for ~1 s before
  submit, so a bad repair is at least visible.
- **Command feedback** — a small text line under the mic, shown on hover:
  `→ opened auth.rs` or `✗ no session matched 'refactor'`. (No toast; no TTS
  for commands — a Cartesia round-trip for "opening file" is not worth the
  latency.)
- **Health dot** — green/red, driven by the Tier A self-test (section 8.2).

---

## 11. Module / file layout

`web.rs` is currently 564 lines doing five jobs (proxy, widget HTML/JS, WS
handler, SSE listener, TTS relay). It is split as part of this work:

```
src/
  main.rs         entry point, audio capture, ASR task loop          (existing)
  config.rs       CLI args, env, defaults                            (existing, extended)
  audio.rs vad.rs local_asr.rs cues.rs reconciler.rs                  (existing, unchanged)
  controller.rs   state machine; now invokes the gate                (existing, extended)
  tts.rs          Cartesia client                                    (existing, unchanged)
  gate.rs         DeepSeek gate client                               (new)
  proxy.rs        TLS reverse proxy, HTML interception, injection     (new — from web.rs)
  transport.rs    WebSocket handler (widget <-> server)               (new — from web.rs)
  response.rs     response-detection strategy runner (SSE, etc.)      (new — from web.rs)
  adapter.rs      Adapter type, bundle loading, registry              (new)

web/
  widget-core.js  widget runtime: mic capture, recipe engine,
                  contract resolver, self-test                        (new — from web.rs string)
  widget.html     widget markup                                       (new — from web.rs string)

adapters/opencode/
  contract.json   selectors, keybinds, version pin
  commands.json   command actions, recipes, gate vocabulary
  response.json   sse strategy config
  adapter.js      (optional) site-specific JS

scripts/
  contract-check.sh         Tier B entry point
  contract-check/           Tier B Playwright suite
```

Widget JS moves from giant string literals in `web.rs` into real `.js`/`.html`
files, embedded via `include_str!`. This is a prerequisite for adapters and a
standalone improvement.

---

## 12. Error handling summary

| Failure | Behaviour |
|---|---|
| Gate network error / timeout / bad JSON | Fail-open: treat as `Prompt { raw text }`, log (section 4.8) |
| Gate stuck on `incomplete` (`hold:false`) | Silence fallback + re-check cap force a submit (section 4.5) |
| Gate stuck on `incomplete` (`hold:true`) | Waits indefinitely; `gate_hold_backstop_ms` returns to `Idle`, buffer discarded (section 4.5) |
| Recipe step element never appears | `pollFor` times out (~1.5 s), recipe aborts, hover line + server log (section 8.3) |
| Contract selector unresolved | Resolver fails over to fallbacks; if none, named failure (section 8.3) |
| Unknown command `action` | Recipe dispatcher rejects it, hover line shows error |
| `POST /abort` fails during barge-in | TTS already cancelled; stale generation ignored; logged (section 4.9) |
| Zen unreachable at startup | `--no-gate` path; gate disabled, immediate submit |
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
- **Prompt regression:** the section 4.10 situational examples become the fixture set
  — transcripts → expected verdicts (incl. `hold`), run against the real model
  to catch gate-prompt drift.
- **Tier A:** the in-page self-test is itself the continuous check.
- **Tier B:** the Playwright `contract-check` suite (section 8.2).

---

## 14. Open questions / future work

- Exact Zen API model id for DeepSeek V4 Flash — confirm via `GET
  https://opencode.ai/zen/v1/models` at implementation time (display name is
  `DeepSeek-V4-Flash-EL`).
- Anaphora resolution ("open *that* file") — needs conversation context;
  deferred.
- Barge-in stop threshold — the minimum speech duration that triggers a stop
  vs. lets a short backchannel pass (section 4.9) needs tuning against real use;
  start conservative.
- Named model-variant selection — deferred (cycle-only in v1).
- Sub-project 2 (adapter generator) and sub-project 3 (more adapters) — own
  specs.
- Tier B on a schedule rather than on-demand — revisit if drift is frequent.
