#!/usr/bin/env python3
"""
Per-category accuracy breakdown from benchmark result files.

Reads gate_evals.json (for each case's category) and every results-*.json in
eval/, then prints, per model, the accuracy within each scenario category -
so you can see *which* kinds of utterance a model gets wrong.

Usage:  python3 eval/analyze.py [model-substring ...]
        (no args = all models; args filter to matching model labels)
"""
import json, glob, os, sys

ROOT = os.path.dirname(os.path.abspath(__file__))
CASES = json.load(open(os.path.join(ROOT, "gate_evals.json")))["cases"]

CAT_OF = {c["id"]: c["category"] for c in CASES}
CATS = []
for c in CASES:                       # preserve first-seen order
    if c["category"] not in CATS:
        CATS.append(c["category"])
CAT_SIZE = {cat: sum(1 for c in CASES if c["category"] == cat) for cat in CATS}

filters = [a.lower() for a in sys.argv[1:]]

rows = []
for path in sorted(glob.glob(os.path.join(ROOT, "results-*.json"))):
    data = json.load(open(path))
    for r in data.get("results", []):
        label = r["label"]
        if filters and not any(f in label.lower() for f in filters):
            continue
        runs = r.get("runs", 1)
        # fails entries look like "<case-id>: <detail>"; count per category
        fail_by_cat = {cat: 0 for cat in CATS}
        for f in r.get("fails", []):
            cid = f.split(":", 1)[0].strip()
            cat = CAT_OF.get(cid)
            if cat:
                fail_by_cat[cat] += 1
        acc = {}
        for cat in CATS:
            total = runs * CAT_SIZE[cat]
            acc[cat] = (total - fail_by_cat[cat]) / total if total else None
        rows.append((label, r.get("accuracy"), acc))

if not rows:
    print("no matching results")
    sys.exit()

ABBR = {"preface": "pref", "fragment": "frag", "trailoff": "trail",
        "prompt": "prmpt", "command": "cmd", "repair": "rep",
        "correction": "corr", "interjection": "intj", "backchannel": "bchan",
        "edge": "edge"}

hdr = f"{'model':<30} {'OVERALL':>8}  " + " ".join(f"{ABBR.get(c,c):>5}" for c in CATS)
print(hdr)
print("-" * len(hdr))
for label, overall, acc in rows:
    cells = []
    for c in CATS:
        v = acc[c]
        cells.append("  -  " if v is None else f"{v*100:4.0f}%")
    ov = f"{overall*100:6.0f}%" if overall is not None else "   -  "
    print(f"{label:<30} {ov:>8}  " + " ".join(cells))

print("\ncategory sizes:", ", ".join(f"{ABBR[c]}={CAT_SIZE[c]}" for c in CATS))
