# Gate evaluation

Latency + accuracy benchmark for the interjections gate (the fast LLM that
classifies each ASR utterance as incomplete / prompt / command / backchannel).

## Files

| File | What it is | How to extend |
|---|---|---|
| `gate_evals.json` | The evaluation dataset — utterances + expected verdicts | **Append cases here** (see below) |
| `gate_prompt.txt` | The gate system prompt under test | Edit to tune the gate |
| `bench.py` | The harness — runs the dataset against models, measures latency + correctness | Add models to the `CONFIGS` list |
| `results.csv` | Rollup log — one row per model per run, appended forever | The analysis log; never edited by hand |
| `results-<timestamp>.json` | Full per-run detail, including which cases each model missed | Disposable history |

## Adding test cases

`gate_evals.json` is a plain JSON array under `"cases"`. To add an example,
append one object — nothing else needs to change:

```json
{
  "id": "unique-short-id",
  "category": "preface|fragment|trailoff|prompt|command|repair|correction|interjection|backchannel|edge",
  "context": "idle | responding",
  "utterance": "what the user said out loud",
  "expect": {
    "status": "incomplete | prompt | command | backchannel",
    "hold": true,
    "action": "open_file",
    "target_contains": "auth.rs",
    "text_contains": "Azerbaijan"
  },
  "note": "why this case matters"
}
```

`expect` only needs the fields relevant to the case:
- `status` — always; checked strictly.
- `hold` — for `incomplete`; checked **softly** (mismatch reported, not failed).
- `action` — for `command`; checked strictly.
- `target_contains` / `text_contains` — case-insensitive substring checks.

`category` and `context` are documentation only — not checked.

## Running

```bash
python3 eval/bench.py                 # all configured models, default run counts
python3 eval/bench.py only=cerebras   # only configs whose label/host matches
python3 eval/bench.py runs=5 pace=3   # 5 runs per case, 3s between calls (rate limits)
```

Keys are read from `../.env`. A model whose `key_env` is absent is skipped, so
provider configs can sit dormant until a key is added.

Each run prints a ranking, writes a detail JSON, and appends to `results.csv`.
`results.csv` is the thing to build on — load it in anything to compare models
over time.
