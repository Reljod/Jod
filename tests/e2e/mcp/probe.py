"""Speak MCP to `jod mcp` the way a harness would, and check the answers.

Not a unit test: this launches the real binary, writes real JSON-RPC to its
stdin and reads its stdout. The point is to prove the wire protocol works, not
that a function returns what its test says it does.
"""
import json
import os
import subprocess
import sys

BIN = "./target/release/jod"
HOME = "/tmp/jod-mcp-demo"


def call(access, requests):
    """Send `requests` to one server process and return the parsed replies."""
    env = {**os.environ, "JOD_HOME": HOME}
    payload = "".join(json.dumps(r) + "\n" for r in requests)
    p = subprocess.run(
        [BIN, "mcp", "--access", access],
        input=payload,
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )
    out = []
    for line in p.stdout.splitlines():
        line = line.strip()
        if line:
            out.append(json.loads(line))
    if p.returncode != 0:
        print(f"  (exit {p.returncode}) stderr: {p.stderr.strip()[:200]}")
    return out


def rpc(i, method, params=None):
    r = {"jsonrpc": "2.0", "id": i, "method": method}
    if params is not None:
        r["params"] = params
    return r


print("=== initialize ===")
[reply] = call("read-only", [rpc(1, "initialize", {"protocolVersion": "2024-11-05"})])
print("  protocol:", reply["result"].get("protocolVersion"))
print("  server:  ", reply["result"].get("serverInfo"))
print("  caps:    ", list(reply["result"].get("capabilities", {})))

print("\n=== tools/list, per access level ===")
names = {}
for level in ("read-only", "delegate", "orchestrate"):
    replies = call(level, [rpc(1, "initialize", {}), rpc(2, "tools/list")])
    tools = replies[-1]["result"]["tools"]
    names[level] = sorted(t["name"] for t in tools)
    print(f"  {level:12} {len(tools):2} tools: {', '.join(names[level])}")

print("\n=== the levels nest ===")
for lo, hi in (("read-only", "delegate"), ("delegate", "orchestrate")):
    missing = set(names[lo]) - set(names[hi])
    print(f"  {lo} ⊆ {hi}: {'yes' if not missing else f'NO — lost {missing}'}")

print("\n=== a tool above the caller's level is refused ===")
replies = call(
    "read-only",
    [
        rpc(1, "initialize", {}),
        rpc(2, "tools/call", {"name": "delegate", "arguments": {"prompt": "do a thing"}}),
    ],
)
last = replies[-1]
if "error" in last:
    print("  refused:", last["error"].get("message", "")[:120])
else:
    body = json.dumps(last.get("result", ""))[:160]
    print("  ANSWERED (check whether it actually acted):", body)

print("\n=== an unknown method is a proper JSON-RPC error ===")
replies = call("read-only", [rpc(1, "initialize", {}), rpc(2, "no/such/method")])
print("  ", replies[-1].get("error"))

print("\n=== a malformed line does not kill the server ===")
env = {**os.environ, "JOD_HOME": HOME}
payload = (
    json.dumps(rpc(1, "initialize", {})) + "\n"
    + "{ this is not json\n"
    + json.dumps(rpc(2, "tools/list")) + "\n"
)
p = subprocess.run(
    [BIN, "mcp", "--access", "read-only"],
    input=payload, capture_output=True, text=True, env=env, timeout=60,
)
lines = [l for l in p.stdout.splitlines() if l.strip()]
print(f"  sent 3 lines (one garbage), got {len(lines)} replies back")
survived = any('"tools"' in l for l in lines)
print("  server answered the request AFTER the garbage:", survived)

print("\n=== a read-only tool actually works ===")
replies = call(
    "read-only",
    [rpc(1, "initialize", {}), rpc(2, "tools/call", {"name": "list_agents", "arguments": {}})],
)
res = replies[-1].get("result") or replies[-1].get("error")
print("  ", json.dumps(res)[:220])
