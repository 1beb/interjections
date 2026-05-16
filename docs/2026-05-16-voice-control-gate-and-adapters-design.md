# Design — Voice-control proxy: agentic gate + modular site adapters

**Date:** 2026-05-16
**Status:** Approved (brainstorm) — pending spec review
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

This document specifies **sub-project 1** only (see Scope).

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
  scrape (see §6), but anaphora ("open *that* file") is deferred.
- Setting a *named* model variant — OpenCode exposes only variant *cycling*
  (see §6); named-variant selection is a stretch goal.
- LLM-assisted self-healing of broken selectors (noted in §8; not built).

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

---

## 4. The agentic gate

### 4.1 Placement

The gate slots into the one existing decision point —
`controller.rs::on_speech_end()`, between `reconciler.reconcile()` and the
broadcast that today unconditionally emits `"submit"`. The `Controller` gains a
`gate: Gate` field.

```
on_speech_end():
    text = reconciler.reconcile()           # raw accumulated ASR
    verdict = gate.classify(text)           # one DeepSeek V4 Flash call
    match verdict:
      Incomplete       -> keep reconciler buffer; return to listening
      Prompt{text}     -> broadcast "submit" with repaired text; reconciler.reset()
      Command{..}      -> broadcast "command"; reconciler.reset()
```

### 4.2 Three jobs, one call

A single DeepSeek V4 Flash call (no thinking) does all of:

1. **Completeness** — is this a finished thought, or a fragment / trailing-off?
2. **ASR repair** — fix phonetic garble ("Aether Bay Jam" → "Azerbaijan",
   "open auth dot rs" → "auth.rs"). Repair only: never add, drop, or answer.
3. **Classification** — prompt for the assistant vs. UI command (+ command args).

### 4.3 Verdict schema

The model must return *only* a JSON object:

```json
{
  "status": "incomplete" | "prompt" | "command",
  "text": "<repaired reconstruction of the utterance>",
  "command": { "action": "<action>", "target": "<search string|null>" }
}
```

- `status: "incomplete"` → return **only** `{"status":"incomplete"}` — see §4.4.
- `status: "prompt"` → include `text` (repaired). No `command`.
- `status: "command"` → include `text` and `command`.

Rust side: `enum Verdict { Incomplete, Prompt { text }, Command { action, target, text } }`.

### 4.4 Latency — protecting the continuance path

The `incomplete` verdict re-fires on every pause, so it is latency-critical.
Verdicts are deliberately **asymmetric**:

- **Incomplete** → model returns only `{"status":"incomplete"}`. Minimal output
  tokens = fastest possible generation. No repair work spent on a fragment.
- **Prompt / command** → the *terminal* call does the full repair into `text`,
  once, on the complete utterance (also where repair is most accurate).

Other latency tactics:

- Persistent `reqwest::Client` (HTTP keep-alive — no per-call TLS handshake).
- `temperature: 0`, no thinking, `response_format: { type: "json_object" }`.
- Compact, static system prompt (cacheable on the Zen side).
- Small `max_tokens` (~120).

### 4.5 The re-listen loop

On `Incomplete`, the reconciler buffer is **not** reset (vs. today's
unconditional `reset()`). The next ASR final appends to the same buffer — the
reconciler already merges consecutive `User` turns (`reconciler.rs:84`). The
next pause re-runs `on_speech_end` on the fuller text. Repair happens once, on
the terminal call, over the whole buffer.

Two safety nets prevent an infinite gather:

- **Silence fallback** — after an `Incomplete` verdict, arm a timer
  (`gate_silence_fallback_ms`, default 5000). If no new speech arrives, submit
  the buffer as-is.
- **Re-check cap** — after `gate_max_rechecks` consecutive `Incomplete`
  verdicts (default 3), submit anyway.

### 4.6 State machine

`Idle → User → Thinking` is unchanged except that `on_speech_end` now calls the
gate. On `Incomplete` it returns to `Idle` **without resetting the reconciler**
— a non-empty reconciler buffer *is* the "gathering" state. A widget signal
distinguishes "gathering / go on…" from cold idle (see §10).

### 4.7 Gate system prompt (sketch)

Final wording is tuned during implementation; the contract is:

```
You are a fast gate between speech-to-text and a coding assistant.
Input: a raw, possibly garbled ASR transcript of something a user said aloud.
Output ONLY a JSON object — no prose, no code fences.

status:
  "incomplete" — a sentence fragment, trailing off, clearly mid-thought.
                 Return ONLY {"status":"incomplete"}.
  "command"    — purely a request to navigate the app's UI, with NO task for
                 the assistant. Carries {action, target}.
  "prompt"     — anything else: a question or task for the assistant.
                 This is the default; when unsure, choose "prompt".

For "prompt" and "command", also return `text`: the utterance with obvious
speech-to-text errors repaired (phonetic garbles, wrong word splits). Repair
ONLY — never add, drop, reorder meaning, or answer. e.g. "Aether Bay Jam"
-> "Azerbaijan".

Rule: "open auth.rs" is a command. "open auth.rs and explain it" is a prompt
— it asks the assistant to do work.

<adapter command vocabulary injected here>
```

The command vocabulary block is supplied by the active adapter (§7) — the gate
engine is core, but command-aware per site.

### 4.8 Fail-open

Any failure — network error, timeout (`gate_timeout_ms`, default 4000),
malformed JSON, unknown `status` — makes `classify` return
`Prompt { text: <raw ASR> }` and log the failure. A flaky LLM must never
swallow the user's words; worst case it submits un-repaired text, i.e. exactly
today's behavior. The `--no-gate` flag disables the gate entirely (debugging,
or a Zen outage), reverting to immediate submit.

---

## 5. Core module: the gate client

New module `src/gate.rs`:

- `struct Gate` holds a persistent `reqwest::Client` and config (endpoint, key,
  model, timeout).
- `async fn classify(&self, text: &str) -> Verdict`.
- POSTs an OpenAI-format chat completion to the Zen endpoint, parses
  `choices[0].message.content` as JSON into `Verdict`, applies fail-open (§4.8).
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
  rather than using fixed delays; on timeout the recipe aborts cleanly (§8.3).

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
  vocabulary string fed into the gate's system prompt (§4.7). Recipe steps are
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
move into the adapter's `contract.json` (§7). A drift fix becomes a one-line
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

New config (`config.rs` / `Cli` / `.env`):

| key | source | default |
|---|---|---|
| `OPENCODE_ZEN_API_KEY` | env | — (required for the gate) |
| `gate_endpoint` | default | `https://opencode.ai/zen/v1/chat/completions` |
| `gate_model` | default | `DeepSeek-V4-Flash-EL` — verify exact API id via Zen `GET /v1/models` |
| `gate_timeout_ms` | default | 4000 |
| `gate_silence_fallback_ms` | default | 5000 |
| `gate_max_rechecks` | default | 3 |
| `--no-gate` | CLI flag | gate disabled (immediate submit) |

The Zen endpoint is OpenAI-compatible; auth is `Authorization: Bearer
<OPENCODE_ZEN_API_KEY>`. The key is an OpenCode account API key (the user's
"Go" plan includes DeepSeek V4 Flash).

---

## 10. Widget UX

The widget gains, beyond today's mic button / state label / transcript:

- **Gate states** in the state label: `checking…` (gate call in flight, brief)
  → `go on…` (incomplete — keep listening, distinct from cold idle) →
  `thinking…` (prompt submitted) .
- **Repaired-text flash** — the repaired `text` is shown for ~1 s before
  submit, so a bad repair is at least visible.
- **Command feedback** — a small text line under the mic, shown on hover:
  `→ opened auth.rs` or `✗ no session matched 'refactor'`. (No toast; no TTS
  for commands — a Cartesia round-trip for "opening file" is not worth the
  latency.)
- **Health dot** — green/red, driven by the Tier A self-test (§8.2).

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
  contract-check.sh   Tier B entry point
tests/ or scripts/contract-check/   Playwright suite
```

Widget JS moves from giant string literals in `web.rs` into real `.js`/`.html`
files, embedded via `include_str!`. This is a prerequisite for adapters and a
standalone improvement.

---

## 12. Error handling summary

| Failure | Behaviour |
|---|---|
| Gate network error / timeout / bad JSON | Fail-open: treat as `Prompt { raw text }`, log (§4.8) |
| Gate stuck returning `incomplete` | Silence fallback + re-check cap force a submit (§4.5) |
| Recipe step element never appears | `pollFor` times out (~1.5 s), recipe aborts, hover line + server log (§8.3) |
| Contract selector unresolved | Resolver fails over to fallbacks; if none, named failure (§8.3) |
| Unknown command `action` | Recipe dispatcher rejects it, hover line shows error |
| Zen unreachable at startup | `--no-gate` path; gate disabled, immediate submit |
| Response strategy emits unexpected shape | Logged loudly; no TTS for that turn (no crash) |

---

## 13. Testing

- **Unit:** verdict JSON parsing (all three statuses + malformed → fail-open);
  reconciler non-reset on `incomplete`; recipe dispatcher action→steps mapping;
  contract resolver primary/fallback logic.
- **Integration:** gate client against a mock OpenAI-compatible endpoint.
- **Prompt regression:** a small fixture set of transcripts → expected verdicts,
  run against the real model, to catch gate-prompt drift.
- **Tier A:** the in-page self-test is itself the continuous check.
- **Tier B:** the Playwright `contract-check` suite (§8.2).

---

## 14. Open questions / future work

- Exact Zen API model id for DeepSeek V4 Flash — confirm via `GET
  https://opencode.ai/zen/v1/models` at implementation time (display name is
  `DeepSeek-V4-Flash-EL`).
- Anaphora resolution ("open *that* file") — needs conversation context;
  deferred.
- Named model-variant selection — deferred (cycle-only in v1).
- Sub-project 2 (adapter generator) and sub-project 3 (more adapters) — own
  specs.
- Tier B on a schedule rather than on-demand — revisit if drift is frequent.
