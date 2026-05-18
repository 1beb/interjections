#!/usr/bin/env python3
"""
Gate model benchmark for interjections.

Runs the gate prompt (eval/gate_prompt.txt) over the eval set
(eval/gate_evals.json) against a list of model/provider configs, measuring
latency (TTFT + total) and correctness. Prints a ranking and writes a results
JSON.

Keys are read from ../.env. A config whose key_env is absent is skipped, so
provider configs can sit dormant until a key is added.

Usage:  python3 eval/bench.py [runs]      # runs overrides per-config default
"""
import json, time, http.client, statistics, os, sys, datetime, re, csv

ROOT = os.path.dirname(os.path.abspath(__file__))


def load_env(path):
    env = {}
    if os.path.exists(path):
        for line in open(path):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip()
    return env


ENV = load_env(os.path.join(ROOT, "..", ".env"))
SYSTEM = open(os.path.join(ROOT, "gate_prompt.txt")).read()
CASES = json.load(open(os.path.join(ROOT, "gate_evals.json")))["cases"]

# Args:  runs=N   override per-config run count
#        only=STR run only configs whose label/host contains STR
RUNS_OVERRIDE = None
ONLY = None
PACE = 0.0
LABEL = None
for _a in sys.argv[1:]:
    if _a.startswith("runs="):
        RUNS_OVERRIDE = int(_a.split("=", 1)[1])
    elif _a.startswith("only="):
        ONLY = _a.split("=", 1)[1].lower()
    elif _a.startswith("pace="):
        PACE = float(_a.split("=", 1)[1])
    elif _a.startswith("label="):
        LABEL = _a.split("=", 1)[1]  # override label of matched configs (flag variants)

# Each config: a model reachable over an OpenAI-compatible /chat/completions
# endpoint. `extra` is merged into the request body. Skipped if key_env missing.
CONFIGS = [
    {"label": "deepseek-v4-flash no-think", "host": "opencode.ai",
     "path": "/zen/go/v1/chat/completions", "key_env": "OPENCODE_GO_API",
     "model": "deepseek-v4-flash", "extra": {"thinking": {"type": "disabled"}},
     "runs": 3},
    {"label": "deepseek-v4-flash THINKING", "host": "opencode.ai",
     "path": "/zen/go/v1/chat/completions", "key_env": "OPENCODE_GO_API",
     "model": "deepseek-v4-flash", "extra": {}, "runs": 1},
    {"label": "qwen3.6-plus", "host": "opencode.ai",
     "path": "/zen/go/v1/chat/completions", "key_env": "OPENCODE_GO_API",
     "model": "qwen3.6-plus", "extra": {}, "runs": 3},
    {"label": "glm-5", "host": "opencode.ai",
     "path": "/zen/go/v1/chat/completions", "key_env": "OPENCODE_GO_API",
     "model": "glm-5", "extra": {}, "runs": 3},
    {"label": "minimax-m2.5", "host": "opencode.ai",
     "path": "/zen/go/v1/chat/completions", "key_env": "OPENCODE_GO_API",
     "model": "minimax-m2.5", "extra": {}, "runs": 3},
    {"label": "cerebras llama3.1-8b", "host": "api.cerebras.ai",
     "path": "/v1/chat/completions", "key_env": "CEREBRAS",
     "model": "llama3.1-8b", "extra": {}, "runs": 3},
    {"label": "cerebras qwen-3-235b-instruct", "host": "api.cerebras.ai",
     "path": "/v1/chat/completions", "key_env": "CEREBRAS",
     "model": "qwen-3-235b-a22b-instruct-2507", "extra": {}, "runs": 3},
    {"label": "cerebras gpt-oss-120b", "host": "api.cerebras.ai",
     "path": "/v1/chat/completions", "key_env": "CEREBRAS",
     "model": "gpt-oss-120b", "extra": {"reasoning_effort": "low"}, "runs": 3},
    {"label": "cerebras zai-glm-4.7", "host": "api.cerebras.ai",
     "path": "/v1/chat/completions", "key_env": "CEREBRAS",
     "model": "zai-glm-4.7", "extra": {}, "runs": 3},
    {"label": "local qwen3.5:4b ollama", "host": "localhost", "port": 11434,
     "scheme": "http", "key_env": None, "path": "/v1/chat/completions",
     "model": "qwen3.5:4b", "extra": {"reasoning_effort": "none"}, "runs": 3},
    {"label": "local qwen3.5:4b Q3_K_M unsloth", "host": "localhost", "port": 11434,
     "scheme": "http", "key_env": None, "path": "/v1/chat/completions",
     "model": "hf.co/unsloth/Qwen3.5-4B-GGUF:Q3_K_M",
     "extra": {"reasoning_effort": "none"}, "runs": 3},
    {"label": "llamacpp qwen3.5-4b", "host": "localhost", "port": 8091,
     "scheme": "http", "key_env": None, "path": "/v1/chat/completions",
     "model": "qwen3.5-4b", "extra": {}, "runs": 3},
    # --- dormant until a key is added (model ids verified at that time) ---
    {"label": "groq llama-3.3-70b", "host": "api.groq.com",
     "path": "/openai/v1/chat/completions", "key_env": "GROQ_API_KEY",
     "model": "llama-3.3-70b-versatile", "extra": {}, "runs": 3},
    {"label": "gemini-3-flash-lite", "host": "generativelanguage.googleapis.com",
     "path": "/v1beta/openai/chat/completions", "key_env": "GEMINI_API_KEY",
     "model": "gemini-3-flash-lite", "extra": {}, "runs": 3},
]

_conns = {}


def _conn_key(cfg):
    return (cfg["host"], cfg.get("port"), cfg.get("scheme", "https"))


def conn_for(cfg):
    key = _conn_key(cfg)
    if key not in _conns:
        if cfg.get("scheme", "https") == "http":
            _conns[key] = http.client.HTTPConnection(cfg["host"], cfg.get("port"), timeout=120)
        else:
            _conns[key] = http.client.HTTPSConnection(cfg["host"], cfg.get("port"), timeout=120)
    return _conns[key]


def reset_conn(cfg):
    key = _conn_key(cfg)
    try:
        _conns[key].close()
    except Exception:
        pass
    _conns.pop(key, None)


def _one_call(cfg, utterance):
    """Returns dict: status, ttft_ms, total_ms, content, reasoning_chars, err."""
    body = {
        "model": cfg["model"],
        "messages": [{"role": "system", "content": SYSTEM},
                     {"role": "user", "content": utterance}],
        "temperature": 0, "max_tokens": 1024, "stream": True,
    }
    body.update(cfg.get("extra", {}))
    hdr = {"Content-Type": "application/json"}
    if cfg.get("key_env") and cfg["key_env"] in ENV:
        hdr["Authorization"] = "Bearer " + ENV[cfg["key_env"]]
    t0 = time.perf_counter()
    try:
        c = conn_for(cfg)
        c.request("POST", cfg["path"], json.dumps(body), hdr)
        r = c.getresponse()
        if r.status != 200:
            err = r.read()[:240].decode("utf-8", "replace")
            return {"err": f"HTTP {r.status}: {err}"}
        ttft, buf = None, b""
        while True:
            chunk = r.read(256)
            if not chunk:
                break
            if ttft is None:
                ttft = (time.perf_counter() - t0) * 1000.0
            buf += chunk
        total = (time.perf_counter() - t0) * 1000.0
    except Exception as e:
        reset_conn(cfg)
        return {"err": f"exception: {e}"}
    content, reasoning = "", ""
    for line in buf.split(b"\n"):
        line = line.strip()
        if not line.startswith(b"data:"):
            continue
        p = line[5:].strip()
        if p == b"[DONE]":
            continue
        try:
            d = json.loads(p)["choices"][0]["delta"]
            content += d.get("content") or ""
            reasoning += d.get("reasoning_content") or d.get("reasoning") or ""
        except Exception:
            pass
    return {"ttft_ms": ttft, "total_ms": total, "content": content.strip(),
            "reasoning_chars": len(reasoning), "err": None}


def call(cfg, utterance):
    """Paced, 429-retrying wrapper around _one_call. The timed measurement is
    the successful attempt only — retries do not pollute latency stats."""
    if PACE:
        time.sleep(PACE)
    for attempt in range(6):
        res = _one_call(cfg, utterance)
        if "HTTP 429" in (res.get("err") or ""):
            reset_conn(cfg)
            time.sleep(10 + attempt * 6)
            continue
        return res
    return {"err": "HTTP 429: rate-limited after retries"}


def parse_verdict(content):
    if not content:
        return None
    m = re.search(r"\{.*\}", content, re.DOTALL)
    if not m:
        return None
    try:
        return json.loads(m.group(0))
    except Exception:
        return None


def check(expect, verdict):
    """Returns (ok, hold_ok_or_None, detail)."""
    if not verdict:
        return False, None, "no JSON"
    st = verdict.get("status")
    if st != expect["status"]:
        return False, None, f"status={st}"
    if st == "incomplete":
        hold_ok = verdict.get("hold") == expect.get("hold")
        return True, hold_ok, "ok" if hold_ok else f"hold={verdict.get('hold')}"
    if st == "command":
        cmd = verdict.get("command") or {}
        if expect.get("action") and cmd.get("action") != expect["action"]:
            return False, None, f"action={cmd.get('action')}"
        tc = expect.get("target_contains")
        if tc and tc.lower() not in str(cmd.get("target", "")).lower():
            return False, None, f"target={cmd.get('target')!r}"
        return True, None, "ok"
    if st == "prompt":
        tc = expect.get("text_contains")
        if tc and tc.lower() not in str(verdict.get("text", "")).lower():
            return False, None, f"text missing {tc!r}"
        return True, None, "ok"
    return True, None, "ok"


def pct(values, p):
    if not values:
        return 0.0
    s = sorted(values)
    k = min(len(s) - 1, int(round((p / 100.0) * (len(s) - 1))))
    return s[k]


results = []
print(f"gate benchmark | {len(CASES)} cases | {datetime.datetime.now():%Y-%m-%d %H:%M}\n")

for cfg in CONFIGS:
    if ONLY and ONLY not in (cfg["label"] + " " + cfg["host"]).lower():
        continue
    if LABEL:
        cfg = {**cfg, "label": LABEL}
    if cfg.get("key_env") and cfg["key_env"] not in ENV:
        print(f"SKIP  {cfg['label']:30s}  (no {cfg['key_env']} in .env)")
        continue
    runs = RUNS_OVERRIDE or cfg["runs"]
    print(f"\n=== {cfg['label']}  (model={cfg['model']}, runs={runs}) ===")
    for _ in range(3):       # discard - warm connection + model load; cold
        call(cfg, "warmup")  # first calls never count toward the stats
    totals, ttfts, reason = [], [], []
    correct = hold_hits = hold_total = run_count = 0
    fails = []
    for case in CASES:
        for _ in range(runs):
            res = call(cfg, case["utterance"])
            if res.get("err"):
                fails.append(f"{case['id']}: {res['err'][:80]}")
                continue
            run_count += 1
            totals.append(res["total_ms"])
            if res["ttft_ms"] is not None:
                ttfts.append(res["ttft_ms"])
            reason.append(res["reasoning_chars"])
            verdict = parse_verdict(res["content"])
            ok, hold_ok, detail = check(case["expect"], verdict)
            if ok:
                correct += 1
            else:
                fails.append(f"{case['id']}: {detail}")
            if hold_ok is not None:
                hold_total += 1
                hold_hits += 1 if hold_ok else 0
    summary = {
        "label": cfg["label"], "model": cfg["model"], "runs": runs,
        "n": run_count,
        "total_min": round(min(totals)) if totals else None,
        "total_median": round(statistics.median(totals)) if totals else None,
        "total_p95": round(pct(totals, 95)) if totals else None,
        "total_max": round(max(totals)) if totals else None,
        "ttft_median": round(statistics.median(ttfts)) if ttfts else None,
        "reasoning_median": round(statistics.median(reason)) if reason else None,
        "accuracy": round(correct / run_count, 3) if run_count else 0.0,
        "hold_accuracy": round(hold_hits / hold_total, 3) if hold_total else None,
        "fails": fails,
    }
    results.append(summary)
    if totals:
        print(f"  latency total:  min {summary['total_min']}  "
              f"median {summary['total_median']}  p95 {summary['total_p95']}  "
              f"max {summary['total_max']} ms")
        print(f"  ttft median {summary['ttft_median']} ms | "
              f"reasoning median {summary['reasoning_median']} chars")
        print(f"  accuracy {correct}/{run_count} = {summary['accuracy']:.0%} | "
              f"hold {hold_hits}/{hold_total}")
        if fails:
            seen = []
            for f in fails:
                if f not in seen:
                    seen.append(f)
            print("  misses: " + "; ".join(seen[:8]))
    else:
        print("  no successful calls; " + "; ".join(fails[:3]))

print("\n\n=== RANKING (by median total latency) ===")
ranked = sorted([r for r in results if r["total_median"] is not None],
                key=lambda r: r["total_median"])
print(f"{'model':30s} {'median':>8s} {'p95':>8s} {'ttft':>7s} {'acc':>6s}")
for r in ranked:
    print(f"{r['label']:30s} {r['total_median']:7d}m {r['total_p95']:7d}m "
          f"{r['ttft_median']:6d}m {r['accuracy']:6.0%}")

# full per-run detail (incl. per-case misses) -> timestamped JSON
out = os.path.join(ROOT, f"results-{datetime.datetime.now():%Y%m%d-%H%M%S}.json")
json.dump({"generated": datetime.datetime.now().isoformat(),
           "cases": len(CASES), "results": results}, open(out, "w"), indent=2)
print(f"\nfull results -> {out}")

# flat rollup, one row per model, appended forever -> results.csv (the analysis log)
csv_path = os.path.join(ROOT, "results.csv")
COLS = ["date", "model", "label", "cases", "runs", "n", "total_min_ms",
        "total_median_ms", "total_p95_ms", "total_max_ms", "ttft_median_ms",
        "reasoning_median_chars", "accuracy", "hold_accuracy"]
new_file = not os.path.exists(csv_path)
with open(csv_path, "a", newline="") as fh:
    w = csv.writer(fh)
    if new_file:
        w.writerow(COLS)
    today = datetime.date.today().isoformat()
    for r in results:
        w.writerow([today, r["model"], r["label"], len(CASES), r["runs"], r["n"],
                    r["total_min"], r["total_median"], r["total_p95"], r["total_max"],
                    r["ttft_median"], r["reasoning_median"], r["accuracy"],
                    r["hold_accuracy"]])
print(f"rollup appended -> {csv_path}")

for c in _conns.values():
    try:
        c.close()
    except Exception:
        pass
