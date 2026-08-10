#!/usr/bin/env python3
"""Find code that is built and tested but wired to nothing.

The lead's standing note: a feature that runs, returns success and demonstrably
does nothing is the most valuable finding here. `Store::new_conversation` was
exactly that shape — a fully unit-tested function whose only callers were its
own tests — and it made eight `jod conv` subcommands inert.

So: every `pub fn` defined outside a `#[cfg(test)]` block in `core/`, against
every call site outside a `#[cfg(test)]` block anywhere in the workspace.
Zero production call sites is a candidate, not a verdict — trait impls, public
API surface for the Tauri app, and functions reached through a generic all show
up here. Each hit still has to be confirmed by running the command.

    wiring.py <path-to-source-root>
"""
import os
import re
import sys
import collections

root = sys.argv[1] if len(sys.argv) > 1 else "."


def production(path):
    """The part of a file that is not its test module."""
    src = open(path, encoding="utf-8").read()
    m = re.search(r"^#\[cfg\(test\)\]", src, re.M)
    return src[: m.start()] if m else src


def walk(rel):
    for base, _, files in os.walk(os.path.join(root, rel)):
        for f in files:
            if f.endswith(".rs"):
                yield os.path.join(base, f)


defs = {}
for p in walk("core/src"):
    for m in re.finditer(r"^\s*pub (?:async )?fn (\w+)", production(p), re.M):
        defs.setdefault(m.group(1), os.path.relpath(p, root))

uses = collections.Counter()
for d in ("core/src", "cli/src", "api/src", "supervisor/src"):
    for p in walk(d):
        src = production(p)
        for name in defs:
            for m in re.finditer(r"\b" + re.escape(name) + r"\s*\(", src):
                bol = src.rfind("\n", 0, m.start()) + 1
                line = src[bol : src.find("\n", m.start())]
                if re.match(r"\s*pub (?:async )?fn ", line):
                    continue  # the definition itself
                uses[name] += 1

never = sorted(((n, p) for n, p in defs.items() if uses[n] == 0), key=lambda x: (x[1], x[0]))
print(f"{len(defs)} pub fns in core/ · {len(never)} with no production call site\n")
for name, path in never:
    print(f"  {path:30} {name}")
