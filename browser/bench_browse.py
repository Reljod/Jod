"""Time a browse, cold and warm, through the same browser Hermes uses.

The number this exists to produce: what one page costs when every page pays for
a Firefox launch, against what the second and third page cost when the browser
is already running.

    bench_browse.py cold <url>            one-shot CLI: one Firefox per URL
    bench_browse.py warm <url> [N]        resident server: one launch, N pages
    bench_browse.py status-first <url>    the call-order bug: status, then navigate
    bench_browse.py direct <url>          the same navigation with no proxy

Times are wall-clock seconds, printed one per line, measured around the whole
call so they include the wait an agent actually experiences.
"""

from __future__ import annotations

import json
import os
import select
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
CLI = HERE / "jodbrowser.py"
SERVER = HERE / "jod_browser_mcp.py"
READ_TIMEOUT = 120.0


def _child_env(direct: bool = False) -> dict:
    env = dict(os.environ)
    if direct:
        env["JOD_PROXY_SERVER"] = ""
    return env


def cold(url: str, direct: bool = False) -> float:
    """One Firefox, one page, process exit. What an agent does today."""
    started = time.monotonic()
    proc = subprocess.run(
        [sys.executable, str(CLI), url],
        capture_output=True, text=True, env=_child_env(direct), timeout=300,
    )
    elapsed = time.monotonic() - started
    if proc.returncode != 0:
        print(f"# cold failed rc={proc.returncode}: {proc.stderr.strip()[:200]}")
    return elapsed


class Server:
    """The MCP server over stdio, which is how the harness talks to it."""

    def __init__(self, direct: bool = False) -> None:
        self.proc = subprocess.Popen(
            [sys.executable, str(SERVER)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True, bufsize=1,
            env=_child_env(direct),
        )
        self.rid = 0

    def _write(self, frame: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(frame) + "\n")
        self.proc.stdin.flush()

    def _read(self, timeout: float = READ_TIMEOUT) -> dict:
        assert self.proc.stdout is not None
        ready, _, _ = select.select([self.proc.stdout], [], [], timeout)
        if not ready:
            raise TimeoutError(f"no response in {timeout:.0f}s")
        return json.loads(self.proc.stdout.readline())

    def initialize(self) -> None:
        self.rid += 1
        self._write({"jsonrpc": "2.0", "id": self.rid, "method": "initialize",
                     "params": {"protocolVersion": "2025-06-18",
                                "clientInfo": {"name": "bench", "version": "0"},
                                "capabilities": {}}})
        self._read()
        self._write({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call(self, tool: str, arguments: dict | None = None,
             timeout: float = READ_TIMEOUT) -> tuple[float, dict]:
        self.rid += 1
        started = time.monotonic()
        self._write({"jsonrpc": "2.0", "id": self.rid, "method": "tools/call",
                     "params": {"name": tool, "arguments": arguments or {}}})
        answer = self._read(timeout)
        return time.monotonic() - started, answer

    def close(self) -> None:
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
            self.proc.wait(timeout=30)
        except Exception:  # noqa: BLE001 — the numbers are already collected
            self.proc.kill()


def main(argv: list[str]) -> int:
    if len(argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    mode, url = argv[1], argv[2]

    if mode == "cold":
        print(f"cold_one_shot {cold(url):.2f}")
        return 0

    if mode == "direct":
        print(f"direct_cold_one_shot {cold(url, direct=True):.2f}")
        return 0

    if mode in ("warm", "status-first"):
        count = int(argv[3]) if len(argv) > 3 else 3
        server = Server()
        try:
            started = time.monotonic()
            server.initialize()
            print(f"server_startup {time.monotonic() - started:.2f}")
            if mode == "status-first":
                took, answer = server.call("browser_status")
                text = answer.get("result", {}).get("content", [{}])[0].get("text", "")
                print(f"status_first_call {took:.2f}  (egress: {text.splitlines()[-1][:60]})")
            for i in range(count):
                took, answer = server.call(
                    "browser_open" if i else "browse", {"url": url})
                note = ""
                if answer.get("result", {}).get("isError"):
                    note = "  ERROR: " + str(answer["result"])[:120]
                print(f"{'navigate' if i else 'navigate_with_launch'}_warm_{i + 1} "
                      f"{took:.2f}{note}")
        except TimeoutError as e:
            print(f"# timed out: {e}")
        finally:
            server.close()
        return 0

    if mode == "read":
        count = int(argv[3]) if len(argv) > 3 else 2
        server = Server()
        try:
            server.initialize()
            for i in range(count):
                tool = "browse" if i == 0 else "browser_read"
                args = {"url": url} if i == 0 else {}
                took, answer = server.call(tool, args)
                text = answer.get("result", {}).get("content", [{}])[0].get("text", "")
                body = text.split("\n\n", 1)[-1].strip()
                print(f"{tool}_{i + 1} {took:.2f}s  {len(body)} chars  "
                      f"| {body.replace(chr(10), ' ')[:90]}")
        finally:
            server.close()
        return 0

    print(f"unknown mode {mode}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
