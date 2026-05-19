# Local gate inference — setup & tuning

Notes for running the gate model locally on `primary`, and the knobs that
affect latency. Goal: see how low we can push gate latency without a network
hop.

## Hardware

- GPU: **NVIDIA RTX 3060 Ti**, 8 GB VRAM (~6.7 GB free), compute capability 8.6
- CUDA driver 580.142 / CUDA 13.0; 12-core CPU, 76 GB RAM

A 4B model at Q3–Q4 (~2.3–3.4 GB) fits entirely in VRAM with room for the KV
cache — so it runs 100% on GPU. An 8B at Q4 (~5 GB) also fits. Anything ~14B+
spills to CPU and is unusably slow for a gate.

## Two local runtimes

### 1. ollama (already installed)

ollama wraps llama.cpp. Easiest path, least control.

```bash
ollama pull qwen3.5:4b                 # Q4_K_M, 4.7B
# OpenAI-compatible endpoint: http://localhost:11434/v1/chat/completions
```

- **No-think switch:** `reasoning_effort: "none"` in the request body. Confirmed
  to disable Qwen3.5 thinking (0 reasoning chars). `/no_think` and
  `enable_thinking:false` do **not** work via ollama's OpenAI endpoint.
- Limitation: ollama's bundled llama.cpp can lag latest GGUF support — the
  Unsloth `Q3_K_M` GGUF failed to load here (HTTP 500). That alone is a reason
  to use llama.cpp directly.

### 2. llama.cpp (built from source)

Latest engine, full flag control, loads any current GGUF.

```bash
cd ~
git clone --depth 1 https://github.com/ggml-org/llama.cpp
cmake -S llama.cpp -B llama.cpp/build \
      -DGGML_CUDA=ON -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_CUDA_ARCHITECTURES=86       # 86 = RTX 3060 Ti; single arch = fast compile
cmake --build llama.cpp/build -j --target llama-server llama-bench
```

Server (OpenAI-compatible, `http://localhost:8080/v1/chat/completions`):

```bash
./llama.cpp/build/bin/llama-server -m ~/models/qwen3.5-4b-Q4_K_M.gguf \
    --host 127.0.0.1 --port 8080 <tuning flags>
```

## Model files

GGUFs copied out of ollama's blob store to `~/models/`:

| file | quant | size | source |
|---|---|---|---|
| `qwen3.5-4b-Q4_K_M.gguf` | Q4_K_M | 3.39 GB | ollama `qwen3.5:4b` |
| `qwen3.5-4b-Q3_K_M.gguf` | Q3_K_M | 2.29 GB | ollama `hf.co/unsloth/Qwen3.5-4B-GGUF:Q3_K_M` |

## llama-server flags — latency reference

What each flag does and whether it should help *our* workload (a fixed ~625-token
system prompt, a short utterance, a ~40-token JSON reply, one user at a time).

| flag | what it does | effect on the gate |
|---|---|---|
| `-ngl, --gpu-layers 99` | offload all layers to GPU | **essential** — CPU layers = 10× slower |
| `-c, --ctx-size N` | KV context window | small is good; `1024` covers prompt+reply, frees VRAM |
| `-fa, --flash-attn on` | fused flash-attention kernel | faster attention + less VRAM on Ampere — expect a win |
| `-ctk / -ctv q8_0` | quantize K/V cache | less memory bandwidth per token; needs `-fa`; tiny quality cost |
| `-ub, --ubatch-size N` | physical micro-batch (prompt) | larger = faster processing of the 625-token prompt |
| `-b, --batch-size N` | logical batch | pairs with `-ub` |
| **prompt-prefix reuse** | llama-server keeps per-slot KV and reuses a matching prefix automatically | **biggest lever** — the system prompt is identical every call, so after call 1 it is not reprocessed → much lower TTFT |
| `--cache-reuse N` | reuse non-contiguous cached chunks | extends prefix reuse |
| `-np, --parallel N` | parallel request slots | keep at `1` for a single-user gate (more slots split the KV) |
| `--reasoning-budget 0` | hard-disable thinking | the llama.cpp no-think switch (with `--jinja`) |
| `--jinja` | use the model's chat template | needed for correct Qwen3.5 formatting + reasoning control |
| `-t, --threads N` | CPU threads | minor when fully GPU-resident |
| `--mlock` / `--no-mmap` | RAM residency | irrelevant when fully on GPU |
| `-md, --model-draft` + `--draft-*` | speculative decoding with a tiny draft model | could speed generation; **future** — needs a draft model |

## Flag variants to benchmark

Run each as a separate `llama-server` launch; `bench.py` hits port 8080. The
label encodes the variant so `results.csv` keeps them distinct.

| variant | flags added (cumulative) | hypothesis |
|---|---|---|
| V1 baseline | `-ngl 99 -c 2048 --jinja --reasoning-budget 0` | reference |
| V2 +flash-attn | `-fa on` | faster attention |
| V3 +kv-quant | `-ctk q8_0 -ctv q8_0` | less KV bandwidth |
| V4 +tight ctx/batch | `-c 1024 -b 2048 -ub 512` | cheaper prompt pass |
| V5 Q3 model | V4 flags, `qwen3.5-4b-Q3_K_M.gguf` | smaller weights = faster tokens |

`bench.py` config fields `model` + `extra` record what was run; this table is
the human description of the launch flags behind each.

Run all five in one shot: `bash eval/bench_llamacpp.sh` (launches each variant,
benchmarks, kills the server; appends rows to `results.csv`).

## Results (2026-05-18)

**GGUF compatibility:** older GGUFs (ollama `qwen3.5:4b`, stale Unsloth) fail
master llama.cpp — `qwen35.rope.dimension_sections expected 4, got 3`. A fresh
Unsloth GGUF (`unsloth/Qwen3.5-4B-MTP-GGUF`, May 2026) loads cleanly:
`~/models/qwen3.5-4b-unsloth-mtp-Q4_K_M.gguf`.

**No-think:** the only switch that works for Qwen3.5 here is request-body
`chat_template_kwargs: {"enable_thinking": false}`. `--reasoning-budget 0`,
`reasoning_effort`, and top-level `enable_thinking` do **not** disable thinking.

**Flag variants** (88-case eval, qwen3.5-4b, no-think):

| variant | median | p95 | acc |
|---|---|---|---|
| V3 (`-fa` + KV-q8) | 339 ms | 452 ms | 85% |
| V4 (+ tight ctx/batch) | 339 ms | 451 ms | 85% |

`-fa`, KV-quant, and batch tuning made **no measurable difference** — the model
is GPU-bound; latency is TTFT (~228 ms) + short generation. V1/V2 failed
intermittently (the driver doesn't reliably free VRAM between variants — the
2 s settle is too short).

**MTP** (`--spec-type draft-mtp`): supported, head discovered — but **OOMs on
the 8 GB RTX 3060 Ti**; MTP needs extra VRAM. Would need a smaller quant to fit.

**Bottom line:** llama.cpp-direct (~339 ms / 85%) ties ollama (~341 ms / 84%) —
same engine, flags don't move it. Local qwen3.5-4b is a viable offline fallback;
the gate model remains Cerebras `gpt-oss-120b` (139 ms / 93%).
