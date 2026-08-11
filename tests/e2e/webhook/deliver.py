#!/usr/bin/env python3
"""Send real, signed GitHub deliveries at a running `jod-api` and report what
each one did.

The point is the *whole* path — HMAC verification, rule matching, condition
narrowing, the delivery ledger — against the running binary rather than against
a test harness that shares its assumptions. Each case states what it expects and
the script exits non-zero if any case disagrees.

    cargo build --release --bin jod --bin jod-api
    python3 tests/e2e/webhook/deliver.py
"""
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

HOME = "/tmp/jod-hook-deliver"
SECRET = "not-a-real-secret-just-for-this-test"
PORT = int(os.environ.get("PORT", "8477"))
URL = f"http://127.0.0.1:{PORT}/webhooks/github"
JOD = os.environ.get("JOD", "./target/release/jod")
API = os.environ.get("JOD_API", "./target/release/jod-api")


def jod(*args):
    env = dict(os.environ, JOD_HOME=HOME)
    r = subprocess.run([JOD, *args], capture_output=True, text=True, env=env)
    return r.stdout.strip()


def post(event, payload, secret=SECRET, delivery=None):
    """One delivery. Returns (status, body)."""
    body = json.dumps(payload).encode()
    tag = hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    req = urllib.request.Request(
        URL,
        data=body,
        headers={
            "Content-Type": "application/json",
            "X-GitHub-Event": event,
            "X-GitHub-Delivery": delivery or f"d-{time.time_ns()}",
            "X-Hub-Signature-256": f"sha256={tag}",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def pr(title, author, labels, action="opened", repo="Reljod/Jod", draft=False):
    return {
        "action": action,
        "repository": {"full_name": repo},
        "pull_request": {
            "title": title,
            "user": {"login": author},
            "labels": [{"name": n} for n in labels],
            "draft": draft,
            "number": 7,
            "head": {"ref": "feat/x"},
            "body": "",
        },
    }


def main():
    subprocess.run(["rm", "-rf", HOME], check=True)
    workdir = f"{HOME}/work"
    os.makedirs(workdir, exist_ok=True)
    # `--cwd` inside the allowlist below. A rule naming a directory the API is
    # not allowed to run in is refused at delivery time, which is the control
    # being exercised — not a detail of this fixture.
    print(jod("webhook", "add", "urgent-prs",
              "A PR needs attention: {{title}} by {{author}}.",
              "--event", "pull_request", "--action", "opened",
              "--repo", "Reljod/Jod", "--label", "urgent", "--cwd", workdir))

    # An unconfigured allowlist means "allow nothing", the same way an
    # unconfigured secret means "accept nothing". Without this the matched
    # delivery lands in the ledger as `failed` with
    # "no working directory is allowed" — correct, and not what we are testing.
    api = subprocess.Popen(
        [API, "serve", "--bind", f"127.0.0.1:{PORT}"],
        env=dict(os.environ, JOD_HOME=HOME, JOD_GITHUB_WEBHOOK_SECRET=SECRET,
                 JOD_API_ALLOWED_CWD=workdir),
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    try:
        for _ in range(60):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{PORT}/", timeout=1)
                break
            except urllib.error.HTTPError:
                break          # answering at all is enough
            except OSError:
                time.sleep(0.25)
        else:
            print("the API never came up", file=sys.stderr)
            return 1

        cases = [
            ("matches the rule",
             ("pull_request", pr("Fix the thing", "reljod", ["urgent"])),
             {}, "accepted"),
            ("wrong label — the condition narrows it",
             ("pull_request", pr("Tidy up", "reljod", ["chore"])),
             {}, "no_match"),
            ("wrong action",
             ("pull_request", pr("Fix", "reljod", ["urgent"], action="closed")),
             {}, "no_match"),
            ("wrong repo",
             ("pull_request", pr("Fix", "x", ["urgent"], repo="someone/else")),
             {}, "no_match"),
            ("a forged signature is refused",
             ("pull_request", pr("Fix the thing", "reljod", ["urgent"])),
             {"secret": "wrong-secret"}, None),
        ]

        failures = []
        for label, (event, payload), kw, expect in cases:
            status, body = post(event, payload, **kw)
            print(f"\n--- {label}")
            print(f"    HTTP {status}  {body[:160]}")
            if expect is None:
                if status < 400:
                    failures.append(f"{label}: a forged signature was accepted")
                else:
                    print("    refused, as it must be")
                continue
            # 202 for a delivery that started an agent, 200 for one that
            # matched nothing. Both are "the endpoint dealt with it"; the
            # distinction between them is the `status` field, checked below.
            if status not in (200, 202):
                failures.append(f"{label}: HTTP {status}")
                continue
            if expect not in body:
                failures.append(f"{label}: expected {expect!r} in {body!r}")

        print("\n=== the ledger ===")
        print(jod("webhook", "deliveries"))
        print("\n=== runs it started ===")
        print(jod("ls") or "(none)")

        if failures:
            print("\nFAILURES:")
            for f in failures:
                print(" -", f)
            return 1
        print("\nall cases behaved as stated")
        return 0
    finally:
        api.terminate()
        try:
            api.wait(timeout=10)
        except subprocess.TimeoutExpired:
            api.kill()


if __name__ == "__main__":
    sys.exit(main())
