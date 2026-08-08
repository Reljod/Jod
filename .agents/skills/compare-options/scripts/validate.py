#!/usr/bin/env python3
"""Validate a study's dataset before anything scores it.

This is the study's runnable check. A mistyped rating (`ip_rep: 6` on a 1-5
scale) silently distorts every downstream ranking, so the pipeline refuses to
proceed until the data is well-formed. Exits non-zero on any error.

The rules come from the dataset's own `_schema` block, so this stays generic:

    "_schema": {
      "required": ["id", "name", "price", "confidence", "sources"],
      "ratings_1_5": ["reliability", "support"],
      "enums": {"currency": ["USD", "EUR"], "tier": ["a", "b"]},
      "non_negative": ["price", "count"]
    }

Usage:
    validate.py <study-dir>
"""

import argparse
import json
import os
import sys

VALID_CONFIDENCE = {"high", "medium", "low"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("study")
    args = ap.parse_args()

    data_path = os.path.join(args.study, "data", "dataset.json")
    with open(data_path) as fh:
        dataset = json.load(fh)

    candidates = dataset.get("candidates", [])
    schema = dataset.get("_schema", {})

    required = schema.get("required", ["id", "confidence", "sources"])
    ratings = schema.get("ratings_1_5", [])
    enums = schema.get("enums", {})
    non_negative = schema.get("non_negative", [])

    errors, warnings = [], []
    seen = set()

    if not candidates:
        errors.append("dataset has no candidates")

    for c in candidates:
        cid = c.get("id", "<missing id>")

        for field in required:
            if field not in c:
                errors.append(f"{cid}: missing required field {field!r}")

        if cid in seen:
            errors.append(f"{cid}: duplicate id")
        seen.add(cid)

        for f in ratings:
            v = c.get(f)
            if v is not None and (not isinstance(v, (int, float)) or not 1 <= v <= 5):
                errors.append(f"{cid}: {f}={v!r} outside the 1-5 scale")

        for field, allowed in enums.items():
            if field in c and c[field] not in allowed:
                errors.append(f"{cid}: {field}={c[field]!r} not one of {allowed}")

        for f in non_negative:
            v = c.get(f)
            if isinstance(v, (int, float)) and v < 0:
                errors.append(f"{cid}: {f}={v} is negative")

        conf = c.get("confidence")
        if conf is not None and conf not in VALID_CONFIDENCE:
            errors.append(f"{cid}: confidence={conf!r} invalid")
        if conf == "low":
            warnings.append(f"{cid}: low confidence — unverified, report must say so")

        if not c.get("sources"):
            warnings.append(f"{cid}: no sources cited")

    if not dataset.get("_meta", {}).get("reference_spec"):
        warnings.append(
            "_meta.reference_spec is missing — without a fixed spec the rows may "
            "not be comparable at all"
        )

    print(f"validated {len(candidates)} candidates from {data_path}")

    if warnings:
        print(f"\n{len(warnings)} warning(s):")
        for w in warnings:
            print(f"  ! {w}")

    if errors:
        print(f"\n{len(errors)} ERROR(s):")
        for e in errors:
            print(f"  x {e}")
        return 1

    print("\nno errors")
    return 0


if __name__ == "__main__":
    sys.exit(main())
