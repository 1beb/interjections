# Architecture Decision Records

This directory captures *why* the major software choices in interjections are
what they are. Each ADR is a short, dated record covering the context, the
decision, its consequences, and the alternatives that lost.

Format: lightweight Nygard-style, one decision per file. See
[`0000-template.md`](0000-template.md).

## Conventions

- **One decision per file.** Don't bundle. If two choices are entangled, write
  one ADR and link to it from the other.
- **Status is the contract.** Read it first.
  - *Accepted* — load-bearing today.
  - *Under review* — still in use but actively being questioned.
  - *Transitional* — placeholder choice; a replacement is planned.
  - *Superseded by ADR-XXXX* — historical; do not act on it.
  - *Deprecated* — was never accepted, or was abandoned without a replacement.
- **ADRs are immutable in spirit.** Edit them to clarify, fix typos, or change
  status; don't rewrite history. To overturn a decision, write a new ADR that
  supersedes the old one.
- **Don't put implementation details here.** ADRs answer *why*. Code and
  `docs/plan.md` answer *what* and *how*.

## Index

| ID | Title | Status |
|---|---|---|
| [0001](0001-rust-tokio-runtime.md) | Rust + Tokio runtime | Accepted |
| [0002](0002-reverse-proxy-transport.md) | Reverse-proxy transport in front of opencode web | Accepted |
| [0003](0003-sherpa-onnx-zipformer-asr.md) | Sherpa-onnx streaming Zipformer for ASR | Under review |
| [0004](0004-cartesia-tts-transitional.md) | Cartesia Sonic-3 TTS (transitional) | Transitional |
| [0005](0005-qwen3.5-4b-gate-llm.md) | qwen3.5:4b via Ollama for the gate | Accepted |
| [0006](0006-local-tts-replacement.md) | Local TTS replacement (Pocket TTS) | Accepted (implemented) |
| [0007](0007-asr-replacement.md) | ASR replacement / upgrade | Open |
