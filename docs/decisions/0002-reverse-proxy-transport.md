# ADR-0002: Reverse-proxy transport in front of opencode web

- **Status:** Accepted
- **Date:** 2026-05-20 (backfilled)
- **Related:** ADR-0001

## Context

The product needs to add a voice layer to OpenCode's existing web UI without
forking it or shipping a browser extension. The integration has to:

- inject a voice widget into the page reliably;
- carry a bidirectional WebSocket for mic audio in and TTS audio out;
- proxy normal HTTP/SSE traffic untouched so OpenCode keeps working;
- run from a single binary the user controls;
- not require the user to install anything in their browser.

OpenCode upstream is `opencode web` on `http://127.0.0.1:4096`. The voice layer
needs to interpose itself between the browser and that origin.

## Decision

Run interjections as a **TLS reverse proxy** that fronts `opencode web` on a
dedicated port (default `0.0.0.0:8765`, HTTPS), rewrites HTML responses to
inject the voice widget, terminates a separate WebSocket for the voice channel,
and passes all other traffic through. See `src/web.rs`.

## Consequences

- Zero-touch from OpenCode's side; upgrading OpenCode does not require changes
  here unless the prompt-editor DOM changes (see the design doc, sections 1.1
  and 6).
- One process owns the whole pipeline — audio in, ASR, gate, injection, TTS
  out, SSE listener — which keeps state machine handoff cheap (in-process
  channels, not network hops).
- TLS is mandatory because browsers require a secure context for `getUserMedia`.
  This forces self-signed-cert management or a real cert, which is a UX wart
  the user trips over once.
- The HTML-rewrite injection is fragile to OpenCode markup changes; the design
  doc's "Tier A drift check" exists precisely to detect this.

## Alternatives considered

- **Browser extension** — avoids the proxy entirely, but means an install step,
  per-browser packaging, and store-review overhead. Wrong fit for a tool
  meant to be one binary you run.
- **Fork OpenCode and merge voice in directly** — tight coupling, ongoing
  rebase pain, kills the "works against any OpenCode build" property.
- **Sidecar daemon + JS bookmarklet/userscript** — works, but pushes setup
  complexity onto the user (`getUserMedia` over HTTPS still needed; the
  bookmarklet has to be re-injected per page load).
